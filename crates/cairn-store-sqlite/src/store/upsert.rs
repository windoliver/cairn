//! `MemoryStore::upsert` impl.
//!
//! State machine (brief §5.2):
//!
//! 1. Compute `body_hash = blake3(record.body)`.
//! 2. In a single read-write transaction, look up the active row for
//!    `record.target_id`.
//! 3. **No prior row** → insert at `version = 1`, `active = 1`,
//!    return `content_changed = true, prior_hash = None`.
//! 4. **Prior row, hashes match** → idempotent no-op, commit, return
//!    `content_changed = false` echoing the prior hash so callers can
//!    confirm dedupe was made against the body they expected.
//! 5. **Prior row, hashes differ** → mark prior `active = 0`, insert a new
//!    row at `prior.version + 1`, return `content_changed = true,
//!    prior_hash = Some(prior_hash)`.
//!
//! All branches commit before returning; idempotent path commits without
//! mutating any row, which is intentionally cheap but still keeps the read
//! lock semantics consistent across the three branches.

use std::sync::Arc;

use cairn_core::contract::memory_store::UpsertOutcome;
use cairn_core::domain::{BodyHash, MemoryRecord, RecordId, TargetId};
use rusqlite::{Transaction, params};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::projection::{ProjectedRow, body_hash_from_str};
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Outcome of the pre-transaction embedding step.
pub(crate) enum EmbedOutcome {
    /// Embedding computed successfully. Contains LE bytes + model label.
    Succeeded {
        vector: Vec<u8>,
        model_label: String,
    },
    /// Embedder errored. Record is written; queued in `pending_embeddings`.
    Failed { error: String },
    /// No embedder configured; skip vector entirely.
    Skipped,
}

impl From<EmbedOutcome> for crate::record_wal::payload::StoredEmbedOutcome {
    fn from(value: EmbedOutcome) -> Self {
        match value {
            EmbedOutcome::Succeeded {
                vector,
                model_label,
            } => Self::Succeeded {
                vector,
                model_label,
            },
            EmbedOutcome::Failed { error } => Self::Failed { error },
            EmbedOutcome::Skipped => Self::Skipped,
        }
    }
}

/// Active-row tuple as read out of the `records` table:
/// `(record_id, version, body_hash, consent_model)`. Only the active
/// row for a given `target_id` is ever returned (partial unique index
/// `records_active_target_idx`). `consent_model` is preserved across
/// supersession so a row stamped `'receipt_timeline'` by Phase-B (#255)
/// does not silently revert to `'legacy_event'` on the next rewrite.
pub(crate) type PriorActive = (String, i64, String, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpsertPlan {
    pub(crate) outcome_record_id: RecordId,
    pub(crate) target_id: TargetId,
    pub(crate) version: u32,
    pub(crate) content_changed: bool,
    pub(crate) prior_record_id: Option<String>,
    pub(crate) prior_hash: Option<BodyHash>,
    pub(crate) consent_model: String,
}

impl SqliteMemoryStore {
    /// Inherent upsert implementation; the trait method [`MemoryStore::upsert`]
    /// guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::upsert`]: cairn_core::contract::memory_store::MemoryStore::upsert
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails (channel closed, panic in worker), [`StoreError::Sqlite`]
    /// for SQL errors surfaced through the worker, [`StoreError::Codec`]
    /// when projecting `record_json`, and [`StoreError::Invariant`] when a
    /// stored value cannot be parsed back (schema drift / corruption) or a
    /// version counter would overflow `u32`.
    #[instrument(
        skip(self, record),
        err,
        fields(
            verb = "upsert",
            record_id = %record.id.as_str(),
            target_id = %record.target_id,
            kind = ?record.kind,
            class = ?record.class,
        ),
    )]
    pub(crate) async fn do_upsert(
        &self,
        record: &MemoryRecord,
    ) -> Result<UpsertOutcome, StoreError> {
        // Validate at the boundary so structural failures surface as a
        // typed `StoreError::InvalidRecord` rather than getting wrapped in
        // `StoreError::Worker(Other(_))` by the `tokio_rusqlite` worker.
        record.validate()?;

        // 1. Compute embedding outside the SQL transaction (CPU-bound, may be slow).
        //    This keeps the transaction short and avoids holding DB locks during
        //    inference. On failure the record is still written; a `pending_embeddings`
        //    row is inserted atomically with the record commit.
        let embed_outcome: EmbedOutcome = if let Some(embedder) = &self.embedder {
            let body = record.body.clone();
            let emb = Arc::clone(embedder);
            let model_label = emb.kind().as_str().to_owned();
            match tokio::task::spawn_blocking(move || emb.embed_document(&body)).await {
                Ok(Ok(vec)) => EmbedOutcome::Succeeded {
                    vector: vec.iter().flat_map(|&f| f.to_le_bytes()).collect(),
                    model_label,
                },
                Ok(Err(e)) => {
                    tracing::warn!(
                        error = %e,
                        "embed failed before upsert tx; will queue in pending_embeddings",
                    );
                    EmbedOutcome::Failed {
                        error: e.to_string(),
                    }
                }
                Err(join_err) => {
                    tracing::warn!(
                        error = %join_err,
                        "embed task panicked before upsert tx; will queue in pending_embeddings",
                    );
                    EmbedOutcome::Failed {
                        error: join_err.to_string(),
                    }
                }
            }
        } else {
            EmbedOutcome::Skipped
        };

        let payload_embed = crate::record_wal::payload::StoredEmbedOutcome::from(embed_outcome);
        crate::record_wal::apply_upsert(self, record, payload_embed).await
    }
}

