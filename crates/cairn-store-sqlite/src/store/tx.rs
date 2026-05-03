//! Transactional closure surface: [`SqliteMemoryStore::with_tx`] +
//! [`StoreTx`].
//!
//! Verb layer code that needs atomic multi-statement writes (consolidate,
//! promote, supersede + edge fan-out) calls `store.with_tx(|tx| { … })`
//! instead of stitching together separate async trait calls. The closure
//! runs synchronously on the dedicated `tokio_rusqlite` worker thread
//! inside a single `rusqlite::Transaction`. Returning `Ok` commits;
//! returning `Err` drops the tx without committing → `SQLite` rolls back.
//!
//! `StoreTx` exposes the *write* verbs (`upsert`, `tombstone`, `put_edge`,
//! `remove_edge`). Read verbs are not exposed here in PR-A — read paths
//! that need a snapshot can call the async trait methods, or open their
//! own transaction in a future PR.
//!
//! `with_tx` is **inherent**, not on the [`MemoryStore`] trait, because
//! the trait must stay object-safe (`dyn MemoryStore`). Generic methods
//! break dyn-compatibility. The verb layer reaches into the concrete
//! `SqliteMemoryStore` for transactional work.
//!
//! [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore

use cairn_core::contract::memory_store::{Edge, EdgeKey, TombstoneReason, UpsertOutcome};
use cairn_core::domain::{MemoryRecord, RecordId, SessionId};
use rusqlite::{OptionalExtension as _, Transaction, params};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::projection::record_from_json;
use crate::store::upsert::upsert_in_tx;
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Transactional handle exposed to closures passed to
/// [`SqliteMemoryStore::with_tx`]. Methods are synchronous because the
/// closure already runs on the DB worker thread; awaiting from inside
/// would deadlock the worker.
pub struct StoreTx<'a> {
    pub(crate) tx: Transaction<'a>,
}

