//! §6.5 — sensor-consent enforcement.
//!
//! Full enforcement (covering-grant resolution, scope binding,
//! expiry/revoke window, state-at-issue) requires the receipt timeline
//! introduced in #253. PR-1 emits a single `deferred_check` info finding
//! pointing at that issue; #253 wires the real sub-check matrix and
//! removes this stub.

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding};

const TRACKING_ISSUE: i64 = 253;

/// Always emits one info-severity `DeferredCheck` finding pointing at
/// the follow-up issue.
#[must_use]
pub fn run(_inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "sensor-consent enforcement requires the receipt timeline introduced in #{TRACKING_ISSUE}"
        ),
    );
    f.tracking_issue = Some(TRACKING_ISSUE);
    f.suggested_fix = Some(format!(
        "ship #{TRACKING_ISSUE} to enable §6.5 (covering grant + scope + window + state checks)"
    ));
    vec![f]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::IndexStats;
    use crate::verbs::lint::SchemaVersion;

    #[test]
    fn always_emits_one_info_finding_pointing_at_253() {
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            schema_version: SchemaVersion { major: 0, minor: 1 },
            author_states: crate::verbs::lint::empty_author_states(),
        };
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Info);
        assert_eq!(f[0].tracking_issue, Some(253));
        assert!(f[0].suggested_fix.is_some());
    }
}
