//! `cairn skill install` — writes the Cairn skill bundle to the harness skill
//! directory (§8.0.a-bis, §18.d).

use anyhow::Result;
use clap::ValueEnum;
use std::path::PathBuf;

/// Supported harnesses for `cairn skill install`.
#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    /// Claude Code harness — registers the skill via CLAUDE.md.
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Codex harness — registers the skill via AGENTS.md.
    Codex,
    /// Gemini CLI harness — registers the skill via GEMINI.md.
    Gemini,
    /// `OpenCode` harness — registers the skill via the opencode config skills path.
    Opencode,
    /// Cursor harness — registers the skill via .cursorrules.
    Cursor,
    /// Custom or unknown harness — prints a generic registration hint.
    Custom,
}

/// Returns the harness-specific registration hint printed after install.
#[must_use]
pub fn registration_hint(harness: &Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Codex => "# Add to your AGENTS.md:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Gemini => "# Add to your GEMINI.md:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Opencode => {
            "# Add to your opencode config skills path:\n~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Cursor => "# Add to your .cursorrules:\n@~/.cairn/skills/cairn/SKILL.md",
        Harness::Custom => "# Skill bundle written. Register it with your harness manually.",
    }
}

// Embedded at compile time from the committed generated artifacts.
// The CI `--check` gate catches drift between these and what cairn-codegen emits.
#[allow(dead_code)] // used in install() (Task 6)
const SKILL_MD: &str = include_str!("../../../skills/cairn/SKILL.md");
#[allow(dead_code)] // used in install() (Task 6)
const CONVENTIONS_MD: &str = include_str!("../../../skills/cairn/conventions.md");
#[allow(dead_code)] // used in install() (Task 6)
const VERSION_FILE: &str = include_str!("../../../skills/cairn/.version");

// Static example stubs — written once on install; user may edit after.
#[allow(dead_code)] // used in install() (Task 6)
const EXAMPLE_01: &str = include_str!("../../../skills/cairn/examples/01-remember-preference.md");
#[allow(dead_code)] // used in install() (Task 6)
const EXAMPLE_02: &str = include_str!("../../../skills/cairn/examples/02-forget-something.md");
#[allow(dead_code)] // used in install() (Task 6)
const EXAMPLE_03: &str = include_str!("../../../skills/cairn/examples/03-search-prior-decision.md");
#[allow(dead_code)] // used in install() (Task 6)
const EXAMPLE_04: &str = include_str!("../../../skills/cairn/examples/04-skillify-this.md");

/// Options for [`install`].
#[derive(Debug, Clone)]
pub struct InstallOpts {
    /// Target directory. Default: `~/.cairn/skills/cairn/`.
    pub target_dir: PathBuf,
    /// Which harness to generate the registration hint for.
    pub harness: Harness,
    /// If `true`, overwrite generated files even if the version matches.
    pub force: bool,
}

/// Result of a skill install run.
#[derive(Debug, serde::Serialize)]
pub struct InstallReceipt {
    /// The directory where the skill was installed.
    pub target_dir: PathBuf,
    /// Version string from the contract (cairn-idl crate version).
    pub contract_version: String,
    /// Version string from the IDL / SKILL.md.
    pub idl_version: String,
    /// Paths to files created during the install.
    pub files_created: Vec<PathBuf>,
    /// Paths to files skipped (already present, version match).
    pub files_skipped: Vec<PathBuf>,
    /// Harness-specific registration hint for the user.
    pub registration_hint: String,
}

/// Resolves the default install directory (`~/.cairn/skills/cairn/`).
///
/// # Errors
/// Returns an error if `HOME` is not set in the environment.
pub fn default_target_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home).join(".cairn/skills/cairn"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_hint_covers_all_harnesses() {
        let cases = [
            (Harness::ClaudeCode, "CLAUDE.md"),
            (Harness::Codex, "AGENTS.md"),
            (Harness::Gemini, "GEMINI.md"),
            (Harness::Opencode, "opencode"),
            (Harness::Cursor, ".cursorrules"),
            (Harness::Custom, "manually"),
        ];
        for (harness, expected_fragment) in &cases {
            let hint = registration_hint(harness);
            assert!(
                hint.contains(expected_fragment),
                "hint for {harness:?} should mention '{expected_fragment}' — got: {hint:?}"
            );
        }
    }
}
