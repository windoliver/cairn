//! §6.6 — `hot_memory_over_budget` check.
//!
//! Static byte-budget estimation requires either a per-record `hot_scope`
//! predicate or a pure-function variant of the assembler's recipe walker
//! — neither exists in `cairn-core` today. PR-1 emits a single
//! `deferred_check` info finding pointing at the follow-up issue; #259
//! chooses the framing and wires the real check.

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding};

const TRACKING_ISSUE: i64 = 259;

/// Always emits one info-severity `DeferredCheck` finding pointing at
/// the follow-up issue.
#[must_use]
pub fn run(_inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "hot-memory budget estimation requires a hot_scope predicate or recipe walker \
             (tracked in #{TRACKING_ISSUE})"
        ),
    );
    f.tracking_issue = Some(TRACKING_ISSUE);
    f.suggested_fix = Some(format!(
        "ship #{TRACKING_ISSUE} to enable the real hot_memory_over_budget check"
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
    fn always_emits_one_info_finding_pointing_at_259() {
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            schema_version: SchemaVersion { major: 0, minor: 1 },
            author_states: None,
        };
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Info);
        assert_eq!(f[0].tracking_issue, Some(259));
        assert!(f[0].suggested_fix.is_some());
    }
}
