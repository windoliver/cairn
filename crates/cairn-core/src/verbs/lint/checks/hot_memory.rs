//! §6.6 — `hot_memory_over_budget` real walker (rewritten for #83;
//! closes #259's deferred-step canary).
//!
//! Walks the configured recipe via the assembler. When a
//! `hot_body_loader` is wired into [`LintInputs`], the walker computes
//! the exact prefix bytes and emits `HotMemoryOverBudget` Error if
//! over budget; the prior canary's `DeferredCheck` findings are gone.
//!
//! Also dispatches three sibling checks via
//! [`super::hot_memory_walker`]:
//! - `BrokenSourceLink` Error for unresolvable filesystem-backed sources
//! - `MissingSummary` Warning for folders that lack `_summary.md`
//! - `StaleProfileLine` Warning for `AutoUserProfile` evidence citations
//!   whose record is no longer active

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::assemble_hot::assembler::{AssembleHotError, assemble_hot_with_loader};
use crate::verbs::lint::{LintInputs, LintRecord, finding, target_path};

/// Issue tracker reference for over-budget findings.
const TRACKING_ISSUE: i64 = 83;

/// Run the rewritten hot-memory walker. See module docs.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();

    // 1. Real walker: only when a body loader is wired.
    if let Some(loader) = inputs.hot_body_loader {
        match assemble_hot_with_loader(&inputs.config.vault.hot_memory, loader) {
            Ok(_data) => {
                // Under budget — no finding.
            }
            Err(AssembleHotError::BudgetExceeded { got, max }) => {
                findings.push(over_budget_finding(got, max));
            }
            Err(e) => {
                findings.push(finding(
                    Kind::DeferredCheck,
                    Severity::Warning,
                    format!("hot_memory walker failed: {e}"),
                ));
            }
        }
    }

    // 2. broken_source_link
    findings.extend(super::hot_memory_walker::broken_source_links(inputs));

    // 3. missing_summary — folders come from config (or empty list when
    // the schema doesn't yet expose `summary_folders`).
    let folders: Vec<String> = summary_folders_from_config(inputs);
    findings.extend(super::hot_memory_walker::missing_summaries(
        inputs, &folders,
    ));

    // 4. stale_profile_line — only when an AutoUserProfile body is in records.
    if let Some(body) = profile_body_from_records(inputs.records) {
        let active_ids: Vec<String> = inputs
            .records
            .iter()
            .map(|r| r.stored.record.id.as_str().to_owned())
            .collect();
        findings.extend(super::hot_memory_walker::stale_profile_lines(
            &body,
            &active_ids,
        ));
    }

    findings
}

/// Build a `HotMemoryOverBudget` Error finding for an over-budget prefix.
fn over_budget_finding(got: u64, max: u64) -> Finding {
    let mut f = finding(
        Kind::HotMemoryOverBudget,
        Severity::Error,
        format!("hot prefix {got} bytes exceeds {max} bytes budget"),
    );
    f.target = Some(target_path(".cairn/config.yaml"));
    f.tracking_issue = Some(TRACKING_ISSUE);
    f
}

/// Return the folders the recipe expects rolling summaries for. Pulled
/// from `inputs.config.vault.hot_memory.summary_folders` if the
/// schema exposes it; otherwise an empty list.
fn summary_folders_from_config(inputs: &LintInputs<'_>) -> Vec<String> {
    // Probe the config for an optional `summary_folders` field. If the
    // field doesn't exist on the schema yet, this returns an empty
    // list and the missing_summary check is a no-op for now.
    // (TODO(#83-followup): extend `HotMemoryConfig` with `summary_folders`.)
    let _ = inputs;
    Vec::new()
}

/// Find the `AutoUserProfile` body in the active record set, if present.
/// Convention from issue #82: the synthesizer writes a record tagged
/// `auto_user_profile`. If the tag changes upstream, update this.
fn profile_body_from_records(records: &[LintRecord]) -> Option<String> {
    records.iter().find_map(|r| {
        if r.stored
            .record
            .tags
            .iter()
            .any(|t| t == "auto_user_profile")
        {
            Some(r.stored.record.body.clone())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::IndexStats;
    use crate::generated::verbs::assemble_hot::HotRecipeStep;
    use crate::verbs::lint::{empty_author_states, empty_unresolvable_authors};

    #[test]
    fn walker_with_loader_runs_assembler_and_no_findings_when_clean() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        // Make filesystem checks pass.
        std::fs::write(dir.path().join("purpose.md"), "x").unwrap();
        std::fs::write(dir.path().join("index.md"), "x").unwrap();

        let loader = |_step: HotRecipeStep| -> Result<String, String> { Ok(String::new()) };
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: Some(dir.path()),
            hot_body_loader: Some(&loader),
        };
        let findings = run(&inputs);
        assert!(
            findings.is_empty(),
            "clean recipe must emit no hot_memory findings; got {:?}",
            findings.iter().map(|f| f.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn walker_with_oversized_loader_emits_over_budget_error() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.max_bytes = 8;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("purpose.md"), "x").unwrap();
        std::fs::write(dir.path().join("index.md"), "x").unwrap();

        let loader = |_step: HotRecipeStep| -> Result<String, String> { Ok("AAAA".to_owned()) };
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: Some(dir.path()),
            hot_body_loader: Some(&loader),
        };
        let findings = run(&inputs);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::HotMemoryOverBudget)
                    && matches!(f.severity, Severity::Error)),
            "expected HotMemoryOverBudget Error; got {:?}",
            findings
                .iter()
                .map(|f| (f.kind, f.severity))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn walker_without_loader_emits_no_walker_findings() {
        // No loader → no assembler call → no over-budget finding (and
        // no DeferredCheck warnings either; the canary is gone).
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: &[],
            config: &cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: None,
            hot_body_loader: None,
        };
        let findings = run(&inputs);
        // No vault_root → broken_source_links / missing_summaries are
        // empty; no records → stale_profile_lines is empty; no loader
        // → walker is empty. Net: zero findings.
        assert!(
            findings.is_empty(),
            "expected no findings; got {:?}",
            findings.iter().map(|f| f.kind).collect::<Vec<_>>()
        );
    }
}
