# cairn-claude-code reference skill-pack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `packs/cairn-claude-code/` (manifest, 6 subagents, 13 slash commands, 5-event hook bindings, manual fragment, dogfood fixture) plus a harness-agnostic pack runtime in `crates/cairn-cli/src/packs/` that embeds, validates, and installs cairn-pack/v1 manifests. Migrate inline Claude-Code content out of `crates/cairn-cli/src/skill.rs` per CLAUDE.md §4 invariant 1.

**Architecture:** Pack content is plain markdown/JSON under `packs/<harness>/`. `cairn-cli` embeds the bundled `cairn-claude-code` pack at build time via `include_dir!` and ships a generic `PackManifest`/`PackInstaller`. Install writes pack files into `.claude/agents/`, `.claude/commands/`, merges `.claude/settings.json` and `.mcp.json`, and injects a block-guarded section into `CLAUDE.md`. `cairn plugins verify` gains a pack tier covering manifest validity, install round-trip, and per-file insta snapshots.

**Tech Stack:** Rust 1.95, edition 2024; `serde`/`serde_json` (workspace), `include_dir` 0.7 (workspace), `thiserror`, `semver` (new cairn-cli dep), `insta` 1.40 (workspace), `proptest` (workspace), `tempfile` (workspace), Claude Code subagent + slash command markdown frontmatter.

**Spec:** `docs/superpowers/specs/2026-05-26-issue-182-cairn-claude-code-pack-design.md`

---

## File Structure

### Pack content (new — `packs/cairn-claude-code/`)
| Path | Responsibility |
|---|---|
| `pack.json` | cairn-pack/v1 manifest |
| `manual.md` | CLAUDE.md fragment, block-guarded |
| `agents/<6>.md` | Subagent frontmatter + procedures |
| `commands/<13>.md` | Slash command frontmatter + bodies |
| `hooks/settings.json` | Claude Code hook bindings (merged into `.claude/settings.json`) |
| `hooks/.mcp.json` | MCP server registration (merged into project `.mcp.json`) |
| `fixtures/dogfood-vault/` | 5-record fixture vault for acceptance |
| `ACCEPTANCE.md` | Dogfood acceptance checklist |

### Pack runtime (new — `crates/cairn-cli/src/packs/`)
| Path | Responsibility |
|---|---|
| `mod.rs` | Re-exports + pack registry (`bundled_pack_for(Harness)`) |
| `embed.rs` | `include_dir!` wrapper, embeds pack content into binary |
| `manifest.rs` | `PackManifest` serde types, `PackError`, Pass A + Pass B validators |
| `install.rs` | `PackInstallOpts`, `PackInstallReceipt`, `install_pack()` |
| `verify.rs` | Tier-1/2/3 conformance cases for `cairn plugins verify` |
| `merge.rs` | `.claude/settings.json` + `.mcp.json` block-marker merge helpers |

### Modifications
| Path | Change |
|---|---|
| `Cargo.toml` (workspace) | Add `semver` to `[workspace.dependencies]` if absent |
| `crates/cairn-cli/Cargo.toml` | Add `include_dir`, `semver` deps |
| `crates/cairn-cli/src/lib.rs` | Add `pub mod packs;` |
| `crates/cairn-cli/src/skill.rs` | Strip `CLAUDE_*_COMMAND` consts, `claude_slash_commands()`, replace `install_claude_code_integration()` body with delegation to `packs::install` |
| `crates/cairn-cli/src/plugins/verify.rs` | Add pack-verify cases alongside existing plugin cases |
| `crates/cairn-cli/src/docgen.rs` | Add pack reference page generator |

### Tests
| Path | Coverage |
|---|---|
| `crates/cairn-cli/src/packs/manifest.rs` (in-file tests) | Pass A + Pass B invariants |
| `crates/cairn-cli/src/packs/install.rs` (in-file tests) | Tempdir install, idempotency, force |
| `crates/cairn-cli/src/packs/merge.rs` (in-file tests) | settings.json merge round-trip |
| `crates/cairn-cli/tests/claude_code_pack_install.rs` | Insta snapshot per emitted file (6 + 13 + 1 + 1 + 1 + 1) |
| `crates/cairn-cli/tests/claude_code_pack_verify.rs` | Pack-verify Tier-1/2/3 |

---

## Phase 1 — Pack runtime skeleton

### Task 1: Add `include_dir` and `semver` deps to `cairn-cli`

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml`
- Modify: `Cargo.toml` (workspace, only if `semver` not already present)

- [ ] **Step 1: Check workspace for `semver`**

Run: `grep -E '^semver' Cargo.toml`
Expected: either an entry (`semver = { version = "1", ... }`) or no output.

- [ ] **Step 2: If absent, add `semver` to workspace deps**

Edit `Cargo.toml`, add under `[workspace.dependencies]`:

```toml
semver = "1"
```

- [ ] **Step 3: Add deps to `cairn-cli`**

Edit `crates/cairn-cli/Cargo.toml`, add under `[dependencies]` (alphabetised among existing deps):

```toml
include_dir = { workspace = true }
semver = { workspace = true }
```

- [ ] **Step 4: Verify build**

Run: `cargo check -p cairn-cli --locked`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/cairn-cli/Cargo.toml Cargo.lock
git commit -m "build(cairn-cli): add include_dir + semver deps for pack runtime (#182)"
```

---

### Task 2: Scaffold `packs/cairn-claude-code/` directory

**Files:**
- Create: `packs/cairn-claude-code/.gitkeep` (placeholder while sub-dirs fill in later tasks)
- Create: `packs/cairn-claude-code/pack.json` (full manifest — Task 6 fills content; for now an empty placeholder so `include_dir!` finds the directory)

- [ ] **Step 1: Create directory tree**

Run:

```bash
mkdir -p packs/cairn-claude-code/agents
mkdir -p packs/cairn-claude-code/commands
mkdir -p packs/cairn-claude-code/hooks
mkdir -p packs/cairn-claude-code/fixtures/dogfood-vault
touch packs/cairn-claude-code/agents/.gitkeep
touch packs/cairn-claude-code/commands/.gitkeep
touch packs/cairn-claude-code/hooks/.gitkeep
touch packs/cairn-claude-code/fixtures/dogfood-vault/.gitkeep
```

- [ ] **Step 2: Write minimal pack.json placeholder**

Create `packs/cairn-claude-code/pack.json`:

```json
{
  "schema": "cairn-pack/v1",
  "pack_id": "cairn-claude-code",
  "name": "cairn-claude-code",
  "version": "0.1.0",
  "harness": "claude-code",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Reference Claude Code skill-pack for Cairn.",
  "requires_capabilities": [],
  "subagents": [],
  "commands": [],
  "hooks": {},
  "manual_fragment": "manual.md"
}
```

- [ ] **Step 3: Commit**

```bash
git add packs/cairn-claude-code/
git commit -m "feat(packs): scaffold packs/cairn-claude-code/ directory (#182)"
```

---

### Task 3: Scaffold `crates/cairn-cli/src/packs/` module

**Files:**
- Create: `crates/cairn-cli/src/packs/mod.rs`
- Create: `crates/cairn-cli/src/packs/embed.rs`
- Modify: `crates/cairn-cli/src/lib.rs`

- [ ] **Step 1: Create empty module files**

Create `crates/cairn-cli/src/packs/mod.rs`:

```rust
//! Generic cairn-pack/v1 runtime: embed, validate, install harness packs.
//!
//! Pack content lives under `packs/<harness>/` (markdown + JSON, no Rust).
//! This module owns the loader, validator, installer, and verify hooks.

pub mod embed;
pub mod manifest;
```

Create `crates/cairn-cli/src/packs/embed.rs`:

```rust
//! Build-time embedding of bundled packs via `include_dir!`.

use include_dir::{Dir, include_dir};

/// Bundled `cairn-claude-code` pack content.
///
/// Embedded from `packs/cairn-claude-code/` at build time. The path is
/// relative to the crate's `CARGO_MANIFEST_DIR`
/// (`crates/cairn-cli/`), so the pack source lives at
/// `<workspace>/packs/cairn-claude-code/`.
pub static CAIRN_CLAUDE_CODE_PACK: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../../packs/cairn-claude-code");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_contains_pack_json() {
        assert!(
            CAIRN_CLAUDE_CODE_PACK.get_file("pack.json").is_some(),
            "embedded pack must contain pack.json"
        );
    }
}
```

Create `crates/cairn-cli/src/packs/manifest.rs`:

```rust
//! cairn-pack/v1 manifest types and validation.
//!
//! See `docs/superpowers/specs/2026-05-26-issue-182-cairn-claude-code-pack-design.md`
//! §4 for the schema and invariant list.

// Real types land in Task 4. This module starts empty so `mod.rs` resolves.
```

- [ ] **Step 2: Wire module into lib.rs**

Edit `crates/cairn-cli/src/lib.rs`, add the `pub mod packs;` declaration alongside the existing `pub mod` lines (alphabetical order; place between `nexus` and `plugins` or wherever fits the existing order).

- [ ] **Step 3: Verify build + embed test**

Run: `cargo test -p cairn-cli --lib packs::embed -- --nocapture`
Expected: `embed_contains_pack_json` passes.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/lib.rs crates/cairn-cli/src/packs/
git commit -m "feat(cairn-cli): scaffold packs/ runtime module + include_dir embed (#182)"
```

---

### Task 4: `PackManifest` serde types + minimal load test

**Files:**
- Modify: `crates/cairn-cli/src/packs/manifest.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cairn-cli/src/packs/manifest.rs`:

```rust
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
```

- [ ] **Step 2: Run test — expect FAIL**

Run: `cargo test -p cairn-cli --lib packs::manifest::load_tests -- --nocapture`
Expected: FAIL — `PackManifest` and `Harness` are not yet defined.

- [ ] **Step 3: Implement types**

Replace contents of `crates/cairn-cli/src/packs/manifest.rs` with:

```rust
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
```

- [ ] **Step 4: Run test — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::manifest::load_tests -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/manifest.rs
git commit -m "feat(cairn-cli): PackManifest serde types for cairn-pack/v1 (#182)"
```

---

### Task 5: Pass A validation — structural invariants

**Files:**
- Modify: `crates/cairn-cli/src/packs/manifest.rs`

Pass A covers spec §4 invariants 1–6 + 9 + 11 (structural; no external table lookup). Invariants 7, 8, 10 land in Task 8 (Pass B).

- [ ] **Step 1: Write the failing tests**

Append a `validate_tests` module to `manifest.rs`:

```rust
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
}
```

- [ ] **Step 2: Run tests — expect FAIL (no validate_pass_a)**

Run: `cargo test -p cairn-cli --lib packs::manifest::validate_tests`
Expected: FAIL — `validate_pass_a` undefined.

- [ ] **Step 3: Implement Pass A**

Add the following to `manifest.rs` (after the `impl Default` / before the tests, anywhere in the file is fine):

