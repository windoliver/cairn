//! `MemoryStoreApply` + `MemoryStoreApplyTx` implementation.
//!
//! Every write runs inside one `tokio::task::spawn_blocking` that holds the
//! connection mutex for the **entire duration** of the closure — from `BEGIN
//! IMMEDIATE` through `COMMIT`/`ROLLBACK`. This prevents concurrent read
//! tasks (`get`, `list`, `version_history`) from acquiring the same mutex and
//! observing uncommitted state on the shared single connection.
//!
//! An explicit `BEGIN IMMEDIATE` / `COMMIT` / `ROLLBACK` SQL sequence manages
//! the transaction so we never need to hold a `rusqlite::Transaction` (which
//! is `!Send`) across the `spawn_blocking` boundary.
//!
//! The `SqliteMemoryStoreApplyTx` wrapper borrows `&Connection` directly from
//! the held `MutexGuard`. Its lifetime is confined to this blocking task; it
//! cannot escape the closure.

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

use crate::rowmap::store_err;

impl Sealed for crate::SqliteMemoryStore {}

#[async_trait::async_trait]
impl MemoryStoreApply for crate::SqliteMemoryStore {
    async fn with_apply_tx<F, T>(&self, _token: ApplyToken, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        if self.is_probe {
            return Err(crate::store::PROBE_REJECT);
        }
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            // Acquire the connection mutex for the ENTIRE closure execution.
            // No other task can read or write through this connection until we
            // return and the guard is dropped. This guarantees that concurrent
            // readers (get/list/version_history) cannot observe uncommitted
            // state from this in-progress write transaction.
            let guard = conn.blocking_lock();

            guard.execute_batch("BEGIN IMMEDIATE").map_err(store_err)?;

            // Borrow `&Connection` directly from the held guard so the wrapper
            // can call SQL methods without any additional locking. The borrow
            // is valid for the lifetime of `guard` on this thread.
            let mut tx = SqliteMemoryStoreApplyTx { conn: &guard };

            // Run the user closure inside catch_unwind so a panic in `f`
            // still triggers a ROLLBACK before propagating. Without this, a
            // panic in the closure would leave an open transaction on the
            // connection, blocking all subsequent writes.
            //
            // `AssertUnwindSafe` is sound here: the closure is `Send + 'static`
            // (the trait bound), `SqliteMemoryStoreApplyTx` holds only a raw
            // reference scoped to this stack frame, and we propagate the panic
            // payload unchanged via `resume_unwind`.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                f(&mut tx as &mut dyn MemoryStoreApplyTx)
            }));

            match outcome {
                Ok(Ok(v)) => {
                    if let Err(commit_err) = guard.execute_batch("COMMIT") {
                        // SQLite can leave the transaction open after a
                        // failed COMMIT (busy, full disk, I/O error).
                        // Issue ROLLBACK so the next caller does not
                        // inherit an in-progress transaction on this
                        // shared connection. The rollback may also
                        // fail (e.g. connection genuinely broken); we
                        // best-effort ignore that failure but propagate
                        // the original commit error.
                        let _ = guard.execute_batch("ROLLBACK");
                        return Err(store_err(commit_err));
                    }
                    Ok(v)
                    // `guard` drops here — mutex released after COMMIT.
                }
                Ok(Err(e)) => {
                    let _ = guard.execute_batch("ROLLBACK");
                    Err(e)
                    // `guard` drops here — mutex released after ROLLBACK.
                }
                Err(payload) => {
                    let _ = guard.execute_batch("ROLLBACK");
                    // Drop `guard` explicitly so the mutex is released before
                    // unwinding; the lock is not held across the panic boundary.
                    drop(guard);
                    std::panic::resume_unwind(payload);
                }
            }
        })
        .await
        .map_err(|e| StoreError::Backend(Box::new(e)))?
    }
}

/// In-transaction write handle.
///
/// Borrows `&Connection` directly from the `MutexGuard` held by
/// `with_apply_tx`. All SQL calls go straight to the connection with no
/// additional locking — safe because the guard is held for the entire
/// duration of the blocking task.
struct SqliteMemoryStoreApplyTx<'conn> {
    conn: &'conn Connection,
}

impl Sealed for SqliteMemoryStoreApplyTx<'_> {}

