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
    /// MCP tool names used by this subagent (e.g. `assemble_hot`).
    /// Cross-validated against MCP TOOLS in Pass B.
    pub uses_mcp_tools: Vec<String>,
}

/// Slash command kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
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
    /// Human-readable display name; must satisfy the path-safe token predicate.
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
#[non_exhaustive]
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
    #[error("harness mismatch: pack declares `{want}`, requested `{got}`")]
    HarnessMismatch {
        /// Declared harness wire string (e.g. `"claude-code"`).
        want: String,
        /// Requested harness wire string.
        got: String,
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

/// Canonical lifecycle hook events (must match
/// `cairn_cli::hooks::HookName::ALL`).
pub(crate) const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];

fn is_safe_path_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn is_safe_relative_path(s: &str) -> bool {
    if s.is_empty() || s.starts_with('/') {
        return false;
    }
    for comp in s.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return false;
        }
        // Reject non-ASCII, backslash, and control characters in path components.
        if comp.bytes().any(|b| !b.is_ascii() || b == b'\\' || b < 0x20) {
            return false;
        }
    }
    true
}

impl PackManifest {
    /// Pass A: structural invariants (no external lookups).
    ///
    /// Covers spec §4 invariants 1–6, 9, 11. Cross-reference checks
    /// (invariants 7, 8, 10) live in [`Self::validate_pass_b`].
    ///
    /// # Errors
    /// Returns [`PackError`] on the first failed invariant.
    pub fn validate_pass_a(&self) -> Result<(), PackError> {
        // 1. Schema.
        if self.schema != "cairn-pack/v1" {
            return Err(PackError::SchemaUnknown {
                got: self.schema.clone(),
            });
        }

        // 2. pack_id / name tokens.
        if !is_safe_path_token(&self.pack_id) {
            return Err(PackError::ManifestInvalid {
                reason: format!("pack_id `{}` is not a safe path token", self.pack_id),
            });
        }
        if !is_safe_path_token(&self.name) {
            return Err(PackError::ManifestInvalid {
                reason: format!("name `{}` is not a safe path token", self.name),
            });
        }

        // 3. Semver.
        if semver::Version::parse(&self.version).is_err() {
            return Err(PackError::ManifestInvalid {
                reason: format!("version `{}` is not valid semver", self.version),
            });
        }

        // 4. cairn_mcp_compat — must start with `>=` and the remainder parse
        //    as semver.
        let Some(rest) = self.cairn_mcp_compat.strip_prefix(">=") else {
            return Err(PackError::ManifestInvalid {
                reason: format!(
                    "cairn_mcp_compat `{}` must start with `>=` in v1",
                    self.cairn_mcp_compat
                ),
            });
        };
        if semver::Version::parse(rest.trim()).is_err() {
            return Err(PackError::ManifestInvalid {
                reason: format!(
                    "cairn_mcp_compat `{}` tail `{}` is not valid semver",
                    self.cairn_mcp_compat,
                    rest.trim()
                ),
            });
        }

        // 5. harness — enum already parsed by serde. Nothing extra to check.

        // 6, 9, 11 — delegated to focused helpers.
        if !is_safe_relative_path(&self.manual_fragment) {
            return Err(PackError::ManifestInvalid {
                reason: format!(
                    "manual_fragment path `{}` escapes pack root",
                    self.manual_fragment
                ),
            });
        }
        self.validate_subagents()?;
        self.validate_commands()?;
        self.validate_hooks()
    }

    /// Validates subagent path safety and uniqueness (invariants 6, 11).
    fn validate_subagents(&self) -> Result<(), PackError> {
        let mut seen = std::collections::BTreeSet::new();
        for s in &self.subagents {
            if !is_safe_relative_path(&s.path) {
                return Err(PackError::ManifestInvalid {
                    reason: format!("subagent `{}` path `{}` escapes pack root", s.id, s.path),
                });
            }
            if !is_safe_path_token(&s.id) {
                return Err(PackError::ManifestInvalid {
                    reason: format!("subagent id `{}` is not a safe path token", s.id),
                });
            }
            if !seen.insert(&s.id) {
                return Err(PackError::ManifestInvalid {
                    reason: format!("duplicate subagent id `{}`", s.id),
                });
            }
        }
        Ok(())
    }

    /// Validates command path safety, verb-kind consistency, and uniqueness
    /// (invariants 6, 11).
    fn validate_commands(&self) -> Result<(), PackError> {
        let mut seen = std::collections::BTreeSet::new();
        for c in &self.commands {
            if !is_safe_relative_path(&c.path) {
                return Err(PackError::ManifestInvalid {
                    reason: format!("command `{}` path `{}` escapes pack root", c.id, c.path),
                });
            }
            if !is_safe_path_token(&c.id) {
                return Err(PackError::ManifestInvalid {
                    reason: format!("command id `{}` is not a safe path token", c.id),
                });
            }
            // verb required iff verb-direct.
            match (c.kind, &c.verb) {
                (CommandKind::VerbDirect, None) => {
                    return Err(PackError::ManifestInvalid {
                        reason: format!(
                            "verb-direct command `{}` missing required `verb` field",
                            c.id
                        ),
                    });
                }
                (CommandKind::Workflow, Some(verb)) => {
                    return Err(PackError::ManifestInvalid {
                        reason: format!(
                            "workflow command `{}` must not declare a `verb` field (got `{verb}`)",
                            c.id
                        ),
                    });
                }
                _ => {}
            }
            if !seen.insert(&c.id) {
                return Err(PackError::ManifestInvalid {
                    reason: format!("duplicate command id `{}`", c.id),
                });
            }
        }
        Ok(())
    }