```rust
/// Canonical lifecycle hook events (must match
/// `cairn_cli::hooks::HookName::ALL`).
const HOOK_EVENTS: &[&str] = &[
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
                    self.cairn_mcp_compat, rest
                ),
            });
        }

        // 5. harness — enum already parsed by serde. Nothing extra to check.

        // 6. paths inside pack.
        if !is_safe_relative_path(&self.manual_fragment) {
            return Err(PackError::ManifestInvalid {
                reason: format!(
                    "manual_fragment path `{}` escapes pack root",
                    self.manual_fragment
                ),
            });
        }
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
        }
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
        }

        // 9. hook event names canonical.
        for hook_name in self.hooks.keys() {
            if !HOOK_EVENTS.contains(&hook_name.as_str()) {
                return Err(PackError::HookUnknown {
                    hook: hook_name.clone(),
                });
            }
        }

        // 11. unique subagent / command ids.
        {
            let mut seen = std::collections::BTreeSet::new();
            for s in &self.subagents {
                if !seen.insert(&s.id) {
                    return Err(PackError::ManifestInvalid {
                        reason: format!("duplicate subagent id `{}`", s.id),
                    });
                }
            }
        }
        {
            let mut seen = std::collections::BTreeSet::new();
            for c in &self.commands {
                if !seen.insert(&c.id) {
                    return Err(PackError::ManifestInvalid {
                        reason: format!("duplicate command id `{}`", c.id),
                    });
                }
            }
        }

        Ok(())
    }
}
```

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::manifest::validate_tests`
Expected: all 10 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/manifest.rs
git commit -m "feat(cairn-cli): pack manifest Pass A structural validation (#182)"
```

---

## Phase 2 — Pack content

### Task 6: Write full `pack.json` manifest

**Files:**
- Modify: `packs/cairn-claude-code/pack.json`

- [ ] **Step 1: Replace placeholder pack.json with full manifest**

Overwrite `packs/cairn-claude-code/pack.json` with:

```json
{
  "schema": "cairn-pack/v1",
  "pack_id": "cairn-claude-code",
  "name": "cairn-claude-code",
  "version": "0.1.0",
  "harness": "claude-code",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Reference Claude Code skill-pack for Cairn — six subagents, thirteen slash commands, five-event hook bindings.",
  "requires_capabilities": [
    "cairn.mcp.v1.search.keyword",
    "cairn.mcp.v1.retrieve.record",
    "cairn.mcp.v1.forget.record"
  ],
  "subagents": [
    { "id": "context-loader",   "path": "agents/context-loader.md",   "uses_mcp_tools": ["assemble_hot","retrieve","search"] },
    { "id": "vault-librarian",  "path": "agents/vault-librarian.md",  "uses_mcp_tools": ["lint"] },
    { "id": "forget-planner",   "path": "agents/forget-planner.md",   "uses_mcp_tools": ["forget"] },
    { "id": "consolidator",     "path": "agents/consolidator.md",     "uses_mcp_tools": ["lint","summarize"] },
    { "id": "replay-checker",   "path": "agents/replay-checker.md",   "uses_mcp_tools": ["capture_trace","retrieve"] },
    { "id": "trace-summarizer", "path": "agents/trace-summarizer.md", "uses_mcp_tools": ["summarize","retrieve"] }
  ],
  "commands": [
    { "id": "cairn-ingest",        "path": "commands/cairn-ingest.md",        "kind": "verb-direct", "verb": "ingest" },
    { "id": "cairn-search",        "path": "commands/cairn-search.md",        "kind": "verb-direct", "verb": "search" },
    { "id": "cairn-retrieve",      "path": "commands/cairn-retrieve.md",      "kind": "verb-direct", "verb": "retrieve" },
    { "id": "cairn-summarize",     "path": "commands/cairn-summarize.md",     "kind": "verb-direct", "verb": "summarize" },
    { "id": "cairn-assemble",      "path": "commands/cairn-assemble.md",      "kind": "verb-direct", "verb": "assemble_hot" },
    { "id": "cairn-capture-trace", "path": "commands/cairn-capture-trace.md", "kind": "verb-direct", "verb": "capture_trace" },
    { "id": "cairn-lint",          "path": "commands/cairn-lint.md",          "kind": "verb-direct", "verb": "lint" },
    { "id": "cairn-forget",        "path": "commands/cairn-forget.md",        "kind": "verb-direct", "verb": "forget" },
    { "id": "cairn-status",        "path": "commands/cairn-status.md",        "kind": "verb-direct", "verb": "status" },
    { "id": "cairn-standup",       "path": "commands/cairn-standup.md",       "kind": "workflow" },
    { "id": "cairn-wrap-up",       "path": "commands/cairn-wrap-up.md",       "kind": "workflow" },
    { "id": "cairn-audit",         "path": "commands/cairn-audit.md",         "kind": "workflow" },
    { "id": "cairn-recall",        "path": "commands/cairn-recall.md",        "kind": "workflow" }
  ],
  "hooks": {
    "SessionStart":     { "command": "cairn hook SessionStart" },
    "UserPromptSubmit": { "command": "cairn hook UserPromptSubmit" },
    "PreToolUse":       { "command": "cairn hook PreToolUse" },
    "PostToolUse":      { "command": "cairn hook PostToolUse" },
    "Stop":             { "command": "cairn hook Stop" }
  },
  "manual_fragment": "manual.md"
}
```

- [ ] **Step 2: Run Pass A test on real manifest**

Add a test to `manifest.rs` `load_tests` module:

```rust
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
```

Run: `cargo test -p cairn-cli --lib packs::manifest::load_tests::bundled_manifest_passes_pass_a`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add packs/cairn-claude-code/pack.json crates/cairn-cli/src/packs/manifest.rs
git commit -m "feat(packs): cairn-claude-code pack.json full manifest (#182)"
```

---

### Task 7: Subagent — `context-loader.md`

**Files:**
- Create: `packs/cairn-claude-code/agents/context-loader.md`
- Delete: `packs/cairn-claude-code/agents/.gitkeep` (replace placeholder once real files exist)

- [ ] **Step 1: Write the file**

Create `packs/cairn-claude-code/agents/context-loader.md`:

```markdown
---
name: context-loader
description: Use when you need Cairn-resident context for a topic, person, or project before generating an answer. Reads only — never ingests or forgets.
tools: mcp__cairn__assemble_hot, mcp__cairn__retrieve, mcp__cairn__search
---

# Context Loader

You are a Cairn context-loader. Your job is to pull the smallest sufficient
context for the asked-about entity from the Cairn vault using MCP tools only.

## Procedure

1. Call `mcp__cairn__assemble_hot` with the topic/person/project as scope to
   get the hot-memory prefix for this turn.
2. If the prefix references record ids you do not yet hold, call
   `mcp__cairn__retrieve` with `target=record` and the record id for each.
3. If the prefix is thin (fewer than three records) or the scope is broad,
   call `mcp__cairn__search` with `mode=hybrid` to discover adjacent records.
4. Return a single concise context block. Do NOT shell out to `cairn` —
   MCP only.

## Boundaries

- Never call `mcp__cairn__ingest` or `mcp__cairn__forget` — read-only.
- Never include record bodies above 500 characters per record in the return.
- If `assemble_hot` returns `CapabilityUnavailable`, fall back to `search` +
  `retrieve` and note the degradation in the response.
```

- [ ] **Step 2: Remove `.gitkeep` placeholder if present**

Run:

```bash
rm -f packs/cairn-claude-code/agents/.gitkeep
```

- [ ] **Step 3: Commit**

```bash
git add packs/cairn-claude-code/agents/
git commit -m "feat(packs): context-loader subagent (#182)"
```

---

### Task 8: Subagent — `vault-librarian.md`

**Files:**
- Create: `packs/cairn-claude-code/agents/vault-librarian.md`

- [ ] **Step 1: Write the file**

```markdown
---
name: vault-librarian
description: Use to audit Cairn vault health — orphans, broken edges, schema drift, contradictions. Read-only.
tools: mcp__cairn__lint
---

# Vault Librarian

You are a Cairn vault-librarian. Your job is to run a single `lint` pass and
report a structured health summary.

## Procedure

1. Call `mcp__cairn__lint` with `fix=false` and `write_report=false`.
2. Group findings by severity (critical, warning, info) and by family
   (orphans, broken edges, schema drift, stale claims, hot-memory budget,
   derived-index drift).
3. Return:
   - One-line summary: `N critical, M warnings, K info`.
   - Per-family bullet list of top three findings.
   - A suggestion of which subagent or command to run next
     (`forget-planner` for orphans, `consolidator` for stale claims,
     etc.) — do not run it yourself.

## Boundaries

- Read-only. Never set `fix=true` or `write_report=true`.
- Never delete records.
- If `lint` returns `CapabilityUnavailable`, return that fact and stop.
```

- [ ] **Step 2: Commit**

```bash
git add packs/cairn-claude-code/agents/vault-librarian.md
git commit -m "feat(packs): vault-librarian subagent (#182)"
```

---

### Task 9: Subagent — `forget-planner.md`

**Files:**
- Create: `packs/cairn-claude-code/agents/forget-planner.md`

- [ ] **Step 1: Write the file**

```markdown
---
name: forget-planner
description: Use to dry-run the forget fan-out for a record, session, or scope before any destructive call. Returns the FlushPlan; the human commits or rejects.
tools: mcp__cairn__forget
---

# Forget Planner

You are a Cairn forget-planner. You ONLY produce dry-run plans. You never
commit a forget.

## Procedure

1. Identify the forget target from user input: a `record_id`, `session_id`,
   or `scope`.
2. Call `mcp__cairn__forget` with the target AND `dry_run=true`. This
   returns a FlushPlan envelope describing what would be removed.
3. Render the FlushPlan as a human-readable diff:
   - Records to delete (id, kind, body preview).
   - Edges to drop (source → target relation).
   - Hot-memory entries to invalidate.
   - WAL operations that would be appended.
4. Ask the user to confirm before any commit. Do NOT call forget without
   `dry_run=true` from inside this subagent.

## Boundaries

- Never call `mcp__cairn__forget` with `dry_run=false`.
- Never call any other write verb.
- If `forget` returns `CapabilityUnavailable`, surface the error verbatim.
```

- [ ] **Step 2: Commit**

```bash
git add packs/cairn-claude-code/agents/forget-planner.md
git commit -m "feat(packs): forget-planner subagent (dry-run only) (#182)"
```

---

### Task 10: Subagent — `consolidator.md`

**Files:**
- Create: `packs/cairn-claude-code/agents/consolidator.md`

- [ ] **Step 1: Write the file**

```markdown
---
name: consolidator
description: Use to summarize stale or redundant records into a canonical synthesis. Persists the new summary record.
tools: mcp__cairn__lint, mcp__cairn__summarize
---

# Consolidator

You are a Cairn consolidator. Your job is to find consolidation candidates
via `lint`, then persist a summary record covering them.

## Procedure

1. Call `mcp__cairn__lint` with `fix=false`, `write_report=false`. Inspect
   the `stale_claims` and `redundant_records` findings.
2. Pick a single coherent cluster of record ids (between 2 and 8 records).
   If no cluster is obvious, return "no consolidation candidates" and stop.
3. Call `mcp__cairn__summarize` with the picked record ids and
   `persist=true`. This writes a new summary record under the appropriate
   scope.
4. Return the new record id and a brief description of the cluster.

## Boundaries

- Never call `mcp__cairn__forget`. Consolidation is additive, not
  destructive.
- Always cluster size 2..=8. Larger sets land in a separate run.
- If `summarize` returns `CapabilityUnavailable` for `persist`, fall back
  to `persist=false` and return the synthesis without writing.
```

- [ ] **Step 2: Commit**

```bash
git add packs/cairn-claude-code/agents/consolidator.md
git commit -m "feat(packs): consolidator subagent (#182)"
```

---

### Task 11: Subagent — `replay-checker.md`

**Files:**
- Create: `packs/cairn-claude-code/agents/replay-checker.md`

- [ ] **Step 1: Write the file**

```markdown
---
name: replay-checker
description: Use to replay a recorded trace against the current Cairn state and report behavioural diffs.
tools: mcp__cairn__capture_trace, mcp__cairn__retrieve
---

