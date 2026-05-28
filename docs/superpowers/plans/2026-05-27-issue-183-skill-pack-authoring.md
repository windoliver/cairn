# Skill-Pack Authoring Scaffold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issue #183 by adding a public skill-pack authoring guide, reference scaffold templates, `cairn skill new`, and path-based pack verification for external authors.

**Architecture:** Keep harness-pack behavior in `cairn-cli::packs`; do not touch `cairn-core`. Refactor bundled-pack verification and install to work against a small pack-source abstraction so both embedded packs and filesystem scaffolds use the same validator. Add fixed-token template rendering for `claude-code`, `codex`, and `gemini`, then expose it through `cairn skill new`.

**Tech Stack:** Rust 1.95, clap, serde/serde_json, include_dir, tempfile, assert_cmd, predicates, shell scripts for generated scaffold smoke tests, Markdown docs.

---

## File Structure

### New Files

| Path | Responsibility |
|---|---|
| `crates/cairn-cli/src/packs/source.rs` | Trait and implementations for reading pack files from either `include_dir::Dir` or a filesystem directory. |
| `crates/cairn-cli/src/packs/template.rs` | Fixed-token scaffold renderer, template harness enum, scaffold receipt, output-directory safety checks. |
| `crates/cairn-cli/tests/skill_new.rs` | End-to-end CLI tests for `cairn skill new` and scaffold verification. |
| `crates/cairn-cli/tests/plugins_verify_pack_path.rs` | End-to-end CLI tests for `cairn plugins verify --pack-path`. |
| `docs/skill-pack-authoring.md` | Author-facing guide with stable anchors and CI instructions. |
| `packs/templates/claude-code/pack.json.template` | Claude Code starter manifest. |
| `packs/templates/claude-code/manual.md.template` | Claude Code operating-manual fragment. |
| `packs/templates/claude-code/agents/context-loader.md.template` | Claude Code starter subagent. |
| `packs/templates/claude-code/commands/cairn-context.md.template` | Claude Code starter slash command. |
| `packs/templates/claude-code/hooks/settings.json.template` | Claude Code hook settings. |
| `packs/templates/claude-code/tests/smoke.sh.template` | Claude Code nonrecursive smoke script. |
| `packs/templates/claude-code/.github/workflows/verify.yml.template` | Claude Code copyable CI workflow. |
| `packs/templates/codex/pack.json.template` | Codex starter manifest. |
| `packs/templates/codex/AGENTS.md.template` | Codex operating-manual fragment. |
| `packs/templates/codex/agents/context-loader.md.template` | Codex starter subagent. |
| `packs/templates/codex/commands/cairn-context.md.template` | Codex starter slash command. |
| `packs/templates/codex/hooks/hooks.json.template` | Codex hook settings. |
| `packs/templates/codex/tests/smoke.sh.template` | Codex nonrecursive smoke script. |
| `packs/templates/codex/.github/workflows/verify.yml.template` | Codex copyable CI workflow. |
| `packs/templates/gemini/pack.json.template` | Gemini starter manifest. |
| `packs/templates/gemini/GEMINI.md.template` | Gemini operating-manual fragment. |
| `packs/templates/gemini/agents/context-loader.md.template` | Gemini starter subagent. |
| `packs/templates/gemini/commands/cairn-context.md.template` | Gemini starter slash command. |
| `packs/templates/gemini/hooks/hooks.json.template` | Gemini hook settings. |
| `packs/templates/gemini/tests/smoke.sh.template` | Gemini nonrecursive smoke script. |
| `packs/templates/gemini/.github/workflows/verify.yml.template` | Gemini copyable CI workflow. |

### Modified Files

| Path | Responsibility |
|---|---|
| `crates/cairn-cli/src/packs/mod.rs` | Export `source` and `template`; keep bundled registry behavior for Claude Code. |
| `crates/cairn-cli/src/packs/manifest.rs` | Add `Codex` and `Gemini` harness variants; make path/frontmatter checks read through `PackSource`. |
| `crates/cairn-cli/src/packs/install.rs` | Add `install_pack_from_source()` and harness-aware install destinations. Preserve existing `install_pack()` wrapper. |
| `crates/cairn-cli/src/packs/verify.rs` | Add `run_pack_path_conformance()` and shared source-based verifier. |
| `crates/cairn-cli/src/plugins/verify.rs` | Add pack-path report helper used by CLI dispatch. |
| `crates/cairn-cli/src/command.rs` | Add `skill new` subcommand and `plugins verify --pack-path`. |
| `crates/cairn-cli/src/main.rs` | Dispatch `skill new` and pack-path verification. |
| `crates/cairn-cli/src/skill.rs` | Add scaffold option and receipt types, render function, and human output. |

---

## Task 1: PackSource Abstraction and Harness Enum

**Files:**
- Create: `crates/cairn-cli/src/packs/source.rs`
- Modify: `crates/cairn-cli/src/packs/mod.rs`
- Modify: `crates/cairn-cli/src/packs/manifest.rs`

- [ ] **Step 1: Write failing PackSource tests**

