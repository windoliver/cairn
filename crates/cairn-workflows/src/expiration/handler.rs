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
use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore, TombstoneReason};
use cairn_core::domain::flush_plan::ExpirationReason;
use cairn_core::domain::record::MemoryRecord;
use cairn_core::pipeline::expiration::decide;
use tracing::{info, warn};

use crate::expiration::ExpirationPayload;
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
}

impl ExpirationHandler {
    /// Construct a handler bound to `store` + `config`. The config
    /// drives both the `enabled` gate and the TTL/salience thresholds
    /// the pure decision function consults.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>, config: ExpirationConfig) -> Self {
        Self { store, config }
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
        let mut cursor = None;
        // Hard cap on records inspected per sweep — keeps a single job
        // from monopolising the worker pool. Subsequent sweeps drain
        // any leftover backlog.
        let cap = self.config.batch_size as usize;

        loop {
            if report.tombstoned() as usize >= cap {
                break;
            }
            let args = ListArgs {
                scope: payload.bound_scope.clone(),
                cursor: cursor.clone(),
                limit: cap,
                ..ListArgs::default()
            };
            let page = self.store.list(&args).await?;
            if page.records.is_empty() {
                break;
            }
            for record in &page.records {
                self.process_record(record, payload.now_ms, &mut report)
                    .await?;
                if report.tombstoned() as usize >= cap {
                    break;
                }
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        info!(
            ttl = report.ttl_expired,
            salience = report.salience_below,
            kept = report.kept,
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
        };
        let report = h.run_once(&payload).await.expect("run_once");
        assert_eq!(report.tombstoned(), 0);
        assert_eq!(report.kept, 0);
    }
}
