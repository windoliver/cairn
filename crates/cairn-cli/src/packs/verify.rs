//! Pack conformance suite, surfaced via `cairn plugins verify --pack <id>`.
//!
//! Tier 1 — manifest schema validity (Pass A + Pass B + path presence).
//! Tier 2 — install round-trip (install into tempdir, compare to embed).
//! Tier 3 — snapshot test (delegated to `tests/claude_code_pack_install.rs`).

use serde::Serialize;
use std::path::Path;
use tempfile::tempdir;

use crate::packs::install::{PackInstallOpts, install_pack};
use crate::packs::manifest::{Harness, PackError, PackManifest};
use crate::packs::source::{EmbeddedPackSource, FsPackSource, PackSource};

/// Tier of a conformance case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Manifest schema validity.
    One,
    /// Install round-trip.
    Two,
    /// Snapshot integrity (delegated; see Tier-3 invocation note).
    Three,
}

/// Outcome of a single case.
#[derive(Debug, Clone, Serialize)]
pub struct CaseOutcome {
    /// Stable static id (`snake_case`; safe to embed in JSON wire output).
    pub id: &'static str,
    /// Case label (human-readable).
    pub name: String,
    /// Tier.
    pub tier: Tier,
    /// `Ok` if passed; `Err` otherwise.
    pub status: Result<(), String>,
}

/// Run the pack-verify suite for the bundled `cairn-claude-code` pack.
///
/// Tier-3 is delegated to the `claude_code_pack_install` integration test
/// — running it here would require regenerating snapshots; the
/// conformance suite only checks that the pack's emitted bytes are
/// deterministic across two installs (the round-trip case).
#[must_use]
pub fn run_pack_conformance(pack_id: &str) -> Vec<CaseOutcome> {
    let mut out = Vec::new();
    if pack_id != "cairn-claude-code" {
        out.push(CaseOutcome {
            id: "pack_unknown",
            name: format!("pack `{pack_id}` is bundled"),
            tier: Tier::One,
            status: Err(format!("unknown bundled pack `{pack_id}`")),
        });
        return out;
    }

    let dir = match crate::packs::bundled_pack_for(Harness::ClaudeCode) {
        Some(dir) => dir,
        None => {
            out.push(CaseOutcome {
                id: "pack_bundled",
                name: "pack has bundled source".to_owned(),
                tier: Tier::One,
                status: Err(PackError::ManifestInvalid {
                    reason: "no bundled pack available for harness `ClaudeCode`".to_owned(),
                }
                .to_string()),
            });
            return out;
        }
    };
    let source = EmbeddedPackSource::new("cairn-claude-code", dir);
    out.extend(run_pack_source_conformance(&source));

    // Tier 2: install round-trip.
    let case = || -> Result<(), PackError> {
        let tmp = tempdir().map_err(PackError::Io)?;
        let opts = PackInstallOpts {
            harness: Harness::ClaudeCode,
            project_dir: tmp.path().to_path_buf(),
            force: false,
        };
        let first = install_pack(&opts)?;
        let second = install_pack(&opts)?;
        if !second.files_created.is_empty() || !second.files_merged.is_empty() {
            return Err(PackError::ManifestInvalid {
                reason: format!(
                    "round-trip not idempotent: created={} merged={}",
                    second.files_created.len(),
                    second.files_merged.len()
                ),
            });
        }
        if first.files_created.is_empty() {
            return Err(PackError::ManifestInvalid {
                reason: "first install created no files".to_owned(),
            });
        }
        Ok(())
    };
    out.push(CaseOutcome {
        id: "pack_install_round_trip",
        name: "install round-trip is idempotent".to_owned(),
        tier: Tier::Two,
        status: case().map_err(|e| format!("{e:#}")),
    });

    out
}

/// Run the pack-verify suite for an author-provided pack directory.
#[must_use]
pub fn run_pack_path_conformance(path: &Path) -> Vec<CaseOutcome> {
    let source = FsPackSource::new(path.to_path_buf());
    run_pack_source_conformance(&source)
}