Append this test module to the new `crates/cairn-cli/src/packs/source.rs` file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_source_reads_pack_relative_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("pack.json"), br#"{"ok":true}"#).expect("write pack");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        assert!(source.has_file("pack.json"));
        assert_eq!(source.read_file("pack.json").expect("read"), br#"{"ok":true}"#);
        assert_eq!(source.label(), tmp.path().display().to_string());
    }

    #[test]
    fn fs_source_rejects_escaping_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        let err = source.read_file("../pack.json").expect_err("escape rejected");
        assert!(
            err.to_string().contains("escapes pack root"),
            "unexpected error: {err}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli packs::source
```

Expected: FAIL because `FsPackSource`, `PackSource`, and `source` module do not exist.

- [ ] **Step 3: Implement `PackSource`**

Create `crates/cairn-cli/src/packs/source.rs` with:

```rust
//! Pack file sources for embedded and filesystem-backed cairn-pack/v1 directories.

use std::path::{Component, Path, PathBuf};

use include_dir::Dir;

use crate::packs::manifest::PackError;

/// Read-only source of pack files addressed by pack-relative paths.
pub trait PackSource {
    /// Human-readable source label for diagnostics.
    fn label(&self) -> String;

    /// Return true if `path` exists as a regular file in this source.
    fn has_file(&self, path: &str) -> bool;

    /// Read a pack-relative file into memory.
    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError>;
}

/// Embedded pack source backed by `include_dir`.
pub struct EmbeddedPackSource {
    label: &'static str,
    dir: &'static Dir<'static>,
}

impl EmbeddedPackSource {
    /// Build an embedded source from a bundled pack directory.
    #[must_use]
    pub const fn new(label: &'static str, dir: &'static Dir<'static>) -> Self {
        Self { label, dir }
    }
}

impl PackSource for EmbeddedPackSource {
    fn label(&self) -> String {
        self.label.to_owned()
    }

    fn has_file(&self, path: &str) -> bool {
        self.dir.get_file(path).is_some()
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError> {
        self.dir
            .get_file(path)
            .map(|file| file.contents().to_vec())
            .ok_or_else(|| PackError::ManifestInvalid {
                reason: format!("pack file `{path}` missing from {}", self.label),
            })
    }
}

/// Filesystem pack source rooted at an author-provided directory.
pub struct FsPackSource {
    root: PathBuf,
}

impl FsPackSource {
    /// Build a filesystem source rooted at `root`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, PackError> {
        reject_unsafe_pack_path(path)?;
        Ok(self.root.join(path))
    }
}

impl PackSource for FsPackSource {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn has_file(&self, path: &str) -> bool {
        self.resolve(path).is_ok_and(|p| p.is_file())
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, PackError> {
        let resolved = self.resolve(path)?;
        std::fs::read(&resolved).map_err(PackError::Io)
    }
}

fn reject_unsafe_pack_path(path: &str) -> Result<(), PackError> {
    let p = Path::new(path);
    if path.is_empty() || p.is_absolute() {
        return Err(PackError::ManifestInvalid {
            reason: format!("path `{path}` escapes pack root"),
        });
    }
    for component in p.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PackError::ManifestInvalid {
                    reason: format!("path `{path}` escapes pack root"),
                });
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Export the module**

Modify `crates/cairn-cli/src/packs/mod.rs`:

```rust
pub mod embed;
pub mod install;
pub mod manifest;
pub mod merge;
pub mod source;
pub mod verify;
```

- [ ] **Step 5: Extend harness enum**

Modify `crates/cairn-cli/src/packs/manifest.rs` so `Harness` is:

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    /// Claude Code (the canonical reference harness).
    ClaudeCode,
    /// Codex harness.
    Codex,
    /// Gemini CLI harness.
    Gemini,
}
```

Modify `crates/cairn-cli/src/packs/mod.rs` so the bundled registry stays Claude Code only:

```rust
#[must_use]
pub fn bundled_pack_for(harness: Harness) -> Option<&'static Dir<'static>> {
    match harness {
        Harness::ClaudeCode => Some(&embed::CAIRN_CLAUDE_CODE_PACK),
        Harness::Codex | Harness::Gemini => None,
    }
}
```

Update existing callers in `install.rs`, `verify.rs`, `docgen.rs`, and tests to unwrap this option with a clear `PackError::ManifestInvalid` when a bundled pack is requested for an unbundled harness.

- [ ] **Step 6: Convert manifest path readers to PackSource**

In `crates/cairn-cli/src/packs/manifest.rs`, change:

```rust
pub fn assert_all_paths_present(&self, dir: &include_dir::Dir<'_>) -> Result<(), PackError>
```

to:

```rust
pub fn assert_all_paths_present<S: crate::packs::source::PackSource + ?Sized>(
    &self,
    source: &S,
) -> Result<(), PackError>
```

Inside it, replace `dir.get_file(path).is_none()` with `!source.has_file(path)`.

Change:

```rust
pub fn assert_subagent_frontmatter_matches_manifest(
    &self,
    dir: &include_dir::Dir<'_>,
) -> Result<(), PackError>
```

to:

```rust
pub fn assert_subagent_frontmatter_matches_manifest<
    S: crate::packs::source::PackSource + ?Sized,
>(
    &self,
    source: &S,
) -> Result<(), PackError>
```

Inside it, replace the existing `dir.get_file` contents-reading block with:

```rust
let bytes = source.read_file(&s.path)?;
let text = std::str::from_utf8(&bytes).map_err(|e| PackError::ManifestInvalid {
    reason: format!("subagent `{}` file is not UTF-8: {e}", s.id),
})?;
```

- [ ] **Step 7: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli packs::source
cargo test --locked -p cairn-cli packs::manifest
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-cli/src/packs/source.rs crates/cairn-cli/src/packs/mod.rs crates/cairn-cli/src/packs/manifest.rs crates/cairn-cli/src/docgen.rs crates/cairn-cli/src/packs/install.rs crates/cairn-cli/src/packs/verify.rs crates/cairn-cli/tests/claude_code_pack_install.rs
git commit -m "feat(packs): add filesystem pack source abstraction (#183)"
```

---

## Task 2: External Pack Verification Path

**Files:**
- Modify: `crates/cairn-cli/src/packs/verify.rs`
- Modify: `crates/cairn-cli/src/plugins/verify.rs`
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Create: `crates/cairn-cli/tests/plugins_verify_pack_path.rs`

- [ ] **Step 1: Write failing CLI test for `--pack-path` JSON shape**

Create `crates/cairn-cli/tests/plugins_verify_pack_path.rs`:

```rust
use assert_cmd::Command;

fn write_minimal_pack(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("agents")).expect("agents");
    std::fs::create_dir_all(root.join("commands")).expect("commands");
    std::fs::write(
        root.join("pack.json"),
        r#"{
  "schema": "cairn-pack/v1",
  "pack_id": "sample-pack",
  "name": "sample-pack",
  "version": "0.1.0",
  "harness": "codex",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Sample pack.",
  "requires_capabilities": ["cairn.mcp.v1.search.keyword"],
  "subagents": [
    {
      "id": "context-loader",
      "path": "agents/context-loader.md",
      "uses_mcp_tools": ["search"]
    }
  ],
  "commands": [
    {
      "id": "cairn-context",
      "path": "commands/cairn-context.md",
      "kind": "verb-direct",
      "verb": "search"
    }
  ],
  "hooks": {
    "SessionStart": { "command": "cairn hook SessionStart" }
  },
  "manual_fragment": "AGENTS.md"
}
"#,
    )
    .expect("pack manifest");
    std::fs::write(
        root.join("agents/context-loader.md"),
        "# Context Loader\n\nUse `mcp__cairn__search` for context lookup.\n",
    )
    .expect("agent");
    std::fs::write(
        root.join("commands/cairn-context.md"),
        "# cairn-context\n\nRun `cairn search \"$1\" --json`.\n",
    )
    .expect("command");
    std::fs::write(
        root.join("AGENTS.md"),
        "<!-- BEGIN CAIRN PACK sample-pack -->\nUse Cairn context.\n<!-- END CAIRN PACK sample-pack -->\n",
    )
    .expect("manual");
}

