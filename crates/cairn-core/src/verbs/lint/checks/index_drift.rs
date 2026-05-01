//! §6.7 — `index_drift` check.
//!
//! Compares `IndexStats.records_active` against `IndexStats.fts5_rows`.
//! A mismatch means the FTS5 mirror has not caught up with the canonical
//! records table — typically a write that didn't propagate to the
//! derived index. Counts-only; content-level resampling is deferred to
//! the projection-drift work in #254.

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_path};

/// Emit one error finding per drifted derived index. Today there is only
/// one derived index in the stack (FTS5); future indexes (sqlite-vec)
/// will add their own counts to `IndexStats` and a corresponding check
/// here.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let s = inputs.index_stats;
    let mut out = Vec::new();
    if s.fts5_rows != s.records_active {
        let mut f = finding(
            Kind::IndexDrift,
            Severity::Error,
            format!(
                "FTS5 rows ({}) do not match active records ({})",
                s.fts5_rows, s.records_active,
            ),
        );
        f.target = Some(target_path("records_fts"));
        f.suggested_fix = Some(
            "rebuild the FTS5 mirror; transient drift will resolve on the next ingest".to_owned(),
        );
        out.push(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::IndexStats;
    use crate::verbs::lint::SchemaVersion;

    fn inputs_with(stats: IndexStats) -> (CairnConfig, IndexStats, SchemaVersion) {
        (
            CairnConfig::default(),
            stats,
            SchemaVersion { major: 0, minor: 1 },
        )
    }

    #[test]
    fn balanced_counts_produce_no_finding() {
        let (cfg, stats, ver) = inputs_with(IndexStats::new(10, 10));
        let li = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: stats,
            schema_version: ver,
        };
        assert!(run(&li).is_empty());
    }

    #[test]
    fn fts_lag_flagged_with_path_target() {
        let (cfg, stats, ver) = inputs_with(IndexStats::new(10, 8));
        let li = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: stats,
            schema_version: ver,
        };
        let f = run(&li);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::IndexDrift);
        assert_eq!(f[0].severity, Severity::Error);
        let target = f[0].target.as_ref().expect("target set");
        assert_eq!(target.path.as_deref(), Some("records_fts"));
        assert!(f[0].suggested_fix.is_some());
        assert!(f[0].message.contains('8'));
        assert!(f[0].message.contains("10"));
    }

    #[test]
    fn fts_overshoot_also_flagged() {
        // FTS5 rows > records_active is also drift (e.g. a delete that
        // did not propagate to the mirror).
        let (cfg, stats, ver) = inputs_with(IndexStats::new(10, 12));
        let li = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: stats,
            schema_version: ver,
        };
        let f = run(&li);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::IndexDrift);
    }

    #[test]
    fn empty_balanced_vault_clean() {
        let (cfg, stats, ver) = inputs_with(IndexStats::new(0, 0));
        let li = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: stats,
            schema_version: ver,
        };
        assert!(run(&li).is_empty());
    }
}
