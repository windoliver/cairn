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

        // Codex review round 1 finding 3: the sync loader (cairn-cli's
        // `lint_step_body_sync`) returns empty bodies for store-backed
        // recipe steps (`pinned_feedback`, `top_salience_project`,
        // `active_playbook`, `recent_user_signal`). Oversized store-
        // backed content cannot trigger `HotMemoryOverBudget` from the
        // lint path. Emit a DeferredCheck so the coverage gap is loud.
        findings.push(deferred_store_backed_walker_finding());
    }

    // 2. broken_source_link
    findings.extend(super::hot_memory_walker::broken_source_links(inputs));

    // 3. missing_summary — folders come from config (currently dormant:
    // `HotMemoryConfig.summary_folders` is not yet in the schema).
    // Codex review round 1 finding 4: emit a DeferredCheck so the
    // missing wiring is visible from `cairn lint --json`, not silent.
    let folders: Vec<String> = summary_folders_from_config(inputs);
    if folders.is_empty() {
        findings.push(deferred_missing_summary_finding());
    } else {
        findings.extend(super::hot_memory_walker::missing_summaries(
            inputs, &folders,
        ));
    }

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

/// Build a `DeferredCheck` Info finding documenting that the lint
/// walker's body loader cannot read store-backed recipe steps
/// synchronously. Without this, an oversized `Project` / `UserSignal` /
/// `Playbook` / pinned record body could push the prefix over budget at
/// runtime while `cairn lint --json` reports a clean walker.
fn deferred_store_backed_walker_finding() -> Finding {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        "hot_memory walker measures filesystem-backed recipe steps only; \
         store-backed steps (pinned_feedback, top_salience_project, \
         active_playbook, recent_user_signal) are not weighed and may \
         push the runtime prefix over budget undetected by `cairn lint`"
            .to_owned(),
    );
    f.tracking_issue = Some(TRACKING_ISSUE);
    f
}

/// Build a `DeferredCheck` Info finding when the summary-folder list
/// is empty (the schema hasn't surfaced `hot_memory.summary_folders`
/// yet). Documents that the `MissingSummary` check is dormant from the
/// integrated walker.
fn deferred_missing_summary_finding() -> Finding {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        "missing_summary check is dormant: hot_memory.summary_folders \
         is not yet in CairnConfig. Direct callers of \
         `hot_memory_walker::missing_summaries` (e.g. CI scripts) can \
         still surface this finding by passing a folder list."
            .to_owned(),
    );
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
        // Post codex round-1 fix: with a loader wired the walker emits
        // two DeferredCheck Info advisories — one for store-backed
        // recipe steps the sync loader can't read, one for the dormant
        // missing_summary check. Neither indicates a problem; assert
        // that nothing more severe fires.
        let nondeferred: Vec<Kind> = findings
            .iter()
            .filter(|f| !matches!(f.kind, Kind::DeferredCheck))
            .map(|f| f.kind)
            .collect();
        assert!(
            nondeferred.is_empty(),
            "clean recipe must emit no non-deferred hot_memory findings; got {nondeferred:?}"
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
    fn walker_without_loader_emits_only_missing_summary_advisory() {
        // No loader → assembler not invoked → no over-budget finding
        // and no store-backed-deferred advisory. No vault_root →
        // broken_source_links is empty. No records → stale_profile_lines
        // is empty. But missing_summary always runs (and currently always
        // dormant because HotMemoryConfig.summary_folders isn't in the
        // schema), so a single DeferredCheck Info finding is expected.
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
        let kinds: Vec<Kind> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(findings.len(), 1, "expected exactly 1 finding; got {kinds:?}");
        assert!(matches!(findings[0].kind, Kind::DeferredCheck));
        assert!(matches!(findings[0].severity, Severity::Info));
        assert!(
            findings[0].message.contains("missing_summary"),
            "expected missing_summary advisory; got {:?}",
            findings[0].message
        );
    }
}
