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

use cairn_core::contract::memory_store::UpsertOutcome;
use cairn_core::domain::{BodyHash, MemoryRecord, RecordId};
use rusqlite::{Transaction, params};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::projection::{ProjectedRow, body_hash_from_str};
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Active-row tuple as read out of the `records` table:
/// `(record_id, version, body_hash)`. Only the active row for a given
/// `target_id` is ever returned (partial unique index `records_active_target_idx`).
type PriorActive = (String, i64, String);

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
        let conn = self.require_conn("upsert")?.clone();
        let record = record.clone();

        let outcome = conn
            .call(move |c| {
                let mut tx = c.transaction()?;
                let outcome = upsert_in_tx(&mut tx, &record)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                tx.commit()?;
                Ok::<_, tokio_rusqlite::Error>(outcome)
            })
            .await?;

        Ok(outcome)
    }
}

/// Synchronous upsert worker — runs inside the `tokio_rusqlite` worker
/// thread holding an open `Transaction`. The caller commits on `Ok` and
/// drops (rolling back) on `Err`. Pulled out of `do_upsert` so the async
/// shell stays under the workspace's `clippy::too_many_lines` limit.
fn upsert_in_tx(
    tx: &mut Transaction<'_>,
    record: &MemoryRecord,
) -> Result<UpsertOutcome, StoreError> {
    let body_hash = BodyHash::compute(&record.body);
    let prior = read_active(tx, record.target_id.as_str())?;
    let now_ms = current_unix_ms();

    if let Some((prior_id, prior_version, prior_hash_str)) = prior.as_ref() {
        let prior_hash = body_hash_from_str(prior_hash_str)?;
        if prior_hash == body_hash {
            return idempotent_outcome(record, prior_id, *prior_version, prior_hash);
        }
    }

    let (version, prior_hash, new_record_id) = match prior.as_ref() {
        Some((prior_id, prior_version, prior_hash_str)) => {
            // Body changed: deactivate prior + mint a fresh PK for the new row.
            tx.execute(
                "UPDATE records SET active = 0, updated_at = ?1 \
                  WHERE record_id = ?2",
                params![now_ms, prior_id],
            )?;
            let next_version = next_version(*prior_version)?;
            let prior_hash = body_hash_from_str(prior_hash_str)?;
            (next_version, Some(prior_hash), Some(mint_record_id()?))
        }
        None => (1u32, None, None),
    };

    // `record_id` is the version-row PK; supersession requires a fresh one.
    // The contract docstring on `UpsertOutcome.record_id` ("produced (or
    // re-used)") explicitly permits the store to synthesize. The first
    // version of a `target_id` keeps the caller-provided id (matches
    // `target_id == id` for fresh records, brief §3).
    let mut row_record = record.clone();
    if let Some(ref synthesized) = new_record_id {
        row_record.id = synthesized.clone();
    }
    insert_row(tx, &row_record, version, now_ms, &body_hash)?;

    let outcome_id = new_record_id.unwrap_or_else(|| record.id.clone());
    Ok(UpsertOutcome {
        record_id: outcome_id,
        target_id: record.target_id.clone(),
        version,
        content_changed: true,
        prior_hash,
    })
}

/// Read the active row for `target_id`, if any. Returns `None` (not an
/// error) when no active row exists.
fn read_active(tx: &Transaction<'_>, target_id: &str) -> Result<Option<PriorActive>, StoreError> {
    let row = tx
        .query_row(
            "SELECT record_id, version, body_hash \
               FROM records \
              WHERE target_id = ?1 AND active = 1 \
              LIMIT 1",
            params![target_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
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

/// Build the [`UpsertOutcome`] for the idempotent (no-op) branch. Echoes
/// the row's actual `record_id` (which may differ from `record.id` if a
/// prior supersession synthesized a new id).
fn idempotent_outcome(
    record: &MemoryRecord,
    prior_id: &str,
    prior_version: i64,
    prior_hash: BodyHash,
) -> Result<UpsertOutcome, StoreError> {
    let version = u32::try_from(prior_version).map_err(|_| StoreError::Invariant {
        what: format!("prior version overflows u32: {prior_version}"),
    })?;
    let row_id = RecordId::parse(prior_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid record_id `{prior_id}`: {e}"),
    })?;
    Ok(UpsertOutcome {
        record_id: row_id,
        target_id: record.target_id.clone(),
        version,
        content_changed: false,
        prior_hash: Some(prior_hash),
    })
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

/// Project + insert one new version row. The caller is responsible for
/// having deactivated any prior active row in the same transaction.
fn insert_row(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    version: u32,
    now_ms: i64,
    body_hash: &BodyHash,
) -> Result<(), StoreError> {
    let row = ProjectedRow::from_record(record, version, now_ms, now_ms, body_hash, true, false)?;
    tx.execute(
        "INSERT INTO records ( \
            record_id, target_id, version, path, kind, class, visibility, \
            scope, actor_chain, body, body_hash, created_at, updated_at, \
            active, tombstoned, is_static, record_json, confidence, \
            salience, target_id_explicit, tags_json \
         ) VALUES ( \
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21 \
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
            row.is_static,
            row.record_json,
            row.confidence,
            row.salience,
            row.target_id_explicit,
            row.tags_json,
        ],
    )?;
    Ok(())
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
