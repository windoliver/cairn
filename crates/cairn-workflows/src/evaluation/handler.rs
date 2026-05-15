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

    /// Resolve the check allow-list against the registered set.
    /// Returns `Err` with a stable string if any requested id is
    /// unknown — the handler escalates that to `Permanent` so a
    /// typo'd / removed id cannot silently produce a green sweep
    /// (round-2 adversarial review #3). When neither payload nor
    /// config nominates an allow-list, every registered check
    /// runs.
    fn select_checks(
        &self,
        payload: &EvaluationPayload,
    ) -> Result<Vec<Arc<dyn GoldenCheck>>, String> {
        let allow: Option<&[String]> = if !payload.check_ids.is_empty() {
            Some(payload.check_ids.as_slice())
        } else if !self.config.checks.is_empty() {
            Some(self.config.checks.as_slice())
        } else {
            None
        };
        let Some(ids) = allow else {
            // No allow-list — fall back to the full registry. Empty
            // registry is itself a misconfiguration; let the empty
            // check below reject it.
            let all = self.checks.clone();
            if all.is_empty() {
                return Err("evaluation: no GoldenChecks registered".into());
            }
            return Ok(all);
        };

        let unknown: Vec<&str> = ids
            .iter()
            .filter(|id| !self.checks.iter().any(|c| c.id() == id.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "evaluation: unknown check ids {:?}",
                unknown.join(",")
            ));
        }
        let selected: Vec<_> = self
            .checks
            .iter()
            .filter(|c| ids.iter().any(|id| id == c.id()))
            .cloned()
            .collect();
        if selected.is_empty() {
            return Err("evaluation: allow-list resolved to zero checks".into());
        }
        Ok(selected)
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
        let checks = self
            .select_checks(payload)
            .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e))?;
        let mut passed = 0_u32;
        let mut failed = 0_u32;
        let mut findings: Vec<(String, CheckOutcome)> = Vec::with_capacity(checks.len());

        for check in &checks {
            let outcome = check
                .run(self.store.as_ref(), payload.bound_scope.as_ref())
                .await?;
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

        // Compute the stable `report_target_id` unconditionally so
        // the metric always carries a deterministic dedupe key even
        // when `write_report_record = false` — without this, two
        // unrelated no-report sweeps at the same `ts_ms` would
        // collapse in downstream `(report_target_id, ts_ms)`
        // queries (round-3 adversarial review #2).
        let target_key = self.report_target_key(payload, &findings);
        let target_id = stable_target_id(&target_key).map_err(|e| {
            Box::<dyn std::error::Error + Send + Sync>::from(e.to_string())
        })?;
        let report_target_id = target_id.as_str().to_owned();

        let _report_was_new = if self.config.write_report_record {
            self.upsert_report_record(payload, &findings, &target_key)
                .await?
        } else {
            true
        };

        // Brief §15 release gating semantics: at-least-once metric
        // emission with deterministic `report_target_id`. Always
        // emit, always retry on sink failure — round-2 adversarial
        // review #1 showed that gating on
        // `upsert_outcome.content_changed` permanently loses the
        // metric when emit fails on the *first* run (next run sees
        // dedupe and skips). Downstream gating queries DISTINCT on
        // `(report_target_id, ts_ms)` to collapse duplicates.
        let metric = MetricEvent::EvaluationCompleted {
            ts_ms: payload.ts_ms,
            report_target_id: report_target_id.clone(),
            checks_run,
            passed,
            failed,
        };
        self.metrics.emit(metric).await.map_err(|e| {
            warn!(error = %e, "evaluation: metrics sink emit failed");
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })?;

        info!(checks_run, passed, failed, "evaluation: sweep complete");
        Ok(EvaluationReport {
            checks_run,
            passed,
            failed,
            report_target_id: Some(report_target_id),
        })
    }

    /// Build the deterministic `target_key` for both the upserted
    /// report record and the emitted metric. Same inputs *AND* same
    /// outcomes → same key → same `target_id` (round-3 adversarial
    /// review #2). The outcome hash is folded in so a retry after a
    /// vault mutation that flips a check's pass/fail does NOT
    /// silently overwrite the earlier report — instead the
    /// downstream gating sees both versions as distinct records
    /// (round-4 adversarial review #3).
    fn report_target_key(
        &self,
        payload: &EvaluationPayload,
        findings: &[(String, CheckOutcome)],
    ) -> String {
        let check_ids = findings
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let scope_wire = payload
            .bound_scope
            .as_ref()
            .map(ScopeTuple::canonical_wire)
            .unwrap_or_default();
        let day = payload.ts_ms / 86_400_000;
        // Outcome digest: stable serialization of pass/fail per
        // check id. `CheckOutcome::Failed { details }` participates
        // in the hash so a failure detail change also splits the
        // target.
        let outcome_basis: String = findings
            .iter()
            .map(|(id, outcome)| match outcome {
                CheckOutcome::Passed => format!("{id}=P"),
                CheckOutcome::Failed { details } => format!("{id}=F:{details}"),
            })
            .collect::<Vec<_>>()
            .join("|");
        let outcome_hash =
            crate::synthetic::sha256_hex(outcome_basis.as_bytes());
        // Use only the first 16 hex chars of the digest to keep the
        // key short; collisions at that prefix are vanishingly
        // unlikely (1 in 2^64) and the full hash lives in the
        // upserted record's `extras.evaluation.outcome_hash`.
        format!(
            "evaluation:{scope_wire}:{day}:{check_ids}:{prefix}",
            prefix = &outcome_hash[..16]
        )
    }

    /// Upsert the synthesized `Reasoning` report record.
    ///
    /// Returns `was_new = false` when the store deduped against the
    /// prior body hash for this target (a replay of an
    /// already-completed sweep). The caller computes the
    /// deterministic `target_key` so the metric and the upsert share
    /// the same `target_id`.
    async fn upsert_report_record(
        &self,
        payload: &EvaluationPayload,
        findings: &[(String, CheckOutcome)],
        target_key: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
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

        let target_id = stable_target_id(target_key)?;
        let target_id_str = target_id.as_str().to_owned();

        let outcome_basis: String = findings
            .iter()
            .map(|(id, outcome)| match outcome {
                CheckOutcome::Passed => format!("{id}=P"),
                CheckOutcome::Failed { details } => format!("{id}=F:{details}"),
            })
            .collect::<Vec<_>>()
            .join("|");
        let outcome_hash = crate::synthetic::sha256_hex(outcome_basis.as_bytes());

        let mut extras = std::collections::BTreeMap::new();
        extras.insert(
            "evaluation".to_owned(),
            serde_json::json!({
                "checks": findings.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                "ts_ms":  payload.ts_ms,
                "outcome_hash": outcome_hash,
                "produced_by": "cairn-workflows::EvaluationHandler",
            }),
        );

        // Stamp the report with the caller's `bound_scope` so a
        // tenant-scoped read sees its own report, never another
        // tenant's. `ScopeTuple::validate` rejects empty tuples, so
        // for the single-tenant P0 path (no scope) fall back to the
        // workflow agent dim (round-1 adversarial review #4).
        let scope = match payload.bound_scope.as_ref() {
            Some(base) => {
                let mut s = base.clone();
                if s.agent.is_none() {
                    s.agent = Some(EVAL_AGENT_ID.to_owned());
                }
                s
            }
            None => ScopeTuple {
                agent: Some(EVAL_AGENT_ID.to_owned()),
                ..ScopeTuple::default()
            },
        };
        let record = build_synthetic_record(SyntheticRecordSpec {
            kind: MemoryKind::Reasoning,
            class: MemoryClass::Procedural,
            scope,
            body,
            target_key,
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
        Ok(outcome.content_changed)
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

        // Reject mis-configured allow-lists permanently — a typo'd
        // check id won't fix itself across retries (round-2
        // adversarial review #3).
        if let Err(e) = self.select_checks(&payload) {
            return HandlerOutcome::Permanent { reason: e };
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
            bound_scope: None,
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
            bound_scope: None,
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
