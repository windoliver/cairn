//! Filesystem + record-graph helpers for the rewritten `hot_memory`
//! lint walker (#83). Each helper emits one of the three new lint
//! kinds: `BrokenSourceLink`, `MissingSummary`, `StaleProfileLine`.
//!
//! These helpers are pure: filesystem reads happen inside, but the
//! result is a `Vec<Finding>` and the helpers do not mutate any
//! state. The dispatch layer (cairn-cli) supplies `vault_root` and
//! `active_record_ids`; the lint walker (Task 24) wires them in.

use std::path::{Path, PathBuf};

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, finding, target_path};

// ───── §21 broken_source_link ─────

/// Emit one `BrokenSourceLink` Error per filesystem-backed hot-memory
/// source that does not resolve from `vault_root`. Returns an empty
/// `Vec` when `vault_root` is `None` (fixture-only tests of unrelated
/// checks remain green).
#[must_use]
pub fn broken_source_links(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(root) = inputs.vault_root else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in mandatory_filesystem_paths(root) {
        if !path.exists() {
            let mut f = finding(
                Kind::BrokenSourceLink,
                Severity::Error,
                format!("hot-memory source missing: {}", path.display()),
            );
            f.target = Some(target_path(path.to_string_lossy().into_owned()));
            out.push(f);
        }
    }
    out
}

/// Returns the set of vault paths that must exist for a valid hot-memory
/// assembly: `purpose.md` (the mission/purpose document) and `index.md`
/// (the vault entry index).
fn mandatory_filesystem_paths(root: &Path) -> Vec<PathBuf> {
    vec![root.join("purpose.md"), root.join("index.md")]
}

// ───── §22 missing_summary ─────

/// Emit one `MissingSummary` Warning per folder reachable from
/// `vault_root` that the recipe references but lacks `_summary.md`.
/// Returns empty when `vault_root` is `None`.
///
/// Folders that do not exist on disk are silently skipped — the
/// `BrokenSourceLink` check owns the "path missing" finding; this
/// check focuses on folders that exist but are missing their rolling
/// summary document.
#[must_use]
pub fn missing_summaries(inputs: &LintInputs<'_>, folders: &[String]) -> Vec<Finding> {
    let Some(root) = inputs.vault_root else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for folder in folders {
        let folder_path = root.join(folder);
        if !folder_path.is_dir() {
            continue;
        }
        let summary = folder_path.join("_summary.md");
        if !summary.exists() {
            let mut f = finding(
                Kind::MissingSummary,
                Severity::Warning,
                format!("rolling _summary.md missing in folder {folder}"),
            );
            f.target = Some(target_path(folder.clone()));
            out.push(f);
        }
    }
    out
}

// ───── §23 stale_profile_line ─────

/// Pure: scan `profile_body` for ULID-bracketed evidence citations
/// (`[01...]`) and emit one `StaleProfileLine` Warning per citation
/// whose id is absent from `active_record_ids`. The synthesizer
/// controls the citation format (#82's `UserProfileSynthesizer`).
///
/// A ULID-like token is exactly 26 ASCII alphanumeric characters
/// enclosed in square brackets. Non-ULID bracket content (e.g.,
/// Markdown links) is silently skipped.
#[must_use]
pub fn stale_profile_lines(profile_body: &str, active_record_ids: &[String]) -> Vec<Finding> {
    let active: std::collections::HashSet<&str> =
        active_record_ids.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let mut cursor = profile_body;
    while let Some(open) = cursor.find('[') {
        let after_open = &cursor[open + 1..];
        let Some(close) = after_open.find(']') else {
            break;
        };
        let candidate = &after_open[..close];
        if is_ulid_like(candidate) && !active.contains(candidate) {
            let f = finding(
                Kind::StaleProfileLine,
                Severity::Warning,
                format!("AutoUserProfile cites forgotten or expired evidence record {candidate}"),
            );
            out.push(f);
        }
        cursor = &after_open[close + 1..];
    }
    out
}

/// Returns `true` when `s` is exactly 26 ASCII alphanumeric characters,
/// consistent with the ULID encoding alphabet used by the synthesizer.
fn is_ulid_like(s: &str) -> bool {
    s.len() == 26 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

// ───── tests ─────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::IndexStats;
    use crate::verbs::lint::{LintInputs, empty_author_states, empty_unresolvable_authors};

    fn fixed_inputs<'a>(cfg: &'a CairnConfig, vault: &'a Path) -> LintInputs<'a> {
        LintInputs {
            records: &[],
            config: cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: Some(vault),
            source_forgets: None,
            hot_body_loader: None,
        }
    }

    fn no_vault_inputs(cfg: &CairnConfig) -> LintInputs<'_> {
        LintInputs {
            records: &[],
            config: cfg,
            index_stats: IndexStats::new(0, 0),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: None,
            source_forgets: None,
            hot_body_loader: None,
        }
    }

    // §21 broken_source_link

    #[test]
    fn missing_purpose_md_emits_broken_source_link() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        // index.md present but purpose.md missing
        std::fs::write(dir.path().join("index.md"), "x").expect("write index.md");
        let findings = broken_source_links(&fixed_inputs(&cfg, dir.path()));
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::BrokenSourceLink)
                    && f.message.contains("purpose.md")),
            "expected BrokenSourceLink for purpose.md; got {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn both_present_emits_no_findings() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("purpose.md"), "x").expect("write purpose.md");
        std::fs::write(dir.path().join("index.md"), "x").expect("write index.md");
        assert!(broken_source_links(&fixed_inputs(&cfg, dir.path())).is_empty());
    }

    #[test]
    fn no_vault_root_returns_empty() {
        let cfg = CairnConfig::default();
        assert!(broken_source_links(&no_vault_inputs(&cfg)).is_empty());
    }

    // §22 missing_summary

    #[test]
    fn folder_without_summary_emits_missing_summary() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("project_a")).expect("create dir");
        // No _summary.md inside project_a.
        let findings = missing_summaries(&fixed_inputs(&cfg, dir.path()), &["project_a".into()]);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::MissingSummary) && f.message.contains("project_a")),
            "expected MissingSummary; got {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn folder_with_summary_emits_no_finding() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("project_a")).expect("create dir");
        std::fs::write(dir.path().join("project_a/_summary.md"), "x").expect("write _summary.md");
        assert!(
            missing_summaries(&fixed_inputs(&cfg, dir.path()), &["project_a".into()]).is_empty()
        );
    }

    // §23 stale_profile_line

    #[test]
    fn forgotten_evidence_id_in_profile_body_emits_stale_profile_line() {
        let body = "synthesised facts. evidence: [01H7K0K0K0K0K0K0K0K0K0K0K0]";
        let active_ids: Vec<String> = vec![]; // cited id is NOT active
        let findings = stale_profile_lines(body, &active_ids);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::StaleProfileLine)
                    && f.message.contains("01H7K0K0K0K0K0K0K0K0K0K0K0")),
            "expected StaleProfileLine; got {:?}",
            findings.iter().map(|f| &f.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn active_evidence_id_emits_no_finding() {
        let id = "01H7K0K0K0K0K0K0K0K0K0K0K0";
        let body = format!("evidence: [{id}]");
        let findings = stale_profile_lines(&body, &[id.to_owned()]);
        assert!(findings.is_empty());
    }
}