impl MemoryStoreApplyTx for SqliteMemoryStoreApplyTx<'_> {
    fn stage_version(
        &mut self,
        target_id: &TargetId,
        record: &MemoryRecord,
        created_by: &ActorRef,
    ) -> Result<RecordId, StoreError> {
        stage_version_impl(self.conn, target_id, record, created_by)
    }

    fn activate_version(
        &mut self,
        target_id: &TargetId,
        version: u64,
        expected_prior: Option<u64>,
        activated_by: &ActorRef,
    ) -> Result<(), StoreError> {
        activate_version_impl(self.conn, target_id, version, expected_prior, activated_by)
    }

    fn tombstone_target(
        &mut self,
        target_id: &TargetId,
        actor: &ActorRef,
    ) -> Result<(), StoreError> {
        tombstone_target_impl(self.conn, target_id, actor)
    }

    fn expire_active(
        &mut self,
        target_id: &TargetId,
        at: Rfc3339Timestamp,
    ) -> Result<(), StoreError> {
        expire_active_impl(self.conn, target_id, &at)
    }

    fn purge_target(
        &mut self,
        target_id: &TargetId,
        op_id: &OpId,
        actor: &ActorRef,
    ) -> Result<PurgeOutcome, StoreError> {
        purge_target_impl(self.conn, target_id, op_id, actor)
    }

    fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError> {
        add_edge_impl(self.conn, edge)
    }

    fn remove_edge(
        &mut self,
        from: &RecordId,
        to: &RecordId,
        kind: EdgeKind,
    ) -> Result<(), StoreError> {
        remove_edge_impl(self.conn, from, to, kind)
    }

    fn append_consent_journal(
        &mut self,
        entry: &ConsentJournalEntry,
    ) -> Result<ConsentJournalRowId, StoreError> {
        append_consent_journal_impl(self.conn, entry)
    }
}

