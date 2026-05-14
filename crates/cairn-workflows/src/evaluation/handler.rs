//! `EvaluationHandler` — runs the configured `GoldenCheck` set,
//! upserts a deterministic `Reasoning` report record, and emits
//! `MetricEvent::EvaluationCompleted` to the configured `MetricsSink`
//! (issue #91, brief §15).

use std::sync::Arc;

use cairn_core::config::EvaluationConfig;
use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::contract::metrics::MetricsSink;
use cairn_core::domain::{
    ScopeTuple,
    metrics::MetricEvent,
    taxonomy::{MemoryClass, MemoryKind},
};
use tracing::{info, warn};

use crate::evaluation::EvaluationPayload;
use crate::evaluation::golden_check::{CheckOutcome, GoldenCheck};
use crate::scheduler::{HandlerOutcome, JobHandler};
use crate::synthetic::{SyntheticRecordSpec, build_synthetic_record, stable_target_id};

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const EVALUATION_KIND: &str = "evaluation.golden_checks";

const EVAL_AGENT_ID: &str = "agt:cairn-workflows:evaluation-handler:v1";
const EVAL_SENSOR_ID: &str = "snr:cairn-workflows:evaluation:v1";
const EVAL_CONSENT_REF: &str = "consent:system:evaluation-workflow";

/// Per-sweep summary surfaced to callers and tests so they can assert
/// the workflow's behaviour without parsing the report record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationReport {
    /// Total number of `GoldenCheck`s executed this sweep.
    pub checks_run: u32,
    /// Subset of `checks_run` that returned `CheckOutcome::Passed`.
    pub passed: u32,
    /// Subset of `checks_run` that returned `CheckOutcome::Failed`.
    pub failed: u32,
    /// Stable `target_id` of the upserted report record (when
    /// `write_report_record = true`).
    pub report_target_id: Option<String>,
}

/// Minimum-path `EvaluationWorkflow` handler.
pub struct EvaluationHandler {
    store: Arc<dyn MemoryStore>,
    metrics: Arc<dyn MetricsSink>,
    checks: Vec<Arc<dyn GoldenCheck>>,
    config: EvaluationConfig,
}

impl EvaluationHandler {
    /// Construct a handler. `checks` is the registry of available
    /// checks; the handler then narrows the actual run-list against
    /// `config.checks` (and the per-payload `check_ids`).
    #[must_use]
    pub fn new(
        store: Arc<dyn MemoryStore>,
        metrics: Arc<dyn MetricsSink>,
        checks: Vec<Arc<dyn GoldenCheck>>,
        config: EvaluationConfig,
    ) -> Self {
        Self {
            store,
            metrics,
            checks,
            config,
        }
    }

    fn select_checks(&self, payload: &EvaluationPayload) -> Vec<Arc<dyn GoldenCheck>> {
        let allow: Option<Vec<String>> = if !payload.check_ids.is_empty() {
            Some(payload.check_ids.clone())
        } else if !self.config.checks.is_empty() {
            Some(self.config.checks.clone())
        } else {
            None
        };
        match allow {
            Some(ids) => self
                .checks
                .iter()
                .filter(|c| ids.iter().any(|id| id == c.id()))
                .cloned()
                .collect(),
            None => self.checks.clone(),
        }
    }

    /// Run the sweep and return the summary. Used directly from tests
    /// to assert idempotency without driving the scheduler.
    ///
    /// # Errors
    /// Propagates any underlying store or check failure.
    pub async fn run_once(
        &self,
        payload: &EvaluationPayload,
    ) -> Result<EvaluationReport, Box<dyn std::error::Error + Send + Sync>> {
        let checks = self.select_checks(payload);
        let mut passed = 0_u32;
        let mut failed = 0_u32;
        let mut findings: Vec<(String, CheckOutcome)> = Vec::with_capacity(checks.len());

        for check in &checks {
            let outcome = check.run(self.store.as_ref()).await?;
            match &outcome {
                CheckOutcome::Passed => passed = passed.saturating_add(1),
                CheckOutcome::Failed { .. } => failed = failed.saturating_add(1),
            }
            findings.push((check.id().to_owned(), outcome));
        }

        // Sort by check id so the report body is deterministic for a
        // given set of outcomes — brief §15 release gating depends on
        // byte-stable replays.
        findings.sort_by(|a, b| a.0.cmp(&b.0));

        let checks_run = u32::try_from(findings.len()).unwrap_or(u32::MAX);
        let report_target_id = if self.config.write_report_record {
            Some(self.upsert_report_record(payload, &findings).await?)
        } else {
            None
        };

        let metric = MetricEvent::EvaluationCompleted {
            ts_ms: payload.ts_ms,
            report_target_id: report_target_id.clone().unwrap_or_default(),
            checks_run,
            passed,
            failed,
        };
        if let Err(e) = self.metrics.emit(metric).await {
            warn!(error = %e, "evaluation: metrics sink emit failed");
        }

        info!(checks_run, passed, failed, "evaluation: sweep complete");
        Ok(EvaluationReport {
            checks_run,
            passed,
            failed,
            report_target_id,
        })
    }

