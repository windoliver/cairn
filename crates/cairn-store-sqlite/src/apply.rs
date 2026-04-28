//! `MemoryStoreApply` + `MemoryStoreApplyTx` implementation.
//!
//! Every write runs inside one `tokio::task::spawn_blocking` that holds the
//! connection mutex for the duration of the closure. An explicit `BEGIN
//! IMMEDIATE` / `COMMIT` / `ROLLBACK` SQL sequence manages the transaction
//! so we never need to hold a `rusqlite::Transaction` (which is `!Send`)
//! across the `spawn_blocking` boundary.
//!
//! The `SqliteApplyTx` wraps the `SharedConn` (`Arc<tokio::sync::Mutex<Connection>>`)
//! and acquires the blocking lock once inside `spawn_blocking`. All write
//! methods lock-acquire on the already-held mutex guard via `blocking_lock()`.
//! Since all work happens on the same blocking thread, there is no contention
//! and no risk of deadlock.

use cairn_core::contract::memory_store::{
    apply::{ApplyToken, MemoryStoreApply, MemoryStoreApplyTx, private::Sealed},
    error::{ConflictKind, StoreError},
    types::{
        ConsentJournalEntry, ConsentJournalRowId, Edge, EdgeKind, OpId, PurgeOutcome, RecordId,
        TargetId,
    },
};
use cairn_core::domain::{actor_ref::ActorRef, record::MemoryRecord, timestamp::Rfc3339Timestamp};
use rusqlite::{Connection, params};

use crate::conn::SharedConn;
use crate::rowmap::store_err;

impl Sealed for crate::SqliteMemoryStore {}

#[async_trait::async_trait]
impl MemoryStoreApply for crate::SqliteMemoryStore {
    async fn with_apply_tx<F, T>(&self, _token: ApplyToken, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            // Phase 1: begin the transaction.
            // Acquire lock → issue BEGIN IMMEDIATE → release lock.
            // BEGIN IMMEDIATE acquires the SQLite write lock at the DB level,
            // preventing any other connection from starting a write transaction.
            // The Rust Mutex is released so individual method calls can re-acquire it.
            {
                let guard = conn.blocking_lock();
                guard.execute_batch("BEGIN IMMEDIATE").map_err(store_err)?;
                // `guard` drops here — Mutex released, SQLite write lock still held.
            }

            let mut tx = SqliteApplyTx { conn: conn.clone() };

            // Phase 2: run the user closure inside catch_unwind so a panic in `f`
            // still triggers a ROLLBACK before propagating. Without this, a panic
            // in the closure would leave an open transaction on the pooled connection,
            // blocking all subsequent writes until the connection is dropped.
            //
            // `AssertUnwindSafe` is sound here: the closure is `Send + 'static` (the
            // trait bound), `SqliteApplyTx` holds only an `Arc<Mutex<Connection>>`,
            // and we propagate the panic payload unchanged via `resume_unwind`, so
            // callers observe the same abort semantics they would without the wrapper.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                f(&mut tx as &mut dyn MemoryStoreApplyTx)
            }));

            // Phase 3: commit, rollback on error, or rollback + re-panic.
            let guard = conn.blocking_lock();
            match outcome {
                Ok(Ok(v)) => {
                    guard.execute_batch("COMMIT").map_err(store_err)?;
                    Ok(v)
                }
                Ok(Err(e)) => {
                    let _ = guard.execute_batch("ROLLBACK");
                    Err(e)
                }
                Err(payload) => {
                    let _ = guard.execute_batch("ROLLBACK");
                    // `guard` drops before resume so the lock is not held across
                    // the unwind.
                    drop(guard);
                    std::panic::resume_unwind(payload);
                }
            }
            // `guard` drops here for the Ok/Err arms.
        })
        .await
        .map_err(|e| StoreError::Backend(Box::new(e)))?
    }
}

/// In-transaction write handle.
///
/// Holds a reference to the shared connection. All methods acquire the
/// connection mutex synchronously via `blocking_lock()` — safe because we
/// are already on a `spawn_blocking` thread and no other task is competing
/// (the outer `with_apply_tx` holds the `MutexGuard` for the outer
/// `BEGIN`/`COMMIT` sequence).
///
/// `SqliteApplyTx` is `Send` because `SharedConn` (`Arc<Mutex<Connection>>`)
/// is `Send` even though `Connection` itself is `!Sync`.
struct SqliteApplyTx {
    conn: SharedConn,
}