#[test]
fn plugins_verify_pack_path_json_reports_pack_contract() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_minimal_pack(tmp.path());

    let output = Command::cargo_bin("cairn")
        .expect("binary")
        .args(["plugins", "verify", "--pack-path"])
        .arg(tmp.path())
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(json["summary"]["failed"], 0, "report: {json}");
    assert_eq!(json["plugins"][0]["name"], "sample-pack");
    assert_eq!(json["plugins"][0]["contract"], "pack");
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli --test plugins_verify_pack_path plugins_verify_pack_path_json_reports_pack_contract
```

Expected: FAIL because clap does not know `--pack-path`.

- [ ] **Step 3: Add `--pack-path` to clap**

Modify the `plugins verify` subcommand in `crates/cairn-cli/src/command.rs`:

```rust
.subcommand(
    clap::Command::new("verify")
        .about("Run the conformance suite against every loaded plugin")
        .arg(
            clap::Arg::new("pack-path")
                .long("pack-path")
                .value_name("DIR")
                .value_parser(clap::value_parser!(std::path::PathBuf))
                .help("Verify an external cairn-pack/v1 directory instead of bundled plugins"),
        )
        .arg(
            clap::Arg::new("strict")
                .long("strict")
                .action(clap::ArgAction::SetTrue)
                .help("Treat tier-2 `pending` cases as failures"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON instead of a human-readable report"),
        ),
)
```

- [ ] **Step 4: Add source-based verifier entry points**

In `crates/cairn-cli/src/packs/verify.rs`, add imports:

```rust
use std::path::Path;

use crate::packs::source::{EmbeddedPackSource, FsPackSource, PackSource};
```

Add:

```rust
#[must_use]
pub fn run_pack_path_conformance(path: &Path) -> Vec<CaseOutcome> {
    let source = FsPackSource::new(path.to_path_buf());
    run_pack_source_conformance(&source)
}

#[must_use]
pub fn run_pack_source_conformance(source: &dyn PackSource) -> Vec<CaseOutcome> {
    let mut out = Vec::new();
    let manifest = source
        .read_file("pack.json")
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice::<PackManifest>(&bytes).map_err(|e| e.to_string()));

    let manifest = match manifest {
        Ok(m) => m,
        Err(e) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: format!("pack.json parses from {}", source.label()),
                tier: Tier::One,
                status: Err(e),
            });
            return out;
        }
    };

    out.push(CaseOutcome {
        id: "pack_json_parses",
        name: format!("pack.json parses from {}", source.label()),
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
        status: manifest.assert_all_paths_present(source).map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        id: "pack_harness_static_checks",
        name: "harness-specific static checks".to_owned(),
        tier: Tier::One,
        status: run_harness_static_checks(&manifest, source).map_err(|e| format!("{e:#}")),
    });
    out
}

fn run_harness_static_checks(
    manifest: &PackManifest,
    source: &dyn PackSource,
) -> Result<(), PackError> {
    match manifest.harness {
        Harness::ClaudeCode => manifest.assert_subagent_frontmatter_matches_manifest(source),
        Harness::Codex => assert_manual_and_hook_json(manifest, source, "AGENTS.md", "hooks/hooks.json"),
        Harness::Gemini => assert_manual_and_hook_json(manifest, source, "GEMINI.md", "hooks/hooks.json"),
    }
}

fn assert_manual_and_hook_json(
    manifest: &PackManifest,
    source: &dyn PackSource,
    expected_manual: &str,
    hook_path: &str,
) -> Result<(), PackError> {
    if manifest.manual_fragment != expected_manual {
        return Err(PackError::ManifestInvalid {
            reason: format!(
                "{} pack manual_fragment must be `{expected_manual}`",
                format!("{:?}", manifest.harness)
            ),
        });
    }
    let manual = source.read_file(expected_manual)?;
    let manual = std::str::from_utf8(&manual).map_err(|e| PackError::ManifestInvalid {
        reason: format!("{expected_manual} is not UTF-8: {e}"),
    })?;
    let begin = format!("<!-- BEGIN CAIRN PACK {} -->", manifest.pack_id);
    let end = format!("<!-- END CAIRN PACK {} -->", manifest.pack_id);
    if !manual.contains(&begin) || !manual.contains(&end) {
        return Err(PackError::ManifestInvalid {
            reason: format!("{expected_manual} missing guarded pack block for {}", manifest.pack_id),
        });
    }
    let hook = source.read_file(hook_path)?;
    serde_json::from_slice::<serde_json::Value>(&hook).map_err(PackError::Json)?;
    Ok(())
}
```

Update `run_pack_conformance()` to use:

```rust
let dir = crate::packs::bundled_pack_for(Harness::ClaudeCode).expect("Claude Code pack bundled");
let source = EmbeddedPackSource::new("cairn-claude-code", dir);
let mut out = run_pack_source_conformance(&source);
```

Keep the existing `pack_install_round_trip` case for bundled packs until Task 3 generalizes it.

- [ ] **Step 5: Add plugin report helper for pack paths**

In `crates/cairn-cli/src/plugins/verify.rs`, add:

```rust
#[must_use]
pub fn run_pack_path(path: &std::path::Path) -> VerifyReport {
    let outcomes = crate::packs::verify::run_pack_path_conformance(path);
    let name = outcomes
        .iter()
        .find(|case| case.id == "pack_json_parses" && case.status.is_ok())
        .map_or_else(|| path.display().to_string(), |_| pack_name_from_path(path));
    report_from_pack_outcomes(name, outcomes)
}

fn pack_name_from_path(path: &std::path::Path) -> String {
    let manifest_path = path.join("pack.json");
    std::fs::read(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::packs::manifest::PackManifest>(&bytes).ok())
        .map_or_else(|| path.display().to_string(), |manifest| manifest.pack_id)
}

fn report_from_pack_outcomes(name: String, outcomes: Vec<crate::packs::verify::CaseOutcome>) -> VerifyReport {
    let mut summary = Summary::default();
    let mut cases = Vec::with_capacity(outcomes.len());
    for outcome in outcomes {
        let tier = match outcome.tier {
            crate::packs::verify::Tier::One => Tier::One,
            crate::packs::verify::Tier::Two | crate::packs::verify::Tier::Three => Tier::Two,
        };
        let status = match outcome.status {
            Ok(()) => {
                summary.ok += 1;
                CaseStatus::Ok
            }
            Err(message) => {
                summary.failed += 1;
                CaseStatus::Failed { message }
            }
        };
        cases.push(CaseOutcome {
            id: outcome.id,
            tier,
            status,
        });
    }
    VerifyReport {
        plugins: vec![PluginReport {
            name,
            contract: "pack".to_owned(),
            cases,
        }],
        summary,
    }
}
```

Then refactor the bundled-pack block in `run()` to call `report_from_pack_outcomes()` and append its single plugin report into the normal report.

- [ ] **Step 6: Dispatch pack-path verification**

In `run_plugins()` in `crates/cairn-cli/src/main.rs`, change the `verify` branch:

```rust
Some(("verify", sub)) => {
    let strict = sub.get_flag("strict");
    let json = sub.get_flag("json");
    let report = if let Some(path) = sub.get_one::<std::path::PathBuf>("pack-path") {
        plugins::verify::run_pack_path(path)
    } else {
        plugins::verify::run(&registry)
    };
    let text = if json {
        plugins::verify::render_json(&report)
    } else {
        plugins::verify::render_human(&report)
    };
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", text.trim_end_matches('\n'));
    ExitCode::from(plugins::verify::exit_code(&report, strict))
}
```

- [ ] **Step 7: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli --test plugins_verify_pack_path plugins_verify_pack_path_json_reports_pack_contract
cargo test --locked -p cairn-cli packs::verify
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-cli/src/packs/verify.rs crates/cairn-cli/src/plugins/verify.rs crates/cairn-cli/src/command.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/plugins_verify_pack_path.rs
git commit -m "feat(packs): verify external pack paths (#183)"
```

---

## Task 3: Source-Based Install Round-Trip

**Files:**
- Modify: `crates/cairn-cli/src/packs/install.rs`
- Modify: `crates/cairn-cli/src/packs/verify.rs`
- Modify: `crates/cairn-cli/tests/plugins_verify_pack_path.rs`