# Replay Checker

You are a Cairn replay-checker. You compare a stored trajectory against
the current vault state.

## Procedure

1. Take a cassette id (a `capture_trace` record id) from user input.
2. Call `mcp__cairn__retrieve` with `target=tool_call` and the cassette id
   to load the recorded MCP calls.
3. For each recorded call, perform the same call against the live vault
   via the appropriate `mcp__cairn__*` tool (NOT through `capture_trace`).
4. Report the diff:
   - Number of identical responses.
   - Number of divergent responses (with the diff body for the first three).
   - Categorise divergences: schema drift, record removed, record updated,
     ordering changed.

## Boundaries

- Read-only against the live vault. Never replay write-verbs
  (`ingest`, `forget`, `summarize --persist`).
- Cassette must be a `capture_trace` record; reject other kinds.
- Stop after first 50 calls if the cassette is longer; note truncation.
```

- [ ] **Step 2: Commit**

```bash
git add packs/cairn-claude-code/agents/replay-checker.md
git commit -m "feat(packs): replay-checker subagent (#182)"
```

---

### Task 12: Subagent — `trace-summarizer.md`

**Files:**
- Create: `packs/cairn-claude-code/agents/trace-summarizer.md`

- [ ] **Step 1: Write the file**

```markdown
---
name: trace-summarizer
description: Use to roll up a session, turn window, or scope into a single synthesis. Default read-only; pass `persist=true` to write.
tools: mcp__cairn__summarize, mcp__cairn__retrieve
---

# Trace Summarizer

You are a Cairn trace-summarizer. Your job is to produce a concise
synthesis of a session window.

## Procedure

1. Resolve the window from user input:
   - `session_id` → call `mcp__cairn__retrieve` with `target=session`.
   - `--days N` → search-and-retrieve sessions in the last N days
     (delegate the search to the user-facing layer; this subagent only
     summarizes given records).
2. Once the candidate record id list is known, call
   `mcp__cairn__summarize` with those ids.
3. Default `persist=false`. Only set `persist=true` if the user explicitly
   requests it (the `cairn-wrap-up` workflow does so).
4. Return the synthesis with a record-id citation per claim.

## Boundaries

- Citations are MANDATORY. Every claim cites at least one record id.
- Never call `mcp__cairn__forget` or `mcp__cairn__ingest`.
- If `summarize` returns `CapabilityUnavailable` for `persist`, fall back
  to `persist=false`.
```

- [ ] **Step 2: Commit**

```bash
git add packs/cairn-claude-code/agents/trace-summarizer.md
git commit -m "feat(packs): trace-summarizer subagent (#182)"
```

---

### Task 13: Verb-direct slash commands (all 9)

**Files:**
- Create: `packs/cairn-claude-code/commands/cairn-{ingest,search,retrieve,summarize,assemble,capture-trace,lint,forget,status}.md`
- Delete: `packs/cairn-claude-code/commands/.gitkeep`

Verb-direct commands all share the same skeleton. Write all 9 in one task — they are siblings.

- [ ] **Step 1: Write `cairn-ingest.md`**

```markdown
---
description: Direct Cairn `ingest` verb.
argument-hint: "<--kind k> <--body 'text'>"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn ingest $ARGUMENTS` and report the receipt.

If the user passed free text without flags, default to:

`cairn ingest --kind user --body "$ARGUMENTS"`

Show the resulting record id and ingest mode.
<!-- END CAIRN PACK -->
```

- [ ] **Step 2: Write `cairn-search.md`**

```markdown
---
description: Direct Cairn `search` verb.
argument-hint: "<query> [--mode keyword|semantic|hybrid]"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn search $ARGUMENTS`.

Default to `--mode hybrid` if the user passed only a query and no mode.

Render the top results with `id`, `score`, and a one-line snippet each.
<!-- END CAIRN PACK -->
```

- [ ] **Step 3: Write `cairn-retrieve.md`**

```markdown
---
description: Direct Cairn `retrieve` verb.
argument-hint: "<record-id> [--target record|session|turn|tool_call|folder|scope|profile]"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn retrieve $ARGUMENTS`.

Default `--target record` if the user passed only an id.

Show the record body. For non-record targets, show the structured envelope.
<!-- END CAIRN PACK -->
```

- [ ] **Step 4: Write `cairn-summarize.md`**

```markdown
---
description: Direct Cairn `summarize` verb.
argument-hint: "<id-1> <id-2> ... [--persist]"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn summarize $ARGUMENTS`.

If `--persist` is present, show the new summary record id.
Otherwise show the synthesis text.
<!-- END CAIRN PACK -->
```

- [ ] **Step 5: Write `cairn-assemble.md`**

```markdown
---
description: Direct Cairn `assemble_hot` verb — fetches the hot-memory prefix.
argument-hint: "[--scope <scope>] [--budget <tokens>]"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn assemble_hot $ARGUMENTS`.

Show the assembled prefix and the records it cites.
<!-- END CAIRN PACK -->
```

- [ ] **Step 6: Write `cairn-capture-trace.md`**

```markdown
---
description: Direct Cairn `capture_trace` verb — persists a reasoning trajectory.
argument-hint: "[--session <id>] [--turn <id>]"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn capture_trace $ARGUMENTS`.

Confirm the capture is inside the user's consent envelope before running.
Show the resulting trace record id.
<!-- END CAIRN PACK -->
```

- [ ] **Step 7: Write `cairn-lint.md`**

```markdown
---
description: Direct Cairn `lint` verb — vault health check.
argument-hint: "[--fix] [--write-report]"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn lint $ARGUMENTS`.

Default read-only — do NOT pass `--fix` or `--write-report` unless the
user explicitly requested it.

Show the per-severity counts and top findings.
<!-- END CAIRN PACK -->
```

- [ ] **Step 8: Write `cairn-forget.md`**

```markdown
---
description: Direct Cairn `forget` verb — deletes a record, session, or scope.
argument-hint: "<--record <id>|--session <id>|--scope <scope>> [--dry-run]"
---

<!-- BEGIN CAIRN PACK -->
ALWAYS run with `--dry-run` first unless the user has explicitly confirmed
a destructive forget on this exact target.

`cairn forget $ARGUMENTS`

Show the FlushPlan and ask for confirmation before any non-dry-run call.
<!-- END CAIRN PACK -->
```

- [ ] **Step 9: Write `cairn-status.md`**

```markdown
---
description: Cairn `status` — show advertised verbs, modes, and capabilities.
---

<!-- BEGIN CAIRN PACK -->
Run `cairn status --json`.

Render the advertised verbs and capabilities as a table.
<!-- END CAIRN PACK -->
```

- [ ] **Step 10: Remove placeholder**

```bash
rm -f packs/cairn-claude-code/commands/.gitkeep
```

- [ ] **Step 11: Commit**

```bash
git add packs/cairn-claude-code/commands/
git commit -m "feat(packs): nine verb-direct slash commands (#182)"
```

---

### Task 14: Workflow slash commands (all 4)

**Files:**
- Create: `packs/cairn-claude-code/commands/cairn-{standup,wrap-up,audit,recall}.md`

- [ ] **Step 1: Write `cairn-standup.md`**

```markdown
---
description: Generate a Cairn-backed standup brief.
argument-hint: "[--days N]"
---

<!-- BEGIN CAIRN PACK -->
Generate a standup brief.

1. Parse `--days N` from `$ARGUMENTS` (default 1).
2. Spawn the `trace-summarizer` subagent: summarize sessions in the last N
   days.
3. Spawn the `context-loader` subagent: surface open threads in the same
   window.
4. Combine into a single brief:
   - **Completed:** ...
   - **In progress:** ...
   - **Blocked:** ...

Cite at least one Cairn record id per bullet.
<!-- END CAIRN PACK -->
```

- [ ] **Step 2: Write `cairn-wrap-up.md`**

```markdown
---
description: End-of-session wrap-up — captures trace + persists summary.
---

<!-- BEGIN CAIRN PACK -->
Wrap up the current session.

1. Run `/cairn-capture-trace --session <current>` to persist the trajectory.
2. Spawn the `consolidator` subagent to lint + summarize any newly-stale
   records produced during the session.
3. Report:
   - Trace record id.
   - Summary record id (if `consolidator` produced one).
   - Open follow-ups not consolidated.
<!-- END CAIRN PACK -->
```

- [ ] **Step 3: Write `cairn-audit.md`**

```markdown
---
description: Vault audit — librarian report + orphan forget dry-run.
---

<!-- BEGIN CAIRN PACK -->
Run a vault audit.

1. Spawn the `vault-librarian` subagent: full lint report.
2. For each orphan finding, spawn the `forget-planner` subagent with the
   orphan record id to get a dry-run FlushPlan.
3. Render a consolidated report:
   - Lint summary (criticals / warnings / info).
   - Orphan candidates with forget cost (records + edges that would be
     dropped).
4. Stop. Never commit a forget from this command — the user runs
   `/cairn-forget` explicitly.
<!-- END CAIRN PACK -->
```

- [ ] **Step 4: Write `cairn-recall.md`**

```markdown
---
description: Recall Cairn context for a named topic, person, or project.
argument-hint: "<topic-or-name>"
---

<!-- BEGIN CAIRN PACK -->
Spawn the `context-loader` subagent with `$ARGUMENTS` as the scope.

Render the returned context block verbatim.
<!-- END CAIRN PACK -->
```

- [ ] **Step 5: Commit**

```bash
git add packs/cairn-claude-code/commands/cairn-standup.md packs/cairn-claude-code/commands/cairn-wrap-up.md packs/cairn-claude-code/commands/cairn-audit.md packs/cairn-claude-code/commands/cairn-recall.md
git commit -m "feat(packs): four workflow slash commands (#182)"
```

---

### Task 15: Hook bindings + MCP server registration

**Files:**
- Create: `packs/cairn-claude-code/hooks/settings.json`
- Create: `packs/cairn-claude-code/hooks/.mcp.json`
- Delete: `packs/cairn-claude-code/hooks/.gitkeep`

- [ ] **Step 1: Write `hooks/settings.json`**

```json
{
  "_pack": "cairn-claude-code@0.1.0",
  "hooks": {
    "SessionStart": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook SessionStart" }] }
    ],
    "UserPromptSubmit": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook UserPromptSubmit" }] }
    ],
    "PreToolUse": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook PreToolUse" }] }
    ],
    "PostToolUse": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook PostToolUse" }] }
    ],
    "Stop": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook Stop" }] }
    ]
  }
}
```

- [ ] **Step 2: Write `hooks/.mcp.json`**

```json
{
  "_pack": "cairn-claude-code@0.1.0",
  "mcpServers": {
    "cairn": {
      "command": "cairn",
      "args": ["serve", "--mcp", "--transport", "stdio"]
    }
  }
}
```

- [ ] **Step 3: Remove placeholder**

```bash
rm -f packs/cairn-claude-code/hooks/.gitkeep
```

- [ ] **Step 4: Commit**

```bash
git add packs/cairn-claude-code/hooks/
git commit -m "feat(packs): hook bindings + MCP server registration (#182)"
```

---

### Task 16: Manual fragment + ACCEPTANCE checklist

**Files:**
- Create: `packs/cairn-claude-code/manual.md`
- Create: `packs/cairn-claude-code/ACCEPTANCE.md`

- [ ] **Step 1: Write `manual.md`**

