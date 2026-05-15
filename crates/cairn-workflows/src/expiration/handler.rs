//! `ExpirationHandler` — TTL/salience-based soft-retirement
//! (issue #91, brief §10.0, §6).
//!
//! Pages through `MemoryStore::list`, asks the pure decision function
//! in `cairn_core::pipeline::expiration::decide` whether each record
//! should retire, and calls
//! [`MemoryStore::tombstone(_, TombstoneReason::Expire)`](
//! cairn_core::contract::memory_store::MemoryStore::tombstone) for the
//! winners. The store's `list_active_stored` filters tombstoned rows
//! out of default reads (brief §10 "removes from default reads"),
//! satisfying AC#2 of issue #91 without a schema change.
//!
//! Hard delete is not this workflow's job — that's the `forget` verb.

use std::sync::Arc;

use cairn_core::config::ExpirationConfig;
use cairn_core::contract::job_store::{
    EnqueueRequest, JobId, JobKind, JobPayload, JobStore, RetryPolicy,
};
use cairn_core::contract::memory_store::{ListArgs, ListCursor, MemoryStore, TombstoneReason};
use cairn_core::domain::RecordId;
use cairn_core::domain::flush_plan::ExpirationReason;
use cairn_core::domain::record::MemoryRecord;
use cairn_core::pipeline::expiration::decide;
use tracing::{info, warn};

use crate::expiration::{ExpirationCursor, ExpirationPayload};
use crate::scheduler::{HandlerOutcome, JobHandler};

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const EXPIRATION_KIND: &str = "expiration.sweep";

/// Per-sweep summary surfaced to callers and tests so they can assert
/// the workflow's behaviour without parsing log lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExpirationSweepReport {
    /// Records tombstoned with reason `TtlExpired`.
    pub ttl_expired: u32,
    /// Records tombstoned with reason `SalienceBelowThreshold`.
    pub salience_below: u32,
    /// Records inspected but kept active.
    pub kept: u32,
}

impl ExpirationSweepReport {
    /// Total tombstones written this sweep.
    #[must_use]
    pub const fn tombstoned(&self) -> u32 {
        self.ttl_expired.saturating_add(self.salience_below)
    }
}

/// Minimum-path `ExpirationWorkflow` handler.
pub struct ExpirationHandler {
    store: Arc<dyn MemoryStore>,
    config: ExpirationConfig,
    /// Optional follow-up enqueue path. When wired, the handler
    /// enqueues a continuation job at the current cursor whenever
    /// `batch_size` records are inspected without exhausting the
    /// store cursor — round-6 adversarial review #1: without this
    /// path, a fresh-prefix vault with > `batch_size` non-expired
    /// records will re-scan the same prefix on every sweep and
    /// never reach the older records that should expire.
    job_store: Option<Arc<dyn JobStore>>,
}