/// Synchronous upsert worker — runs inside the `tokio_rusqlite` worker
/// thread holding an open `Transaction`. The caller commits on `Ok` and
/// drops (rolling back) on `Err`. Pulled out of `do_upsert` so the async
/// shell stays under the workspace's `clippy::too_many_lines` limit.
/// Synchronous upsert against an open `rusqlite::Transaction`. Shared by
/// `SqliteMemoryStore::do_upsert` (which opens its own tx via the worker
/// thread) and `crate::store::tx::StoreTx::upsert` (which inherits the
/// caller's tx from `with_tx`). Encapsulates the content-hash idempotency,
/// version bump, and `record_id` synthesis decisions so both call sites
/// stay in lockstep.
pub(crate) fn upsert_in_tx(
    tx: &mut Transaction<'_>,
    record: &MemoryRecord,
) -> Result<UpsertOutcome, StoreError> {
    let plan = plan_upsert_in_tx(tx, record)?;
    stage_upsert_cow_in_tx(tx, record, &plan)?;
    activate_upsert_in_tx(tx, &plan)?;
    Ok(UpsertOutcome {
        record_id: plan.outcome_record_id,
        target_id: plan.target_id,
        version: plan.version,
        content_changed: plan.content_changed,
        prior_hash: plan.prior_hash,
    })
}

pub(crate) fn plan_upsert_in_tx(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
) -> Result<UpsertPlan, StoreError> {
    record.validate()?;
    let body_hash = BodyHash::compute(&record.body);
    let prior = read_active(tx, record.target_id.as_str())?;
    if let Some((prior_id, prior_version, prior_hash_str, prior_consent_model)) = prior.as_ref() {
        let _: cairn_core::domain::consent_timeline::ConsentModel =
            parse_consent_model(prior_consent_model)?;
        let prior_hash = body_hash_from_str(prior_hash_str)?;
        if prior_hash == body_hash {
            let prior_record_id =
                RecordId::parse(prior_id.clone()).map_err(|e| StoreError::Invariant {
                    what: format!("invalid prior record_id `{prior_id}`: {e}"),
                })?;
            return Ok(UpsertPlan {
                outcome_record_id: prior_record_id,
                target_id: record.target_id.clone(),
                version: u32::try_from(*prior_version).map_err(|_| StoreError::Invariant {
                    what: format!("prior version overflows u32: {prior_version}"),
                })?,
                content_changed: false,
                prior_record_id: Some(prior_id.clone()),
                prior_hash: Some(prior_hash),
                consent_model: prior_consent_model.clone(),
            });
        }

        return Ok(UpsertPlan {
            outcome_record_id: mint_record_id()?,
            target_id: record.target_id.clone(),
            version: next_version(*prior_version)?,
            content_changed: true,
            prior_record_id: Some(prior_id.clone()),
            prior_hash: Some(prior_hash),
            consent_model: prior_consent_model.clone(),
        });
    }

    let max_version: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM records WHERE target_id = ?1",
        params![record.target_id.as_str()],
        |row| row.get(0),
    )?;
    let version = next_version(max_version)?;
    let outcome_record_id = if max_version == 0 {
        record.id.clone()
    } else {
        mint_record_id()?
    };
    Ok(UpsertPlan {
        outcome_record_id,
        target_id: record.target_id.clone(),
        version,
        content_changed: true,
        prior_record_id: None,
        prior_hash: None,
        consent_model: "legacy_event".to_owned(),
    })
}

pub(crate) fn stage_upsert_cow_in_tx(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    plan: &UpsertPlan,
) -> Result<(), StoreError> {
    record.validate()?;
    let body_hash = BodyHash::compute(&record.body);
    let now_ms = current_unix_ms();

    if !plan.content_changed {
        if let Some(prior_id) = plan.prior_record_id.as_deref() {
            reproject_in_place(
                tx,
                record,
                prior_id,
                i64::from(plan.version),
                &body_hash,
                now_ms,
            )?;
        }
        return Ok(());
    }

    let mut row_record = record.clone();
    row_record.id = plan.outcome_record_id.clone();
    insert_row_with_active(
        tx,
        &row_record,
        plan.version,
        now_ms,
        &body_hash,
        &plan.consent_model,
        false,
    )
}