- [ ] **Step 1: Add failing install-round-trip assertion to external verifier test**

In `crates/cairn-cli/tests/plugins_verify_pack_path.rs`, extend `write_minimal_pack()` to create hooks:

```rust
std::fs::create_dir_all(root.join("hooks")).expect("hooks");
std::fs::write(
    root.join("hooks/hooks.json"),
    r#"{
  "hooks": {
    "SessionStart": [
      {
        "type": "command",
        "command": "cairn hook SessionStart"
      }
    ]
  }
}
"#,
)
.expect("hooks");
```

Add this assertion to `plugins_verify_pack_path_json_reports_pack_contract()`:

```rust
let cases = json["plugins"][0]["cases"].as_array().expect("cases");
assert!(
    cases
        .iter()
        .any(|case| case["id"] == "pack_install_round_trip" && case["status"] == "ok"),
    "install round-trip missing or failed: {json}"
);
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli --test plugins_verify_pack_path plugins_verify_pack_path_json_reports_pack_contract
```

Expected: FAIL because path verification does not emit `pack_install_round_trip`.

- [ ] **Step 3: Add source install entry point**

In `crates/cairn-cli/src/packs/install.rs`, add:

```rust
use crate::packs::source::{EmbeddedPackSource, PackSource};
```

Change `install_pack()` to a wrapper:

```rust
pub fn install_pack(opts: &PackInstallOpts) -> Result<PackInstallReceipt, PackError> {
    let dir = crate::packs::bundled_pack_for(opts.harness).ok_or_else(|| PackError::ManifestInvalid {
        reason: format!("no bundled pack registered for {:?}", opts.harness),
    })?;
    let source = EmbeddedPackSource::new("bundled pack", dir);
    install_pack_from_source(&source, opts)
}
```

Move the existing install body into:

```rust
pub fn install_pack_from_source(
    source: &dyn PackSource,
    opts: &PackInstallOpts,
) -> Result<PackInstallReceipt, PackError> {
    let manifest_bytes = source.read_file("pack.json")?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)?;

    manifest.validate_pass_a()?;
    manifest.validate_pass_b()?;
    manifest.assert_all_paths_present(source)?;
    if manifest.harness == Harness::ClaudeCode {
        manifest.assert_subagent_frontmatter_matches_manifest(source)?;
    }
    if manifest.harness != opts.harness {
        return Err(PackError::HarnessMismatch {
            want: format!("{:?}", manifest.harness),
            got: format!("{:?}", opts.harness),
        });
    }

    let mut receipt = PackInstallReceipt {
        pack_id: manifest.pack_id.clone(),
        version: manifest.version.clone(),
        ..Default::default()
    };

    install_subagents(source, &manifest, opts, &mut receipt)?;
    install_commands(source, &manifest, opts, &mut receipt)?;
    install_hooks(source, &manifest, opts, &mut receipt)?;
    install_manual(source, &manifest, opts, &mut receipt)?;
    Ok(receipt)
}
```

Add helper functions in the same file:

```rust
fn install_subagents(
    source: &dyn PackSource,
    manifest: &PackManifest,
    opts: &PackInstallOpts,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    for subagent in &manifest.subagents {
        let bytes = source.read_file(&subagent.path)?;
        let target = opts.project_dir.join(harness_agent_dir(manifest.harness)).join(format!("{}.md", subagent.id));
        write_pack_file(&opts.project_dir, &target, &bytes, opts.force, receipt)?;
    }
    Ok(())
}

fn install_commands(
    source: &dyn PackSource,
    manifest: &PackManifest,
    opts: &PackInstallOpts,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    for command in &manifest.commands {
        let bytes = source.read_file(&command.path)?;
        let target = opts.project_dir.join(harness_command_dir(manifest.harness)).join(format!("{}.md", command.id));
        write_pack_file(&opts.project_dir, &target, &bytes, opts.force, receipt)?;
    }
    Ok(())
}

fn install_hooks(
    source: &dyn PackSource,
    manifest: &PackManifest,
    opts: &PackInstallOpts,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    match manifest.harness {
        Harness::ClaudeCode => {
            install_hook_payloads(
                source,
                &opts.project_dir,
                opts.force,
                &render_project_dir(&opts.project_dir),
                &format!("{}@{}", manifest.pack_id, manifest.version),
                receipt,
            )
        }
        Harness::Codex | Harness::Gemini => {
            let bytes = source.read_file("hooks/hooks.json")?;
            let target = opts.project_dir.join(harness_hook_file(manifest.harness));
            write_pack_file(&opts.project_dir, &target, &bytes, opts.force, receipt)
        }
    }
}

fn install_manual(
    source: &dyn PackSource,
    manifest: &PackManifest,
    opts: &PackInstallOpts,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    let bytes = source.read_file(&manifest.manual_fragment)?;
    let target = opts.project_dir.join(harness_manual_file(manifest.harness));
    write_pack_file(&opts.project_dir, &target, &bytes, opts.force, receipt)
}

const fn harness_agent_dir(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => ".claude/agents",
        Harness::Codex => ".codex/agents",
        Harness::Gemini => ".gemini/agents",
    }
}

const fn harness_command_dir(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => ".claude/commands",
        Harness::Codex => ".codex/commands",
        Harness::Gemini => ".gemini/commands",
    }
}

const fn harness_hook_file(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => ".claude/settings.json",
        Harness::Codex => ".codex/hooks.json",
        Harness::Gemini => ".gemini/hooks.json",
    }
}

const fn harness_manual_file(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "CLAUDE.md",
        Harness::Codex => "AGENTS.md",
        Harness::Gemini => "GEMINI.md",
    }
}
```

When moving existing logic, adjust `install_hook_payloads()` to accept `&dyn PackSource` and replace `dir.get_file("hooks/settings.json")` with `source.read_file("hooks/settings.json")?`.

- [ ] **Step 4: Add install case to path verifier**

In `run_pack_source_conformance()` in `crates/cairn-cli/src/packs/verify.rs`, append:

```rust
let install_case = || -> Result<(), PackError> {
    let manifest_bytes = source.read_file("pack.json")?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)?;
    let tmp = tempdir().map_err(PackError::Io)?;
    let opts = PackInstallOpts {
        harness: manifest.harness,
        project_dir: tmp.path().to_path_buf(),
        force: false,
    };
    let first = crate::packs::install::install_pack_from_source(source, &opts)?;
    let second = crate::packs::install::install_pack_from_source(source, &opts)?;
    if first.files_created.is_empty() {
        return Err(PackError::ManifestInvalid {
            reason: "first install created no files".to_owned(),
        });
    }
    if !second.files_created.is_empty() || !second.files_merged.is_empty() {
        return Err(PackError::ManifestInvalid {
            reason: format!(
                "round-trip not idempotent: created={} merged={}",
                second.files_created.len(),
                second.files_merged.len()
            ),
        });
    }
    Ok(())
};
out.push(CaseOutcome {
    id: "pack_install_round_trip",
    name: "install round-trip is idempotent".to_owned(),
    tier: Tier::Two,
    status: install_case().map_err(|e| format!("{e:#}")),
});
```

