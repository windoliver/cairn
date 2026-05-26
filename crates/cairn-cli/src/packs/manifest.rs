//! cairn-pack/v1 manifest types and validation.
//!
//! See `docs/superpowers/specs/2026-05-26-issue-182-cairn-claude-code-pack-design.md`
//! §4 for the schema and invariant list.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Supported harness identifiers in v1. Extensible: unknown values
/// reject at install with `PackError::HarnessMismatch`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    /// Claude Code (the canonical reference harness).
    ClaudeCode,
}

/// Subagent declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentDecl {
    /// Subagent id (matches `<id>` in `.claude/agents/<id>.md`).
    pub id: String,
    /// Pack-relative path to the subagent markdown file.
    pub path: String,
    /// Bare verb names used by this subagent (e.g. `assemble_hot`).
    /// Cross-validated against MCP TOOLS in Pass B.
    pub uses_mcp_tools: Vec<String>,
}

/// Slash command kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandKind {
    /// Direct shell-out to `cairn <verb>`.
    VerbDirect,
    /// Composes one or more subagents + verbs.
    Workflow,
}

/// Slash command declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDecl {
    /// Command id (file slug under `.claude/commands/<id>.md`).
    pub id: String,
    /// Pack-relative path to the command markdown file.
    pub path: String,
    /// Command kind.
    pub kind: CommandKind,
    /// Bare verb name (present iff `kind == VerbDirect`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
}

/// Hook event binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookBinding {
    /// Command string to invoke on this hook event.
    pub command: String,
}

/// cairn-pack/v1 manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    /// Schema identifier — MUST be `"cairn-pack/v1"`.
    pub schema: String,
    /// Pack id (path-safe token).
    pub pack_id: String,
    /// Display name (path-safe token).
    pub name: String,
    /// Pack semver.
    pub version: String,
    /// Target harness.
    pub harness: Harness,
    /// Required cairn MCP contract range, e.g. `">=1.0.0"`.
    pub cairn_mcp_compat: String,
    /// Pack description (human-readable).
    pub description: String,
    /// Required capability strings.
    pub requires_capabilities: Vec<String>,
    /// Subagents.
    pub subagents: Vec<SubagentDecl>,
    /// Slash commands.
    pub commands: Vec<CommandDecl>,
    /// Hook event → command bindings.
    pub hooks: BTreeMap<String, HookBinding>,
    /// Pack-relative path to the CLAUDE.md fragment.
    pub manual_fragment: String,
}

/// Pack-runtime error.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// Manifest validation failed.
    #[error("manifest invalid: {reason}")]
    ManifestInvalid {
        /// Failure reason.
        reason: String,
    },
    /// Unknown schema identifier.
    #[error("unknown schema: {got}")]
    SchemaUnknown {
        /// Observed schema string.
        got: String,
    },
    /// Harness in manifest differs from requested harness.
    #[error("harness mismatch: pack declares {want:?}, requested {got:?}")]
    HarnessMismatch {
        /// Declared harness.
        want: Harness,
        /// Requested harness.
        got: Harness,
    },
    /// Capability referenced by `requires_capabilities` not in advertise table.
    #[error("unknown capability `{cap}`")]
    CapabilityUnknown {
        /// Capability string.
        cap: String,
    },
    /// MCP tool name not in `cairn_mcp::generated::TOOLS`.
    #[error("unknown MCP tool `{tool}`")]
    McpToolUnknown {
        /// Tool name.
        tool: String,
    },
    /// Hook event name not in canonical lifecycle list.
    #[error("unknown hook event `{hook}`")]
    HookUnknown {
        /// Event name.
        hook: String,
    },
    /// Filesystem merge conflict.
    #[error("merge conflict in {file}: {reason}")]
    MergeConflict {
        /// File path.
        file: String,
        /// Failure reason.
        reason: String,
    },
    /// I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON parse error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod load_tests {
    use super::*;

    #[test]
    fn parses_bundled_pack_manifest() {
        let bytes = crate::packs::embed::CAIRN_CLAUDE_CODE_PACK
            .get_file("pack.json")
            .expect("embedded pack.json present")
            .contents();
        let manifest: PackManifest =
            serde_json::from_slice(bytes).expect("pack.json parses as PackManifest");
        assert_eq!(manifest.schema, "cairn-pack/v1");
        assert_eq!(manifest.pack_id, "cairn-claude-code");
        assert_eq!(manifest.harness, Harness::ClaudeCode);
    }
}