pub(crate) fn activate_upsert_in_tx(
    tx: &Transaction<'_>,
    plan: &UpsertPlan,
) -> Result<(), StoreError> {
    if !plan.content_changed {
        return Ok(());
    }
    let now_ms = current_unix_ms();
    let staged_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM records \
          WHERE record_id = ?1 AND target_id = ?2 AND version = ?3",
        params![
            plan.outcome_record_id.as_str(),
            plan.target_id.as_str(),
            i64::from(plan.version),
        ],
        |row| row.get(0),
    )?;
    if staged_count != 1 {
        return Err(StoreError::Invariant {
            what: format!(
                "planned upsert row missing before activation: record_id={} target_id={} version={}",
                plan.outcome_record_id.as_str(),
                plan.target_id.as_str(),
                plan.version
            ),
        });
    }
    tx.execute(
        "UPDATE records SET active = 0, updated_at = ?1 \
          WHERE target_id = ?2 AND active = 1 AND record_id != ?3",
        params![
            now_ms,
            plan.target_id.as_str(),
            plan.outcome_record_id.as_str()
        ],
    )?;
    let activated = tx.execute(
        "UPDATE records SET active = 1, tombstoned = 0, cow_staged = 0, updated_at = ?1 \
          WHERE record_id = ?2 AND target_id = ?3 AND version = ?4",
        params![
            now_ms,
            plan.outcome_record_id.as_str(),
            plan.target_id.as_str(),
            i64::from(plan.version),
        ],
    )?;
    if activated != 1 {
        return Err(StoreError::Invariant {
            what: format!(
                "planned upsert activation affected {activated} rows for record_id={} target_id={} version={}",
                plan.outcome_record_id.as_str(),
                plan.target_id.as_str(),
                plan.version
            ),
        });
    }
    Ok(())
}

/// Idempotent re-ingest helper: re-project the row under the current
/// projection logic and refresh every schema-versioned hot column
/// before advancing the stamp.
///
/// Codex round 7: bumping only `schema_version_*` would let a future
/// projection change (new derived column, evolved JSON shape, updated
/// path scheme) clear the §6.4 `stale_schema` lint without actually
/// rewriting the row. The stamp must mean "this binary re-projected
/// the row under its current schema," so we run the same projection
/// the non-idempotent insert path uses and UPDATE the projected
/// columns. `body` / `body_hash` / `version` / `created_at` /
/// `consent_model` are deliberately preserved (body bytes are
/// unchanged, idempotent semantics forbid bumping the version, and
/// `consent_model` is store-side authorization metadata only mutable
/// through the privileged Phase-B transition API).
fn reproject_in_place(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    prior_id: &str,
    prior_version: i64,
    body_hash: &BodyHash,
    now_ms: i64,
) -> Result<(), StoreError> {
    let projection_version = u32::try_from(prior_version).map_err(|_| StoreError::Invariant {
        what: format!("prior version overflows u32: {prior_version}"),
    })?;
    let row = ProjectedRow::from_record(
        record,
        projection_version,
        now_ms,
        now_ms,
        body_hash,
        true,
        false,
    )?;
    tx.execute(
        "UPDATE records SET \
            target_id            = ?1, \
            path                 = ?2, \
            kind                 = ?3, \
            class                = ?4, \
            visibility           = ?5, \
            scope                = ?6, \
            actor_chain          = ?7, \
            record_json          = ?8, \
            confidence           = ?9, \
            salience             = ?10, \
            target_id_explicit   = ?11, \
            tags_json            = ?12, \
            schema_version_major = ?13, \
            schema_version_minor = ?14 \
          WHERE record_id = ?15",
        params![
            row.target_id,
            row.path,
            row.kind,
            row.class,
            row.visibility,
            row.scope,
            row.actor_chain,
            row.record_json,
            row.confidence,
            row.salience,
            row.target_id_explicit,
            row.tags_json,
            row.schema_version_major,
            row.schema_version_minor,
            prior_id,
        ],
    )?;
    Ok(())
}