/// Run source-backed pack conformance checks shared by bundled and external packs.
#[must_use]
pub fn run_pack_source_conformance(source: &dyn PackSource) -> Vec<CaseOutcome> {
    let mut out = Vec::new();
    let manifest_bytes = match source.read_file("pack.json") {
        Ok(bytes) => bytes,
        Err(e) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Err(format!("{e:#}")),
            });
            return out;
        }
    };

    let manifest = match serde_json::from_slice::<PackManifest>(&manifest_bytes) {
        Ok(m) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Ok(()),
            });
            m
        }
        Err(e) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Err(format!("{e:#}")),
            });
            return out;
        }
    };

    out.push(CaseOutcome {
        id: "pack_pass_a",
        name: "Pass A structural validation".to_owned(),
        tier: Tier::One,
        status: manifest.validate_pass_a().map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        id: "pack_pass_b",
        name: "Pass B cross-reference validation".to_owned(),
        tier: Tier::One,
        status: manifest.validate_pass_b().map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        id: "pack_paths_present",
        name: "all referenced paths present".to_owned(),
        tier: Tier::One,
        status: manifest
            .assert_all_paths_present(source)
            .map_err(|e| format!("{e:#}")),
    });
    out.extend(run_harness_static_checks(&manifest, source));
    out
}

/// Run harness-specific static checks against source-backed pack content.
#[must_use]
pub fn run_harness_static_checks(
    manifest: &PackManifest,
    source: &dyn PackSource,
) -> Vec<CaseOutcome> {
    match manifest.harness {
        Harness::ClaudeCode => vec![CaseOutcome {
            id: "pack_subagent_frontmatter",
            name: "subagent frontmatter tools match manifest".to_owned(),
            tier: Tier::One,
            status: manifest
                .assert_subagent_frontmatter_matches_manifest(source)
                .map_err(|e| format!("{e:#}")),
        }],
        Harness::Codex => vec![CaseOutcome {
            id: "pack_codex_static_files",
            name: "Codex manual and hooks files are valid".to_owned(),
            tier: Tier::One,
            status: assert_manual_and_hook_json(manifest, source, "AGENTS.md")
                .map_err(|e| format!("{e:#}")),
        }],
        Harness::Gemini => vec![CaseOutcome {
            id: "pack_gemini_static_files",
            name: "Gemini manual and hooks files are valid".to_owned(),
            tier: Tier::One,
            status: assert_manual_and_hook_json(manifest, source, "GEMINI.md")
                .map_err(|e| format!("{e:#}")),
        }],
    }
}

fn assert_manual_and_hook_json(
    manifest: &PackManifest,
    source: &dyn PackSource,
    expected_manual: &str,
) -> Result<(), PackError> {
    if manifest.manual_fragment != expected_manual {
        return Err(PackError::ManifestInvalid {
            reason: format!(
                "manual_fragment `{}` must be `{expected_manual}` for {:?}",
                manifest.manual_fragment, manifest.harness
            ),
        });
    }

    let manual_bytes = source.read_file(expected_manual)?;
    let manual = std::str::from_utf8(&manual_bytes).map_err(|e| PackError::ManifestInvalid {
        reason: format!("{expected_manual} is not UTF-8: {e}"),
    })?;
    let begin = format!("<!-- BEGIN CAIRN PACK {} -->", manifest.pack_id);
    let end = format!("<!-- END CAIRN PACK {} -->", manifest.pack_id);
    if !manual.contains(&begin) || !manual.contains(&end) {
        return Err(PackError::ManifestInvalid {
            reason: format!("{expected_manual} must contain guarded block `{begin}` ... `{end}`"),
        });
    }

    let hooks_bytes = source.read_file("hooks/hooks.json")?;
    serde_json::from_slice::<serde_json::Value>(&hooks_bytes)?;
    Ok(())
}

/// Pack ids the verify suite knows how to run.
#[must_use]
pub fn bundled_pack_ids() -> Vec<&'static str> {
    vec!["cairn-claude-code"]
}

/// Render outcomes as a quick human summary.
#[must_use]
pub fn render_outcomes(outcomes: &[CaseOutcome]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for o in outcomes {
        let status = match &o.status {
            Ok(()) => "OK".to_owned(),
            Err(reason) => format!("FAIL — {reason}"),
        };
        let _ = writeln!(s, "{:?} {:30} {}", o.tier, o.name, status);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_pack_passes_full_conformance() {
        let outcomes = run_pack_conformance("cairn-claude-code");
        for o in &outcomes {
            assert!(
                o.status.is_ok(),
                "case `{}` (tier {:?}) failed: {:?}",
                o.name,
                o.tier,
                o.status
            );
        }
    }

    #[test]
    fn unknown_pack_returns_single_fail() {
        let outcomes = run_pack_conformance("does-not-exist");
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].status.is_err());
    }
}