impl StoreTx<'_> {
    /// Synchronous upsert against the open transaction. Delegates to the
    /// same internal `upsert_in_tx` helper that
    /// `SqliteMemoryStore::do_upsert` uses, so this and the async trait
    /// method follow identical idempotency, version-bump, and `record_id`
    /// synthesis rules.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures, or
    /// [`StoreError::Invariant`] if a stored row violates a structural
    /// invariant (corrupt `body_hash`, version overflow, etc.).
    pub fn upsert(&mut self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        upsert_in_tx(&mut self.tx, record)
    }
    // Note: `upsert_in_tx` performs `record.validate()` first thing, so
    // structural failures surface as `StoreError::InvalidRecord` here too.

    /// Synchronous tombstone. Marks one specific `record_id` row as
    /// tombstoned with the given reason. Idempotent: re-tombstoning the
    /// same row writes the same flag without producing a new row.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError> {
        let now_ms = current_unix_ms();
        self.tx.execute(
            "UPDATE records \
                SET tombstoned = 1, tombstone_reason = ?1, updated_at = ?2 \
              WHERE record_id = ?3",
            params![reason.as_db_str(), now_ms, id.as_str()],
        )?;
        Ok(())
    }

    /// Synchronous edge upsert. `INSERT OR REPLACE` keyed on
    /// `(src, dst, kind)` — re-putting updates only the `weight`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures, including
    /// `updates`-edge schema-trigger violations.
    pub fn put_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        self.tx.execute(
            "INSERT OR REPLACE INTO edges (src, dst, kind, weight) \
               VALUES (?1, ?2, ?3, ?4)",
            params![
                edge.src.as_str(),
                edge.dst.as_str(),
                edge.kind.as_db_str(),
                edge.weight.map(f64::from),
            ],
        )?;
        Ok(())
    }

    /// Read every non-tombstoned, non-summary trace record for a turn,
    /// ordered by `trace_sequence` ASC.
    ///
    /// Uses the canonical [`record_from_json`] decoder from
    /// [`crate::store::projection`], so the hydration path is identical
    /// to all other read paths in this adapter.
    ///
    /// Rows with `trace_event = 'turn_summary'` are excluded — they are
    /// aggregated records, not individual events. Tombstoned rows are
    /// excluded via `tombstoned = 0`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures or
    /// [`StoreError::Codec`] if a stored `record_json` cannot be
    /// deserialized.
    pub fn list_trace_events(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<Vec<MemoryRecord>, StoreError> {
        let mut stmt = self.tx.prepare(
            "SELECT record_json \
               FROM records \
              WHERE trace_session_id = ?1 \
                AND trace_turn_id = ?2 \
                AND trace_event IS NOT NULL \
                AND trace_event != 'turn_summary' \
                AND tombstoned = 0 \
              ORDER BY trace_sequence ASC",
        )?;
        let rows = stmt.query_map(
            params![session_id.as_str(), turn_id],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|r| {
            let json = r?;
            record_from_json(&json)
        })
        .collect()
    }

    /// Returns `true` iff a non-tombstoned `turn_summary` row exists for
    /// the given session + turn.
    ///
    /// Used by the `capture_trace` pipeline to decide whether a summary
    /// has already been written for a turn before attempting to insert a
    /// new one (brief §8.1).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn turn_summary_exists(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<bool, StoreError> {
        let mut stmt = self.tx.prepare(
            "SELECT 1 FROM records \
              WHERE trace_event = 'turn_summary' \
                AND trace_session_id = ?1 \
                AND trace_turn_id = ?2 \
                AND tombstoned = 0 \
              LIMIT 1",
        )?;
        let exists = stmt
            .query_row(params![session_id.as_str(), turn_id], |_| Ok(()))
            .optional()?
            .is_some();
        Ok(exists)
    }

    /// Count non-tombstoned trace records whose `trace_payload_hash`
    /// matches `payload_hash` AND whose scope `(tenant, user, agent)`
    /// matches the given dimensions, excluding the listed `record_id`s.
    ///
    /// `NULL` matches `NULL` for each scope dimension via
    /// `coalesce(json_extract(scope, '$.X'), '') = coalesce(?N, '')`.
    ///
    /// Used by the dedup gate in `capture_trace` to determine whether an
    /// identical payload already exists in the same scope before inserting
    /// a new trace record (brief §8.1).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn payload_hash_count_in_scope(
        &self,
        payload_hash: &str,
        tenant: Option<&str>,
        user: Option<&str>,
        agent: Option<&str>,
        exclude: &[&str],
    ) -> Result<u64, StoreError> {
        let where_exclude = if exclude.is_empty() {
            String::new()
        } else {
            let placeholders = (0..exclude.len())
                .map(|i| format!("?{}", i + 5)) // first 4 params: hash + 3 scopes
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND record_id NOT IN ({placeholders})")
        };
        let sql = format!(
            "SELECT COUNT(*) FROM records \
              WHERE trace_payload_hash = ?1 \
                AND coalesce(json_extract(scope, '$.tenant'), '') = coalesce(?2, '') \
                AND coalesce(json_extract(scope, '$.user'),   '') = coalesce(?3, '') \
                AND coalesce(json_extract(scope, '$.agent'),  '') = coalesce(?4, '') \
                AND tombstoned = 0{where_exclude}"
        );
        let mut params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Text(payload_hash.to_owned()),
            tenant.map_or(rusqlite::types::Value::Null, |s| {
                rusqlite::types::Value::Text(s.to_owned())
            }),
            user.map_or(rusqlite::types::Value::Null, |s| {
                rusqlite::types::Value::Text(s.to_owned())
            }),
            agent.map_or(rusqlite::types::Value::Null, |s| {
                rusqlite::types::Value::Text(s.to_owned())
            }),
        ];
        for id in exclude {
            params.push(rusqlite::types::Value::Text((*id).to_owned()));
        }
        let mut stmt = self.tx.prepare(&sql)?;
        let n: i64 =
            stmt.query_row(rusqlite::params_from_iter(params), |row| row.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Idempotent trace-record upsert. Replays of the same
    /// `capture_event_id` converge on the same `record_id` (the
    /// projector derives one from the other, Task 6) and become no-op
    /// upserts via the primary-key idempotency in `upsert_in_tx`.
    ///
    /// Unique-constraint violations on the `records_trace_seq` and
    /// `records_trace_event_id` indices are translated to typed errors
    /// so callers can distinguish a sequence collision (two different
    /// events competing for the same turn slot) from an
    /// `capture_event_id` collision (projector contract violation).
    ///
    /// # Errors
    ///
    /// - [`StoreError::TraceSequenceConflict`] if two distinct events
    ///   tried to claim the same `(session_id, turn_id, sequence)`.
    /// - [`StoreError::TraceCaptureEventIdConflict`] if two distinct
    ///   record ids carry the same `trace.capture_event_id` (should not
    ///   happen under the documented projector contract).
    /// - Any other [`StoreError`] from the underlying upsert path.
    pub fn upsert_trace(&mut self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        match upsert_in_tx(&mut self.tx, record) {
            Ok(out) => Ok(out),
            // `records_trace_seq` index columns: trace_session_id,
            // trace_turn_id, trace_sequence.  SQLite error message:
            // "UNIQUE constraint failed: records.trace_session_id,
            //  records.trace_turn_id, records.trace_sequence"
            Err(StoreError::Sqlite(ref e))
                if is_unique_constraint(e, "records.trace_sequence") =>
            {
                Err(extract_seq_conflict(record))
            }
            // `records_trace_event_id` index column: trace_capture_event_id.
            // SQLite error message:
            // "UNIQUE constraint failed: records.trace_capture_event_id"
            Err(StoreError::Sqlite(ref e))
                if is_unique_constraint(e, "records.trace_capture_event_id") =>
            {
                Err(StoreError::TraceCaptureEventIdConflict {
                    capture_event_id: extract_capture_event_id(record),
                })
            }
            Err(other) => Err(other),
        }
    }

    /// Two-phase renumber of a turn's trace events when out-of-order
    /// backfill arrives (spec §4 "Ordering").
    ///
    /// Phase 1: park existing rows on unique negative sentinels so the
    ///          `UNIQUE` sequence index never sees a transient collision.
    /// Phase 2: reassign `0..N` sorted by `(captured_at, capture_event_id)`
    ///          across the union of existing and incoming records.
    ///
    /// All writes happen inside `self.tx`. A crash mid-renumber rolls
    /// back to pre-renumber state.
    ///
    /// `incoming` records have their `extra_frontmatter.trace.sequence`
    /// overridden by this call; caller-supplied sequence values are
    /// ignored.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from the underlying upserts, or
    /// [`StoreError::Invariant`] if a record is missing the expected
    /// `extra_frontmatter.trace` object.
    pub fn renumber_turn_with(
        &mut self,
        session_id: &SessionId,
        turn_id: &str,
        incoming: &[MemoryRecord],
    ) -> Result<(), StoreError> {
        let existing = self.list_trace_events(session_id, turn_id)?;

        // Phase 1: park each existing row at a distinct negative sentinel by
        // directly patching `record_json` in the DB. Direct SQL UPDATE (rather
        // than `upsert_in_tx`) is required because `upsert_in_tx` creates a new
        // version row and leaves an inactive row still holding the old sequence
        // value; since `records_trace_seq` covers ALL rows (not just active),
        // that inactive row would hold onto the slot and block Phase 2.
        for (i, row) in existing.iter().enumerate() {
            let park_seq = -(i64::try_from(i).unwrap_or(i64::MAX) + 1);
            let record_id = row.id.as_str();
            let n = self.tx.execute(
                "UPDATE records \
                    SET record_json = json_set(record_json, \
                        '$.extra_frontmatter.trace.sequence', ?1) \
                  WHERE record_id = ?2 AND active = 1",
                params![park_seq, record_id],
            )?;
            if n == 0 {
                return Err(StoreError::Invariant {
                    what: format!(
                        "renumber park: no active row found for record_id `{record_id}`"
                    ),
                });
            }
        }

        // Phase 2: union existing + incoming and sort by (captured_at,
        // capture_event_id), then assign monotonic 0..N sequences.
        //
        // Existing rows are updated in-place (same direct SQL UPDATE pattern).
        // Incoming rows are brand-new records — `upsert_in_tx` inserts them
        // directly with the final sequence.
        let mut all: Vec<MemoryRecord> =
            Vec::with_capacity(existing.len() + incoming.len());
        all.extend(existing.iter().cloned());
        all.extend(incoming.iter().cloned());
        all.sort_by(compare_by_captured_at_and_event_id);

        let existing_ids: std::collections::HashSet<&str> =
            existing.iter().map(|r| r.id.as_str()).collect();

        for (i, row) in all.iter().enumerate() {
            let final_seq = i64::try_from(i).unwrap_or(i64::MAX);
            if existing_ids.contains(row.id.as_str()) {
                // Existing row: update in-place.
                let record_id = row.id.as_str();
                self.tx.execute(
                    "UPDATE records \
                        SET record_json = json_set(record_json, \
                            '$.extra_frontmatter.trace.sequence', ?1) \
                      WHERE record_id = ?2 AND active = 1",
                    params![final_seq, record_id],
                )?;
            } else {
                // Incoming new record: set its final sequence and insert.
                let final_row = with_sequence(row, final_seq)?;
                upsert_in_tx(&mut self.tx, &final_row)?;
            }
        }
        Ok(())
    }

    /// Synchronous edge removal. Returns `true` if a row was deleted,
    /// `false` if no row matched the key.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError> {
        let n = self.tx.execute(
            "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND kind = ?3",
            params![key.src.as_str(), key.dst.as_str(), key.kind.as_db_str()],
        )?;
        Ok(n > 0)
    }
}