// ---------------------------------------------------------------------------
// Implementation functions (take &Connection directly)
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "linear admission/INSERT scenario; splitting would obscure the audit invariants"
)]
fn stage_version_impl(
    conn: &Connection,
    target_id: &TargetId,
    record: &MemoryRecord,
    created_by: &ActorRef,
) -> Result<RecordId, StoreError> {
    // Domain validation: this is the last durable boundary before
    // `records`/`record_json`. Upstream callers should validate, but
    // relying on that is too weak — a malformed record (out-of-range
    // scalars, missing scope.user on a private record, empty/invalid
    // actor chain, sensor/role inconsistency) becomes unreadable
    // store state that requires manual repair. Reject early.
    record
        .validate()
        .map_err(|e| StoreError::Backend(Box::new(e)))?;

    let target_id_str = target_id.as_str();

    // Once a target_id has been purged, the namespace is permanently
    // retired. Re-staging would start at version 1 again and produce
    // the same deterministic `record_id = BLAKE3(target_id#1)` as the
    // purged record, splicing a new logical record into old audit
    // history. Reject stage_version against any target with an extant
    // purge marker — callers must use a fresh target_id.
    let purged: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM record_purges WHERE target_id = ?1",
            params![target_id_str],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if purged > 0 {
        return Err(StoreError::Conflict {
            kind: ConflictKind::UniqueViolation,
        });
    }

    // Tombstone retirement: design brief Phase A says a tombstoned
    // target must stay hidden until an explicit restore flow exists.
    // Without this gate, a caller could tombstone a target, stage a
    // fresh version under the same target_id, activate it, and surface
    // forgotten data again — bypassing the forget pipeline. Reject
    // stage_version against any target that has at least one tombstoned
    // row; restoration is a separate (not-yet-implemented) verb.
    let tombstoned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND tombstoned = 1",
            params![target_id_str],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if tombstoned > 0 {
        return Err(StoreError::Conflict {
            kind: ConflictKind::UniqueViolation,
        });
    }

    // Visibility-tier admission. The store admits any tier whose
    // read-side semantics are implemented in `rebac.rs`. Tiers
    // currently supported: private, session, project, public.
    // team/org remain blocked at write time because ScopeTuple does
    // not yet carry team/org membership dimensions — persisting them
    // would create records no caller can ever read again.
    match record.visibility {
        cairn_core::domain::MemoryVisibility::Private
        | cairn_core::domain::MemoryVisibility::Session
        | cairn_core::domain::MemoryVisibility::Project
        | cairn_core::domain::MemoryVisibility::Public => {}
        _ => {
            return Err(StoreError::Invariant(
                "visibility tier not yet supported: team/org require ScopeTuple \
                 dimensions that have not yet been added (brief-level change)",
            ));
        }
    }

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
    // FTS triggers (migration 0004) extract `$.title` and `$.tags`
    // from the stored taxonomy JSON. `tags` is a domain field on
    // MemoryRecord; `title` is conventional frontmatter and lives on
    // `extra_frontmatter`. Persist both so title/tag search hits
    // newly written rows. Tags are space-joined for FTS tokenization;
    // missing title falls through to the trigger's COALESCE-to-empty.
    let title = record
        .extra_frontmatter
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let tags_joined = record.tags.join(" ");
    let taxonomy = serde_json::json!({
        "kind": record.kind,
        "class": record.class,
        "visibility": record.visibility,
        "title": title,
        "tags": tags_joined,
    })
    .to_string();
    let record_json = serde_json::to_string(record)?;

    let created_at = Rfc3339Timestamp::now();
    // Trusted actor: passed in by the WAL executor. Do NOT derive
    // `created_by` from caller-controlled record fields like
    // `record.actor_chain` — those are payload data and forgeable, and
    // would let any caller with an `ApplyToken` misattribute writes.
    let created_by = created_by.as_str();

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
    activated_by: &ActorRef,
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

    // Tombstone retirement: a target with any tombstoned version is
    // retired (Phase A forget). Stage already rejects new versions for
    // such targets, but a delayed/retried activate against a version
    // staged before the tombstone could still flip `active` flags
    // post-retirement. Reads stay hidden because get/list filter on
    // tombstoned, but the lifecycle mutation skews purge snapshots
    // and audit history. Reject unconditionally.
    let tombstoned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND tombstoned = 1",
            params![target_id.as_str()],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if tombstoned > 0 {
        return Err(StoreError::Conflict {
            kind: ConflictKind::UniqueViolation,
        });
    }

    // Distinguish 'no active row' (legitimate first activation) from
    // backend errors (schema drift, corruption, lock failure). Mapping
    // every error to None would silently rewrite `active` flags on top
    // of a damaged database.
    let current: Option<i64> = match conn.query_row(
        "SELECT version FROM records WHERE target_id = ?1 AND active = 1",
        params![target_id.as_str()],
        |r| r.get(0),
    ) {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(store_err(e)),
    };

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

    // Mark the chosen version active and stamp the activation audit
    // columns. The trusted activator is supplied by the WAL executor
    // (separate from the stager — round-3 review found that conflating
    // the two hides who actually promoted the version). For idempotent
    // re-activations of the same version, keep the original
    // activated_at/by via COALESCE.
    let now = Rfc3339Timestamp::now();
    conn.execute(
        "UPDATE records SET \
             active = (CAST(version AS INTEGER) = ?2), \
             activated_at = CASE \
                 WHEN CAST(version AS INTEGER) = ?2 \
                 THEN COALESCE(activated_at, ?3) \
                 ELSE activated_at \
             END, \
             activated_by = CASE \
                 WHEN CAST(version AS INTEGER) = ?2 \
                 THEN COALESCE(activated_by, ?4) \
                 ELSE activated_by \
             END \
         WHERE target_id = ?1",
        params![
            target_id.as_str(),
            version_i64,
            now.as_str(),
            activated_by.as_str()
        ],
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
    let rows = conn
        .execute(
            "UPDATE records \
             SET tombstoned = 1, \
                 tombstoned_at = COALESCE(tombstoned_at, ?2), \
                 tombstoned_by = COALESCE(tombstoned_by, ?3) \
             WHERE target_id = ?1",
            params![target_id.as_str(), now.as_str(), actor.as_str()],
        )
        .map_err(store_err)?;
    // Bad target_id (or already-purged) → no rows touched. Fail loud
    // so consent-journal entries cannot record a mutation that never
    // landed in the records table.
    if rows == 0 {
        return Err(StoreError::NotFound(target_id.clone()));
    }
    Ok(())
}

fn expire_active_impl(
    conn: &Connection,
    target_id: &TargetId,
    at: &Rfc3339Timestamp,
) -> Result<(), StoreError> {
    // Tombstone retirement: stage_version and activate_version both
    // reject tombstoned targets to keep retired data frozen. expire
    // must enforce the same invariant — without it, a delayed retry
    // can append an Expire event after Phase A forget started, and
    // version_history would then surface an expire-after-tombstone
    // ordering on a target that should be frozen.
    let tombstoned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND tombstoned = 1",
            params![target_id.as_str()],
            |r| r.get(0),
        )
        .map_err(store_err)?;
    if tombstoned > 0 {
        return Err(StoreError::Conflict {
            kind: ConflictKind::UniqueViolation,
        });
    }

    let rows = conn
        .execute(
            "UPDATE records \
             SET expired_at = COALESCE(expired_at, ?2) \
             WHERE target_id = ?1 AND active = 1",
            params![target_id.as_str(), at.as_str()],
        )
        .map_err(store_err)?;
    // Bad target_id or no active row → fail loud (same reasoning as
    // tombstone above).
    if rows == 0 {
        return Err(StoreError::NotFound(target_id.clone()));
    }
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

    // Capture record_ids and a scope/taxonomy snapshot from one of the
    // versions so `version_history` can rebac-filter the purge marker
    // later. Use the active row when present; otherwise the highest
    // version. If the target has no rows AND no prior purge exists,
    // refuse to write a fake audit marker.
    let mut stmt = conn
        .prepare(
            "SELECT record_id, scope, taxonomy, active, version \
             FROM records WHERE target_id = ?1 ORDER BY active DESC, version DESC",
        )
        .map_err(store_err)?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map(params![target_id.as_str()], |r| {
            let rid: String = r.get(0)?;
            let scope: String = r.get(1)?;
            let taxonomy: String = r.get(2)?;
            Ok((rid, scope, taxonomy))
        })
        .map_err(store_err)?
        .collect::<Result<_, _>>()
        .map_err(store_err)?;
    drop(stmt);

    let record_ids: Vec<String> = rows.iter().map(|(rid, _, _)| rid.clone()).collect();
    if rows.is_empty() {
        return Err(StoreError::NotFound(target_id.clone()));
    }
    // Latest-version snapshot (kept for the legacy `scope_snapshot` /
    // `taxonomy_snapshot` columns; reads prefer `version_snapshots`).
    let (scope_snapshot, taxonomy_snapshot) = rows
        .first()
        .map(|(_, s, t)| (Some(s.clone()), Some(t.clone())))
        .unwrap_or_default();
    // Per-version snapshots (JSON array). version_history grants the
    // purge marker to a principal who could read at least one of these
    // pre-purge versions, so visibility-changing targets do not lose
    // deletion history for previously authorized readers.
    let version_snapshots = serde_json::to_string(
        &rows
            .iter()
            .map(|(_, scope, taxonomy)| {
                serde_json::json!({
                    "scope": serde_json::from_str::<serde_json::Value>(scope)
                        .unwrap_or(serde_json::Value::Null),
                    "taxonomy": serde_json::from_str::<serde_json::Value>(taxonomy)
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .collect::<Vec<_>>(),
    )?;

    let now = Rfc3339Timestamp::now();
    let salt_input = format!("{}{}{}", now.as_str(), target_id.as_str(), op_id.as_str());
    let salt = blake3::hash(salt_input.as_bytes()).to_hex().to_string();

    conn.execute(
        "INSERT INTO record_purges \
         (target_id, op_id, purged_at, purged_by, body_hash_salt, \
          scope_snapshot, taxonomy_snapshot, version_snapshots) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            target_id.as_str(),
            op_id.as_str(),
            now.as_str(),
            actor.as_str(),
            salt,
            scope_snapshot,
            taxonomy_snapshot,
            version_snapshots,
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

    // Endpoint integrity: both record_ids must exist in `records` AND
    // belong to a target that has not been tombstoned. Schema-level FKs
    // would require rebuilding the existing table; in the meantime
    // fail-closed at write time so dangling edges cannot persist
    // (purge cleanup walks `records`, so dangling rows would be
    // silently retained). The tombstone gate prevents new live
    // references from accumulating onto retired records during the
    // tombstone→purge window — without it, backlink/audit traversals
    // could keep revealing the forgotten target's existence.
    for endpoint in [edge.from.as_str(), edge.to.as_str()] {
        let row: Option<(i64,)> = match conn.query_row(
            "SELECT EXISTS( \
                 SELECT 1 FROM records r2 \
                 WHERE r2.target_id = ( \
                     SELECT target_id FROM records WHERE record_id = ?1 \
                 ) AND r2.tombstoned = 1 \
             ) FROM records WHERE record_id = ?1",
            params![endpoint],
            |r| Ok((r.get(0)?,)),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(store_err(e)),
        };
        let Some((target_tombstoned,)) = row else {
            return Err(StoreError::Conflict {
                kind: ConflictKind::ForeignKey,
            });
        };
        if target_tombstoned != 0 {
            return Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation,
            });
        }
    }

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

/// Canonicalize a `serde_json::Value` for stable equality comparison:
/// recursively sort object keys so the same logical document always
/// serializes to the same byte sequence regardless of input key order.
/// Used by the consent-journal idempotency path so that retries with
/// equivalent-but-reordered JSON do not get rejected as divergent.
fn canonicalize_json(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(String, serde_json::Value)> = map
                .iter()
                .map(|(k, val)| (k.clone(), canonicalize_json(val)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json).collect())
        }
        _ => v.clone(),
    }
}

fn append_consent_journal_impl(
    conn: &Connection,
    entry: &ConsentJournalEntry,
) -> Result<ConsentJournalRowId, StoreError> {
    let payload = serde_json::to_string(&entry.payload)?;
    let canonical_payload = serde_json::to_string(&canonicalize_json(&entry.payload))?;
    let target_id_str = entry.target_id.as_ref().map(TargetId::as_str);

    // The store owns the canonical journal time. Caller-supplied
    // `entry.at` is intentionally ignored: it is forgeable, can drift
    // across retries, and conflating "audit time" with caller wall
    // clock makes incident reconstruction harder. Stamping the `at`
    // here means the persisted timestamp reflects when the journal
    // row actually landed, and retries cannot silently rewrite it.
    let stamped_at = Rfc3339Timestamp::now();

    // Two partial unique indexes (migration 0013) give idempotency
    // without an in-band sentinel: one over `(op_id, kind, target_id)`
    // when target_id IS NOT NULL, and one over `(op_id, kind)` when
    // target_id IS NULL. Use the bare INSERT and let
    // `SqliteFailure(ConstraintViolation)` mean "row already exists" —
    // ON CONFLICT cannot reference partial indexes by predicate.
    let exec_result = conn.execute(
        "INSERT INTO consent_journal \
         (op_id, kind, target_id, actor, payload, at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.op_id.as_str(),
            entry.kind,
            target_id_str,
            entry.actor.as_str(),
            payload,
            stamped_at.as_str(),
        ],
    );

    let inserted = match exec_result {
        Ok(rows) => rows,
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _,
        )) => 0,
        Err(other) => return Err(store_err(other)),
    };

    if inserted == 0 {
        // Idempotent retry: fetch the existing row and check that the
        // logical content matches. We compare:
        //   - actor: exact string match
        //   - payload: canonicalized JSON equality (key order and
        //     whitespace ignored, so a retry whose JSON was rebuilt
        //     from a hashmap with different ordering still matches)
        //   - at: NOT compared. The `at` field is regenerated on
        //     replay/recovery; requiring byte-identical timestamps
        //     would turn legitimate retries into hard transaction
        //     failures. The original timestamp wins; the retry
        //     simply reuses the existing row id.
        // A retry whose canonical payload diverges from the original
        // is split-brain or tampering, and accepting it would let the
        // store quietly keep the older audit content while telling
        // the caller everything succeeded. Fail closed with
        // `Conflict { UniqueViolation }`.
        // Mirror the partial-index split: target_id IS NULL must look
        // up rows with NULL target_id, while NOT NULL must match
        // exactly. A naive `target_id = ?3` with a NULL bind value
        // would never match (NULL <> NULL in SQL).
        let lookup_sql = if target_id_str.is_some() {
            "SELECT id, actor, payload FROM consent_journal \
             WHERE op_id = ?1 AND kind = ?2 AND target_id = ?3"
        } else {
            "SELECT id, actor, payload FROM consent_journal \
             WHERE op_id = ?1 AND kind = ?2 AND target_id IS NULL"
        };
        let (id, existing_actor, existing_payload): (i64, String, String) = conn
            .query_row(
                lookup_sql,
                params![entry.op_id.as_str(), entry.kind, target_id_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(store_err)?;

        let existing_canonical = serde_json::from_str::<serde_json::Value>(&existing_payload)
            .map(|v| {
                serde_json::to_string(&canonicalize_json(&v)).unwrap_or(existing_payload.clone())
            })
            .unwrap_or(existing_payload);

        if existing_actor != entry.actor.as_str() || existing_canonical != canonical_payload {
            return Err(StoreError::Conflict {
                kind: ConflictKind::UniqueViolation,
            });
        }
        return Ok(ConsentJournalRowId(id));
    }

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