impl Sealed for SqliteApplyTx {}

// Design note: each method acquires the connection mutex for the duration of
// its SQL call, then releases it. The overall transaction is maintained by
// the explicit `BEGIN IMMEDIATE` / `COMMIT` / `ROLLBACK` SQL commands in
// `with_apply_tx`. The Mutex serializes concurrent access from read tasks;
// the SQLite write lock from `BEGIN IMMEDIATE` prevents any other writer from
// interleaving at the database level.

impl MemoryStoreApplyTx for SqliteApplyTx {
    fn stage_version(
        &mut self,
        target_id: &TargetId,
        record: &MemoryRecord,
    ) -> Result<RecordId, StoreError> {
        let conn = self.conn.blocking_lock();
        stage_version_impl(&conn, target_id, record)
    }

    fn activate_version(
        &mut self,
        target_id: &TargetId,
        version: u64,
        expected_prior: Option<u64>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        activate_version_impl(&conn, target_id, version, expected_prior)
    }

    fn tombstone_target(
        &mut self,
        target_id: &TargetId,
        actor: &ActorRef,
    ) -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        tombstone_target_impl(&conn, target_id, actor)
    }

    fn expire_active(
        &mut self,
        target_id: &TargetId,
        at: Rfc3339Timestamp,
    ) -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        expire_active_impl(&conn, target_id, &at)
    }

    fn purge_target(
        &mut self,
        target_id: &TargetId,
        op_id: &OpId,
        actor: &ActorRef,
    ) -> Result<PurgeOutcome, StoreError> {
        let conn = self.conn.blocking_lock();
        purge_target_impl(&conn, target_id, op_id, actor)
    }

    fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        add_edge_impl(&conn, edge)
    }

    fn remove_edge(
        &mut self,
        from: &RecordId,
        to: &RecordId,
        kind: EdgeKind,
    ) -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        remove_edge_impl(&conn, from, to, kind)
    }

    fn append_consent_journal(
        &mut self,
        entry: &ConsentJournalEntry,
    ) -> Result<ConsentJournalRowId, StoreError> {
        let conn = self.conn.blocking_lock();
        append_consent_journal_impl(&conn, entry)
    }
}

// ---------------------------------------------------------------------------
// Implementation functions (take &Connection directly)
// ---------------------------------------------------------------------------

fn stage_version_impl(
    conn: &Connection,
    target_id: &TargetId,
    record: &MemoryRecord,
) -> Result<RecordId, StoreError> {
    let target_id_str = target_id.as_str();

    let next: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM records WHERE target_id = ?1",
            params![target_id_str],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    let version = u64::try_from(next).unwrap_or(1);

    let record_id = RecordId::from_target_version(target_id, version);

    let body = &record.body;
    let provenance = serde_json::to_string(&record.provenance)?;
    let actor_chain = serde_json::to_string(&record.actor_chain)?;
    let evidence = serde_json::to_string(&record.evidence)?;
    let scope = serde_json::to_string(&record.scope)?;
    let taxonomy = serde_json::json!({
        "kind": record.kind,
        "class": record.class,
        "visibility": record.visibility,
    })
    .to_string();
    let record_json = serde_json::to_string(record)?;

    let created_at = Rfc3339Timestamp::now();
    let created_by = record
        .actor_chain
        .iter()
        .find(|e| matches!(e.role, cairn_core::domain::actor_chain::ChainRole::Author))
        .map_or("system", |e| e.identity.as_str());

    let version_i64 =
        i64::try_from(version).map_err(|_| StoreError::Invariant("version overflows i64"))?;
    conn.execute(
        "INSERT INTO records ( \
             record_id, target_id, version, active, tombstoned, \
             created_at, created_by, body, provenance, actor_chain, \
             evidence, scope, taxonomy, confidence, salience, record_json \
         ) VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            record_id.as_str(),
            target_id_str,
            version_i64,
            created_at.as_str(),
            created_by,
            body,
            provenance,
            actor_chain,
            evidence,
            scope,
            taxonomy,
            f64::from(record.confidence),
            f64::from(record.salience),
            record_json,
        ],
    )
    .map_err(|e| {
        if let rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _,
        ) = &e
        {
            StoreError::Conflict {
                kind: ConflictKind::VersionAlreadyStaged,
            }
        } else {
            store_err(e)
        }
    })?;

    Ok(record_id)
}

