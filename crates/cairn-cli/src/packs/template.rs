//! Fixed-token scaffold rendering for `cairn skill new`.

use std::path::{Path, PathBuf};

use crate::packs::manifest::{Harness, PackError};
use include_dir::{Dir, include_dir};

/// Embedded `cairn skill new` reference templates.
pub static PACK_TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../packs/templates");

/// Values available to fixed-token scaffold templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateVars {
    /// Safe path token used as the generated pack id.
    pub pack_id: String,
    /// Human-readable pack name derived from `pack_id`.
    pub display_name: String,
    /// Harness id written to `pack.json`.
    pub harness: String,
    /// Initial pack semver.
    pub version: String,
    /// Harness-specific manual fragment filename.
    pub manual_fragment: String,
    /// Slash command id.
    pub command_id: String,
    /// Subagent id.
    pub subagent_id: String,
}

/// Options for rendering a skill-pack scaffold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldOpts {
    /// Requested pack name.
    pub name: String,
    /// Target harness.
    pub harness: Harness,
    /// Directory where the scaffold should be written.
    pub output_dir: PathBuf,
}

/// Receipt emitted after a skill-pack scaffold is written.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScaffoldReceipt {
    /// Generated pack id.
    pub pack_id: String,
    /// Target harness id.
    pub harness: String,
    /// Directory where files were written.
    pub output_dir: PathBuf,
    /// Pack-relative files created by the renderer.
    pub files_created: Vec<PathBuf>,
    /// Suggested verification command.
    pub verify_command: String,
}

/// Errors from skill-pack scaffold rendering.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScaffoldError {
    /// Pack name is not a safe scaffold token.
    #[error("invalid pack name `{name}`: use nonempty ASCII alphanumeric characters, '-' or '_'")]
    InvalidPackName {
        /// Invalid requested pack name.
        name: String,
    },
    /// Output directory already has content.
    #[error("output directory is not empty: {}", path.display())]
    OutputDirNotEmpty {
        /// Non-empty output directory.
        path: PathBuf,
    },
    /// No template tree exists for the requested harness.
    #[error("template missing for harness `{harness}` at `{path}`")]
    TemplateMissing {
        /// Harness id.
        harness: String,
        /// Missing template path.
        path: String,
    },
    /// Template rendering left a fixed token unresolved.
    #[error("unresolved template token in `{token}`")]
    UnresolvedToken {
        /// Remaining unresolved token text.
        token: String,
    },
    /// Pack validation or install error.
    #[error(transparent)]
    Pack(#[from] PackError),
    /// Filesystem error while writing the scaffold.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Replace fixed scaffold tokens with values from `vars`.
///
/// # Errors
///
/// Returns [`ScaffoldError::UnresolvedToken`] if template delimiters remain
/// after known token replacement.
pub fn render_tokens(input: &str, vars: &TemplateVars) -> Result<String, ScaffoldError> {
    let mut rendered = input.to_string();
    for (token, value) in [
        ("{{pack_id}}", vars.pack_id.as_str()),
        ("{{display_name}}", vars.display_name.as_str()),
        ("{{harness}}", vars.harness.as_str()),
        ("{{version}}", vars.version.as_str()),
        ("{{manual_fragment}}", vars.manual_fragment.as_str()),
        ("{{command_id}}", vars.command_id.as_str()),
        ("{{subagent_id}}", vars.subagent_id.as_str()),
    ] {
        rendered = rendered.replace(token, value);
    }

    if rendered.contains("{{") || rendered.contains("}}") {
        return Err(ScaffoldError::UnresolvedToken {
            token: first_unresolved_token(&rendered),
        });
    }

    Ok(rendered)
}

/// Convert a safe pack id into a title-cased display name.
#[must_use]
pub fn display_name(pack_id: &str) -> String {
    pack_id
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(title_case_ascii)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Return the template harness id.
#[must_use]
pub fn harness_id(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::Gemini => "gemini",
    }
}

/// Return the harness-specific manual fragment path.
#[must_use]
pub fn manual_fragment(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "manual.md",
        Harness::Codex => "AGENTS.md",
        Harness::Gemini => "GEMINI.md",
    }
}

/// Return true when `name` can safely be used as a pack directory token.
#[must_use]
pub fn is_safe_pack_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Return true when `path` has no directory entries.
///
/// # Errors
///
/// Returns an I/O error if the directory cannot be read.
pub fn is_dir_empty(path: &Path) -> Result<bool, std::io::Error> {
    Ok(std::fs::read_dir(path)?.next().is_none())
}

fn title_case_ascii(part: &str) -> String {
    let lower = part.to_ascii_lowercase();
    let mut chars = lower.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut title = String::new();
    title.extend(first.to_uppercase());
    title.push_str(chars.as_str());
    title
}

fn first_unresolved_token(rendered: &str) -> String {
    if let Some(start) = rendered.find("{{") {
        if let Some(end) = rendered[start + 2..].find("}}") {
            return rendered[start..start + 2 + end + 2].to_string();
        }
    }
    if rendered.contains("{{") {
        return "{{".to_string();
    }
    "}}".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vars() -> TemplateVars {
        TemplateVars {
            pack_id: "sample-pack".to_string(),
            display_name: "Sample Pack".to_string(),
            harness: "codex".to_string(),
            version: "0.1.0".to_string(),
            manual_fragment: "AGENTS.md".to_string(),
            command_id: "cairn-context".to_string(),
            subagent_id: "context-loader".to_string(),
        }
    }

    #[test]
    fn render_tokens_replaces_known_values_and_rejects_unresolved_tokens() {
        let vars = sample_vars();

        assert_eq!(
            render_tokens("{{pack_id}} {{display_name}}", &vars).unwrap(),
            "sample-pack Sample Pack"
        );

        let err = render_tokens("{{missing_token}}", &vars).unwrap_err();
        assert!(
            err.to_string().contains("unresolved template token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn display_name_title_cases_safe_pack_id() {
        assert_eq!(display_name("my-pack"), "My Pack");
        assert_eq!(display_name("ops_pack"), "Ops Pack");
    }
}