```markdown
<!-- BEGIN CAIRN PACK MANUAL -->
## Cairn (Claude Code reference pack)

This project uses the Cairn memory layer. Six subagents and 13 slash
commands are available.

### Subagents

| Agent | Purpose | MCP tools |
|---|---|---|
| context-loader | Pull minimal context for a topic | assemble_hot, retrieve, search |
| vault-librarian | Vault health report | lint |
| forget-planner | Dry-run forget plan | forget (dry-run only) |
| consolidator | Consolidate + summarize | lint, summarize |
| replay-checker | Replay vs golden cassette | capture_trace, retrieve |
| trace-summarizer | Session / turn rollups | summarize, retrieve |

### Slash commands

**Verb-direct:** `/cairn-ingest`, `/cairn-search`, `/cairn-retrieve`,
`/cairn-summarize`, `/cairn-assemble`, `/cairn-capture-trace`,
`/cairn-lint`, `/cairn-forget`, `/cairn-status`.

**Workflow:** `/cairn-standup`, `/cairn-wrap-up`, `/cairn-audit`,
`/cairn-recall`.

### Safety boundaries

- `forget-planner` is dry-run only. Human approval is required before
  any commit.
- Subagents never shell out to `cairn` — they use MCP tools only.
- Verb-direct slash commands shell out to the local `cairn` binary.
- `capture_trace` commands MUST run inside the user's consent envelope
  (see brief §14).
<!-- END CAIRN PACK MANUAL -->
```

- [ ] **Step 2: Write `ACCEPTANCE.md`**

```markdown
# cairn-claude-code dogfood acceptance checklist

Run against `packs/cairn-claude-code/fixtures/dogfood-vault/` (5-record
fixture). Mark each step pass / fail.

1. [ ] Install: `cairn skill install --harness claude-code --target <tmp>`.
2. [ ] `/cairn-status` returns the capability table and the advertised
       verbs.
3. [ ] `/cairn-ingest --kind user --body "test"` returns a new record id.
4. [ ] `/cairn-search test` finds the record from step 3.
5. [ ] `/cairn-retrieve <id>` returns the record body.
6. [ ] Spawning `context-loader` for topic "cairn" returns at least one
       record, all calls via `mcp__cairn__*`.
7. [ ] Spawning `vault-librarian` returns a lint report with zero
       criticals.
8. [ ] Spawning `forget-planner` for the record from step 3 returns a
       dry-run FlushPlan and does NOT delete.
9. [ ] Spawning `consolidator` records a `summarize --persist` call and
       returns a new summary record id.
10. [ ] Spawning `replay-checker` against a recorded cassette returns
        zero diffs.
11. [ ] Spawning `trace-summarizer` for the last session returns a
        cited synthesis.
12. [ ] `/cairn-standup --days 1` returns a combined `trace-summarizer`
        + `context-loader` output.
13. [ ] `/cairn-wrap-up` runs `capture_trace` then `summarize --persist`.
14. [ ] `/cairn-audit` returns lint + orphan dry-run output.
15. [ ] `/cairn-recall cairn` returns context-loader output.
```

- [ ] **Step 3: Commit**

```bash
git add packs/cairn-claude-code/manual.md packs/cairn-claude-code/ACCEPTANCE.md
git commit -m "docs(packs): manual fragment + dogfood acceptance checklist (#182)"
```

---

### Task 17: Dogfood fixture vault

**Files:**
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/.cairn/config.yaml`
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/purpose.md`
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/sources/2026-05-01-spec.md`
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/raw/r_001.md`
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/raw/r_002.md`
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/raw/r_003.md`
- Create: `packs/cairn-claude-code/fixtures/dogfood-vault/wiki/concept-cairn.md`
- Delete: `packs/cairn-claude-code/fixtures/dogfood-vault/.gitkeep`

- [ ] **Step 1: Create directory structure**

```bash
mkdir -p packs/cairn-claude-code/fixtures/dogfood-vault/.cairn
mkdir -p packs/cairn-claude-code/fixtures/dogfood-vault/sources
mkdir -p packs/cairn-claude-code/fixtures/dogfood-vault/raw
mkdir -p packs/cairn-claude-code/fixtures/dogfood-vault/wiki
```

- [ ] **Step 2: Write `.cairn/config.yaml`**

```yaml
vault:
  layout: v1
profile:
  default_scope: dogfood
```

- [ ] **Step 3: Write `purpose.md`**

```markdown
# Dogfood vault

A five-record fixture used to exercise the cairn-claude-code reference
skill-pack in acceptance tests. Records cover ingest, search, retrieve,
summarize, lint, and forget paths.
```

- [ ] **Step 4: Write `sources/2026-05-01-spec.md`**

```markdown
---
source_id: src_spec_2026_05_01
kind: document
ingested_at: 2026-05-01T10:00:00Z
---

# Dogfood spec

This fixture is the spec for the dogfood vault. It enumerates the records
the test suite expects to find.
```

- [ ] **Step 5: Write `raw/r_001.md`**

```markdown
---
record_id: r_001
kind: user
scope: dogfood
created_at: 2026-05-01T10:01:00Z
---

The user prefers concise summaries with at least one citation per bullet.
```

- [ ] **Step 6: Write `raw/r_002.md`**

```markdown
---
record_id: r_002
kind: clip
scope: dogfood
created_at: 2026-05-01T10:02:00Z
source: src_spec_2026_05_01
---

Cairn ships eight verbs across four isomorphic surfaces (CLI, MCP, SDK,
skill). The CLI is the ground truth; the other three mirror it.
```

- [ ] **Step 7: Write `raw/r_003.md`**

```markdown
---
record_id: r_003
kind: trace
scope: dogfood
created_at: 2026-05-01T10:03:00Z
session: sess_dogfood_001
---

User asked for a standup. Trace-summarizer surfaced records r_001 and
r_002. Context-loader added concept-cairn wiki page.
```

- [ ] **Step 8: Write `wiki/concept-cairn.md`**

```markdown
---
record_id: w_cairn
kind: concept
scope: dogfood
---

# Concept: Cairn

Cairn is the standalone, harness-agnostic agent-memory framework backing
this vault. See brief §2.
```

- [ ] **Step 9: Remove placeholder**

```bash
rm -f packs/cairn-claude-code/fixtures/dogfood-vault/.gitkeep
```

- [ ] **Step 10: Commit**

```bash
git add packs/cairn-claude-code/fixtures/
git commit -m "feat(packs): dogfood fixture vault (5 records) (#182)"
```

---

## Phase 3 — Pass B cross-validation

### Task 18: McpToolIndex helper + Pass B

**Files:**
- Modify: `crates/cairn-cli/src/packs/manifest.rs`

Pass B covers spec §4 invariants 7, 8, 10 (cross-reference to MCP TOOLS table, capability table, hook list).

- [ ] **Step 1: Write the failing tests**

Append to `manifest.rs`:

```rust
#[cfg(test)]
mod pass_b_tests {
    use super::*;

    fn minimal_for_pass_b() -> PackManifest {
        let mut m = PackManifest {
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
        };
        m.subagents.push(SubagentDecl {
            id: "loader".to_owned(),
            path: "agents/loader.md".to_owned(),
            uses_mcp_tools: vec!["assemble_hot".to_owned()],
        });
        m.commands.push(CommandDecl {
            id: "cairn-ingest".to_owned(),
            path: "commands/cairn-ingest.md".to_owned(),
            kind: CommandKind::VerbDirect,
            verb: Some("ingest".to_owned()),
        });
        m
    }

    #[test]
    fn pass_b_accepts_known_mcp_tools_and_caps() {
        let m = minimal_for_pass_b();
        m.validate_pass_b().expect("known tools pass");
    }

    #[test]
    fn pass_b_rejects_unknown_mcp_tool_in_subagent() {
        let mut m = minimal_for_pass_b();
        m.subagents[0].uses_mcp_tools.push("does_not_exist".to_owned());
        match m.validate_pass_b() {
            Err(PackError::McpToolUnknown { tool }) => assert_eq!(tool, "does_not_exist"),
            other => panic!("expected McpToolUnknown, got {other:?}"),
        }
    }

    #[test]
    fn pass_b_rejects_unknown_verb_in_command() {
        let mut m = minimal_for_pass_b();
        m.commands[0].verb = Some("fictional".to_owned());
        match m.validate_pass_b() {
            Err(PackError::McpToolUnknown { tool }) => assert_eq!(tool, "fictional"),
            other => panic!("expected McpToolUnknown, got {other:?}"),
        }
    }

    #[test]
    fn pass_b_rejects_unknown_capability() {
        let mut m = minimal_for_pass_b();
        m.requires_capabilities
            .push("cairn.mcp.v1.does.not.exist".to_owned());
        match m.validate_pass_b() {
            Err(PackError::CapabilityUnknown { cap }) => {
                assert_eq!(cap, "cairn.mcp.v1.does.not.exist");
            }
            other => panic!("expected CapabilityUnknown, got {other:?}"),
        }
    }

    #[test]
    fn bundled_manifest_passes_pass_b() {
        let bytes = crate::packs::embed::CAIRN_CLAUDE_CODE_PACK
            .get_file("pack.json")
            .expect("pack.json present")
            .contents();
        let m: PackManifest = serde_json::from_slice(bytes).expect("parse");
        m.validate_pass_b().expect("real manifest passes Pass B");
    }
}
```

- [ ] **Step 2: Run tests — expect FAIL (no validate_pass_b)**

Run: `cargo test -p cairn-cli --lib packs::manifest::pass_b_tests`
Expected: FAIL — `validate_pass_b` undefined.

- [ ] **Step 3: Implement Pass B**

Add to `manifest.rs`:

```rust
impl PackManifest {
    /// Pass B: cross-reference validation against MCP TOOLS, capability
    /// advertise table, and hook lifecycle list.
    ///
    /// Covers spec §4 invariants 7, 8, 10.
    ///
    /// # Errors
    /// Returns [`PackError`] on the first failed invariant.
    pub fn validate_pass_b(&self) -> Result<(), PackError> {
        // 7 + 8. Tool names: collect known tool names from cairn-mcp.
        let known_tools: std::collections::BTreeSet<&str> =
            cairn_mcp::generated::TOOLS
                .iter()
                .map(|t| t.name)
                .collect();

        for c in &self.commands {
            if let Some(verb) = &c.verb {
                if !known_tools.contains(verb.as_str()) {
                    return Err(PackError::McpToolUnknown { tool: verb.clone() });
                }
            }
        }
        for s in &self.subagents {
            for tool in &s.uses_mcp_tools {
                if !known_tools.contains(tool.as_str()) {
                    return Err(PackError::McpToolUnknown { tool: tool.clone() });
                }
            }
        }

        // 10. Capabilities: validate against the cairn-core advertise table.
        //     The table is exposed via `cairn_core::status::advertise::capability_known`.
        for cap in &self.requires_capabilities {
            if !cairn_core::status::advertise::capability_known(cap) {
                return Err(PackError::CapabilityUnknown { cap: cap.clone() });
            }
        }

        Ok(())
    }
}
```

> NOTE FOR IMPLEMENTER: If `cairn_core::status::advertise::capability_known` does NOT exist yet, search for the canonical lookup function (try `cairn_core::status::REMEDIATION` keys, or `advertise::is_capability_advertised`, or inspect `crates/cairn-core/src/status/`). If none exists, add a thin `pub fn capability_known(cap: &str) -> bool` to that module returning whether `cap` matches any entry in the advertise table — this is a one-line lookup since the table is already there per CLAUDE.md §4 invariant 6. Land that helper in a precursor commit before this task.

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::manifest::pass_b_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/manifest.rs
git commit -m "feat(cairn-cli): pack manifest Pass B cross-reference validation (#182)"
```

---

### Task 19: Verify every referenced pack file is present in embed

**Files:**
- Modify: `crates/cairn-cli/src/packs/manifest.rs`

Spec §4 invariant 6 also requires that every `path` in the manifest resolve to an actual file in the embedded pack. Tested here.

- [ ] **Step 1: Write the failing test**

Append to `manifest.rs`:

```rust
#[cfg(test)]
mod presence_tests {
    use super::*;

