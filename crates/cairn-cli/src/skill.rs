//! `cairn skill install` — writes the Cairn skill bundle to the harness skill
//! directory (§8.0.a-bis, §18.d).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::ValueEnum;

/// Supported harnesses for `cairn skill install`.
#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
    Gemini,
    Opencode,
    Cursor,
    Custom,
}

/// Returns the harness-specific registration hint printed after install.
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