Remove the bundled-only install round-trip closure from `run_pack_conformance()` so there is exactly one `pack_install_round_trip` case.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli --test plugins_verify_pack_path
cargo test --locked -p cairn-cli --test claude_code_pack_verify
cargo test --locked -p cairn-cli --test claude_code_pack_install
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/packs/install.rs crates/cairn-cli/src/packs/verify.rs crates/cairn-cli/tests/plugins_verify_pack_path.rs
git commit -m "feat(packs): install external pack sources for verification (#183)"
```

---

## Task 4: Scaffold Template Renderer and Template Files

**Files:**
- Create: `crates/cairn-cli/src/packs/template.rs`
- Modify: `crates/cairn-cli/src/packs/mod.rs`
- Create: `packs/templates/claude-code/**`
- Create: `packs/templates/codex/**`
- Create: `packs/templates/gemini/**`

- [ ] **Step 1: Write failing renderer tests**

Create `crates/cairn-cli/src/packs/template.rs` with only the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_tokens_replaces_known_values_and_rejects_unresolved_tokens() {
        let vars = TemplateVars {
            pack_id: "sample-pack".to_owned(),
            display_name: "Sample Pack".to_owned(),
            harness: "codex".to_owned(),
            version: "0.1.0".to_owned(),
            manual_fragment: "AGENTS.md".to_owned(),
            command_id: "cairn-context".to_owned(),
            subagent_id: "context-loader".to_owned(),
        };

        let rendered = render_tokens("{{pack_id}} {{display_name}}", &vars).expect("rendered");
        assert_eq!(rendered, "sample-pack Sample Pack");

        let err = render_tokens("{{missing_token}}", &vars).expect_err("unresolved rejected");
        assert!(err.to_string().contains("unresolved template token"));
    }

    #[test]
    fn display_name_title_cases_safe_pack_id() {
        assert_eq!(display_name("my-pack"), "My Pack");
        assert_eq!(display_name("ops_pack"), "Ops Pack");
    }
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli packs::template
```

Expected: FAIL because `template` module and types do not exist.

- [ ] **Step 3: Implement renderer core**

Create `crates/cairn-cli/src/packs/template.rs`:

```rust
//! Fixed-token scaffold rendering for `cairn skill new`.

use std::path::{Path, PathBuf};

use include_dir::{Dir, include_dir};

use crate::packs::manifest::{Harness, PackError};

static TEMPLATE_ROOT: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../packs/templates");

#[derive(Debug, Clone)]
pub struct TemplateVars {
    pub pack_id: String,
    pub display_name: String,
    pub harness: String,
    pub version: String,
    pub manual_fragment: String,
    pub command_id: String,
    pub subagent_id: String,
}

#[derive(Debug, Clone)]
pub struct ScaffoldOpts {
    pub name: String,
    pub harness: Harness,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScaffoldReceipt {
    pub pack_id: String,
    pub harness: String,
    pub output_dir: PathBuf,
    pub files_created: Vec<PathBuf>,
    pub verify_command: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error("invalid pack name `{name}`")]
    InvalidPackName { name: String },
    #[error("output directory is not empty: {path}")]
    OutputDirectoryNotEmpty { path: String },
    #[error("template missing: {path}")]
    TemplateMissing { path: String },
    #[error("unresolved template token in {path}")]
    UnresolvedToken { path: String },
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn render_tokens(input: &str, vars: &TemplateVars) -> Result<String, ScaffoldError> {
    let rendered = input
        .replace("{{pack_id}}", &vars.pack_id)
        .replace("{{display_name}}", &vars.display_name)
        .replace("{{harness}}", &vars.harness)
        .replace("{{version}}", &vars.version)
        .replace("{{manual_fragment}}", &vars.manual_fragment)
        .replace("{{command_id}}", &vars.command_id)
        .replace("{{subagent_id}}", &vars.subagent_id);
    if rendered.contains("{{") || rendered.contains("}}") {
        return Err(ScaffoldError::UnresolvedToken {
            path: "<inline>".to_owned(),
        });
    }
    Ok(rendered)
}

#[must_use]
pub fn display_name(pack_id: &str) -> String {
    pack_id
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[must_use]
pub fn harness_id(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::Gemini => "gemini",
    }
}

#[must_use]
pub fn manual_fragment(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "manual.md",
        Harness::Codex => "AGENTS.md",
        Harness::Gemini => "GEMINI.md",
    }
}

fn is_safe_pack_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
```

- [ ] **Step 4: Export module**

Modify `crates/cairn-cli/src/packs/mod.rs`:

```rust
pub mod source;
pub mod template;
pub mod verify;
```

- [ ] **Step 5: Add template files**

Create all files listed in the File Structure section. Use these contents and adjust only the harness-specific manual filename and hook file path.

`packs/templates/codex/pack.json.template`:

```json
{
  "schema": "cairn-pack/v1",
  "pack_id": "{{pack_id}}",
  "name": "{{pack_id}}",
  "version": "{{version}}",
  "harness": "{{harness}}",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Starter Cairn skill-pack for {{display_name}}.",
  "requires_capabilities": [
    "cairn.mcp.v1.search.keyword",
    "cairn.mcp.v1.retrieve.record"
  ],
  "subagents": [
    {
      "id": "{{subagent_id}}",
      "path": "agents/{{subagent_id}}.md",
      "uses_mcp_tools": ["assemble_hot", "retrieve", "search"]
    }
  ],
  "commands": [
    {
      "id": "{{command_id}}",
      "path": "commands/{{command_id}}.md",
      "kind": "verb-direct",
      "verb": "assemble_hot"
    }
  ],
  "hooks": {
    "SessionStart": { "command": "cairn hook SessionStart" }
  },
  "manual_fragment": "{{manual_fragment}}"
}
```

Use the same `pack.json.template` for `claude-code` and `gemini`.

`packs/templates/codex/agents/context-loader.md.template`:

```markdown
# Context Loader

Use typed Cairn MCP tools only:

- `mcp__cairn__assemble_hot`
- `mcp__cairn__retrieve`
- `mcp__cairn__search`

Load compact project context, cite record ids, and never write directly to the
vault or database.
```

For `packs/templates/claude-code/agents/context-loader.md.template`, include Claude Code frontmatter:

```markdown
---
name: context-loader
description: Load compact Cairn memory context for the current task.
tools: mcp__cairn__assemble_hot, mcp__cairn__retrieve, mcp__cairn__search
---

# Context Loader

Load compact project context, cite record ids, and never write directly to the
vault or database.
```

`packs/templates/codex/commands/cairn-context.md.template`:

````markdown
# /{{command_id}}

Run:

```bash
cairn assemble_hot --json
```

Return the JSON envelope unchanged.
````

Use the same command template for `claude-code` and `gemini`.

`packs/templates/codex/AGENTS.md.template`:

```markdown
<!-- BEGIN CAIRN PACK {{pack_id}} -->
## Cairn Pack: {{display_name}}

Use the `{{command_id}}` command or `{{subagent_id}}` subagent when durable
Cairn memory context is useful. Prefer typed Cairn MCP calls over shelling out
for core verb behavior.
<!-- END CAIRN PACK {{pack_id}} -->
```

`packs/templates/gemini/GEMINI.md.template` has the same body. `packs/templates/claude-code/manual.md.template` has the same body with `CLAUDE.md` conventions.

`packs/templates/codex/hooks/hooks.json.template`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "type": "command",
        "command": "cairn hook SessionStart --payload-file - --json"
      }
    ]
  }
}
```

Use the same `hooks/hooks.json.template` for Gemini.

`packs/templates/claude-code/hooks/settings.json.template`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "cairn hook SessionStart --payload-file - --json"
          }
        ]
      }
    ]
  }
}
```

`packs/templates/codex/tests/smoke.sh.template`:

```bash
#!/usr/bin/env bash
set -euo pipefail

test -f pack.json
test -f "{{manual_fragment}}"
test -f "agents/{{subagent_id}}.md"
test -f "commands/{{command_id}}.md"
grep -q "{{pack_id}}" "{{manual_fragment}}"
```

Use the same smoke template for all harnesses.

`packs/templates/codex/.github/workflows/verify.yml.template`:

```yaml
name: verify-cairn-pack

on:
  pull_request:
  push:
    branches: [main]

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install Cairn
        run: cargo install cairn-cli --locked
      - name: Verify pack
        run: cairn plugins verify --pack-path . --strict
      - name: Smoke test
        run: bash tests/smoke.sh
```

Use the same workflow template for all harnesses.

- [ ] **Step 6: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli packs::template
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/packs/template.rs crates/cairn-cli/src/packs/mod.rs packs/templates/
git commit -m "feat(packs): add skill-pack scaffold templates (#183)"
```

---

## Task 5: `cairn skill new` CLI

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Create: `crates/cairn-cli/tests/skill_new.rs`

- [ ] **Step 1: Write failing CLI tests**

Create `crates/cairn-cli/tests/skill_new.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn skill_new_rejects_unsafe_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    Command::cargo_bin("cairn")
        .expect("binary")
        .current_dir(tmp.path())
        .args(["skill", "new", "../bad", "--harness", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid pack name"));
    assert!(!tmp.path().join("bad").exists());
}

#[test]
fn skill_new_fails_on_non_empty_output() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("existing");
    std::fs::create_dir_all(&output).expect("dir");
    std::fs::write(output.join("keep.txt"), "user content").expect("file");

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["skill", "new", "sample-pack", "--harness", "codex", "--output"])
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("output directory is not empty"));

    assert_eq!(
        std::fs::read_to_string(output.join("keep.txt")).expect("file"),
        "user content"
    );
}

#[test]
fn skill_new_codex_scaffold_verifies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("sample-pack");

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["skill", "new", "sample-pack", "--harness", "codex", "--output"])
        .arg(&output)
        .assert()
        .success()
        .stdout(predicate::str::contains("cairn plugins verify --pack-path"));

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["plugins", "verify", "--pack-path"])
        .arg(&output)
        .arg("--strict")
        .assert()
        .success();
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli --test skill_new
```

Expected: FAIL because `skill new` is not a known subcommand.

- [ ] **Step 3: Add scaffold API in `skill.rs`**

Add imports:

```rust
use crate::packs::manifest::Harness as PackHarness;
use crate::packs::template::{ScaffoldError, ScaffoldOpts, ScaffoldReceipt};
```

Add:

```rust
pub use crate::packs::template::ScaffoldReceipt as SkillScaffoldReceipt;

pub fn scaffold(opts: &ScaffoldOpts) -> Result<ScaffoldReceipt, ScaffoldError> {
    crate::packs::template::render_scaffold(opts)
}

#[must_use]
pub fn render_scaffold_human(receipt: &ScaffoldReceipt) -> String {
    format!(
        "cairn skill new: scaffold written to {}\n  verify: {}\n",
        receipt.output_dir.display(),
        receipt.verify_command
    )
}

pub fn pack_harness_from_skill_harness(harness: &Harness) -> Option<PackHarness> {
    match harness {
        Harness::ClaudeCode => Some(PackHarness::ClaudeCode),
        Harness::Codex => Some(PackHarness::Codex),
        Harness::Gemini => Some(PackHarness::Gemini),
        Harness::Opencode | Harness::Cursor | Harness::Custom => None,
    }
}
```

In `crates/cairn-cli/src/packs/template.rs`, implement `render_scaffold()`:

```rust
pub fn render_scaffold(opts: &ScaffoldOpts) -> Result<ScaffoldReceipt, ScaffoldError> {
    if !is_safe_pack_name(&opts.name) {
        return Err(ScaffoldError::InvalidPackName {
            name: opts.name.clone(),
        });
    }
    if opts.output_dir.exists() && opts.output_dir.read_dir()?.next().is_some() {
        return Err(ScaffoldError::OutputDirectoryNotEmpty {
            path: opts.output_dir.display().to_string(),
        });
    }

    let vars = TemplateVars {
        pack_id: opts.name.clone(),
        display_name: display_name(&opts.name),
        harness: harness_id(opts.harness).to_owned(),
        version: "0.1.0".to_owned(),
        manual_fragment: manual_fragment(opts.harness).to_owned(),
        command_id: "cairn-context".to_owned(),
        subagent_id: "context-loader".to_owned(),
    };

    let template_dir = TEMPLATE_ROOT
        .get_dir(harness_id(opts.harness))
        .ok_or_else(|| ScaffoldError::TemplateMissing {
            path: harness_id(opts.harness).to_owned(),
        })?;
    let mut created = Vec::new();
    render_dir(template_dir, Path::new(""), &opts.output_dir, &vars, &mut created)?;

    let source = crate::packs::source::FsPackSource::new(opts.output_dir.clone());
    let outcomes = crate::packs::verify::run_pack_source_conformance(&source);
    if let Some(failure) = outcomes.iter().find(|case| case.status.is_err()) {
        return Err(ScaffoldError::Pack(PackError::ManifestInvalid {
            reason: format!("generated scaffold failed {}: {:?}", failure.id, failure.status),
        }));
    }

    Ok(ScaffoldReceipt {
        pack_id: opts.name.clone(),
        harness: harness_id(opts.harness).to_owned(),
        output_dir: opts.output_dir.clone(),
        files_created: created,
        verify_command: format!(
            "cairn plugins verify --pack-path {} --strict",
            opts.output_dir.display()
        ),
    })
}

fn render_dir(
    dir: &include_dir::Dir<'_>,
    rel: &Path,
    output_dir: &Path,
    vars: &TemplateVars,
    created: &mut Vec<PathBuf>,
) -> Result<(), ScaffoldError> {
    for file in dir.files() {
        let file_rel = rel.join(file.path().file_name().expect("template file name"));
        let mut target_rel = file_rel.clone();
        if let Some(name) = target_rel.file_name().and_then(|name| name.to_str())
            && let Some(stripped) = name.strip_suffix(".template")
        {
            target_rel.set_file_name(stripped);
        }
        let text = std::str::from_utf8(file.contents()).map_err(|e| ScaffoldError::Pack(
            PackError::ManifestInvalid {
                reason: format!("template {} is not UTF-8: {e}", file.path().display()),
            },
        ))?;
        let rendered = render_tokens(text, vars).map_err(|err| match err {
            ScaffoldError::UnresolvedToken { .. } => ScaffoldError::UnresolvedToken {
                path: file.path().display().to_string(),
            },
            other => other,
        })?;
        let target = output_dir.join(&target_rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, rendered)?;
        created.push(target);
    }
    for child in dir.dirs() {
        let child_name = child.path().file_name().expect("template dir name");
        render_dir(child, &rel.join(child_name), output_dir, vars, created)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Add clap subcommand**

In `skill_subcommand()` in `crates/cairn-cli/src/command.rs`, add a sibling to `install`:

```rust
.subcommand(
    clap::Command::new("new")
        .about("Create a starter cairn-pack/v1 skill-pack scaffold")
        .arg(
            clap::Arg::new("name")
                .value_name("NAME")
                .required(true)
                .help("Path-safe pack id to create"),
        )
        .arg(
            clap::Arg::new("harness")
                .long("harness")
                .value_name("HARNESS")
                .required(true)
                .value_parser(clap::builder::EnumValueParser::<skill::Harness>::new())
                .help("Target harness (claude-code, codex, gemini)"),
        )
        .arg(
            clap::Arg::new("output")
                .long("output")
                .value_name("DIR")
                .value_parser(clap::value_parser!(std::path::PathBuf))
                .help("Output directory (default: ./NAME)"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON receipt instead of human-readable output"),
        ),
)
```

- [ ] **Step 5: Dispatch `skill new`**

In `main.rs`, change the `skill` branch to dispatch on subcommands:

```rust
Some(("skill", sub)) => match sub.subcommand() {
    Some(("install", install)) => run_skill_install(install),
    Some(("new", new)) => run_skill_new(new),
    _ => unreachable!("clap subcommand_required(true) on skill ensures a subcommand is set"),
},
```

Add:

```rust
fn run_skill_new(matches: &ArgMatches) -> ExitCode {
    let name = matches
        .get_one::<String>("name")
        .expect("invariant: skill new requires name")
        .clone();
    let harness = matches
        .get_one::<cairn_cli::skill::Harness>("harness")
        .expect("invariant: skill new requires harness");
    let Some(pack_harness) = cairn_cli::skill::pack_harness_from_skill_harness(harness) else {
        eprintln!("cairn skill new: harness {harness:?} does not support pack scaffolds");
        return ExitCode::from(64);
    };
    let output_dir = matches
        .get_one::<std::path::PathBuf>("output")
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from(&name));
    let opts = cairn_cli::packs::template::ScaffoldOpts {
        name,
        harness: pack_harness,
        output_dir,
    };
    match cairn_cli::skill::scaffold(&opts) {
        Ok(receipt) => {
            if matches.get_flag("json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt)
                        .expect("invariant: ScaffoldReceipt is serializable")
                );
            } else {
                println!("{}", cairn_cli::skill::render_scaffold_human(&receipt));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skill new: {e:#}");
            ExitCode::from(74)
        }
    }
}
```

- [ ] **Step 6: Add Claude Code and Gemini scaffold tests**

Append to `crates/cairn-cli/tests/skill_new.rs`:

```rust
#[test]
fn skill_new_claude_code_scaffold_verifies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("sample-claude-pack");

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["skill", "new", "sample-claude-pack", "--harness", "claude-code", "--output"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["plugins", "verify", "--pack-path"])
        .arg(&output)
        .arg("--strict")
        .assert()
        .success();
}

#[test]
fn skill_new_gemini_scaffold_verifies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("sample-gemini-pack");

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["skill", "new", "sample-gemini-pack", "--harness", "gemini", "--output"])
        .arg(&output)
        .assert()
        .success();

    Command::cargo_bin("cairn")
        .expect("binary")
        .args(["plugins", "verify", "--pack-path"])
        .arg(&output)
        .arg("--strict")
        .assert()
        .success();
}
```

- [ ] **Step 7: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli --test skill_new
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-cli/src/skill.rs crates/cairn-cli/src/command.rs crates/cairn-cli/src/main.rs crates/cairn-cli/src/packs/template.rs crates/cairn-cli/tests/skill_new.rs
git commit -m "feat(skill): scaffold new cairn skill packs (#183)"
```

---

## Task 6: Authoring Guide and Anchor Tests

**Files:**
- Create: `docs/skill-pack-authoring.md`
- Modify: `crates/cairn-cli/tests/skill_new.rs`

- [ ] **Step 1: Write failing doc anchor test**

Append to `crates/cairn-cli/tests/skill_new.rs`:

```rust
#[test]
fn skill_pack_authoring_guide_has_required_anchors() {
    let guide = std::fs::read_to_string("docs/skill-pack-authoring.md")
        .expect("docs/skill-pack-authoring.md");
    for heading in [
        "## Pack Layout",
        "## Manifest Schema",
        "## Capability Declarations",
        "## Hook Binding Contract",
        "## Subagent Prompt Contract",
        "## Slash Command Contract",
        "## Operating Manual Fragments",
        "## Versioning And Compatibility",
        "## Publishing And CI",
        "## Not In Scope For Packs",
        "## Verification",
    ] {
        assert!(guide.contains(heading), "missing heading {heading}");
    }
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli --test skill_new skill_pack_authoring_guide_has_required_anchors
```

Expected: FAIL because the guide file does not exist.

- [ ] **Step 3: Create guide**

Create `docs/skill-pack-authoring.md` with these sections:

````markdown
# Skill-Pack Authoring Guide

This guide describes harness packs: `cairn-pack/v1` directories that install
subagents, slash commands, hook bindings, and operating-manual fragments for a
specific harness. Harness packs are distinct from Skillify `.cairnpack`
archives managed by `cairn skillpack`.

## Pack Layout

A starter pack from `cairn skill new my-pack --harness codex` contains
`pack.json`, one manual fragment, one subagent, one command, one hook file, one
smoke script, and one GitHub Actions workflow.

## Manifest Schema

`pack.json` must use `"schema": "cairn-pack/v1"`. Required fields are
`pack_id`, `name`, `version`, `harness`, `cairn_mcp_compat`, `description`,
`requires_capabilities`, `subagents`, `commands`, `hooks`, and
`manual_fragment`.

Pack ids and entry ids are path-safe ASCII tokens: letters, digits, `-`, and
`_`. Paths are pack-relative and must not be absolute, empty, `.` or `..`.

## Capability Declarations

List every required Cairn capability in `requires_capabilities`. Capability
strings use the stable `cairn.mcp.v1` identifiers advertised by `cairn status`.
Verification fails on unknown capability strings.

## Hook Binding Contract

The canonical hook events are `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
`PostToolUse`, and `Stop`. Hook commands should call
`cairn hook <event> --payload-file - --json` so payloads flow through the
standard hook parser.

## Subagent Prompt Contract

Subagents use typed MCP tool calls only. They must not write directly to the
database, mutate the vault outside Cairn verbs, bypass the WAL, or shell out for
core verb behavior when a typed MCP tool exists.

## Slash Command Contract

Slash commands wrap CLI ground truth. Prefer JSON output and deterministic text
so command files can be snapshot-tested.

## Operating Manual Fragments

Manual fragments must be guarded:

```markdown
<!-- BEGIN CAIRN PACK my-pack -->
Pack instructions.
<!-- END CAIRN PACK my-pack -->
```

Use `CLAUDE.md`, `AGENTS.md`, or `GEMINI.md` according to the target harness.

## Versioning And Compatibility

Pack `version` is semver. `cairn_mcp_compat` is a lower-bound range such as
`>=1.0.0`. Backward-compatible prompt clarifications are patch changes; new
commands, subagents, hooks, or required capabilities are minor changes; removed
entries are major changes.

## Publishing And CI

Before publishing, run:

```bash
cairn plugins verify --pack-path . --strict
bash tests/smoke.sh
```

The scaffold includes `.github/workflows/verify.yml` with those commands.

## Not In Scope For Packs

Packs must not perform direct DB writes, bypass the WAL, introduce hidden global
state, add harness-specific code to `cairn-core`, or make cloud calls unless the
pack clearly documents and gates them.

## Verification

Use `cairn skill new <name> --harness <claude-code|codex|gemini>` for a fresh
scaffold. Use `cairn plugins verify --pack-path <dir> --strict` to verify a
pack from any repository.
````

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli --test skill_new skill_pack_authoring_guide_has_required_anchors
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/skill-pack-authoring.md crates/cairn-cli/tests/skill_new.rs
git commit -m "docs: add skill-pack authoring guide (#183)"
```

---

## Task 7: Scaffold Smoke Script Execution and CI Snippet Coverage

**Files:**
- Modify: `crates/cairn-cli/src/packs/verify.rs`
- Modify: `crates/cairn-cli/tests/skill_new.rs`

- [ ] **Step 1: Write failing smoke execution assertion**

Append to `crates/cairn-cli/tests/skill_new.rs`:

```rust
#[test]
fn generated_templates_include_ci_and_smoke() {
    for harness in ["claude-code", "codex", "gemini"] {
        let root = std::path::Path::new("packs/templates").join(harness);
        assert!(
            root.join(".github/workflows/verify.yml.template").is_file(),
            "{harness} workflow template missing"
        );
        let smoke = root.join("tests/smoke.sh.template");
        assert!(smoke.is_file(), "{harness} smoke template missing");
        let smoke_text = std::fs::read_to_string(smoke).expect("smoke");
        assert!(
            !smoke_text.contains("plugins verify"),
            "{harness} smoke script must be nonrecursive"
        );
    }
}
```

In `plugins_verify_pack_path_json_reports_pack_contract()`, add:

```rust
let cases = json["plugins"][0]["cases"].as_array().expect("cases");
assert!(
    cases
        .iter()
        .any(|case| case["id"] == "pack_smoke_script" && case["status"] == "ok"),
    "smoke script case missing or failed: {json}"
);
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test --locked -p cairn-cli --test plugins_verify_pack_path plugins_verify_pack_path_json_reports_pack_contract
```

Expected: FAIL because path verification does not emit `pack_smoke_script`.

- [ ] **Step 3: Add smoke script case**

In `run_pack_source_conformance()` in `crates/cairn-cli/src/packs/verify.rs`, append:

```rust
let smoke_case = || -> Result<(), PackError> {
    if !source.has_file("tests/smoke.sh") {
        return Ok(());
    }
    let manifest_bytes = source.read_file("pack.json")?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)?;
    let tmp = tempdir().map_err(PackError::Io)?;
    let opts = PackInstallOpts {
        harness: manifest.harness,
        project_dir: tmp.path().to_path_buf(),
        force: false,
    };
    crate::packs::install::install_pack_from_source(source, &opts)?;
    let script = source.read_file("tests/smoke.sh")?;
    let script_path = tmp.path().join("smoke.sh");
    std::fs::write(&script_path, script).map_err(PackError::Io)?;
    let status = std::process::Command::new("bash")
        .arg(&script_path)
        .current_dir(tmp.path())
        .status()
        .map_err(PackError::Io)?;
    if !status.success() {
        return Err(PackError::ManifestInvalid {
            reason: format!("tests/smoke.sh exited with {status}"),
        });
    }
    Ok(())
};
out.push(CaseOutcome {
    id: "pack_smoke_script",
    name: "optional scaffold smoke script".to_owned(),
    tier: Tier::Two,
    status: smoke_case().map_err(|e| format!("{e:#}")),
});
```

Update `write_minimal_pack()` in `plugins_verify_pack_path.rs` to create a nonrecursive smoke script:

```rust
std::fs::create_dir_all(root.join("tests")).expect("tests");
std::fs::write(
    root.join("tests/smoke.sh"),
    "#!/usr/bin/env bash\nset -euo pipefail\ntest -f AGENTS.md\ntest -f .codex/hooks.json\n",
)
.expect("smoke");
```

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test --locked -p cairn-cli --test plugins_verify_pack_path
cargo test --locked -p cairn-cli --test skill_new generated_templates_include_ci_and_smoke
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/packs/verify.rs crates/cairn-cli/tests/plugins_verify_pack_path.rs crates/cairn-cli/tests/skill_new.rs
git commit -m "test(packs): verify scaffold smoke scripts (#183)"
```

---

## Task 8: Final Verification and Docs Freeze Check

**Files:**
- Review all files changed in Tasks 1-7.

- [ ] **Step 1: Run scaffold smoke manually**

Run:

```bash
rm -rf /tmp/cairn-pack-smoke
cargo run -p cairn-cli -- skill new my-pack --harness codex --output /tmp/cairn-pack-smoke
cargo run -p cairn-cli -- plugins verify --pack-path /tmp/cairn-pack-smoke --strict
bash /tmp/cairn-pack-smoke/tests/smoke.sh
```

Expected: all commands exit 0. The first command prints a verification command. The second command reports `0 failed`.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo test --locked -p cairn-cli packs::source
cargo test --locked -p cairn-cli packs::template
cargo test --locked -p cairn-cli packs::verify
cargo test --locked -p cairn-cli --test plugins_verify_pack_path
cargo test --locked -p cairn-cli --test skill_new
cargo test --locked -p cairn-cli --test claude_code_pack_install
cargo test --locked -p cairn-cli --test claude_code_pack_verify
```

Expected: PASS.

- [ ] **Step 3: Run formatting and boundary checks**

Run:

```bash
cargo fmt --all --check
./scripts/check-core-boundary.sh
```

Expected: PASS.

- [ ] **Step 4: Run workspace check if time permits**

Run:

```bash
cargo check --workspace --all-targets --locked
```

Expected: PASS.

- [ ] **Step 5: Inspect diff**

Run:

```bash
git diff --check
git status --short
```

Expected: `git diff --check` has no output. `git status --short` shows only files intentionally changed for issue #183.

- [ ] **Step 6: Commit final verification adjustments**

If Task 8 required fixes, commit them:

```bash
git add crates/cairn-cli docs packs
git commit -m "chore: verify skill-pack authoring scaffold (#183)"
```

Skip this commit if Task 8 made no file changes.