    #[test]
    fn every_referenced_path_exists_in_embed() {
        let dir = &crate::packs::embed::CAIRN_CLAUDE_CODE_PACK;
        let bytes = dir.get_file("pack.json").unwrap().contents();
        let m: PackManifest = serde_json::from_slice(bytes).unwrap();

        m.assert_all_paths_present(dir).expect("all paths present");
    }

    #[test]
    fn missing_path_is_detected() {
        let dir = &crate::packs::embed::CAIRN_CLAUDE_CODE_PACK;
        let mut m: PackManifest =
            serde_json::from_slice(dir.get_file("pack.json").unwrap().contents()).unwrap();
        m.subagents.push(SubagentDecl {
            id: "ghost".to_owned(),
            path: "agents/ghost.md".to_owned(),
            uses_mcp_tools: vec!["search".to_owned()],
        });
        let err = m.assert_all_paths_present(dir).unwrap_err();
        assert!(matches!(err, PackError::ManifestInvalid { .. }));
    }
}
```

- [ ] **Step 2: Run tests — expect FAIL**

Run: `cargo test -p cairn-cli --lib packs::manifest::presence_tests`
Expected: FAIL.

- [ ] **Step 3: Implement `assert_all_paths_present`**

Add to `manifest.rs`:

```rust
use include_dir::Dir;

impl PackManifest {
    /// Verify every path referenced by the manifest exists in the
    /// supplied embedded pack directory.
    ///
    /// # Errors
    /// Returns [`PackError::ManifestInvalid`] naming the first missing
    /// file.
    pub fn assert_all_paths_present(&self, dir: &Dir<'_>) -> Result<(), PackError> {
        let check = |path: &str, label: &str, id: &str| -> Result<(), PackError> {
            if dir.get_file(path).is_none() {
                Err(PackError::ManifestInvalid {
                    reason: format!("{label} `{id}` references missing file `{path}`"),
                })
            } else {
                Ok(())
            }
        };
        check(&self.manual_fragment, "manual_fragment", "")?;
        for s in &self.subagents {
            check(&s.path, "subagent", &s.id)?;
        }
        for c in &self.commands {
            check(&c.path, "command", &c.id)?;
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::manifest::presence_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/manifest.rs
git commit -m "feat(cairn-cli): pack manifest path-presence check (#182)"
```

---

## Phase 4 — Install

### Task 20: `merge.rs` — `.claude/settings.json` block-marker merge

**Files:**
- Create: `crates/cairn-cli/src/packs/merge.rs`
- Modify: `crates/cairn-cli/src/packs/mod.rs`

Settings.json merge must preserve user-added hooks for the same event AND tag pack-owned entries with `_pack` marker so re-install round-trips.

- [ ] **Step 1: Add module declaration**

Edit `crates/cairn-cli/src/packs/mod.rs`, add line `pub mod merge;` after `pub mod manifest;`.

- [ ] **Step 2: Write the failing test**

Create `crates/cairn-cli/src/packs/merge.rs`:

```rust
//! Block-marker merge helpers for `.claude/settings.json` and `.mcp.json`.
//!
//! Pack-owned entries are tagged with a `_pack` sibling field so re-install
//! can identify and update them without trampling user customisations.

use serde_json::Value;

use crate::packs::manifest::PackError;

const PACK_MARKER_KEY: &str = "_pack";

/// Merge the pack's `settings.json` payload into the existing user JSON.
///
/// User-added entries for the same hook event are preserved. Pack-owned
/// entries are identified by a sibling `_pack` field.
///
/// # Errors
/// Returns [`PackError::MergeConflict`] if the existing JSON has a
/// non-object value at `hooks` or a non-array value at any event key.
pub fn merge_settings_json(
    existing: Value,
    pack_payload: &Value,
    pack_id_at_version: &str,
) -> Result<Value, PackError> {
    let mut out = match existing {
        Value::Null => serde_json::json!({}),
        Value::Object(_) => existing,
        other => {
            return Err(PackError::MergeConflict {
                file: ".claude/settings.json".to_owned(),
                reason: format!("expected object, got {other:?}"),
            });
        }
    };

    let pack_hooks = pack_payload
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or_else(|| PackError::MergeConflict {
            file: ".claude/settings.json".to_owned(),
            reason: "pack payload missing `hooks` object".to_owned(),
        })?;

    let out_hooks = out
        .as_object_mut()
        .expect("out is object")
        .entry("hooks")
        .or_insert_with(|| Value::Object(Default::default()));
    let out_hooks = out_hooks
        .as_object_mut()
        .ok_or_else(|| PackError::MergeConflict {
            file: ".claude/settings.json".to_owned(),
            reason: "existing `hooks` is not an object".to_owned(),
        })?;

    for (event, pack_entries) in pack_hooks {
        let pack_array = pack_entries
            .as_array()
            .ok_or_else(|| PackError::MergeConflict {
                file: ".claude/settings.json".to_owned(),
                reason: format!("pack `hooks.{event}` is not an array"),
            })?;

        let existing_array = match out_hooks.entry(event.clone()) {
            serde_json::map::Entry::Vacant(v) => {
                v.insert(Value::Array(vec![]));
                out_hooks.get_mut(event).unwrap().as_array_mut().unwrap()
            }
            serde_json::map::Entry::Occupied(o) => {
                let v = o.into_mut();
                v.as_array_mut().ok_or_else(|| PackError::MergeConflict {
                    file: ".claude/settings.json".to_owned(),
                    reason: format!("existing `hooks.{event}` is not an array"),
                })?
            }
        };

        // Drop any prior pack-owned entries for this pack id (round-trip).
        existing_array.retain(|entry| {
            entry.get(PACK_MARKER_KEY).and_then(Value::as_str) != Some(pack_id_at_version)
        });

        // Append the pack entries, each tagged with the marker.
        for entry in pack_array {
            let mut tagged = entry.clone();
            if let Some(obj) = tagged.as_object_mut() {
                obj.insert(
                    PACK_MARKER_KEY.to_owned(),
                    Value::String(pack_id_at_version.to_owned()),
                );
            }
            existing_array.push(tagged);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_payload() -> Value {
        serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook SessionStart" }] }
                ]
            }
        })
    }

    #[test]
    fn merge_into_empty_adds_tagged_entries() {
        let out = merge_settings_json(Value::Null, &pack_payload(), "cairn-claude-code@0.1.0")
            .expect("merge ok");
        let event = &out["hooks"]["SessionStart"][0];
        assert_eq!(
            event[PACK_MARKER_KEY].as_str(),
            Some("cairn-claude-code@0.1.0")
        );
    }

    #[test]
    fn merge_preserves_user_entries() {
        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "user-custom" }] }
                ]
            }
        });
        let out =
            merge_settings_json(existing, &pack_payload(), "cairn-claude-code@0.1.0").unwrap();
        let arr = out["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0][PACK_MARKER_KEY].is_null());      // user entry untouched
        assert!(arr[1][PACK_MARKER_KEY].is_string());     // pack entry tagged
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_settings_json(Value::Null, &pack_payload(), "cairn-claude-code@0.1.0")
            .unwrap();
        let twice = merge_settings_json(
            once.clone(),
            &pack_payload(),
            "cairn-claude-code@0.1.0",
        )
        .unwrap();
        assert_eq!(once, twice);
    }
}
```

- [ ] **Step 3: Run tests — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::merge`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/packs/mod.rs crates/cairn-cli/src/packs/merge.rs
git commit -m "feat(cairn-cli): pack settings.json block-marker merge (#182)"
```

---

### Task 21: CLAUDE.md block injection helper

**Files:**
- Modify: `crates/cairn-cli/src/packs/merge.rs`

CLAUDE.md must be modifiable by the user outside the block markers; the installer only owns the region between the markers.

- [ ] **Step 1: Write the failing test**

Append to `merge.rs`:

```rust
#[cfg(test)]
mod claude_md_tests {
    use super::*;

    const BEGIN: &str = "<!-- BEGIN CAIRN PACK MANUAL -->";
    const END: &str = "<!-- END CAIRN PACK MANUAL -->";

    #[test]
    fn injects_into_empty_file() {
        let body = format!("{BEGIN}\nfragment\n{END}");
        let out = inject_block(None, &body).unwrap();
        assert!(out.contains("fragment"));
        assert!(out.contains(BEGIN));
        assert!(out.contains(END));
    }

    #[test]
    fn replaces_existing_block_preserving_surrounding() {
        let existing = format!(
            "# Project\n\nbefore\n\n{BEGIN}\nold fragment\n{END}\n\nafter\n"
        );
        let body = format!("{BEGIN}\nnew fragment\n{END}");
        let out = inject_block(Some(existing), &body).unwrap();
        assert!(out.contains("new fragment"));
        assert!(!out.contains("old fragment"));
        assert!(out.contains("# Project"));
        assert!(out.contains("after"));
    }

    #[test]
    fn appends_block_when_absent() {
        let existing = "# Project\n\nuser stuff\n".to_owned();
        let body = format!("{BEGIN}\nfragment\n{END}");
        let out = inject_block(Some(existing), &body).unwrap();
        assert!(out.contains("user stuff"));
        assert!(out.ends_with("fragment\n<!-- END CAIRN PACK MANUAL -->\n"));
    }
}
```

- [ ] **Step 2: Run tests — expect FAIL (no inject_block)**

Run: `cargo test -p cairn-cli --lib packs::merge::claude_md_tests`
Expected: FAIL.

- [ ] **Step 3: Implement `inject_block`**

Add to `merge.rs`:

```rust
const CLAUDE_MD_BEGIN: &str = "<!-- BEGIN CAIRN PACK MANUAL -->";
const CLAUDE_MD_END: &str = "<!-- END CAIRN PACK MANUAL -->";

/// Inject (or replace) the cairn pack manual block in a `CLAUDE.md` body.
///
/// `block_body` MUST start with [`CLAUDE_MD_BEGIN`] and end with
/// [`CLAUDE_MD_END`]; it is written between those markers verbatim.
///
/// # Errors
/// Returns [`PackError::MergeConflict`] if `block_body` is malformed
/// (missing markers) or the existing file has only one marker.
pub fn inject_block(existing: Option<String>, block_body: &str) -> Result<String, PackError> {
    if !block_body.starts_with(CLAUDE_MD_BEGIN) || !block_body.trim_end().ends_with(CLAUDE_MD_END)
    {
        return Err(PackError::MergeConflict {
            file: "CLAUDE.md".to_owned(),
            reason: "block_body must be wrapped with CAIRN PACK MANUAL markers".to_owned(),
        });
    }
    let normalised_body = block_body.trim_end();
    let Some(existing) = existing else {
        return Ok(format!("{normalised_body}\n"));
    };

    let begin = existing.find(CLAUDE_MD_BEGIN);
    let end = existing.find(CLAUDE_MD_END);
    match (begin, end) {
        (Some(b), Some(e)) if b < e => {
            let end_after = e + CLAUDE_MD_END.len();
            let mut out = String::with_capacity(existing.len());
            out.push_str(&existing[..b]);
            out.push_str(normalised_body);
            out.push_str(&existing[end_after..]);
            Ok(out)
        }
        (None, None) => {
            let separator = if existing.ends_with('\n') { "" } else { "\n" };
            Ok(format!("{existing}{separator}{normalised_body}\n"))
        }
        _ => Err(PackError::MergeConflict {
            file: "CLAUDE.md".to_owned(),
            reason: "existing file has unbalanced CAIRN PACK MANUAL markers".to_owned(),
        }),
    }
}
```

- [ ] **Step 4: Run tests — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::merge::claude_md_tests`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/merge.rs
git commit -m "feat(cairn-cli): pack CLAUDE.md block injection helper (#182)"
```

---

### Task 22: `install.rs` — `PackInstallOpts`, `PackInstallReceipt`, `install_pack`

**Files:**
- Create: `crates/cairn-cli/src/packs/install.rs`
- Modify: `crates/cairn-cli/src/packs/mod.rs`

- [ ] **Step 1: Wire module**

Edit `crates/cairn-cli/src/packs/mod.rs`, add `pub mod install;` after `pub mod merge;`. Also add a `bundled_pack_for` helper:

```rust
use include_dir::Dir;