    /// Validates that all hook event names are in the canonical list
    /// (invariant 9).
    fn validate_hooks(&self) -> Result<(), PackError> {
        for hook_name in self.hooks.keys() {
            if !HOOK_EVENTS.contains(&hook_name.as_str()) {
                return Err(PackError::HookUnknown {
                    hook: hook_name.clone(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    fn minimal_valid() -> PackManifest {
        PackManifest {
            schema: "cairn-pack/v1".to_owned(),
            pack_id: "test-pack".to_owned(),
            name: "test-pack".to_owned(),
            version: "0.1.0".to_owned(),
            harness: Harness::ClaudeCode,
            cairn_mcp_compat: ">=1.0.0".to_owned(),
            description: "test".to_owned(),
            requires_capabilities: vec![],
            subagents: vec![],
            commands: vec![],
            hooks: BTreeMap::new(),
            manual_fragment: "manual.md".to_owned(),
        }
    }

    #[test]
    fn valid_minimal_manifest_passes_pass_a() {
        minimal_valid().validate_pass_a().expect("pass A");
    }

    #[test]
    fn rejects_unknown_schema() {
        let mut m = minimal_valid();
        m.schema = "cairn-pack/v9".to_owned();
        match m.validate_pass_a() {
            Err(PackError::SchemaUnknown { got }) => assert_eq!(got, "cairn-pack/v9"),
            other => panic!("expected SchemaUnknown, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_pack_id_path_token() {
        let mut m = minimal_valid();
        m.pack_id = "../evil".to_owned();
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_bad_name_path_token() {
        let mut m = minimal_valid();
        m.name = "with/slash".to_owned();
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_non_semver_version() {
        let mut m = minimal_valid();
        m.version = "latest".to_owned();
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_cairn_mcp_compat_without_ge() {
        let mut m = minimal_valid();
        m.cairn_mcp_compat = "1.0.0".to_owned();
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_unknown_hook_event() {
        let mut m = minimal_valid();
        m.hooks.insert(
            "BadHook".to_owned(),
            HookBinding {
                command: "cairn hook X".to_owned(),
            },
        );
        match m.validate_pass_a() {
            Err(PackError::HookUnknown { hook }) => assert_eq!(hook, "BadHook"),
            other => panic!("expected HookUnknown, got {other:?}"),
        }
    }

    #[test]
    fn rejects_path_traversal_in_subagent_path() {
        let mut m = minimal_valid();
        m.subagents.push(SubagentDecl {
            id: "x".to_owned(),
            path: "../escape.md".to_owned(),
            uses_mcp_tools: vec![],
        });
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_duplicate_subagent_id() {
        let mut m = minimal_valid();
        m.subagents.push(SubagentDecl {
            id: "a".to_owned(),
            path: "agents/a.md".to_owned(),
            uses_mcp_tools: vec![],
        });
        m.subagents.push(SubagentDecl {
            id: "a".to_owned(),
            path: "agents/a2.md".to_owned(),
            uses_mcp_tools: vec![],
        });
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_verb_direct_command_without_verb() {
        let mut m = minimal_valid();
        m.commands.push(CommandDecl {
            id: "c".to_owned(),
            path: "commands/c.md".to_owned(),
            kind: CommandKind::VerbDirect,
            verb: None,
        });
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_backslash_in_subagent_path() {
        let mut m = minimal_valid();
        m.subagents.push(SubagentDecl {
            id: "x".to_owned(),
            path: "agents\\evil.md".to_owned(),
            uses_mcp_tools: vec![],
        });
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn rejects_nul_byte_in_subagent_path() {
        let mut m = minimal_valid();
        m.subagents.push(SubagentDecl {
            id: "x".to_owned(),
            path: "agents/evil\0.md".to_owned(),
            uses_mcp_tools: vec![],
        });
        let err = m.validate_pass_a().unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }

    #[test]
    fn hook_events_matches_hook_name_all() {
        assert_eq!(
            HOOK_EVENTS,
            &crate::hooks::HookName::ALL[..],
            "HOOK_EVENTS must stay in sync with HookName::ALL"
        );
    }

    #[test]
    fn valid_manifest_with_populated_entries_passes_pass_a() {
        let mut m = minimal_valid();
        m.subagents.push(SubagentDecl {
            id: "context-loader".to_owned(),
            path: "agents/context-loader.md".to_owned(),
            uses_mcp_tools: vec!["assemble_hot".to_owned(), "retrieve".to_owned()],
        });
        m.commands.push(CommandDecl {
            id: "cairn-ingest".to_owned(),
            path: "commands/cairn-ingest.md".to_owned(),
            kind: CommandKind::VerbDirect,
            verb: Some("ingest".to_owned()),
        });
        m.commands.push(CommandDecl {
            id: "cairn-standup".to_owned(),
            path: "commands/cairn-standup.md".to_owned(),
            kind: CommandKind::Workflow,
            verb: None,
        });
        m.hooks.insert(
            "SessionStart".to_owned(),
            HookBinding { command: "cairn hook SessionStart".to_owned() },
        );
        m.validate_pass_a().expect("populated valid manifest passes Pass A");
    }
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

    #[test]
    fn bundled_manifest_passes_pass_a() {
        let bytes = crate::packs::embed::CAIRN_CLAUDE_CODE_PACK
            .get_file("pack.json")
            .expect("pack.json present")
            .contents();
        let manifest: PackManifest =
            serde_json::from_slice(bytes).expect("parses");
        manifest.validate_pass_a().expect("real manifest passes Pass A");
    }
}