    async fn upsert_report_record(
        &self,
        payload: &EvaluationPayload,
        findings: &[(String, CheckOutcome)],
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // Build a deterministic Markdown body. Same findings → same
        // bytes → same body hash → store dedupes on idempotent
        // replays.
        let mut body = String::with_capacity(256 + findings.len() * 64);
        body.push_str("# Evaluation report\n\n");
        for (id, outcome) in findings {
            body.push_str("- ");
            body.push_str(id);
            body.push_str(": ");
            match outcome {
                CheckOutcome::Passed => body.push_str("PASS"),
                CheckOutcome::Failed { details } => {
                    body.push_str("FAIL — ");
                    body.push_str(details);
                }
            }
            body.push('\n');
        }

        let key_basis = findings
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        // `target_key` includes the calendar day from `ts_ms` so a
        // sweep run twice on the same day with the same check set
        // hits the same target_id and the store body-hash dedupe
        // makes it a no-op.
        let day = payload.ts_ms / 86_400_000;
        let target_key = format!("evaluation:{day}:{key_basis}");
        let target_id = stable_target_id(&target_key)?;
        let target_id_str = target_id.as_str().to_owned();

        let mut extras = std::collections::BTreeMap::new();
        extras.insert(
            "evaluation".to_owned(),
            serde_json::json!({
                "checks": findings.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                "ts_ms":  payload.ts_ms,
                "produced_by": "cairn-workflows::EvaluationHandler",
            }),
        );

        // Evaluation report records are global — there's no obvious
        // tenant/session to stamp. `ScopeTuple::validate` rejects an
        // empty tuple, so we always set the `agent` dimension to the
        // workflow's identity. Brief §6.5 validation passes; the
        // record is still findable by every scope filter the verb
        // layer applies (none narrow on `agent` by default).
        let scope = ScopeTuple {
            agent: Some(EVAL_AGENT_ID.to_owned()),
            ..ScopeTuple::default()
        };
        let record = build_synthetic_record(SyntheticRecordSpec {
            kind: MemoryKind::Reasoning,
            class: MemoryClass::Procedural,
            scope,
            body,
            target_key: &target_key,
            extras,
            agent_id: EVAL_AGENT_ID,
            sensor_id: EVAL_SENSOR_ID,
            consent_ref: EVAL_CONSENT_REF,
            record_id_override: None,
        })?;

        let outcome = self.store.upsert(&record).await?;
        if outcome.content_changed {
            info!(target_id = %target_id_str, "evaluation: report record upserted");
        } else {
            info!(
                target_id = %target_id_str,
                "evaluation: report record idempotent replay"
            );
        }
        Ok(target_id_str)
    }
}

#[async_trait::async_trait]
impl JobHandler for EvaluationHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(EVALUATION_KIND)
    }

    async fn handle(&self, payload_bytes: &JobPayload) -> HandlerOutcome {
        let payload = match EvaluationPayload::from_bytes(payload_bytes) {
            Ok(p) => p,
            Err(e) => {
                return HandlerOutcome::Permanent {
                    reason: format!("evaluation payload decode failed: {e}"),
                };
            }
        };

        if !self.config.enabled {
            return HandlerOutcome::Permanent {
                reason: "evaluation.enabled = false in config".into(),
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
    use crate::evaluation::golden_check::default_checks;
    use crate::test_support::NoopMemoryStore;
    use cairn_core::contract::metrics::CapturingMetricsSink;

    fn metrics() -> Arc<CapturingMetricsSink> {
        Arc::new(CapturingMetricsSink::new())
    }

    #[tokio::test]
    async fn handle_returns_permanent_when_disabled() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let sink = metrics();
        let h = EvaluationHandler::new(
            store,
            sink.clone() as Arc<dyn MetricsSink>,
            default_checks(),
            EvaluationConfig {
                enabled: false,
                ..EvaluationConfig::default()
            },
        );
        let p = EvaluationPayload {
            ts_ms: 0,
            check_ids: vec![],
        };
        let bytes = p.to_bytes().expect("encode");
        let outcome = h.handle(&bytes).await;
        assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
        assert!(sink.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn empty_store_passes_starter_checks_and_emits_metric() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let sink = metrics();
        let h = EvaluationHandler::new(
            store,
            sink.clone() as Arc<dyn MetricsSink>,
            default_checks(),
            EvaluationConfig {
                enabled: true,
                write_report_record: false,
                ..EvaluationConfig::default()
            },
        );
        let payload = EvaluationPayload {
            ts_ms: 1_700_000_000_000,
            check_ids: vec![],
        };
        let report = h.run_once(&payload).await.expect("run_once");
        assert_eq!(report.checks_run, 2);
        assert_eq!(report.passed, 2);
        assert_eq!(report.failed, 0);
        let events = sink.snapshot().await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            MetricEvent::EvaluationCompleted {
                passed,
                failed,
                checks_run,
                ..
            } => {
                assert_eq!(*passed, 2);
                assert_eq!(*failed, 0);
                assert_eq!(*checks_run, 2);
            }
            _ => panic!("expected EvaluationCompleted"),
        }
    }
}