use self::manifest::Harness;

/// Return the embedded pack content for the given harness.
///
/// # Panics (no — function is total)
/// `Harness` is `non_exhaustive` but only `ClaudeCode` is reachable in v1.
#[must_use]
pub fn bundled_pack_for(harness: Harness) -> &'static Dir<'static> {
    match harness {
        Harness::ClaudeCode => &embed::CAIRN_CLAUDE_CODE_PACK,
    }
}
```

- [ ] **Step 2: Write the failing test**

Create `crates/cairn-cli/src/packs/install.rs` with:

```rust
//! Pack installer: write a validated `PackManifest`'s files into a target
//! project directory.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::packs::manifest::{Harness, PackError, PackManifest};

/// Install options.
#[derive(Debug, Clone)]
pub struct PackInstallOpts {
    /// Harness to install for.
    pub harness: Harness,
    /// Target project directory (will be created if needed).
    pub project_dir: PathBuf,
    /// Overwrite existing files even when they differ.
    pub force: bool,
}

/// Receipt returned by [`install_pack`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PackInstallReceipt {
    /// Pack id installed.
    pub pack_id: String,
    /// Pack version installed.
    pub version: String,
    /// Files created (did not exist before).
    pub files_created: Vec<PathBuf>,
    /// Files merged with existing content.
    pub files_merged: Vec<PathBuf>,
    /// Files skipped (content matched, or skip-due-to-existing without force).
    pub files_skipped: Vec<PathBuf>,
    /// Non-fatal warnings (e.g. missing capabilities).
    pub warnings: Vec<String>,
    /// True if any `requires_capabilities` entry is not advertised locally.
    pub degraded: bool,
}

/// Install the pack for `opts.harness` into `opts.project_dir`.
///
/// # Errors
/// Returns [`PackError`] on validation failure, IO failure, or
/// non-recoverable merge conflict.
pub fn install_pack(opts: &PackInstallOpts) -> Result<PackInstallReceipt, PackError> {
    let dir = crate::packs::bundled_pack_for(opts.harness);

    let manifest_bytes = dir
        .get_file("pack.json")
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "embedded pack missing pack.json".to_owned(),
        })?
        .contents();
    let manifest: PackManifest = serde_json::from_slice(manifest_bytes)?;

    manifest.validate_pass_a()?;
    manifest.validate_pass_b()?;
    manifest.assert_all_paths_present(dir)?;

    if manifest.harness != opts.harness {
        return Err(PackError::HarnessMismatch {
            want: manifest.harness,
            got: opts.harness,
        });
    }

    let mut receipt = PackInstallReceipt {
        pack_id: manifest.pack_id.clone(),
        version: manifest.version.clone(),
        ..Default::default()
    };

    // 1. Subagents → .claude/agents/<id>.md
    for s in &manifest.subagents {
        let bytes = dir.get_file(&s.path).unwrap().contents();
        let target = opts
            .project_dir
            .join(".claude/agents")
            .join(format!("{}.md", s.id));
        write_pack_file(&target, bytes, opts.force, &mut receipt)?;
    }

    // 2. Commands → .claude/commands/<id>.md
    for c in &manifest.commands {
        let bytes = dir.get_file(&c.path).unwrap().contents();
        let target = opts
            .project_dir
            .join(".claude/commands")
            .join(format!("{}.md", c.id));
        write_pack_file(&target, bytes, opts.force, &mut receipt)?;
    }

    // 3. hooks/settings.json → .claude/settings.json (merged).
    let pack_id_at_version = format!("{}@{}", manifest.pack_id, manifest.version);
    let pack_settings_bytes = dir
        .get_file("hooks/settings.json")
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "embedded pack missing hooks/settings.json".to_owned(),
        })?
        .contents();
    let pack_settings: Value = serde_json::from_slice(pack_settings_bytes)?;
    let settings_target = opts.project_dir.join(".claude/settings.json");
    let existing_settings = read_optional_json(&settings_target)?;
    let merged_settings = crate::packs::merge::merge_settings_json(
        existing_settings,
        &pack_settings,
        &pack_id_at_version,
    )?;
    write_json_pretty(&settings_target, &merged_settings, &mut receipt)?;

    // 4. hooks/.mcp.json → project .mcp.json (deep merge of mcpServers).
    if let Some(mcp_file) = dir.get_file("hooks/.mcp.json") {
        let pack_mcp: Value = serde_json::from_slice(mcp_file.contents())?;
        let mcp_target = opts.project_dir.join(".mcp.json");
        let existing_mcp = read_optional_json(&mcp_target)?;
        let merged_mcp = merge_mcp_json(existing_mcp, &pack_mcp, &pack_id_at_version)?;
        write_json_pretty(&mcp_target, &merged_mcp, &mut receipt)?;
    }

    // 5. manual.md → CLAUDE.md (block-injected).
    let manual_bytes = dir.get_file(&manifest.manual_fragment).unwrap().contents();
    let manual_text = std::str::from_utf8(manual_bytes).map_err(|e| PackError::ManifestInvalid {
        reason: format!("manual_fragment is not UTF-8: {e}"),
    })?;
    let claude_md_target = opts.project_dir.join("CLAUDE.md");
    let existing_claude = read_optional_text(&claude_md_target)?;
    let injected = crate::packs::merge::inject_block(existing_claude, manual_text)?;
    write_text(&claude_md_target, &injected, &mut receipt)?;

    // 6. Capability advertise — soft check.
    for cap in &manifest.requires_capabilities {
        if !cairn_core::status::advertise::capability_known(cap) {
            receipt
                .warnings
                .push(format!("capability `{cap}` not advertised — install proceeds, runtime will fail closed"));
            receipt.degraded = true;
        }
    }

    Ok(receipt)
}

fn write_pack_file(
    target: &Path,
    bytes: &[u8],
    force: bool,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    ensure_parent(target)?;
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        if !force {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        std::fs::write(target, bytes)?;
        receipt.files_merged.push(target.to_path_buf());
    } else {
        std::fs::write(target, bytes)?;
        receipt.files_created.push(target.to_path_buf());
    }
    Ok(())
}

fn write_json_pretty(
    target: &Path,
    value: &Value,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    ensure_parent(target)?;
    let pretty = format!("{}\n", serde_json::to_string_pretty(value)?);
    let bytes = pretty.as_bytes();
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        std::fs::write(target, bytes)?;
        receipt.files_merged.push(target.to_path_buf());
    } else {
        std::fs::write(target, bytes)?;
        receipt.files_created.push(target.to_path_buf());
    }
    Ok(())
}

fn write_text(
    target: &Path,
    text: &str,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    ensure_parent(target)?;
    let bytes = text.as_bytes();
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        std::fs::write(target, bytes)?;
        receipt.files_merged.push(target.to_path_buf());
    } else {
        std::fs::write(target, bytes)?;
        receipt.files_created.push(target.to_path_buf());
    }
    Ok(())
}

fn ensure_parent(target: &Path) -> Result<(), PackError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn read_optional_json(path: &Path) -> Result<Value, PackError> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        Ok(value)
    } else {
        Ok(Value::Null)
    }
}

fn read_optional_text(path: &Path) -> Result<Option<String>, PackError> {
    if path.exists() {
        Ok(Some(std::fs::read_to_string(path)?))
    } else {
        Ok(None)
    }
}

fn merge_mcp_json(
    existing: Value,
    pack: &Value,
    pack_id_at_version: &str,
) -> Result<Value, PackError> {
    let mut out = match existing {
        Value::Null => serde_json::json!({}),
        Value::Object(_) => existing,
        other => {
            return Err(PackError::MergeConflict {
                file: ".mcp.json".to_owned(),
                reason: format!("expected object, got {other:?}"),
            });
        }
    };

    let pack_servers = pack
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| PackError::MergeConflict {
            file: ".mcp.json".to_owned(),
            reason: "pack payload missing `mcpServers`".to_owned(),
        })?;

    let out_servers = out
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Default::default()));
    let out_servers = out_servers
        .as_object_mut()
        .ok_or_else(|| PackError::MergeConflict {
            file: ".mcp.json".to_owned(),
            reason: "existing `mcpServers` is not an object".to_owned(),
        })?;
    for (name, server) in pack_servers {
        let mut tagged = server.clone();
        if let Some(obj) = tagged.as_object_mut() {
            obj.insert(
                "_pack".to_owned(),
                Value::String(pack_id_at_version.to_owned()),
            );
        }
        out_servers.insert(name.clone(), tagged);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn opts(dir: &Path) -> PackInstallOpts {
        PackInstallOpts {
            harness: Harness::ClaudeCode,
            project_dir: dir.to_path_buf(),
            force: false,
        }
    }

    #[test]
    fn install_into_empty_dir_creates_expected_files() {
        let tmp = tempdir().unwrap();
        let receipt = install_pack(&opts(tmp.path())).expect("install ok");
        assert_eq!(receipt.pack_id, "cairn-claude-code");
        assert!(tmp.path().join(".claude/agents/context-loader.md").exists());
        assert!(tmp.path().join(".claude/commands/cairn-ingest.md").exists());
        assert!(tmp.path().join(".claude/settings.json").exists());
        assert!(tmp.path().join(".mcp.json").exists());
        assert!(tmp.path().join("CLAUDE.md").exists());
        assert!(!receipt.files_created.is_empty());
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempdir().unwrap();
        let first = install_pack(&opts(tmp.path())).unwrap();
        let second = install_pack(&opts(tmp.path())).unwrap();
        assert!(!first.files_created.is_empty());
        assert!(second.files_created.is_empty(), "second run creates nothing");
        // Every file in the second run should be in skipped (already
        // matching) — no merges either.
        assert!(second.files_merged.is_empty(), "second run merges nothing");
    }

    #[test]
    fn install_preserves_user_claude_md_content() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# Project\n\nuser content\n").unwrap();
        install_pack(&opts(tmp.path())).unwrap();
        let body = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(body.contains("# Project"));
        assert!(body.contains("user content"));
        assert!(body.contains("Cairn (Claude Code reference pack)"));
    }
}
```

- [ ] **Step 3: Run tests — expect PASS**

Run: `cargo test -p cairn-cli --lib packs::install`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/packs/install.rs crates/cairn-cli/src/packs/mod.rs
git commit -m "feat(cairn-cli): pack installer with tempdir + idempotency tests (#182)"
```

---

## Phase 5 — Snapshot suite

### Task 23: Integration test with insta per-file snapshots

**Files:**
- Create: `crates/cairn-cli/tests/claude_code_pack_install.rs`

- [ ] **Step 1: Write the snapshot integration test**