fn activate_version_impl(
    conn: &Connection,
    target_id: &TargetId,
    version: u64,
    expected_prior: Option<u64>,
) -> Result<(), StoreError> {
    let version_i64 =
        i64::try_from(version).map_err(|_| StoreError::Invariant("version overflows i64"))?;

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND version = ?2",
            params![target_id.as_str(), version_i64],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if exists == 0 {
        return Err(StoreError::NotFound(target_id.clone()));
    }

    let current: Option<i64> = conn
        .query_row(
            "SELECT version FROM records WHERE target_id = ?1 AND active = 1",
            params![target_id.as_str()],
            |r| r.get(0),
        )
        .ok();

    if let Some(cur) = current {
        let cur_u64 = u64::try_from(cur).unwrap_or(0);
        if let Some(expected) = expected_prior
            && cur_u64 != expected
        {
            return Err(StoreError::Conflict {
                kind: ConflictKind::ActivationRaced,
            });
        }
        if cur_u64 >= version {
            return Err(StoreError::Conflict {
                kind: ConflictKind::ActivationRaced,
            });
        }
    } else if expected_prior.is_some() {
        return Err(StoreError::Conflict {
            kind: ConflictKind::ActivationRaced,
        });
    }

    conn.execute(
        "UPDATE records SET active = (CAST(version AS INTEGER) = ?2) WHERE target_id = ?1",
        params![target_id.as_str(), version_i64],
    )
    .map_err(store_err)?;

    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1",
            params![target_id.as_str()],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if active_count != 1 {
        return Err(StoreError::Invariant(
            "activate_version: post-update active count != 1",
        ));
    }

    Ok(())
}

fn tombstone_target_impl(
    conn: &Connection,
    target_id: &TargetId,
    actor: &ActorRef,
) -> Result<(), StoreError> {
    let now = Rfc3339Timestamp::now();
    conn.execute(
        "UPDATE records \
         SET tombstoned = 1, \
             tombstoned_at = COALESCE(tombstoned_at, ?2), \
             tombstoned_by = COALESCE(tombstoned_by, ?3) \
         WHERE target_id = ?1",
        params![target_id.as_str(), now.as_str(), actor.as_str()],
    )
    .map_err(store_err)?;
    Ok(())
}

fn expire_active_impl(
    conn: &Connection,
    target_id: &TargetId,
    at: &Rfc3339Timestamp,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE records \
         SET expired_at = COALESCE(expired_at, ?2) \
         WHERE target_id = ?1 AND active = 1",
        params![target_id.as_str(), at.as_str()],
    )
    .map_err(store_err)?;
    Ok(())
}

fn purge_target_impl(
    conn: &Connection,
    target_id: &TargetId,
    op_id: &OpId,
    actor: &ActorRef,
) -> Result<PurgeOutcome, StoreError> {
    let existing: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM record_purges WHERE target_id = ?1 AND op_id = ?2",
            params![target_id.as_str(), op_id.as_str()],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if existing > 0 {
        return Ok(PurgeOutcome::AlreadyPurged);
    }

    let mut stmt = conn
        .prepare("SELECT record_id FROM records WHERE target_id = ?1")
        .map_err(store_err)?;
    let record_ids: Vec<String> = stmt
        .query_map(params![target_id.as_str()], |r| r.get(0))
        .map_err(store_err)?
        .collect::<Result<_, _>>()
        .map_err(store_err)?;
    drop(stmt);

    let now = Rfc3339Timestamp::now();
    let salt_input = format!("{}{}{}", now.as_str(), target_id.as_str(), op_id.as_str());
    let salt = blake3::hash(salt_input.as_bytes()).to_hex().to_string();

    conn.execute(
        "INSERT INTO record_purges (target_id, op_id, purged_at, purged_by, body_hash_salt) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            target_id.as_str(),
            op_id.as_str(),
            now.as_str(),
            actor.as_str(),
            salt,
        ],
    )
    .map_err(store_err)?;

    for rid in &record_ids {
        conn.execute(
            "DELETE FROM edges WHERE from_id = ?1 OR to_id = ?1",
            params![rid],
        )
        .map_err(store_err)?;
        conn.execute(
            "DELETE FROM edge_versions WHERE from_id = ?1 OR to_id = ?1",
            params![rid],
        )
        .map_err(store_err)?;
    }

    // FTS rows removed automatically by AFTER DELETE trigger on records.

    conn.execute(
        "DELETE FROM records WHERE target_id = ?1",
        params![target_id.as_str()],
    )
    .map_err(store_err)?;

    Ok(PurgeOutcome::Purged)
}