/// Clone `row` with `extra_frontmatter.trace.sequence` set to `sequence`.
///
/// Returns [`StoreError::Invariant`] if the record lacks the expected
/// `extra_frontmatter.trace` object (structural invariant of a trace record).
fn with_sequence(row: &MemoryRecord, sequence: i64) -> Result<MemoryRecord, StoreError> {
    let mut next = row.clone();
    let trace = next
        .extra_frontmatter
        .get_mut("trace")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| StoreError::Invariant {
            what: "trace record missing extra_frontmatter.trace".into(),
        })?;
    trace.insert(
        "sequence".into(),
        serde_json::Value::Number(sequence.into()),
    );
    Ok(next)
}

/// Comparator for the two-phase renumber sort: primary key is chronological
/// order of `updated_at` (which mirrors `captured_at` from the originating
/// `CaptureEvent`); secondary key is `capture_event_id` for deterministic
/// tie-breaking.
///
/// Uses [`cairn_core::domain::Rfc3339Timestamp::cmp_chronological`] for the
/// timestamp comparison, which accounts for timezone offsets correctly — lexical
/// `as_str()` comparison is wrong once offsets are involved.
fn compare_by_captured_at_and_event_id(
    a: &MemoryRecord,
    b: &MemoryRecord,
) -> std::cmp::Ordering {
    let cap_id = |r: &MemoryRecord| -> String {
        r.extra_frontmatter
            .get("trace")
            .and_then(|t| t.get("capture_event_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };
    a.updated_at
        .cmp_chronological(&b.updated_at)
        .then_with(|| cap_id(a).cmp(&cap_id(b)))
}

/// Returns `true` when `e` is a `UNIQUE` constraint failure whose error
/// message contains `column_signature`. `SQLite` formats the message as
/// `"UNIQUE constraint failed: TABLE.COL, …"` using the indexed
/// column names, not the index name, so we match on the column signature
/// (`column_signature`) that uniquely identifies each index.
///
/// | Index                    | Column signature                  |
/// |---|---|
/// | `records_trace_seq`      | `"records.trace_sequence"`        |
/// | `records_trace_event_id` | `"records.trace_capture_event_id"` |
fn is_unique_constraint(e: &rusqlite::Error, column_signature: &str) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.contains(column_signature)
    )
}

/// Extract `(session_id, turn_id, sequence)` from `extra_frontmatter["trace"]`
/// and return a [`StoreError::TraceSequenceConflict`]. Falls back to empty
/// strings / 0 if the keys are absent (caller-side bug; still typed).
fn extract_seq_conflict(r: &MemoryRecord) -> StoreError {
    let trace = r
        .extra_frontmatter
        .get("trace")
        .and_then(|v| v.as_object());
    let session_id = trace
        .and_then(|t| t.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let turn_id = trace
        .and_then(|t| t.get("turn_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let sequence = trace
        .and_then(|t| t.get("sequence"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    StoreError::TraceSequenceConflict {
        session_id,
        turn_id,
        sequence,
    }
}

/// Extract `trace.capture_event_id` from `extra_frontmatter`. Falls back
/// to an empty string if absent.
fn extract_capture_event_id(r: &MemoryRecord) -> String {
    r.extra_frontmatter
        .get("trace")
        .and_then(|v| v.get("capture_event_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned()
}

impl SqliteMemoryStore {
    /// Run `f` inside a single `SQLite` transaction on the dedicated DB
    /// worker thread. `Ok` commits; `Err` rolls back (the tx is dropped
    /// without committing, which `SQLite` treats as ROLLBACK).
    ///
    /// `with_tx` is inherent (not on the [`MemoryStore`] trait) because
    /// the trait must stay `dyn`-compatible — generic methods break
    /// object safety.
    ///
    /// [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] if the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] if the
    /// background worker channel fails. Otherwise returns the closure's
    /// own `Err`.
    #[allow(
        clippy::let_and_return,
        reason = "explicit binding ensures `conn` Arc drops before .await? \
                  yields, satisfying Rust 2024 tail-expr-drop-order"
    )]
    #[instrument(skip(self, f), err, fields(verb = "with_tx"))]
    pub async fn with_tx<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut StoreTx<'_>) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.require_conn("with_tx")?.clone();
        let result = conn
            .call(move |c| {
                let tx = c.transaction()?;
                let mut handle = StoreTx { tx };
                match f(&mut handle) {
                    Ok(value) => {
                        handle
                            .tx
                            .commit()
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                        Ok::<Result<T, StoreError>, tokio_rusqlite::Error>(Ok(value))
                    }
                    Err(e) => {
                        // Drop without commit → SQLite rolls back.
                        Ok::<Result<T, StoreError>, tokio_rusqlite::Error>(Err(e))
                    }
                }
            })
            .await?;
        result
    }
}