Create `crates/cairn-cli/tests/claude_code_pack_install.rs`:

```rust
//! Per-emitted-file snapshot test for the cairn-claude-code pack install.
//!
//! Asserts that the bundled pack installs into an empty tempdir
//! deterministically. Snapshot files land under
//! `crates/cairn-cli/tests/snapshots/claude_code_pack_install__*.snap`.

use std::path::Path;

use cairn_cli::packs::{
    install::{PackInstallOpts, install_pack},
    manifest::Harness,
};
use tempfile::tempdir;

const EXPECTED_AGENT_IDS: &[&str] = &[
    "context-loader",
    "vault-librarian",
    "forget-planner",
    "consolidator",
    "replay-checker",
    "trace-summarizer",
];

const EXPECTED_COMMAND_IDS: &[&str] = &[
    "cairn-ingest",
    "cairn-search",
    "cairn-retrieve",
    "cairn-summarize",
    "cairn-assemble",
    "cairn-capture-trace",
    "cairn-lint",
    "cairn-forget",
    "cairn-status",
    "cairn-standup",
    "cairn-wrap-up",
    "cairn-audit",
    "cairn-recall",
];

#[test]
fn install_bundled_pack_into_tempdir() {
    let tmp = tempdir().expect("tempdir");
    let opts = PackInstallOpts {
        harness: Harness::ClaudeCode,
        project_dir: tmp.path().to_path_buf(),
        force: false,
    };
    let receipt = install_pack(&opts).expect("install");

    // 1. Receipt envelope snapshot.
    let mut receipt_for_snapshot = receipt.clone();
    receipt_for_snapshot.files_created = strip_tempdir(&receipt.files_created, tmp.path());
    receipt_for_snapshot.files_merged = strip_tempdir(&receipt.files_merged, tmp.path());
    receipt_for_snapshot.files_skipped = strip_tempdir(&receipt.files_skipped, tmp.path());
    insta::assert_json_snapshot!("receipt", receipt_for_snapshot);

    // 2. Subagents.
    for id in EXPECTED_AGENT_IDS {
        let body = std::fs::read_to_string(tmp.path().join(format!(".claude/agents/{id}.md")))
            .unwrap_or_else(|_| panic!("agent {id} present"));
        insta::assert_snapshot!(format!("agent-{id}"), body);
    }

    // 3. Commands.
    for id in EXPECTED_COMMAND_IDS {
        let body = std::fs::read_to_string(tmp.path().join(format!(".claude/commands/{id}.md")))
            .unwrap_or_else(|_| panic!("command {id} present"));
        insta::assert_snapshot!(format!("command-{id}"), body);
    }

    // 4. settings.json (post-merge).
    let settings = std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap();
    insta::assert_snapshot!("settings", settings);

    // 5. .mcp.json (post-merge).
    let mcp = std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap();
    insta::assert_snapshot!("mcp-json", mcp);

    // 6. CLAUDE.md (block-injected).
    let claude_md = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
    insta::assert_snapshot!("claude-md", claude_md);
}

fn strip_tempdir(paths: &[std::path::PathBuf], base: &Path) -> Vec<std::path::PathBuf> {
    paths
        .iter()
        .map(|p| {
            p.strip_prefix(base)
                .map(|sub| std::path::PathBuf::from("./").join(sub))
                .unwrap_or_else(|_| p.clone())
        })
        .collect()
}
```

- [ ] **Step 2: Run test — accept new snapshots**

Run:

```bash
cargo test -p cairn-cli --test claude_code_pack_install -- --nocapture
```

Expected: test fails with "snapshot pending" or "no snapshot" the first time. Then:

```bash
INSTA_UPDATE=always cargo test -p cairn-cli --test claude_code_pack_install
```

Snapshots land under `crates/cairn-cli/tests/snapshots/`. Inspect them:

```bash
git diff --stat crates/cairn-cli/tests/snapshots/
```

- [ ] **Step 3: Run again — expect PASS now**

Run: `cargo test -p cairn-cli --test claude_code_pack_install`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/tests/claude_code_pack_install.rs crates/cairn-cli/tests/snapshots/
git commit -m "test(cairn-cli): insta snapshots for cairn-claude-code pack install (#182)"
```

---

## Phase 6 — Plugin verify integration

### Task 24: Pack-verify cases in `cairn plugins verify`

**Files:**
- Create: `crates/cairn-cli/src/packs/verify.rs`
- Modify: `crates/cairn-cli/src/packs/mod.rs`
- Modify: `crates/cairn-cli/src/plugins/verify.rs` (call pack-verify when `--pack` is supplied or by default)
- Create: `crates/cairn-cli/tests/claude_code_pack_verify.rs`

- [ ] **Step 1: Wire module + write verify.rs**

Add `pub mod verify;` to `crates/cairn-cli/src/packs/mod.rs`.

Create `crates/cairn-cli/src/packs/verify.rs`:

```rust
//! Pack conformance suite, surfaced via `cairn plugins verify --pack <id>`.
//!
//! Tier 1 — manifest schema validity (Pass A + Pass B + path presence).
//! Tier 2 — install round-trip (install into tempdir, compare to embed).
//! Tier 3 — snapshot test (delegated to `tests/claude_code_pack_install.rs`).

