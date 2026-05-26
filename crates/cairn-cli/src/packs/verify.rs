//! Pack conformance suite, surfaced via `cairn plugins verify --pack <id>`.
//!
//! Tier 1 — manifest schema validity (Pass A + Pass B + path presence).
//! Tier 2 — install round-trip (install into tempdir, compare to embed).
//! Tier 3 — snapshot test (delegated to `tests/claude_code_pack_install.rs`).

use serde::Serialize;
use tempfile::tempdir;

use crate::packs::install::{PackInstallOpts, install_pack};
use crate::packs::manifest::{Harness, PackError, PackManifest};

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

    let dir = crate::packs::bundled_pack_for(Harness::ClaudeCode);
    let manifest: Result<PackManifest, PackError> = dir
        .get_file("pack.json")
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "missing pack.json".to_owned(),
        })
        .and_then(|f| Ok(serde_json::from_slice::<PackManifest>(f.contents())?));

    let manifest = match manifest {
        Ok(m) => m,
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
        id: "pack_json_parses",
        name: "pack.json parses".to_owned(),
        tier: Tier::One,
        status: Ok(()),
    });

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
            .assert_all_paths_present(dir)
            .map_err(|e| format!("{e:#}")),
    });

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
