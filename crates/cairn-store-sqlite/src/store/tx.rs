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

use cairn_core::contract::memory_store::{
    Edge, EdgeKey, EdgeKind, StoredRecord, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::SchemaVersion;
use cairn_core::domain::{
    BodyHash, MemoryRecord, RecordId, Session, SessionId, SignedAdmission, TargetId,
};
use rusqlite::{OptionalExtension as _, Transaction, params};
use tracing::instrument;

use crate::error::StoreError;
use crate::replay::challenge::MintedChallenge;
use crate::replay::{ReplayError, WalPrepareInputs};
use crate::store::projection::{ProjectedRow, record_from_json};
use crate::store::sessions::{InTxError, read_session_by_id};
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
    /// Round 9 review fix: returns `true` if any row (active or
    /// tombstoned) in the records table has ever owned this `target_id`.
    /// `flush apply rename` uses this to reject destinations that would
    /// hijack a retired lineage and merge unrelated histories under one
    /// target.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] on SQL failures.
    pub fn target_id_ever_used(&self, target: &TargetId) -> Result<bool, StoreError> {
        let row: Option<i64> = self
            .tx
            .query_row(
                "SELECT 1 FROM records WHERE target_id = ?1 LIMIT 1",
                params![target.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// Snapshot-read the active, non-tombstoned row for one target inside the
    /// caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures, [`StoreError::Codec`]
    /// when the stored `record_json` cannot be deserialized, and
    /// [`StoreError::Invariant`] when version or schema-version columns carry
    /// structurally invalid values.
    pub fn get_active_by_target(
        &self,
        target: &TargetId,
    ) -> Result<Option<StoredRecord>, StoreError> {
        let row: Option<(String, i64, Option<i64>, Option<i64>)> = self
            .tx
            .query_row(
                "SELECT record_json, version, schema_version_major, schema_version_minor \
                   FROM records \
                  WHERE target_id = ?1 AND active = 1 AND tombstoned = 0 \
                  LIMIT 1",
                params![target.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((record_json, version_i64, schema_major, schema_minor)) = row else {
            return Ok(None);
        };
        let record = record_from_json(&record_json)?;
        let version = u32::try_from(version_i64).map_err(|_| StoreError::Invariant {
            what: format!("stored version overflows u32: {version_i64}"),
        })?;
        let schema_version = match (schema_major, schema_minor) {
            (Some(major), Some(minor)) => Some(SchemaVersion {
                major: u32::try_from(major).map_err(|_| StoreError::Invariant {
                    what: format!("schema_version_major out of u32 range: {major}"),
                })?,
                minor: u32::try_from(minor).map_err(|_| StoreError::Invariant {
                    what: format!("schema_version_minor out of u32 range: {minor}"),
                })?,
            }),
            (None, None) => None,
            (major, minor) => {
                return Err(StoreError::Invariant {
                    what: format!(
                        "records.schema_version_(major,minor) inconsistent: ({major:?}, {minor:?})"
                    ),
                });
            }
        };
        Ok(Some(StoredRecord {
            record,
            version,
            schema_version,
        }))
    }

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

    /// Mark a specific active row inactive without deleting its historical
    /// version record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn deactivate_record(&self, id: &RecordId) -> Result<(), StoreError> {
        let now_ms = current_unix_ms();
        self.tx.execute(
            "UPDATE records SET active = 0, updated_at = ?1 WHERE record_id = ?2 AND active = 1",
            params![now_ms, id.as_str()],
        )?;
        Ok(())
    }

    /// Read one explicit session row inside the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SessionNotFound`] when no row exists,
    /// [`StoreError::SessionEnded`] when the row is closed, and the usual
    /// [`StoreError`] variants for SQL / codec / invariant failures.
    pub fn get_live_session(&self, session_id: &SessionId) -> Result<Session, StoreError> {
        let id_str = session_id.as_str();
        let Some(session) = read_session_by_id(&self.tx, id_str).map_err(|e| match e {
            InTxError::Sqlite(err) => StoreError::Sqlite(err),
            InTxError::Codec(err) => StoreError::Codec(err),
            InTxError::Invariant(what) => StoreError::Invariant { what },
            InTxError::Terminal(err) => err,
            InTxError::UniqueViolation | InTxError::StaleSnapshot => StoreError::Invariant {
                what: "unexpected retry-only session read state".into(),
            },
        })?
        else {
            return Err(StoreError::SessionNotFound {
                session_id: id_str.to_owned(),
            });
        };
        if let Some(ended_at_unix_ms) = session.ended_at_unix_ms {
            return Err(StoreError::SessionEnded {
                session_id: id_str.to_owned(),
                ended_at_unix_ms,
            });
        }
        Ok(session)
    }

    /// Persist session metadata fields in place and treat the patch as session
    /// activity by bumping `last_activity_at`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Codec`] when tag serialization fails and
    /// [`StoreError::Sqlite`] for SQL failures.
    pub fn update_session_metadata(&self, session: &Session) -> Result<(), StoreError> {
        let now_ms = current_unix_ms();
        let tags_json = if session.tags.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&session.tags)?)
        };
        self.tx.execute(
            "UPDATE sessions \
                SET title = ?1, channel = ?2, priority = ?3, tags = ?4, last_activity_at = ?5 \
              WHERE session_id = ?6 AND ended_at IS NULL",
            params![
                session.title.as_str(),
                session.channel.as_deref(),
                session.priority.as_deref(),
                tags_json,
                now_ms,
                session.id.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Append a row to `session_metadata_audit` capturing the
    /// pre-mutation and post-mutation session document JSON inside the
    /// same transaction as the in-place `UPDATE sessions ...`. Provides
    /// durable rollback evidence for `flush apply` session patches
    /// (issue #289 re-loop r6). Pre/post are serialized by the caller
    /// so they can use whatever canonical document shape the planner
    /// recognized.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] on SQL failures.
    pub fn append_session_metadata_audit<T>(
        &self,
        session_id: &SessionId,
        operation_id: &str,
        pre_state: &T,
        post_state: &T,
    ) -> Result<(), StoreError>
    where
        T: serde::Serialize,
    {
        let pre_json = serde_json::to_string(pre_state)?;
        let post_json = serde_json::to_string(post_state)?;
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        self.tx.execute(
            "INSERT INTO session_metadata_audit \
                 (operation_id, session_id, pre_state, post_state, applied_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                operation_id,
                session_id.as_str(),
                pre_json,
                post_json,
                now_ms,
            ],
        )?;
        Ok(())
    }

    /// Copy every non-`updates` edge attached to `old_id` onto `new_id`.
    /// Round 7/8 review fix: the previous version also deleted the
    /// source rows, which irrecoverably erased the predecessor's graph
    /// history. Now copies are additive — `new_id` gains the same edges
    /// for live traversal while `old_id` retains its original
    /// relationships so audit/rollback tooling can still reconstruct the
    /// pre-rename neighbourhood. Uses `INSERT OR IGNORE` so an
    /// already-existing edge on `new_id` is not silently overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn rewrite_non_updates_edges(
        &self,
        old_id: &RecordId,
        new_id: &RecordId,
    ) -> Result<(), StoreError> {
        self.tx.execute(
            "INSERT OR IGNORE INTO edges (src, dst, kind, weight) \
             SELECT CASE WHEN src = ?1 THEN ?2 ELSE src END, \
                    CASE WHEN dst = ?1 THEN ?2 ELSE dst END, \
                    kind, \
                    weight \
               FROM edges \
              WHERE kind != ?3 \
                AND (src = ?1 OR dst = ?1)",
            params![
                old_id.as_str(),
                new_id.as_str(),
                EdgeKind::Updates.as_db_str()
            ],
        )?;
        Ok(())
    }

    /// Mint and persist a fresh single-use server challenge for
    /// `issuer`. Returns the base64 nonce + absolute expiry.
    /// See [`crate::replay::challenge::mint_challenge`] for full
    /// semantics. Used by the `handshake` prelude (issue #52).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the underlying insert fails.
    pub fn mint_challenge(
        &self,
        issuer: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> Result<MintedChallenge, StoreError> {
        crate::replay::challenge::mint_challenge(&self.tx, issuer, now_ms, ttl_ms)
    }

    /// Atomically admit a verified envelope: insert the WAL `PREPARED`
    /// row, then consume the replay-ledger entry. See
    /// `crate::replay::prepare_wal_with_replay` for full semantics
    /// (issue #52, brief §4.2).
    ///
    /// # Verification boundary
    ///
    /// The argument is a [`SignedAdmission`] — the sealed token whose
    /// constructor recomputes `sha256(payload)` and asserts equality
    /// with `intent.target_hash`. That binds the verb-layer's chosen
    /// `kind` and `plan_ref` to the issuer's signature: a buggy
    /// dispatcher cannot stage replay state for one `kind` under a
    /// signature meant for another. See [`SignedAdmission::new`] for
    /// the construction contract. Round-12 review #1.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError`] for replay-ledger violations (duplicate
    /// `operation_id` / nonce, out-of-order sequence, missing or
    /// expired challenge, an envelope missing both modes, or admission
    /// past the signed `expires_at`).
    pub fn prepare_wal_with_replay(&self, admission: &SignedAdmission) -> Result<(), ReplayError> {
        // Round-3 review #1: replay enforcement runs against `now_ms`.
        // Saturating clocks would either admit expired intents/challenges
        // (`now = 0` < every real expiry) or reject every admit
        // (`now = i64::MAX` >= every real expiry). Surface a typed
        // `ClockFault` instead and let the verb layer reject up the
        // stack.
        let now_ms = cairn_core::time::checked_now_ms()?;
        let inputs = WalPrepareInputs {
            kind: admission.kind().as_db_str(),
            plan_ref: admission.plan_ref(),
        };
        crate::replay::prepare_wal_with_replay(
            &self.tx,
            admission.intent().as_inner(),
            &inputs,
            now_ms,
        )
    }

    /// Transition the prepared WAL row for an admitted envelope to
    /// `COMMITTED`.
    ///
    /// Call this in the same transaction as the mutation authorized by
    /// [`Self::prepare_wal_with_replay`]. If the transition fails, the
    /// caller's transaction should return `Err` so both the mutation and
    /// prepared WAL row roll back together.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures or
    /// [`StoreError::Invariant`] when the row was not in `PREPARED`.
    pub fn commit_prepared_wal(&self, admission: &SignedAdmission) -> Result<(), StoreError> {
        let op_id = &admission.intent().as_inner().operation_id.0;
        let now_ms = current_unix_ms();
        let rows = self.tx.execute(
            "UPDATE wal_ops \
                SET state = 'COMMITTED', updated_at = ?2 \
              WHERE operation_id = ?1 AND state = 'PREPARED'",
            params![op_id, now_ms],
        )?;
        if rows != 1 {
            return Err(StoreError::Invariant {
                what: format!("prepared WAL op {op_id} could not be committed"),
            });
        }
        Ok(())
    }

    /// In-crate test-only escape hatch: admit a raw, unverified
    /// `SignedIntent` through the replay ledger. **Not part of any
    /// production surface** — `pub(crate)` so even an external crate
    /// that flips a build feature cannot reach it. The production
    /// [`Self::prepare_wal_with_replay`] takes a sealed
    /// [`VerifiedSignedIntent`]; round-11 review #2 closed the
    /// previous `test-helpers`-feature-gated public hole.
    ///
    /// # Errors
    ///
    /// Same as [`Self::prepare_wal_with_replay`].
    #[cfg(test)]
    pub(crate) fn prepare_wal_with_replay_unverified(
        &self,
        intent: &cairn_core::generated::envelope::SignedIntent,
        inputs: &WalPrepareInputs<'_>,
        now_ms: i64,
    ) -> Result<(), ReplayError> {
        crate::replay::prepare_wal_with_replay(&self.tx, intent, inputs, now_ms)
    }

    /// Drop expired rows from `outstanding_challenges`. Returns the
    /// number deleted. See [`crate::replay::challenge::purge_expired_challenges`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the underlying delete fails.
    pub fn purge_expired_challenges(&self, now_ms: i64) -> Result<usize, StoreError> {
        crate::replay::challenge::purge_expired_challenges(&self.tx, now_ms)
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
    /// Uses the canonical `record_from_json` decoder from the projection
    /// module, so the hydration path is identical to all other read
    /// paths in this adapter.
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
        // `active = 1` excludes superseded version rows; without it,
        // any future re-upsert of the same target_id would let
        // turn-reconstruction read both the live and the superseded
        // copies (defence-in-depth — the trace_capture_event_id UNIQUE
        // index makes this hard to trigger today, but the read should
        // not depend on that).
        let mut stmt = self.tx.prepare(
            "SELECT record_json \
               FROM records \
              WHERE trace_session_id = ?1 \
                AND trace_turn_id = ?2 \
                AND trace_event IS NOT NULL \
                AND trace_event != 'turn_summary' \
                AND active = 1 \
                AND tombstoned = 0 \
              ORDER BY trace_sequence ASC",
        )?;
        let rows = stmt.query_map(params![session_id.as_str(), turn_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|r| {
            let json = r?;
            record_from_json(&json)
        })
        .collect()
    }

    /// Verify referential invariants among trace records for a turn (spec
    /// §4 "Referential"). Call after writing all events for the turn,
    /// before committing.
    ///
    /// Reads all non-tombstoned, non-summary trace records for the turn via
    /// [`Self::list_trace_events`], then checks that every `post_tool` and
    /// `tool_output` record:
    ///
    /// 1. Has a non-empty `parent_event_id`.
    /// 2. References a parent that exists in the same turn.
    /// 3. The parent's `trace_event` is `pre_tool`.
    /// 4. The parent's `tool_call_id` matches the child's `tool_call_id`.
    ///
    /// # Errors
    ///
    /// - [`StoreError::TraceLinkOrphan`] if a `post_tool`/`tool_output`
    ///   parent is missing, cross-turn, the wrong event type, or has a
    ///   mismatched `tool_call_id`.
    /// - Other [`StoreError`] from the underlying read.
    pub fn validate_turn_links(
        &self,
        session_id: &SessionId,
        turn_id: &str,
    ) -> Result<(), StoreError> {
        use std::collections::HashMap;

        let rows = self.list_trace_events(session_id, turn_id)?;

        // Index by capture_event_id for O(1) parent lookup.
        // Value: (trace_event, tool_call_id).
        let by_id: HashMap<String, (&str, Option<&str>)> = rows
            .iter()
            .filter_map(|r| {
                let trace = r.extra_frontmatter.get("trace")?.as_object()?;
                let id = trace.get("capture_event_id")?.as_str()?.to_owned();
                let evt = r.extra_frontmatter.get("trace_event")?.as_str()?;
                let tcid = trace.get("tool_call_id").and_then(|v| v.as_str());
                Some((id, (evt, tcid)))
            })
            .collect();

        for r in &rows {
            let trace = r
                .extra_frontmatter
                .get("trace")
                .and_then(|v| v.as_object())
                .ok_or_else(|| StoreError::Invariant {
                    what: "trace record missing extra_frontmatter.trace".into(),
                })?;
            let event = r
                .extra_frontmatter
                .get("trace_event")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !matches!(event, "post_tool" | "tool_output") {
                continue;
            }

            let child_id = trace
                .get("capture_event_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let child_tcid = trace
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let parent_id = trace
                .get("parent_event_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| StoreError::TraceLinkOrphan {
                    child_id: child_id.to_owned(),
                    parent_id: String::new(),
                    reason: "missing parent_event_id".into(),
                })?;

            let Some((parent_evt, parent_tcid)) = by_id.get(parent_id) else {
                return Err(StoreError::TraceLinkOrphan {
                    child_id: child_id.to_owned(),
                    parent_id: parent_id.to_owned(),
                    reason: "parent not found in turn".into(),
                });
            };
            if *parent_evt != "pre_tool" {
                return Err(StoreError::TraceLinkOrphan {
                    child_id: child_id.to_owned(),
                    parent_id: parent_id.to_owned(),
                    reason: format!("parent has trace_event={parent_evt}, expected pre_tool"),
                });
            }
            let parent_tcid = parent_tcid.unwrap_or("");
            if child_tcid != parent_tcid {
                return Err(StoreError::TraceLinkOrphan {
                    child_id: child_id.to_owned(),
                    parent_id: parent_id.to_owned(),
                    reason: format!(
                        "tool_call_id mismatch: child={child_tcid}, parent={parent_tcid}"
                    ),
                });
            }
        }
        Ok(())
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
        let n: i64 = stmt.query_row(rusqlite::params_from_iter(params), |row| row.get(0))?;
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
        // `turn_summary` records use a deterministic `record_id` derived from
        // `(session_id, turn_id)`.  The `records_trace_summary` UNIQUE index
        // covers all rows (active *and* inactive) whose `trace_event` is
        // `'turn_summary'`, so the standard `upsert_in_tx` flow
        // (deactivate-old + insert-new) would leave an inactive summary row
        // in the index and cause a UNIQUE constraint violation on the next
        // re-summarize.  We avoid this by doing an in-place JSON UPDATE on
        // the existing active summary row instead of the deactivate+insert
        // pattern.
        let is_summary = record
            .extra_frontmatter
            .get("trace_event")
            .and_then(|v| v.as_str())
            == Some("turn_summary");

        if is_summary {
            // Re-summarize must keep `record_json` and the denormalized
            // columns (body, body_hash, scope, kind/class/visibility,
            // confidence, salience, tags, updated_at) in lock-step. The
            // standard `upsert_in_tx` deactivate+insert path can't be used
            // here: `records_trace_summary` is a UNIQUE index over
            // `(trace_session_id, trace_turn_id) WHERE trace_event =
            // 'turn_summary'` covering both active and inactive rows, so an
            // inactive prior summary would collide with the new insert.
            // Build a `ProjectedRow` and update every column from it.
            let body_hash = BodyHash::compute(&record.body);
            let now_ms = current_unix_ms();
            let target_id = record.target_id.as_str();

            // Look up the existing active row to preserve its created_at
            // and version (in-place rewrite, not a new version row).
            let prior: Option<(String, i64, i64)> = self
                .tx
                .query_row(
                    "SELECT record_id, version, created_at FROM records \
                       WHERE target_id = ?1 AND active = 1 LIMIT 1",
                    params![target_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;

            let Some((prior_id, prior_version, created_at)) = prior else {
                // No existing row: standard insert path is fine — there's no
                // inactive summary row to collide with the partial UNIQUE.
                return upsert_in_tx(&mut self.tx, record);
            };

            let version_u32 = u32::try_from(prior_version).map_err(|_| StoreError::Invariant {
                what: format!("prior summary version overflows u32: {prior_version}"),
            })?;
            let row = ProjectedRow::from_record(
                record,
                version_u32,
                created_at,
                now_ms,
                &body_hash,
                true,
                false,
            )?;

            // Re-stamp schema_version from the running binary on every
            // in-place rewrite — the writer is always the current host,
            // so leaving the prior stamp would let a contract-bumped
            // build silently lint as stale against rows it just
            // refreshed (Issue #258 review round 2).
            let n = self.tx.execute(
                "UPDATE records SET \
                    record_json = ?1, \
                    body = ?2, \
                    body_hash = ?3, \
                    updated_at = ?4, \
                    scope = ?5, \
                    actor_chain = ?6, \
                    kind = ?7, \
                    class = ?8, \
                    visibility = ?9, \
                    confidence = ?10, \
                    salience = ?11, \
                    tags_json = ?12, \
                    path = ?13, \
                    schema_version_major = ?14, \
                    schema_version_minor = ?15 \
                  WHERE record_id = ?16",
                params![
                    row.record_json,
                    row.body,
                    row.body_hash,
                    row.updated_at,
                    row.scope,
                    row.actor_chain,
                    row.kind,
                    row.class,
                    row.visibility,
                    row.confidence,
                    row.salience,
                    row.tags_json,
                    row.path,
                    row.schema_version_major,
                    row.schema_version_minor,
                    prior_id,
                ],
            )?;
            debug_assert_eq!(n, 1, "summary in-place UPDATE matched 0 rows");

            return Ok(UpsertOutcome {
                record_id: record.id.clone(),
                target_id: record.target_id.clone(),
                version: version_u32,
                content_changed: true,
                prior_hash: Some(body_hash),
            });
        }

        match upsert_in_tx(&mut self.tx, record) {
            Ok(out) => Ok(out),
            // `records_trace_seq` index columns: trace_session_id,
            // trace_turn_id, trace_sequence.  SQLite error message:
            // "UNIQUE constraint failed: records.trace_session_id,
            //  records.trace_turn_id, records.trace_sequence"
            Err(StoreError::Sqlite(ref e)) if is_unique_constraint(e, "records.trace_sequence") => {
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
                    what: format!("renumber park: no active row found for record_id `{record_id}`"),
                });
            }
        }

        // Phase 2: union existing + incoming and sort by (captured_at,
        // capture_event_id), then assign monotonic 0..N sequences.
        //
        // Existing rows are updated in-place (same direct SQL UPDATE pattern).
        // Incoming rows are brand-new records — `upsert_in_tx` inserts them
        // directly with the final sequence.
        //
        // Deduplication: if an incoming record carries the same `record_id`
        // as an existing row (i.e., a replay of an already-persisted event),
        // it is excluded from `all`.  The existing row already covers it and
        // will be updated to its final sequence in the loop below.  Including
        // the duplicate would inflate the total count, assign wrong sequences
        // (0..2N instead of 0..N), and hit the UNIQUE index on
        // `(session_id, turn_id, sequence)` on the second write to the same
        // record_id.
        let existing_ids: std::collections::HashSet<&str> =
            existing.iter().map(|r| r.id.as_str()).collect();

        let mut all: Vec<MemoryRecord> = Vec::with_capacity(existing.len() + incoming.len());
        all.extend(existing.iter().cloned());
        all.extend(
            incoming
                .iter()
                .filter(|r| !existing_ids.contains(r.id.as_str()))
                .cloned(),
        );
        all.sort_by(compare_by_captured_at_and_event_id);

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
fn compare_by_captured_at_and_event_id(a: &MemoryRecord, b: &MemoryRecord) -> std::cmp::Ordering {
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
    let trace = r.extra_frontmatter.get("trace").and_then(|v| v.as_object());
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
    /// The transaction is opened with [`TransactionBehavior::Immediate`]
    /// so the writer lock is acquired up front (issue #52 round-9
    /// review #1). Without this, multi-connection write paths that
    /// allocate per-row identifiers via `MAX(...) + 1` race: two
    /// readers see the same `MAX` snapshot, one wins, the other surfaces
    /// the table's `must_advance` trigger as `ReplayError::Sqlite`. With
    /// IMMEDIATE the second writer waits on the file lock; its initial
    /// snapshot is acquired AFTER the first commit, so its
    /// `MAX(issued_seq)` reflects the new state. WAL-mode read traffic
    /// remains unblocked.
    ///
    /// [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore
    /// [`TransactionBehavior::Immediate`]: rusqlite::TransactionBehavior::Immediate
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
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