use std::path::PathBuf;

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
    /// Case label.
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
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Err(format!("{e:#}")),
            });
            return out;
        }
    };
    out.push(CaseOutcome {
        name: "pack.json parses".to_owned(),
        tier: Tier::One,
        status: Ok(()),
    });

    out.push(CaseOutcome {
        name: "Pass A structural validation".to_owned(),
        tier: Tier::One,
        status: manifest.validate_pass_a().map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        name: "Pass B cross-reference validation".to_owned(),
        tier: Tier::One,
        status: manifest.validate_pass_b().map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
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

/// Render outcomes as JSON (for `--json`) plus a quick human summary.
#[must_use]
pub fn render_outcomes(outcomes: &[CaseOutcome]) -> String {
    let mut s = String::new();
    for o in outcomes {
        s.push_str(&format!(
            "{:?} {:30} {}\n",
            o.tier,
            o.name,
            match &o.status {
                Ok(()) => "OK".to_owned(),
                Err(reason) => format!("FAIL — {reason}"),
            }
        ));
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

    // suppress unused-import warning
    #[allow(dead_code)]
    fn _unused() -> Vec<PathBuf> {
        Vec::new()
    }
}
```

- [ ] **Step 2: Wire `--pack` flag in plugins::verify dispatch**

Edit `crates/cairn-cli/src/plugins/verify.rs`:
- Add a `clap` `--pack <id>` argument (optional, repeatable).
- When `--pack` is supplied, run `crate::packs::verify::run_pack_conformance(id)` for each.
- When not supplied, run BOTH the existing plugin suite AND
  `run_pack_conformance` over every `bundled_pack_ids()`.
- Merge pack outcomes into the existing `PluginReport` table under a
  new `packs` section, OR if simpler, append them to the rendered
  output with a clear header `## Packs`.

Concrete diff sketch (apply to the dispatcher in `verify.rs` and any clap setup in `mod.rs` or `main.rs`):

```rust
// In the run() function or equivalent:
let outcomes_for_packs: Vec<(String, Vec<crate::packs::verify::CaseOutcome>)> =
    crate::packs::verify::bundled_pack_ids()
        .into_iter()
        .map(|id| (id.to_owned(), crate::packs::verify::run_pack_conformance(id)))
        .collect();
// ...later when rendering / exiting:
let mut any_failed = false;
for (id, cases) in &outcomes_for_packs {
    print!("## Pack `{id}`\n{}", crate::packs::verify::render_outcomes(cases));
    if cases.iter().any(|c| c.status.is_err()) {
        any_failed = true;
    }
}
// Combine `any_failed` with the existing plugins-failed flag for exit code.
```

If the existing rendering uses structured `PluginReport`, add a parallel `pub packs: Vec<PackReport>` field and a render branch. The integration test below treats the function-level pack conformance as the source of truth, so the CLI plumbing is a small wiring change.

- [ ] **Step 3: Write integration test**

Create `crates/cairn-cli/tests/claude_code_pack_verify.rs`:

```rust
//! End-to-end test: `cairn plugins verify` should exit 0 with the bundled
//! pack conformance passing.

use cairn_cli::packs::verify::run_pack_conformance;

#[test]
fn bundled_pack_passes_all_cases() {
    let outcomes = run_pack_conformance("cairn-claude-code");
    let failures: Vec<_> = outcomes
        .iter()
        .filter(|o| o.status.is_err())
        .map(|o| format!("{:?} {} — {:?}", o.tier, o.name, o.status))
        .collect();
    assert!(failures.is_empty(), "failures: {failures:?}");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-cli --test claude_code_pack_verify`
Expected: PASS.

Run: `cargo test -p cairn-cli --lib packs::verify`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/verify.rs crates/cairn-cli/src/packs/mod.rs crates/cairn-cli/src/plugins/verify.rs crates/cairn-cli/tests/claude_code_pack_verify.rs
git commit -m "feat(cairn-cli): pack conformance suite + cairn plugins verify integration (#182)"
```

---

## Phase 7 — Migration from inline content in `skill.rs`

### Task 25: Strip inline Claude-Code constants

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Remove inline command consts**

Open `crates/cairn-cli/src/skill.rs`. Find and DELETE:

- `const CLAUDE_REMEMBER_COMMAND: &str = ...`
- `const CLAUDE_FORGET_COMMAND: &str = ...`
- `const CLAUDE_RECALL_COMMAND: &str = ...`
- `const CLAUDE_GRAPH_COMMAND: &str = ...`
- `fn claude_slash_commands() -> ...`

Also delete the `for (name, content) in claude_slash_commands()` loop inside `install_claude_code_integration`.

- [ ] **Step 2: Replace `install_claude_code_integration` body with delegation**

Replace the function (currently ~37 lines from `let mut receipt = ...` through `Ok(receipt)`) with:

```rust
fn install_claude_code_integration(
    project_dir: &std::path::Path,
    force: bool,
) -> Result<AgentIntegrationReceipt, anyhow::Error> {
    let mut receipt = AgentIntegrationReceipt::new(Agent::ClaudeCode);
    let pack_receipt = crate::packs::install::install_pack(&crate::packs::install::PackInstallOpts {
        harness: crate::packs::manifest::Harness::ClaudeCode,
        project_dir: project_dir.to_path_buf(),
        force,
    })
    .map_err(|e| anyhow::anyhow!("install cairn-claude-code pack: {e}"))?;
    receipt.absorb_pack_install(&pack_receipt);
    Ok(receipt)
}
```

Add a small helper to `AgentIntegrationReceipt` impl (same file, near the existing impl block):

```rust
impl AgentIntegrationReceipt {
    /// Fold a [`PackInstallReceipt`] into this harness-receipt's file lists.
    pub fn absorb_pack_install(&mut self, p: &crate::packs::install::PackInstallReceipt) {
        self.files_created
            .extend(p.files_created.iter().cloned());
        self.files_merged.extend(p.files_merged.iter().cloned());
        self.files_skipped.extend(p.files_skipped.iter().cloned());
    }
}
```

(If `AgentIntegrationReceipt` field names differ — e.g. `files_created` vs `created` — match the existing names.)

- [ ] **Step 3: Run impact tests**

Run: `cargo nextest run -p cairn-cli --locked claude_code`
Expected: existing snapshot tests for the inline integration fail because content changed (commands now match pack instead of inline). Re-run with `INSTA_UPDATE=always` to accept the new expected output:

```bash
INSTA_UPDATE=always cargo nextest run -p cairn-cli --locked claude_code
```

Inspect the diff in `crates/cairn-cli/snapshots/` and confirm the new snapshot reflects pack-installed content (13 commands, 6 agents, merged settings, manual fragment).

- [ ] **Step 4: Run full cairn-cli test suite**

Run: `cargo nextest run -p cairn-cli --locked`
Expected: PASS.

- [ ] **Step 5: Verify skill.rs size dropped**

Run: `wc -l crates/cairn-cli/src/skill.rs`
Expected: roughly 200-300 LOC (down from 1645).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/skill.rs crates/cairn-cli/snapshots/
git commit -m "refactor(cairn-cli): migrate Claude-Code integration into cairn-claude-code pack (#182)"
```

---

### Task 26: Confirm core-boundary check still passes

**Files:** (none modified; verification step)

- [ ] **Step 1: Run boundary check**

Run: `./scripts/check-core-boundary.sh`
Expected: PASS (no `cairn-cli` or pack content in `cairn-core` deps).

- [ ] **Step 2: Verify no Claude-Code-specific strings remain in cairn-cli outside packs/**

Run:

```bash
rg -l "claude_code|claude-code|\\.claude/" crates/cairn-cli/src/ \
  | grep -v packs/
```

Expected: very small list — `skill.rs` (for the other harnesses' integration paths), `hooks/`, possibly `main.rs` for the CLI subcommand. No inline Claude-Code content; the pack runtime is the canonical source.

- [ ] **Step 3: Commit (if any cleanup needed; otherwise skip)**

```bash
git status
git commit --allow-empty -m "chore: confirm core-boundary clean after pack migration (#182)"
```

(Only commit if there's a cleanup to record; an `--allow-empty` checkpoint is optional.)

---

## Phase 8 — Docs

### Task 27: `cairn-docgen` pack reference page

**Files:**
- Modify: `crates/cairn-cli/src/docgen.rs`
- Add: `docs/site/src/reference/generated/packs/cairn-claude-code.md` (generated, committed)

- [ ] **Step 1: Add a pack-docgen renderer**

Inspect existing `crates/cairn-cli/src/docgen.rs`. Add a new function:

```rust
fn render_pack_reference(manifest: &crate::packs::manifest::PackManifest) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Pack: `{}`\n\n", manifest.pack_id));
    s.push_str(&format!("Version: `{}`  \nHarness: `{:?}`  \nMCP compat: `{}`\n\n",
        manifest.version, manifest.harness, manifest.cairn_mcp_compat));
    s.push_str(&format!("{}\n\n", manifest.description));
    s.push_str("## Subagents\n\n| id | MCP tools |\n|---|---|\n");
    for a in &manifest.subagents {
        s.push_str(&format!("| `{}` | {} |\n", a.id, a.uses_mcp_tools.join(", ")));
    }
    s.push_str("\n## Slash commands\n\n| id | kind | verb |\n|---|---|---|\n");
    for c in &manifest.commands {
        let kind = match c.kind {
            crate::packs::manifest::CommandKind::VerbDirect => "verb-direct",
            crate::packs::manifest::CommandKind::Workflow => "workflow",
        };
        s.push_str(&format!(
            "| `{}` | {} | {} |\n",
            c.id,
            kind,
            c.verb.as_deref().unwrap_or("—")
        ));
    }
    s.push_str("\n## Hook bindings\n\n| event | command |\n|---|---|\n");
    for (event, binding) in &manifest.hooks {
        s.push_str(&format!("| `{}` | `{}` |\n", event, binding.command));
    }
    s.push_str("\n## Required capabilities\n\n");
    for cap in &manifest.requires_capabilities {
        s.push_str(&format!("- `{cap}`\n"));
    }
    s
}
```

Wire it into the docgen entry-point: where docgen iterates plugins, add a parallel loop over `crate::packs::verify::bundled_pack_ids()`, fetch the manifest, render via `render_pack_reference`, write to `docs/site/src/reference/generated/packs/<pack_id>.md`.

- [ ] **Step 2: Run docgen `--write`**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
```

This emits the generated markdown under `docs/site/src/reference/generated/packs/cairn-claude-code.md`.

- [ ] **Step 3: Verify `--check` mode passes**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Expected: success — generated output already on disk.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/docgen.rs docs/site/src/reference/generated/packs/
git commit -m "docs: cairn-docgen renders cairn-claude-code pack reference (#182)"
```

---

### Task 28: Traceability map entry

**Files:**
- Modify: `docs/design/traceability.md`

- [ ] **Step 1: Locate the right section in the traceability map**

Run:

```bash
grep -n "182\|#19\|harness pack\|skill pack" docs/design/traceability.md | head -20
```

Find the section indexing parent #19 or harness packs.

- [ ] **Step 2: Add a row mapping the cairn-pack/v1 schema introduction**

Insert into the appropriate table:

```
| Brief §8 (CLI ground truth) | #182 | cairn-pack/v1 manifest schema; reference Claude Code pack |
| CLAUDE.md §4 invariant 1    | #182 | harness-pack content moved out of cairn-cli/src/skill.rs into packs/cairn-claude-code/ |
```

(Match existing column layout — read the surrounding rows.)

- [ ] **Step 3: Commit**

```bash
git add docs/design/traceability.md
git commit -m "docs(traceability): map cairn-pack/v1 + cairn-claude-code (#182)"
```

---

## Phase 9 — Full verification

### Task 29: Run the §8 verification checklist

**Files:** (none — verification only)

- [ ] **Step 1: Format check**

Run: `cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Workspace check**

Run: `cargo check --workspace --all-targets --locked`
Expected: PASS.

- [ ] **Step 4: nextest**

Run: `cargo nextest run --workspace --locked --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Doctests**

Run: `cargo test --doc --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Core boundary**

Run: `./scripts/check-core-boundary.sh`
Expected: PASS.

- [ ] **Step 7: Docgen check**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
Expected: PASS.

- [ ] **Step 8: Plugin verify**

Run: `cargo run -p cairn-cli --locked -- plugins verify`
Expected: pass — all bundled plugins + the bundled `cairn-claude-code` pack OK.

- [ ] **Step 9: Manual smoke test of skill install**

Run:

```bash
TMP=$(mktemp -d)
cargo run -p cairn-cli --locked -- skill install --harness claude-code --target "$TMP"
ls -R "$TMP/.claude/"
cat "$TMP/CLAUDE.md"
```

Expected: `.claude/agents/` has 6 files; `.claude/commands/` has 13 files; `.claude/settings.json` has 5 hook events with `_pack` markers; `.mcp.json` has the cairn server; `CLAUDE.md` has the Cairn pack manual block.

- [ ] **Step 10: No further commits expected**

Run: `git status`
Expected: clean working tree (all earlier commits already landed).

---

### Task 30: Open the PR

**Files:** (none — PR open)

- [ ] **Step 1: Confirm commit log is clean**

Run: `git log --oneline main..HEAD`
Expected: a focused commit series, one per task, all referencing `#182` in the subject.

- [ ] **Step 2: Push the branch**

Run: `git push -u origin HEAD`

- [ ] **Step 3: Create PR using `gh pr create`**

```bash
gh pr create --title "feat(packs): cairn-claude-code reference skill-pack (#182)" --body "$(cat <<'EOF'
## Summary

- Ships `packs/cairn-claude-code/`: 6 subagents (MCP-only), 13 slash commands (9 verb-direct + 4 workflow), 5-event hook bindings, manual fragment, dogfood fixture vault.
- Introduces `cairn-pack/v1` manifest schema for harness packs (distinct from skillify `SkillPackManifest`).
- Adds generic pack runtime in `crates/cairn-cli/src/packs/` (embed, validate, install, verify).
- Migrates Claude-Code-specific content out of `crates/cairn-cli/src/skill.rs` into the pack — restores the harness-agnostic invariant (CLAUDE.md §4 invariant 1).
- Adds insta snapshots for every emitted file and integrates pack conformance into `cairn plugins verify`.

Closes #182.

## Test plan

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo nextest run --workspace --locked --no-fail-fast`
- [ ] `cargo test --doc --workspace --locked`
- [ ] `./scripts/check-core-boundary.sh`
- [ ] `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
- [ ] `cargo run -p cairn-cli --locked -- plugins verify` — bundled pack passes
- [ ] Manual: `cairn skill install --harness claude-code --target <tmp>` writes expected files
- [ ] Dogfood acceptance checklist (`packs/cairn-claude-code/ACCEPTANCE.md`) against the fixture vault
EOF
)"
```

- [ ] **Step 4: Capture PR URL in conversation**

Run: `gh pr view --json url -q .url`

---

## Self-Review

Walking through the spec section by section to confirm coverage.

| Spec section | Plan task |
|---|---|
| §3 Layout (`packs/cairn-claude-code/` content) | T2 (scaffold), T6–T17 (content) |
| §3 Layout (`crates/cairn-cli/src/packs/`) | T3 (skeleton), T4–T5 (manifest), T18–T19 (Pass B + presence), T20–T21 (merge), T22 (install), T24 (verify) |
| §3 Boundary rules | T2, T3 (rule 1, 2); T7-T17 (no Rust in pack content); T25 (rule 3, 4 — migration restores invariant) |
| §4 Manifest schema + invariants 1-11 | T4 (types), T5 (Pass A: 1-6, 9, 11), T18 (Pass B: 7, 8, 10), T19 (path presence: 6 cont.) |
| §4.1 No SkillPackManifest reuse | Documented in spec; plan reflects (separate `PackManifest` type). |
| §5.1 `include_dir!` embed | T3 |
| §5.2 Manifest loading + validation | T4, T5, T18, T19 |
| §5.3 Install algorithm | T20 (settings merge), T21 (CLAUDE.md block), T22 (install) |
| §5.4 Verify integration | T24 |
| §6.1 Subagent shape | T7–T12 (one task per subagent) |
| §6.2 Verb-direct command shape | T13 (all 9) |
| §6.3 Workflow command shape | T14 (all 4) |
| §6.4 Hook bindings | T15 |
| §6.5 Manual fragment | T16 |
| §7 Migration from `skill.rs` | T25 |
| §8 Error handling | Wired through `PackError` in T4, used throughout. Exit-code mapping inherits from existing `cairn-cli` infrastructure. |
| §9 Test plan (5 layers) | T5 (unit Pass A), T18 (unit Pass B), T22 (unit install), T23 (integration snapshot), T24 (conformance) |
| §10 Dogfood fixture | T17 |
| §11 Acceptance checklist | T16 (ships ACCEPTANCE.md), T29 step 9 (manual smoke) |
| §12 Verification commands | T29 |
| §13 Risks | Per-risk mitigations land in earlier tasks (embed path check in T3 step 3; merge complexity in T20; pack version in T6; subagent allowlist in T18; cassette in T17/T11) |

Type consistency: `PackManifest`, `PackError`, `PackInstallOpts`, `PackInstallReceipt`, `Harness`, `CommandKind`, `SubagentDecl`, `CommandDecl`, `HookBinding` used consistently from T4 onward. Function names (`validate_pass_a`, `validate_pass_b`, `assert_all_paths_present`, `install_pack`, `merge_settings_json`, `inject_block`, `run_pack_conformance`, `bundled_pack_for`, `bundled_pack_ids`, `render_outcomes`) are reused in later tasks consistent with their definitions.

Placeholder scan: one `--days N` example argument-hint and one "NOTE FOR IMPLEMENTER" inline note in T18 directing the implementer to find or create `capability_known`. That note is intentional and bounded — it names exact files to inspect and the one-line helper to write if missing.

Spec coverage: complete.