fn add_edge_impl(conn: &Connection, edge: &Edge) -> Result<(), StoreError> {
    let metadata = serde_json::to_string(&edge.metadata)?;
    let now = Rfc3339Timestamp::now();
    let kind_str = edge_kind_str(edge.kind);

    let prior: Option<(f64, String)> = conn
        .query_row(
            "SELECT weight, metadata FROM edges \
             WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
            params![edge.from.as_str(), edge.to.as_str(), kind_str],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

    let change_kind = if prior.is_some() { "update" } else { "insert" };
    if let Some((w, m)) = &prior {
        conn.execute(
            "INSERT INTO edge_versions \
             (from_id, to_id, kind, weight, metadata, change_kind, at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                edge.from.as_str(),
                edge.to.as_str(),
                kind_str,
                w,
                m,
                change_kind,
                now.as_str()
            ],
        )
        .map_err(store_err)?;
    } else {
        conn.execute(
            "INSERT INTO edge_versions \
             (from_id, to_id, kind, change_kind, at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                edge.from.as_str(),
                edge.to.as_str(),
                kind_str,
                change_kind,
                now.as_str()
            ],
        )
        .map_err(store_err)?;
    }

    conn.execute(
        "INSERT INTO edges (from_id, to_id, kind, weight, metadata, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(from_id, to_id, kind) DO UPDATE SET \
             weight = excluded.weight, \
             metadata = excluded.metadata",
        params![
            edge.from.as_str(),
            edge.to.as_str(),
            kind_str,
            f64::from(edge.weight),
            metadata,
            now.as_str(),
        ],
    )
    .map_err(store_err)?;

    Ok(())
}

fn remove_edge_impl(
    conn: &Connection,
    from: &RecordId,
    to: &RecordId,
    kind: EdgeKind,
) -> Result<(), StoreError> {
    let now = Rfc3339Timestamp::now();
    let kind_str = edge_kind_str(kind);

    let prior: Option<(f64, String)> = conn
        .query_row(
            "SELECT weight, metadata FROM edges \
             WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
            params![from.as_str(), to.as_str(), kind_str],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

    if let Some((w, m)) = prior {
        conn.execute(
            "INSERT INTO edge_versions \
             (from_id, to_id, kind, weight, metadata, change_kind, at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'remove', ?6)",
            params![from.as_str(), to.as_str(), kind_str, w, m, now.as_str()],
        )
        .map_err(store_err)?;
    }

    conn.execute(
        "DELETE FROM edges WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3",
        params![from.as_str(), to.as_str(), kind_str],
    )
    .map_err(store_err)?;

    Ok(())
}

fn append_consent_journal_impl(
    conn: &Connection,
    entry: &ConsentJournalEntry,
) -> Result<ConsentJournalRowId, StoreError> {
    let payload = serde_json::to_string(&entry.payload)?;
    conn.execute(
        "INSERT INTO consent_journal \
         (op_id, kind, target_id, actor, payload, at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.op_id.as_str(),
            entry.kind,
            entry.target_id.as_ref().map(TargetId::as_str),
            entry.actor.as_str(),
            payload,
            entry.at.as_str(),
        ],
    )
    .map_err(store_err)?;
    let id = conn.last_insert_rowid();
    Ok(ConsentJournalRowId(id))
}

fn edge_kind_str(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::Refines => "refines",
        EdgeKind::Contradicts => "contradicts",
        EdgeKind::DerivedFrom => "derived_from",
        EdgeKind::SeeAlso => "see_also",
        EdgeKind::Mentions => "mentions",
        _ => "unknown",
    }
}