/// Read the active row for `target_id`, if any. Returns `None` (not an
/// error) when no active row exists.
fn read_active(tx: &Transaction<'_>, target_id: &str) -> Result<Option<PriorActive>, StoreError> {
    let row = tx
        .query_row(
            "SELECT record_id, version, body_hash, consent_model \
               FROM records \
              WHERE target_id = ?1 AND active = 1 \
              LIMIT 1",
            params![target_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(row)
}

/// Parse a stored `consent_model` string against the closed enum,
/// rejecting anything else as `StoreError::Invariant`. Used on both
/// the prior-row read path and the about-to-write path so schema drift
/// / manual SQL corruption cannot silently downgrade to `legacy_event`
/// on the next rewrite.
pub(crate) fn parse_consent_model(
    raw: &str,
) -> Result<cairn_core::domain::consent_timeline::ConsentModel, StoreError> {
    match raw {
        "legacy_event" => Ok(cairn_core::domain::consent_timeline::ConsentModel::LegacyEvent),
        "receipt_timeline" => {
            Ok(cairn_core::domain::consent_timeline::ConsentModel::ReceiptTimeline)
        }
        other => Err(StoreError::Invariant {
            what: format!(
                "records.consent_model has unknown value `{other}`; \
                 expected one of: legacy_event, receipt_timeline. \
                 This indicates schema drift or manual SQL corruption \
                 — refusing to rewrite the row to avoid a silent \
                 downgrade of consent enforcement."
            ),
        }),
    }
}

/// Compute the next version, returning typed errors for both the i64
/// overflow on `+ 1` and the u32 narrowing.
fn next_version(prior_version: i64) -> Result<u32, StoreError> {
    let next = prior_version
        .checked_add(1)
        .ok_or_else(|| StoreError::Invariant {
            what: format!("prior version + 1 overflows i64: {prior_version}"),
        })?;
    u32::try_from(next).map_err(|_| StoreError::Invariant {
        what: format!("next version overflows u32: {next}"),
    })
}

/// Project + insert one new version row. Inactive inserts are pre-activation
/// COW staging rows, so they carry `cow_staged = 1` until activation clears
/// the internal marker. The caller is responsible for having deactivated any
/// prior active row in the same transaction.
fn insert_row_with_active(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    version: u32,
    now_ms: i64,
    body_hash: &BodyHash,
    consent_model: &str,
    active: bool,
) -> Result<(), StoreError> {
    // Reject anything outside the closed enum *before* projecting, so
    // schema drift / manual SQL corruption cannot ride forward into a
    // silently-downgraded `legacy_event` rewrite. This mirrors the
    // strict parse in `upsert_in_tx` for prior rows.
    let _: cairn_core::domain::consent_timeline::ConsentModel = parse_consent_model(consent_model)?;
    let cow_staged = !active;
    let mut row =
        ProjectedRow::from_record(record, version, now_ms, now_ms, body_hash, active, false)?;
    // Caller-supplied consent_model overrides the Phase-A default in
    // `from_record` so supersession preserves the prior row's value.
    row.consent_model = match consent_model {
        "receipt_timeline" => "receipt_timeline",
        "legacy_event" => "legacy_event",
        // Unreachable — parse_consent_model above already rejected
        // anything outside the closed enum.
        other => {
            return Err(StoreError::Invariant {
                what: format!("consent_model `{other}` survived parse_consent_model gate"),
            });
        }
    };
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO records ( \
            record_id, target_id, version, path, kind, class, visibility, \
            scope, actor_chain, body, body_hash, created_at, updated_at, \
            active, tombstoned, cow_staged, is_static, record_json, confidence, \
            salience, target_id_explicit, tags_json, consent_model, \
            schema_version_major, schema_version_minor \
         ) VALUES ( \
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25 \
         )",
        params![
            row.record_id,
            row.target_id,
            row.version,
            row.path,
            row.kind,
            row.class,
            row.visibility,
            row.scope,
            row.actor_chain,
            row.body,
            row.body_hash,
            row.created_at,
            row.updated_at,
            row.active,
            row.tombstoned,
            cow_staged,
            row.is_static,
            row.record_json,
            row.confidence,
            row.salience,
            row.target_id_explicit,
            row.tags_json,
            row.consent_model,
            row.schema_version_major,
            row.schema_version_minor,
        ],
    )?;
    if inserted == 0 {
        validate_existing_staged_row(
            tx,
            &row.record_id,
            &row.target_id,
            version,
            body_hash,
            active,
        )?;
    }
    Ok(())
}

fn validate_existing_staged_row(
    tx: &Transaction<'_>,
    record_id: &str,
    target_id: &str,
    version: u32,
    body_hash: &BodyHash,
    active: bool,
) -> Result<(), StoreError> {
    let existing = tx
        .query_row(
            "SELECT target_id, version, body_hash, active, tombstoned, cow_staged \
               FROM records WHERE record_id = ?1",
            params![record_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let expected_active = if active { 1 } else { 0 };
    let expected_tombstoned = 0;
    let expected_cow_staged = if active { 0 } else { 1 };
    match existing {
        Some((
            existing_target,
            existing_version,
            existing_hash,
            existing_active,
            tombstoned,
            cow_staged,
        )) if existing_target == target_id
            && existing_version == i64::from(version)
            && existing_hash == body_hash.as_str()
            && existing_active == expected_active
            && tombstoned == expected_tombstoned
            && cow_staged == expected_cow_staged =>
        {
            Ok(())
        }
        Some((
            existing_target,
            existing_version,
            existing_hash,
            existing_active,
            tombstoned,
            cow_staged,
        )) => Err(StoreError::Invariant {
            what: format!(
                "record_id conflict while staging upsert: record_id={record_id} \
                     existing target_id={existing_target} version={existing_version} \
                     body_hash={existing_hash} active={existing_active} tombstoned={tombstoned} \
                     cow_staged={cow_staged}; \
                     planned target_id={target_id} version={version} body_hash={} \
                     active={expected_active} tombstoned={expected_tombstoned} \
                     cow_staged={expected_cow_staged}",
                body_hash.as_str()
            ),
        }),
        None => Err(StoreError::Invariant {
            what: format!("staged upsert insert ignored without existing record_id={record_id}"),
        }),
    }
}

/// Mint a fresh ULID as a [`RecordId`]. Used by the body-changed branch of
/// [`SqliteMemoryStore::do_upsert`] to satisfy the `record_id` PRIMARY KEY
/// constraint while reusing the caller's `target_id` for supersession
/// lineage. Returns [`StoreError::Invariant`] if the ULID parser rejects the
/// minted string — ULID's contract guarantees a valid Crockford base32
/// 26-char output, so this branch is unreachable in practice but typed
/// rather than panicking.
fn mint_record_id() -> Result<RecordId, StoreError> {
    let raw = ulid::Ulid::new().to_string();
    RecordId::parse(raw.clone()).map_err(|e| StoreError::Invariant {
        what: format!("ulid produced invalid RecordId `{raw}`: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::record::tests_export::sample_record;

    use crate::open_in_memory;

    /// Issue #253: supersession must preserve `consent_model`. Without
    /// preservation, a row stamped `'receipt_timeline'` by Phase-B (#255)
    /// would silently revert to `'legacy_event'` on the next
    /// content-changing upsert — disabling §6.5 enforcement on the new
    /// active version.
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_supersession_preserves_consent_model() {
        let store = open_in_memory().await.expect("open in-memory store");

        let mut r = sample_record();
        r.body = "v1".to_owned();
        let outcome1 = store.upsert(&r).await.expect("upsert v1");
        assert!(outcome1.content_changed);

        let conn = store
            .require_conn("test.consent_model.preserve")
            .expect("invariant: connected store")
            .clone();

        // Phase-B simulation: stamp the active row 'receipt_timeline' by
        // direct SQL (Phase-A writers don't yet stamp). Stands in for the
        // future ingest writer so we can prove the supersession path
        // round-trips the column.
        conn.call(|c| {
            let n = c.execute(
                "UPDATE records SET consent_model = 'receipt_timeline' WHERE active = 1",
                [],
            )?;
            assert_eq!(n, 1, "expected one active row to stamp");
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("stamp receipt_timeline");

        // Content-changing upsert that supersedes.
        r.body = "v2 — different body".to_owned();
        let outcome2 = store.upsert(&r).await.expect("upsert v2");
        assert!(outcome2.content_changed);
        assert_eq!(outcome2.version, 2);

        // New active row must carry over 'receipt_timeline', not revert
        // to the migration default 'legacy_event'.
        let active_consent_model: String = conn
            .call(|c| {
                c.query_row(
                    "SELECT consent_model FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query active consent_model");
        assert_eq!(
            active_consent_model, "receipt_timeline",
            "supersession must preserve consent_model from prior active row",
        );
    }

    /// First-version inserts (no prior row) explicitly persist
    /// `'legacy_event'` rather than relying on the migration default.
    /// Pins the Phase-A behavior; Phase-B (#255) will source this from
    /// the record itself.
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_first_version_writes_legacy_event_explicitly() {
        let store = open_in_memory().await.expect("open in-memory store");
        let r = sample_record();
        store.upsert(&r).await.expect("upsert");

        let conn = store
            .require_conn("test.consent_model.first_version")
            .expect("invariant: connected store")
            .clone();
        let model: String = conn
            .call(|c| {
                c.query_row(
                    "SELECT consent_model FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query");
        assert_eq!(model, "legacy_event");
    }

    /// Round 8 (signature safety): `consent_model` is store-side
    /// authorization metadata, NOT signed payload. Persisting it inside
    /// `record_json` would silently invalidate any signature computed
    /// over the canonical bytes. Pin the contract: the column carries
    /// the value, JSON does not. Reads (`get`/`list`) re-hydrate the
    /// field from the column after JSON deserialize, so callers see a
    /// consistent post-store record without paying a signature break.
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_keeps_consent_model_out_of_record_json() {
        use cairn_core::contract::memory_store::MemoryStore;
        use cairn_core::domain::consent_timeline::ConsentModel;

        let store = open_in_memory().await.expect("open in-memory store");

        // Round 9: ordinary upsert ignores caller-supplied
        // consent_model. Use direct SQL to simulate Phase-B's
        // privileged transition API stamping receipt_timeline.
        let r = sample_record();
        let _ = store.upsert(&r).await.expect("upsert");

        let conn = store
            .require_conn("test.consent_model.json_excluded")
            .expect("invariant: connected store")
            .clone();
        conn.call(|c| {
            c.execute(
                "UPDATE records SET consent_model = 'receipt_timeline' WHERE active = 1",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("stamp");

        let (col, json_str): (String, String) = conn
            .call(|c| {
                c.query_row(
                    "SELECT consent_model, record_json FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query");
        assert_eq!(col, "receipt_timeline", "hot column carries the value");
        assert!(
            !json_str.contains("consent_model"),
            "record_json must NOT include consent_model (signature-safety): {json_str}"
        );
        // And the read path stamps the column value back onto the
        // hydrated record so callers see the authoritative model.
        let got = store.get(&r.id).await.expect("get").expect("present");
        assert_eq!(got.consent_model, Some(ConsentModel::ReceiptTimeline));
    }

    /// Round 7: a corrupted `consent_model` value in the prior active
    /// row must surface as `StoreError::Invariant`, not be silently
    /// rewritten to `legacy_event`.
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_rejects_corrupt_prior_consent_model() {
        use cairn_core::contract::memory_store::MemoryStore;

        let store = open_in_memory().await.expect("open in-memory store");
        let mut r = sample_record();
        r.body = "v1".to_owned();
        store.upsert(&r).await.expect("upsert v1");

        // Bypass the CHECK constraint with PRAGMA writable_schema =
        // OFF; we can't actually inject an unknown enum into a CHECK'd
        // column, so simulate corruption by dropping the CHECK first.
        // (This mirrors what schema drift / manual SQL repair would do
        // before subsequently rewriting the row.)
        let conn = store
            .require_conn("test.consent_model.corrupt_prior")
            .expect("invariant: connected store")
            .clone();
        conn.call(|c| {
            // Disable the CHECK by recreating the column without it,
            // then write a bogus value. SQLite ALTER TABLE cannot drop
            // a CHECK in-place without a full table rewrite, so we use
            // sqlite_master rewrite under writable_schema.
            c.execute("PRAGMA writable_schema = ON", [])?;
            c.execute(
                "UPDATE sqlite_master SET sql = REPLACE(sql, \
                 'CHECK (consent_model IN (''legacy_event'', ''receipt_timeline''))', '') \
                 WHERE type = 'table' AND name = 'records'",
                [],
            )?;
            c.execute("PRAGMA writable_schema = OFF", [])?;
            // Force the schema change to take effect on this conn.
            c.execute_batch("VACUUM")?;
            // Now corrupt the active row.
            let n = c.execute(
                "UPDATE records SET consent_model = 'banana' WHERE active = 1",
                [],
            )?;
            assert_eq!(n, 1, "expected one active row to corrupt");
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("inject corruption");

        // Body-changing upsert should now fail rather than silently
        // rewriting the corrupt row to legacy_event.
        r.body = "v2".to_owned();
        let res = store.upsert(&r).await;
        assert!(
            res.is_err(),
            "upsert against corrupt prior consent_model must fail; got {res:?}",
        );
    }

    /// Round 9 (trust boundary): an ordinary upsert must NOT let a
    /// caller flip the `consent_model` gate. The field sits outside the
    /// signed bytes, so accepting `record.consent_model = Some(...)`
    /// would let anyone resubmit the same signed record with a
    /// different model and downgrade enforcement. The store ignores
    /// caller-supplied intent on write — transitions go through the
    /// Phase-B (#255) privileged transition API.
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_ignores_caller_supplied_consent_model_on_write() {
        use cairn_core::contract::memory_store::MemoryStore;
        use cairn_core::domain::consent_timeline::ConsentModel;

        let store = open_in_memory().await.expect("open in-memory store");

        // v1: caller tries to stamp ReceiptTimeline. The store must
        // ignore this and persist legacy_event (the fresh-insert
        // default).
        let mut r = sample_record();
        r.consent_model = Some(ConsentModel::ReceiptTimeline);
        store.upsert(&r).await.expect("upsert v1");

        let conn = store
            .require_conn("test.consent_model.ignored_on_write")
            .expect("invariant: connected store")
            .clone();
        let model_after_v1: String = conn
            .call(|c| {
                c.query_row(
                    "SELECT consent_model FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query v1");
        assert_eq!(
            model_after_v1, "legacy_event",
            "caller-supplied consent_model must be ignored on fresh insert",
        );

        // Stamp the row receipt_timeline via direct SQL (simulating
        // Phase-B's privileged transition API).
        conn.call(|c| {
            c.execute(
                "UPDATE records SET consent_model = 'receipt_timeline' WHERE active = 1",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("stamp via privileged path");

        // v2: caller tries to flip back to legacy_event via ordinary
        // upsert. Must be ignored — supersession inherits prior.
        r.body = "v2 body".to_owned();
        r.consent_model = Some(ConsentModel::LegacyEvent);
        store.upsert(&r).await.expect("upsert v2");

        let model_after_v2: String = conn
            .call(|c| {
                c.query_row(
                    "SELECT consent_model FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query v2");
        assert_eq!(
            model_after_v2, "receipt_timeline",
            "supersession must inherit prior consent_model, ignoring caller downgrade",
        );
    }

    /// Round 6: idempotent re-ingest of a stamped-but-stale row must
    /// restamp the schema-version columns to the host's current value.
    /// The §6.4 lint's `suggested_fix` instructs operators to "re-ingest
    /// the affected record(s)" — if this branch only touched NULL
    /// legacy stamps, an operator following that remediation against a
    /// stamped-but-stale row (e.g. row stamped at 0.0 while host is at
    /// 0.1) would see no change and the lint would never clear.
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_idempotent_restamps_stale_schema_version() {
        let store = open_in_memory().await.expect("open in-memory store");
        let r = sample_record();
        store.upsert(&r).await.expect("upsert v1");

        // Simulate a stale stamp: host wrote (0,1) above, force the row
        // back to (0,0) to mimic a row authored under an older binary.
        let conn = store
            .require_conn("test.schema_version.restamp")
            .expect("invariant: connected store")
            .clone();
        conn.call(|c| {
            c.execute(
                "UPDATE records SET schema_version_major = 0, schema_version_minor = 0 \
                 WHERE active = 1",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("force stale stamp");

        // Re-ingest the same body. The hash matches, hits the
        // idempotent branch — must restamp to current.
        store.upsert(&r).await.expect("re-ingest");

        let (major, minor): (i64, i64) = conn
            .call(|c| {
                c.query_row(
                    "SELECT schema_version_major, schema_version_minor \
                       FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query stamp");
        let current = cairn_core::contract::version::SchemaVersion::current();
        assert_eq!(
            (major, minor),
            (i64::from(current.major), i64::from(current.minor)),
            "idempotent re-ingest must restamp stale (0,0) to current",
        );
    }

    /// Round 6 companion: NULL legacy stamps still get backfilled by
    /// the same path (regression for the round-3 behavior, kept after
    /// the NULL gate was dropped in round 6).
    #[tokio::test(flavor = "current_thread")]
    async fn upsert_idempotent_backfills_null_legacy_stamp() {
        let store = open_in_memory().await.expect("open in-memory store");
        let r = sample_record();
        store.upsert(&r).await.expect("upsert v1");

        let conn = store
            .require_conn("test.schema_version.null_backfill")
            .expect("invariant: connected store")
            .clone();
        conn.call(|c| {
            c.execute(
                "UPDATE records SET schema_version_major = NULL, schema_version_minor = NULL \
                 WHERE active = 1",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("force NULL legacy stamp");

        store.upsert(&r).await.expect("re-ingest");

        let (major, minor): (Option<i64>, Option<i64>) = conn
            .call(|c| {
                c.query_row(
                    "SELECT schema_version_major, schema_version_minor \
                       FROM records WHERE active = 1 LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
                )
                .map_err(Into::into)
            })
            .await
            .expect("query stamp");
        let current = cairn_core::contract::version::SchemaVersion::current();
        assert_eq!(
            (major, minor),
            (
                Some(i64::from(current.major)),
                Some(i64::from(current.minor))
            ),
            "idempotent re-ingest must backfill NULL legacy stamp to current",
        );
    }
}

#[cfg(test)]
mod cow_tests {
    use std::sync::Arc;

    use cairn_core::config::EmbeddingModelKind;
    use cairn_core::contract::memory_store::{
        KeywordSearchArgs, ListArgs, MemoryStore, SemanticSearchArgs,
    };
    use cairn_core::domain::record::tests_export::sample_record;
    use cairn_embeddings_local::{EmbeddingModel, MockEmbedder};

    use crate::store::upsert::{activate_upsert_in_tx, plan_upsert_in_tx, stage_upsert_cow_in_tx};
    use crate::{open_in_memory, open_in_memory_with_embedder};

    fn keyword_args(query: &str) -> KeywordSearchArgs<'static> {
        KeywordSearchArgs {
            query: query.to_owned(),
            filter: None,
            auth_scope: cairn_core::domain::ScopeTuple::default(),
            visibility_allowlist: vec![],
            limit: 10,
            cursor: None,
            with_explain: false,
        }
    }

    fn semantic_args(query: &str, model: EmbeddingModelKind) -> SemanticSearchArgs<'static> {
        SemanticSearchArgs {
            query: query.to_owned(),
            filter: None,
            auth_scope: cairn_core::domain::ScopeTuple::default(),
            visibility_allowlist: vec![],
            limit: 10,
            model_label: model.as_str().to_owned(),
            with_explain: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cow_stage_inserts_inactive_row() {
        let store = open_in_memory().await.expect("open");
        let conn = store.require_conn("test").expect("connected").clone();
        let record = sample_record();

        conn.call(move |c| {
            let tx = c.transaction()?;
            let plan = plan_upsert_in_tx(&tx, &record)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            stage_upsert_cow_in_tx(&tx, &record, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let (active, tombstoned, cow_staged): (i64, i64, i64) = tx.query_row(
                "SELECT active, tombstoned, cow_staged FROM records WHERE record_id = ?1",
                rusqlite::params![plan.outcome_record_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(active, 0);
            assert_eq!(tombstoned, 0);
            assert_eq!(cow_staged, 1);
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("cow stage");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cow_stage_allows_replay_of_same_inactive_row() {
        let store = open_in_memory().await.expect("open");
        let conn = store.require_conn("test").expect("connected").clone();
        let record = sample_record();

        conn.call(move |c| {
            let tx = c.transaction()?;
            let plan = plan_upsert_in_tx(&tx, &record)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            stage_upsert_cow_in_tx(&tx, &record, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            stage_upsert_cow_in_tx(&tx, &record, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let (count, active): (i64, i64) = tx.query_row(
                "SELECT COUNT(*), COALESCE(MAX(active), 0) FROM records WHERE record_id = ?1",
                rusqlite::params![plan.outcome_record_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(count, 1);
            assert_eq!(active, 0);
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("cow replay");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cow_staged_supersession_is_hidden_until_activation() {
        let model = EmbeddingModelKind::BgeSmallEnV1_5;
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(model));
        let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
            .await
            .expect("open");

        let mut record = sample_record();
        record.body = "olderactiveonly baseline body".to_owned();
        let active = store.upsert(&record).await.expect("seed active row");

        let mut staged = record.clone();
        staged.body = "freshstagedtoken body staged before activation".to_owned();
        let staged_vector: Vec<u8> = embedder
            .embed_document(&staged.body)
            .expect("embed staged body")
            .iter()
            .flat_map(|&f| f.to_le_bytes())
            .collect();
        let model_label = model.as_str().to_owned();

        let conn = store.require_conn("test").expect("connected").clone();
        let staged_id = conn
            .call(move |c| {
                let tx = c.transaction()?;
                let plan = plan_upsert_in_tx(&tx, &staged)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                stage_upsert_cow_in_tx(&tx, &staged, &plan)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                tx.execute(
                    "INSERT INTO record_vectors(record_id, embedding, model) VALUES (?1, ?2, ?3)",
                    rusqlite::params![plan.outcome_record_id.as_str(), staged_vector, model_label,],
                )?;
                let staged_id = plan.outcome_record_id.clone();
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(staged_id)
            })
            .await
            .expect("stage supersession without activation");

        assert!(
            store
                .get(&active.record_id)
                .await
                .expect("get active")
                .is_some(),
            "the previously active row must remain readable",
        );
        assert!(
            store.get(&staged_id).await.expect("get staged").is_none(),
            "a COW row staged by WAL must remain hidden until primary.activate runs",
        );

        let listed = store
            .list(&ListArgs {
                limit: 10,
                ..ListArgs::default()
            })
            .await
            .expect("list");
        assert_eq!(listed.records.len(), 1);
        assert_eq!(listed.records[0].id, active.record_id);

        let versions = store.versions(&record.target_id).await.expect("versions");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].record_id, active.record_id);

        let keyword_page = store
            .search_keyword(&keyword_args("freshstagedtoken"))
            .await
            .expect("keyword search");
        assert!(
            keyword_page.candidates.is_empty(),
            "keyword search must not expose a staged COW row",
        );

        let semantic_page = store
            .search_semantic(&semantic_args("freshstagedtoken", model))
            .await
            .expect("semantic search");
        assert!(
            semantic_page
                .candidates
                .iter()
                .all(|candidate| candidate.record_id != staged_id),
            "semantic search must not expose a staged COW row",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cow_activate_flips_active_pointer_in_one_transaction() {
        let store = open_in_memory().await.expect("open");
        let conn = store.require_conn("test").expect("connected").clone();
        let record = sample_record();

        conn.call(move |c| {
            let tx = c.transaction()?;
            let plan = plan_upsert_in_tx(&tx, &record)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            stage_upsert_cow_in_tx(&tx, &record, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            activate_upsert_in_tx(&tx, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let active: i64 = tx.query_row(
                "SELECT active FROM records WHERE record_id = ?1",
                rusqlite::params![plan.outcome_record_id.as_str()],
                |row| row.get(0),
            )?;
            assert_eq!(active, 1);
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("cow activate");
    }
}
