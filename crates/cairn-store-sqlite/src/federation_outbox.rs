//! `SQLite` `FederationOutbox` adapter (brief §12.a, issue #123 task T13).
//!
//! Each method runs its two side effects — consent-journal append + job
//! enqueue (or record upsert / tombstone) — inside a single
//! `SqliteMemoryStore::with_tx` closure so they share one `rusqlite`
//! `IMMEDIATE` transaction. `Ok` commits both; `Err` rolls back both.
//!
//! The consent insert delegates to [`crate::consent::append_in_tx`].
//! Job insertion uses inline SQL against the `workflow_jobs` schema
//! (migration 0020). Record upsert delegates to
//! `upsert_in_tx`. Tombstone uses the
//! `StoreTx::tombstone` helper.

use async_trait::async_trait;

use cairn_core::contract::federation_outbox::{FederationOutbox, FederationOutboxError};
use cairn_core::contract::job_store::EnqueueRequest;
use cairn_core::contract::memory_store::TombstoneReason;
use cairn_core::domain::{ConsentEvent, MemoryRecord};

use crate::store::SqliteMemoryStore;

/// Detect whether a `StoreError` wraps a `UNIQUE constraint` violation
/// on the `workflow_jobs_dedupe_uniq` index. `SQLite` formats the message
/// as `"UNIQUE constraint failed: workflow_jobs.kind,
/// workflow_jobs.dedupe_key"`.
fn is_dedupe_unique_violation(err: &crate::error::StoreError) -> bool {
    match err {
        crate::error::StoreError::Sqlite(rusqlite::Error::SqliteFailure(_, Some(msg))) => {
            msg.contains("workflow_jobs.dedupe_key")
        }
        _ => false,
    }
}

#[async_trait]
impl FederationOutbox for SqliteMemoryStore {
    async fn record_share_grant(
        &self,
        event: &ConsentEvent,
        job: EnqueueRequest,
    ) -> Result<(), FederationOutboxError> {
        let event = event.clone();
        self.with_tx(move |tx| {
            tx.append_consent_event(&event)?;
            enqueue_job_in_tx(&tx.tx, &job)?;
            Ok(())
        })
        .await
        .map_err(store_error_to_outbox_with_job)
    }

    async fn record_share_accept(
        &self,
        event: &ConsentEvent,
        upserts: &[MemoryRecord],
    ) -> Result<(), FederationOutboxError> {
        let event = event.clone();
        let upserts = upserts.to_vec();
        self.with_tx(move |tx| {
            tx.append_consent_event(&event)?;
            for record in &upserts {
                tx.upsert(record)?;
            }
            Ok(())
        })
        .await
        .map_err(store_error_to_outbox_no_job)
    }

    async fn record_share_revoke(
        &self,
        event: &ConsentEvent,
        tombstone_ids: &[String],
        consent_ref: Option<&str>,
    ) -> Result<(), FederationOutboxError> {
        let event = event.clone();
        let tombstone_ids = tombstone_ids.to_vec();
        let consent_ref = consent_ref.map(ToOwned::to_owned);
        self.with_tx(move |tx| {
            tx.append_consent_event(&event)?;
            // Tombstone by explicit IDs first.
            for id_str in &tombstone_ids {
                let id = cairn_core::domain::RecordId::parse(id_str.clone()).map_err(|err| {
                    crate::error::StoreError::Invariant {
                        what: format!("invalid record id in tombstone_ids: {err}"),
                    }
                })?;
                tx.tombstone(&id, TombstoneReason::FederationRevoke)?;
            }
            // If no explicit IDs were provided but a consent_ref was,
            // query records by the consent_ref provenance marker and
            // tombstone those. This handles the case where the consent
            // payload stores hashes (per §14) instead of raw IDs.
            if tombstone_ids.is_empty() && let Some(ref cref) = consent_ref {
                let mut stmt = tx.tx.prepare(
                    "SELECT id FROM records \
                     WHERE json_extract(extra_frontmatter, '$.consent_ref') = ?1 \
                       AND tombstoned = 0",
                )?;
                let ids: Vec<String> = stmt
                    .query_map(rusqlite::params![cref], |row| row.get::<_, String>(0))?
                    .collect::<Result<_, _>>()?;
                for id_str in ids {
                    if let Ok(id) = cairn_core::domain::RecordId::parse(id_str) {
                        tx.tombstone(&id, TombstoneReason::FederationRevoke)?;
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(store_error_to_outbox_no_job)
    }

    async fn record_share_revoke_grant(
        &self,
        event: &ConsentEvent,
        job: EnqueueRequest,
    ) -> Result<(), FederationOutboxError> {
        let event = event.clone();
        self.with_tx(move |tx| {
            tx.append_consent_event(&event)?;
            enqueue_job_in_tx(&tx.tx, &job)?;
            Ok(())
        })
        .await
        .map_err(store_error_to_outbox_with_job)
    }
}

/// Insert a `workflow_jobs` row inside an existing transaction.
///
/// Mirrors the column set from migration 0020. State starts at `queued`,
/// `attempts = 0`, `delivery_count = 0`, no lease fields.
fn enqueue_job_in_tx(
    tx: &rusqlite::Transaction<'_>,
    req: &EnqueueRequest,
) -> Result<(), crate::error::StoreError> {
    let now_ms = crate::store::current_unix_ms();
    tx.execute(
        "INSERT INTO workflow_jobs \
           (job_id, kind, payload, state, attempts, delivery_count, \
            max_attempts, base_backoff_ms, backoff_multiplier, max_backoff_ms, \
            queue_key, dedupe_key, next_run_at, enqueued_at, updated_at) \
         VALUES (?1, ?2, ?3, 'queued', 0, 0, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            req.job_id.as_str(),
            req.kind.as_str(),
            req.payload,
            req.retry.max_attempts,
            req.retry.base_backoff_ms,
            req.retry.backoff_multiplier,
            req.retry.max_backoff_ms,
            req.queue_key,
            req.dedupe_key,
            req.not_before_ms,
            now_ms,
            now_ms,
        ],
    )?;
    Ok(())
}

/// Map a `StoreError` from a grant/revoke-grant path (which can produce
/// `DuplicateJob`) into a `FederationOutboxError`.
fn store_error_to_outbox_with_job(err: crate::error::StoreError) -> FederationOutboxError {
    if is_dedupe_unique_violation(&err) {
        return FederationOutboxError::DuplicateJob;
    }
    store_error_to_outbox_no_job(err)
}

/// Map a `StoreError` from a path that cannot produce `DuplicateJob`.
fn store_error_to_outbox_no_job(err: crate::error::StoreError) -> FederationOutboxError {
    match err {
        crate::error::StoreError::InvalidConsentEvent(e) => {
            FederationOutboxError::ConsentRejected {
                message: e.to_string(),
            }
        }
        other => FederationOutboxError::Backend {
            source: Box::new(other),
        },
    }
}
