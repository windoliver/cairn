//! §6.2 — `broken_actor_chain` check.
//!
//! Cryptographic re-verification of stored records' signatures requires a
//! `verify_record_at_rest` function that does not exist in `cairn-core`
//! today (the existing `verifier::verify_signed_intent` is ingest-time
//! only). PR-1 emits a single `deferred_check` info finding pointing at
//! the follow-up issue; #256 removes this stub and wires the real check.

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{finding, LintInputs};

const TRACKING_ISSUE: i64 = 256;

/// Run the §6.2 broken-actor-chain check.
///
/// Until `verify_record_at_rest` exists (tracked in #256), this always emits
/// a single `deferred_check` info finding so the gap is visible in lint output.
#[must_use]
pub fn run(_inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "actor-chain crypto re-verification requires verify_record_at_rest \
             (tracked in #{TRACKING_ISSUE})"
        ),
    );
    f.tracking_issue = Some(TRACKING_ISSUE);
    f.suggested_fix = Some(format!(
        "ship #{TRACKING_ISSUE} to enable the real broken_actor_chain check"
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
    fn always_emits_one_info_finding_pointing_at_256() {
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            schema_version: SchemaVersion { major: 0, minor: 1 },
        };
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Info);
        assert_eq!(f[0].tracking_issue, Some(256));
        assert!(f[0].suggested_fix.is_some());
    }
}
