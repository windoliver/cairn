//! `lint` verb — read-only health checks.
//!
//! Spec: `docs/superpowers/specs/2026-04-30-lint-checks-design.md`.
//! Issue: <https://github.com/windoliver/cairn/issues/96>.

use crate::config::CairnConfig;
use crate::contract::memory_store::{IndexStats, StoredRecord};
use crate::domain::record::RecordId;
use crate::generated::verbs::lint::{
    Finding, Kind, LintData, LintDataSummary, LintDataSummaryBySeverity, Severity, Target,
};

pub mod checks;

/// One linted record + the per-row `consent_model` gate from the records
/// table. PR-1 always carries `LegacyEvent` because the migration that
/// adds the column is part of #253; lint behavior in PR-1 is independent
/// of this value (the §6.5 deferred-info finding is emitted unconditionally).
#[derive(Debug, Clone)]
pub struct LintRecord {
    /// The stored record under audit.
    pub stored: StoredRecord,
    /// Per-row consent-model gate; see #253.
    pub consent_model: ConsentModel,
}

/// Per-record consent-storage model. PR-1 always sees `LegacyEvent`;
/// `ReceiptTimeline` is wired in #253.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConsentModel {
    /// Pre-#253 storage model: generic consent events.
    LegacyEvent,
    /// Post-#253 storage model: per-grant timeline.
    ReceiptTimeline,
}

/// Snapshot the check engine operates over. Pure inputs; no I/O.
#[derive(Debug)]
pub struct LintInputs<'a> {
    /// Active records under audit.
    pub records: &'a [LintRecord],
    /// Resolved config snapshot.
    pub config: &'a CairnConfig,
    /// Counts driving the index-drift check.
    pub index_stats: IndexStats,
    /// Current contract version reported by the runtime — drives §6.4.
    pub schema_version: SchemaVersion,
}

/// Major.minor schema version for the §6.4 staleness check. Patch is
/// irrelevant for schema lag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaVersion {
    /// Contract major.
    pub major: u32,
    /// Contract minor.
    pub minor: u32,
}

impl SchemaVersion {
    /// Minor delta `record → current`. Returns `u32::MAX` on major
    /// mismatch (treated as ≥2 minors behind by the schema check).
    #[must_use]
    pub fn minors_behind(self, record: SchemaVersion) -> u32 {
        if record.major != self.major {
            return u32::MAX;
        }
        self.minor.saturating_sub(record.minor)
    }
}

/// Run every check, aggregate findings, return the canonical `LintData`.
#[must_use]
pub fn run_checks(inputs: &LintInputs<'_>) -> LintData {
    let mut findings: Vec<Finding> = Vec::new();
    findings.extend(checks::malformed::run(inputs));
    findings.extend(checks::actor_chain::run(inputs));
    findings.extend(checks::provenance::run(inputs));
    findings.extend(checks::schema::run(inputs));
    findings.extend(checks::hot_memory::run(inputs));
    findings.extend(checks::index_drift::run(inputs));
    findings.extend(checks::consent_deferred::run(inputs));
    let summary = summarize(&findings);
    LintData {
        findings,
        summary,
        report_path: None,
    }
}

fn summarize(findings: &[Finding]) -> LintDataSummary {
    let mut by_severity = LintDataSummaryBySeverity {
        error: 0,
        warning: 0,
        info: 0,
    };
    let mut by_kind = serde_json::Map::new();
    for f in findings {
        match f.severity {
            Severity::Error => by_severity.error += 1,
            Severity::Warning => by_severity.warning += 1,
            Severity::Info => by_severity.info += 1,
        }
        let key = kind_key(f.kind);
        let entry = by_kind
            .entry(key)
            .or_insert_with(|| serde_json::Value::Number(0.into()));
        if let Some(n) = entry.as_u64() {
            *entry = serde_json::Value::Number((n + 1).into());
        }
    }
    LintDataSummary {
        total: findings.len() as u64,
        by_severity,
        by_kind: serde_json::Value::Object(by_kind),
    }
}

fn kind_key(k: Kind) -> String {
    match k {
        Kind::Contradiction => "contradiction",
        Kind::Orphan => "orphan",
        Kind::Stale => "stale",
        Kind::MissingConcept => "missing_concept",
        Kind::DataGap => "data_gap",
        Kind::MalformedRecord => "malformed_record",
        Kind::BrokenActorChain => "broken_actor_chain",
        Kind::MissingProvenance => "missing_provenance",
        Kind::StaleSchema => "stale_schema",
        Kind::HotMemoryOverBudget => "hot_memory_over_budget",
        Kind::IndexDrift => "index_drift",
        Kind::DeferredCheck => "deferred_check",
    }
    .to_owned()
}

/// Construct a finding with no target / fix / tracking issue.
pub(crate) fn finding(kind: Kind, severity: Severity, message: impl Into<String>) -> Finding {
    Finding {
        kind,
        message: message.into(),
        severity,
        suggested_fix: None,
        target: None,
        tracking_issue: None,
    }
}

/// Build a `Target` pointing at a record id.
///
/// `Ulid` is `pub struct Ulid(pub String)` with no infallible constructor
/// validation — the newtype wraps without copying the validation logic here
/// because `RecordId::as_str()` already guarantees a syntactically valid ULID
/// was accepted at parse time.
pub(crate) fn target_record(id: &RecordId) -> Target {
    Target {
        record_id: Some(crate::generated::common::Ulid(id.as_str().to_owned())),
        operation_id: None,
        path: None,
    }
}

/// Build a `Target` pointing at a vault path or table name.
// Allow dead_code: used by forthcoming check stubs (index_drift, schema, etc.); scaffolded here so all checks share one helper.
#[allow(dead_code)]
pub(crate) fn target_path(path: impl Into<String>) -> Target {
    Target {
        record_id: None,
        operation_id: None,
        path: Some(path.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::record::tests_export::sample_record;

    fn legacy_lint_record() -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record: sample_record(),
                version: 1,
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    #[test]
    fn run_checks_on_empty_inputs_returns_no_findings_yet() {
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            schema_version: SchemaVersion { major: 0, minor: 1 },
        };
        let data = run_checks(&inputs);
        // actor_chain emits one deferred-check info finding (#256); all other
        // stubs return empty.
        assert_eq!(data.summary.total, data.findings.len() as u64);
        assert_eq!(data.summary.by_severity.error, 0);
        assert_eq!(data.summary.by_severity.warning, 0);
        // Exactly the one deferred info finding from actor_chain (§6.2/#256).
        assert_eq!(data.summary.by_severity.info, 1);
    }

    #[test]
    fn run_checks_with_one_record_aggregates_summary_correctly() {
        let cfg = CairnConfig::default();
        let r = legacy_lint_record();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            schema_version: SchemaVersion { major: 0, minor: 1 },
        };
        let data = run_checks(&inputs);
        assert_eq!(data.summary.total, data.findings.len() as u64);
        // actor_chain emits one deferred-check info finding (#256); all other
        // stubs return empty.
        assert_eq!(data.summary.by_severity.error, 0);
        assert_eq!(data.summary.by_severity.warning, 0);
        assert_eq!(data.summary.by_severity.info, 1);
    }
}
