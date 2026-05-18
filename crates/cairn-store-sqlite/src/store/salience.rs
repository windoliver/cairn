//! Salience access tracking and decay helpers.

use cairn_core::contract::memory_store::{
    AccessUpdate, DecayBatchOutcome, DecayPolicy, EvictionCandidate,
};
use cairn_core::domain::RecordId;
use cairn_core::pipeline::salience::{apply_access, decay_salience};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::record_from_json;

const MILLIS_PER_DAY: i64 = 86_400_000;

#[allow(
    clippy::cast_possible_truncation,
    reason = "SQLite REAL is f64, but salience is a bounded f32 domain value"
)]
fn stored_salience_to_f32(value: f64) -> f32 {
    value as f32
}

impl SqliteMemoryStore {
    pub(crate) async fn do_record_access(
        &self,
        record_ids: &[RecordId],
        accessed_at_ms: i64,
        reason: &str,
    ) -> Result<Vec<AccessUpdate>, StoreError> {
        let conn = self.require_conn("record_access")?.clone();
        let ids: Vec<String> = record_ids.iter().map(|id| id.as_str().to_owned()).collect();
        let reason = reason.to_owned();

        conn.call(move |c| {
            let tx = c.transaction()?;
            let mut updates = Vec::new();
            for id in ids {
                let row = tx
                    .query_row(
                        "SELECT salience FROM records \
                         WHERE record_id = ?1 AND active = 1 AND tombstoned = 0 AND cow_staged = 0",
                        params![id],
                        |row| row.get::<_, f64>(0),
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;
                let Some(old_salience) = row else {
                    continue;
                };
                let old = stored_salience_to_f32(old_salience);
                let new = apply_access(old);
                tx.execute(
                    "UPDATE records \
                     SET salience = ?2, last_accessed_at_ms = ?3 \
                     WHERE record_id = ?1 AND active = 1 AND tombstoned = 0 AND cow_staged = 0",
                    params![id, f64::from(new), accessed_at_ms],
                )?;
                let record_id = RecordId::parse(id).map_err(|e| {
                    tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                        what: format!("invalid stored record_id during access update: {e}"),
                    }))
                })?;
                updates.push(AccessUpdate {
                    record_id,
                    old_salience: old,
                    new_salience: new,
                    last_accessed_at_ms: accessed_at_ms,
                    reason: reason.clone(),
                });
            }
            tx.commit()?;
            Ok(updates)
        })
        .await
        .map_err(StoreError::from)
    }

    pub(crate) async fn do_pin_record(
        &self,
        record_id: &RecordId,
        pinned: bool,
    ) -> Result<(), StoreError> {
        let conn = self.require_conn("pin_record")?.clone();
        let id = record_id.as_str().to_owned();
        let value = i64::from(pinned);
        conn.call(move |c| {
            let changed = c.execute(
                "UPDATE records SET pinned = ?2 \
                 WHERE record_id = ?1 AND active = 1 AND tombstoned = 0 AND cow_staged = 0",
                params![id, value],
            )?;
            if changed == 0 {
                return Ok::<_, tokio_rusqlite::Error>(Err(StoreError::NotFound { id }));
            }
            Ok(Ok(()))
        })
        .await?
    }

    pub(crate) async fn do_decay_salience_batch(
        &self,
        now_ms: i64,
        policy: DecayPolicy,
    ) -> Result<DecayBatchOutcome, StoreError> {
        let conn = self.require_conn("decay_salience_batch")?.clone();
        let limit = i64::from(policy.batch_limit.max(1));

        conn.call(move |c| {
            let tx = c.transaction()?;
            let rows = {
                let mut stmt = tx.prepare(
                    "SELECT record_id, record_json, salience, updated_at, last_accessed_at_ms \
                     FROM records \
                     WHERE active = 1 AND tombstoned = 0 AND cow_staged = 0 AND pinned = 0 \
                     ORDER BY COALESCE(last_accessed_at_ms, updated_at), record_id \
                     LIMIT ?1",
                )?;
                stmt.query_map(params![limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
            };

            let mut candidates = Vec::new();
            let mut processed = 0u32;
            for (id, json, salience, updated_at, last_accessed_at_ms) in rows {
                let basis = last_accessed_at_ms.unwrap_or(updated_at);
                let elapsed_ms = now_ms.saturating_sub(basis);
                let days_i64 = elapsed_ms / MILLIS_PER_DAY;
                let days = u32::try_from(days_i64).unwrap_or(u32::MAX);
                if days == 0 {
                    continue;
                }
                processed = processed.saturating_add(1);
                let old = stored_salience_to_f32(salience);
                let new = decay_salience(old, policy.decay_rate, days);
                tx.execute(
                    "UPDATE records SET salience = ?2 WHERE record_id = ?1",
                    params![id, f64::from(new)],
                )?;
                let record = record_from_json(&json)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                if new < policy.eviction_threshold
                    && days > policy.min_age_days
                    && source_forget_permitted(
                        &tx,
                        &record.provenance.source_hash,
                        &record.scope.canonical_wire(),
                        now_ms,
                    )?
                {
                    let record_id = RecordId::parse(id).map_err(|e| {
                        tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                            what: format!("invalid stored record_id during salience decay: {e}"),
                        }))
                    })?;
                    candidates.push(EvictionCandidate {
                        record_id,
                        old_salience: old,
                        new_salience: new,
                        age_days: days,
                    });
                }
            }
            tx.commit()?;
            Ok(DecayBatchOutcome {
                records_processed: processed,
                eviction_candidates: candidates,
            })
        })
        .await
        .map_err(StoreError::from)
    }
}

fn source_forget_permitted(
    tx: &Transaction<'_>,
    source_hash: &str,
    scope: &str,
    now_ms: i64,
) -> Result<bool, rusqlite::Error> {
    let source_hash = source_hash.trim();
    if source_hash.is_empty() {
        return Ok(false);
    }
    let latest_kind = tx
        .query_row(
            "SELECT kind \
             FROM consent_journal \
             WHERE subject = ?1 \
               AND scope = ?2 \
               AND kind IN ('grant', 'revoke') \
               AND decided_at <= ?3 \
               AND (expires_at IS NULL OR expires_at > ?3) \
             ORDER BY decided_at DESC, rowid DESC \
             LIMIT 1",
            params![source_hash, scope, now_ms],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(matches!(latest_kind.as_deref(), Some("grant")))
}