impl ExpirationHandler {
    /// Construct a handler bound to `store` + `config`. The config
    /// drives both the `enabled` gate and the TTL/salience thresholds
    /// the pure decision function consults.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>, config: ExpirationConfig) -> Self {
        Self {
            store,
            config,
            job_store: None,
        }
    }

    /// Construct a handler with a continuation-enqueue path. After
    /// every sweep that hits the inspect-cap without exhausting the
    /// store cursor the handler enqueues a follow-up sweep starting
    /// at the cursor, so older records cannot be permanently starved
    /// behind a fresh prefix.
    #[must_use]
    pub fn with_job_store(
        store: Arc<dyn MemoryStore>,
        config: ExpirationConfig,
        job_store: Arc<dyn JobStore>,
    ) -> Self {
        Self {
            store,
            config,
            job_store: Some(job_store),
        }
    }

    /// Run a sweep and return the per-reason counts. Tests use this
    /// to assert idempotency without driving the scheduler.
    ///
    /// # Errors
    /// Surfaces any `MemoryStore` error encountered while paging the
    /// active records or tombstoning the picked subset.
    pub async fn run_once(
        &self,
        payload: &ExpirationPayload,
    ) -> Result<ExpirationSweepReport, Box<dyn std::error::Error + Send + Sync>> {
        let mut report = ExpirationSweepReport::default();
        // Seed `cursor` from the payload so continuation jobs
        // resume exactly where the prior sweep stopped instead of
        // re-scanning the newest-first prefix every time (round-6
        // adversarial review #1).
        let mut cursor: Option<ListCursor> = payload
            .cursor
            .as_ref()
            .map(|c| {
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(ListCursor {
                    updated_at: c.updated_at,
                    record_id: RecordId::parse(c.record_id.clone())?,
                })
            })
            .transpose()?;
        // Hard cap on records *inspected* per sweep — keeps a single
        // job from monopolising the worker pool (round-5 adversarial
        // review #3). Previously this cap was on
        // `report.tombstoned()`, which in a vault of mostly-fresh
        // records never tripped and let one job page through the
        // entire tenant. When the cap fires *without* exhausting
        // the cursor, enqueue a continuation so older records
        // cannot starve behind a fresh prefix.
        let cap = self.config.batch_size as usize;
        let mut inspected: usize = 0;
        let mut cursor_exhausted = false;

        while inspected < cap {
            let remaining = cap.saturating_sub(inspected);
            let args = ListArgs {
                scope: payload.bound_scope.clone(),
                cursor: cursor.clone(),
                limit: remaining.max(1),
                ..ListArgs::default()
            };
            let page = self.store.list(&args).await?;
            if page.records.is_empty() {
                cursor_exhausted = true;
                break;
            }
            for record in &page.records {
                self.process_record(record, payload.now_ms, &mut report)
                    .await?;
                inspected = inspected.saturating_add(1);
                if inspected >= cap {
                    break;
                }
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => {
                    cursor_exhausted = true;
                    break;
                }
            }
        }

        // Continuation enqueue. Only fires when the cap stopped us
        // before the cursor naturally exhausted AND a JobStore is
        // wired (round-6 adversarial review #1).
        //
        // Dedupe identity (round-7 adversarial review #1): the
        // `job_id` folds in *every* dimension that distinguishes one
        // logical sweep handoff from another — `now_ms`, the
        // canonical bound-scope wire, and the cursor position. Two
        // independent sweeps that happen to share a cursor but
        // differ in `now_ms` or `bound_scope` get distinct ids and
        // do not collide; a re-enqueue from a retry after the same
        // sweep crashed at the same cursor gets the same id and the
        // store's `dedupe_key` makes it a no-op.
        //
        // Enqueue *failures* propagate as `Err` so the scheduler
        // retries the parent sweep — silently warning would drop a
        // logical continuation, starving old records exactly the
        // way round-6 #1 was trying to prevent.
        if !cursor_exhausted
            && let Some(jobs) = self.job_store.as_ref()
            && let Some(next_cursor) = cursor.as_ref()
        {
            let scope_wire = payload
                .bound_scope
                .as_ref()
                .map(cairn_core::domain::ScopeTuple::canonical_wire)
                .unwrap_or_default();
            let job_id = JobId::new(format!(
                "expiration:continuation:{scope_wire}:{now_ms}:{at}:{rid}",
                now_ms = payload.now_ms,
                at = next_cursor.updated_at,
                rid = next_cursor.record_id.as_str(),
            ));
            let continuation = ExpirationPayload {
                now_ms: payload.now_ms,
                bound_scope: payload.bound_scope.clone(),
                cursor: Some(ExpirationCursor {
                    updated_at: next_cursor.updated_at,
                    record_id: next_cursor.record_id.as_str().to_owned(),
                }),
            };
            let bytes = continuation.to_bytes()?;
            let req = EnqueueRequest {
                job_id: job_id.clone(),
                kind: JobKind::new(EXPIRATION_KIND),
                payload: bytes,
                queue_key: None,
                dedupe_key: Some(job_id.as_str().to_owned()),
                not_before_ms: 0,
                retry: RetryPolicy::DEFAULT,
            };
            match jobs.enqueue(req).await {
                Ok(()) => {}
                // Idempotent re-enqueue from a handler retry: the
                // continuation is already queued / leased / done.
                // Treat as success and let the existing job drive
                // the next sweep.
                Err(
                    cairn_core::contract::job_store::JobStoreError::DuplicateDedupeKey { .. },
                ) => {
                    info!(
                        job_id = job_id.as_str(),
                        "expiration: continuation already enqueued (dedupe hit)"
                    );
                }
                Err(e) => return Err(Box::new(e)),
            }
        }

        info!(
            ttl = report.ttl_expired,
            salience = report.salience_below,
            kept = report.kept,
            inspected,
            cursor_exhausted,
            "expiration: sweep complete"
        );
        Ok(report)
    }

    async fn process_record(
        &self,
        record: &MemoryRecord,
        now_ms: i64,
        report: &mut ExpirationSweepReport,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let updated_ms = record.updated_at.as_chrono().timestamp_millis();
        match decide(record.salience, updated_ms, now_ms, &self.config) {
            Some(ExpirationReason::TtlExpired) => {
                self.store
                    .tombstone(&record.id, TombstoneReason::Expire)
                    .await?;
                report.ttl_expired = report.ttl_expired.saturating_add(1);
            }
            Some(ExpirationReason::SalienceBelowThreshold) => {
                self.store
                    .tombstone(&record.id, TombstoneReason::Expire)
                    .await?;
                report.salience_below = report.salience_below.saturating_add(1);
            }
            Some(other) => {
                // The decision function currently only emits `TtlExpired`
                // or `SalienceBelowThreshold`; future reasons (e.g.
                // `SupersededByCanonical`) flow through the same
                // tombstone call. Counted under `ttl_expired` until a
                // per-reason counter lands.
                warn!(reason = ?other, record_id = %record.id.as_str(),
                    "expiration: unexpected reason — tombstoning under TTL bucket");
                self.store
                    .tombstone(&record.id, TombstoneReason::Expire)
                    .await?;
                report.ttl_expired = report.ttl_expired.saturating_add(1);
            }
            None => {
                report.kept = report.kept.saturating_add(1);
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl JobHandler for ExpirationHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(EXPIRATION_KIND)
    }

    async fn handle(&self, payload_bytes: &JobPayload) -> HandlerOutcome {
        let payload = match ExpirationPayload::from_bytes(payload_bytes) {
            Ok(p) => p,
            Err(e) => {
                return HandlerOutcome::Permanent {
                    reason: format!("expiration payload decode failed: {e}"),
                };
            }
        };

        if !self.config.enabled {
            return HandlerOutcome::Permanent {
                reason: "expiration.enabled = false in config".into(),
            };
        }

        match self.run_once(&payload).await {
            Ok(_report) => HandlerOutcome::Done,
            Err(e) => HandlerOutcome::Retry {
                reason: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::NoopMemoryStore;

    #[tokio::test]
    async fn handle_returns_permanent_when_disabled() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let h = ExpirationHandler::new(
            store,
            ExpirationConfig {
                enabled: false,
                ..ExpirationConfig::default()
            },
        );
        let p = ExpirationPayload {
            now_ms: 0,
            bound_scope: None,
            cursor: None,
        };
        let bytes = p.to_bytes().expect("encode");
        let outcome = h.handle(&bytes).await;
        assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    }

    #[tokio::test]
    async fn handle_returns_permanent_when_decode_fails() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let h = ExpirationHandler::new(store, ExpirationConfig::default());
        let outcome = h.handle(&b"{nope".to_vec()).await;
        assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    }

    #[tokio::test]
    async fn empty_store_keeps_counts_zero() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let h = ExpirationHandler::new(
            store,
            ExpirationConfig {
                enabled: true,
                ttl_days: 30,
                salience_floor: 0.0,
                batch_size: 8,
            },
        );
        let payload = ExpirationPayload {
            now_ms: 100,
            bound_scope: None,
            cursor: None,
        };
        let report = h.run_once(&payload).await.expect("run_once");
        assert_eq!(report.tombstoned(), 0);
        assert_eq!(report.kept, 0);
    }
}
