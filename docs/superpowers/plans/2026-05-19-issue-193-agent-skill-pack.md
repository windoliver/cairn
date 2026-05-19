# Issue 193 Agent Skill-Pack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add first-slice `cairn skill install --agent ...` and `--all` support that writes idempotent harness integration files for Claude Code, Codex, Kiro, and Cursor.

**Architecture:** Preserve the existing issue #68 bundle installer as `skill::install`. Add a second `skill::install_agent_pack` wrapper that calls the bundle installer once, then writes guarded markdown and generated JSON integration files in the current project directory. Keep all new behavior in `crates/cairn-cli/src/skill.rs` and CLI argument plumbing in `command.rs`/`main.rs`.

**Tech Stack:** Rust 2024, clap `ValueEnum`, serde/serde_json, tempfile and insta for existing tests.

---

### Task 1: Agent Types And Rendered Fragments

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write failing unit tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `crates/cairn-cli/src/skill.rs`:

```rust
#[test]
fn agent_values_render_expected_fragments() {
    assert_eq!(Agent::ClaudeCode.to_possible_value().unwrap().get_name(), "claude-code");
    assert_eq!(Agent::Codex.to_possible_value().unwrap().get_name(), "codex");
    assert_eq!(Agent::Kiro.to_possible_value().unwrap().get_name(), "kiro");
    assert_eq!(Agent::Cursor.to_possible_value().unwrap().get_name(), "cursor");

    let block = render_agent_markdown_block(Agent::Codex);
    assert!(block.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
    assert!(block.contains("cairn ingest --folder . --mode keyword"));
    assert!(block.contains("Do not use Cairn tools for ordinary file reads or code execution."));
    assert!(block.contains("/remember"));
    assert!(block.contains("/forget"));
}

#[test]
fn harness_maps_to_first_slice_agent_when_supported() {
    assert_eq!(Agent::from_harness(&Harness::ClaudeCode), Some(Agent::ClaudeCode));
    assert_eq!(Agent::from_harness(&Harness::Codex), Some(Agent::Codex));
    assert_eq!(Agent::from_harness(&Harness::Cursor), Some(Agent::Cursor));
    assert_eq!(Agent::from_harness(&Harness::Gemini), None);
    assert_eq!(Agent::from_harness(&Harness::Opencode), None);
    assert_eq!(Agent::from_harness(&Harness::Custom), None);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p cairn-cli skill::tests::agent_values_render_expected_fragments skill::tests::harness_maps_to_first_slice_agent_when_supported
```

Expected: compile failure because `Agent`, `from_harness`, and `render_agent_markdown_block` do not exist.

- [ ] **Step 3: Implement minimal agent type and renderer**

In `crates/cairn-cli/src/skill.rs`, add:

```rust
/// Agents with first-slice generated skill-pack integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Claude Code project integration.
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Codex/OpenCode project instructions.
    Codex,
    /// Kiro always-included steering file.
    Kiro,
    /// Cursor always-applied rule file.
    Cursor,
}

impl Agent {
    /// First-slice agents in deterministic receipt order.
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::Codex, Self::Kiro, Self::Cursor];

    /// Compatibility mapping from the older issue #68 harness flag.
    #[must_use]
    pub const fn from_harness(harness: &Harness) -> Option<Self> {
        match harness {
            Harness::ClaudeCode => Some(Self::ClaudeCode),
            Harness::Codex => Some(Self::Codex),
            Harness::Cursor => Some(Self::Cursor),
            Harness::Gemini | Harness::Opencode | Harness::Custom => None,
        }
    }
}
```

Add `render_agent_markdown_block(agent: Agent) -> String` that returns a guarded markdown block containing the shared guidance and an agent-specific heading.

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p cairn-cli skill::tests::agent_values_render_expected_fragments skill::tests::harness_maps_to_first_slice_agent_when_supported
```

Expected: both tests pass.

---

### Task 2: Guarded Markdown Updates

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write failing unit tests**

Add tests:

```rust
#[test]
fn guarded_markdown_appends_to_user_content() {
    let original = "# Existing\n\nKeep me.\n";
    let block = render_agent_markdown_block(Agent::Codex);
    let updated = upsert_guarded_markdown(original, &block);

    assert!(updated.starts_with(original));
    assert!(updated.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
    assert!(updated.ends_with('\n'));
}

#[test]
fn guarded_markdown_replaces_existing_block_once() {
    let old = "# Existing\n\n<!-- BEGIN CAIRN AGENT SKILL -->\nold\n<!-- END CAIRN AGENT SKILL -->\n\nTail\n";
    let block = render_agent_markdown_block(Agent::Codex);
    let updated = upsert_guarded_markdown(old, &block);

    assert!(updated.contains("# Existing"));
    assert!(updated.contains("Tail"));
    assert!(!updated.contains("\nold\n"));
    assert_eq!(updated.matches("<!-- BEGIN CAIRN AGENT SKILL -->").count(), 1);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p cairn-cli skill::tests::guarded_markdown_appends_to_user_content skill::tests::guarded_markdown_replaces_existing_block_once
```

Expected: compile failure because `upsert_guarded_markdown` does not exist.

- [ ] **Step 3: Implement guarded updater**

Add constants:

```rust
const AGENT_BLOCK_BEGIN: &str = "<!-- BEGIN CAIRN AGENT SKILL -->";
const AGENT_BLOCK_END: &str = "<!-- END CAIRN AGENT SKILL -->";
```

Add:

```rust
fn upsert_guarded_markdown(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(AGENT_BLOCK_BEGIN)
        && let Some(end_rel) = existing[start..].find(AGENT_BLOCK_END)
    {
        let end = start + end_rel + AGENT_BLOCK_END.len();
        let mut out = String::new();
        out.push_str(&existing[..start]);
        out.push_str(block.trim_end());
        out.push('\n');
        out.push_str(&existing[end..]);
        return ensure_trailing_newline(out);
    }

    let mut out = ensure_trailing_newline(existing.to_owned());
    if !out.trim().is_empty() {
        out.push('\n');
    }
    out.push_str(block.trim_end());
    out.push('\n');
    out
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}
```

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p cairn-cli skill::tests::guarded_markdown_appends_to_user_content skill::tests::guarded_markdown_replaces_existing_block_once
```

Expected: both tests pass.

---

### Task 3: Integration Writers

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write failing unit tests**

Add tests:

```rust
#[test]
fn install_agent_pack_codex_writes_agents_md_and_bundle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    std::fs::write(project.join("AGENTS.md"), "# Project\n").expect("seed agents");
    let target = tmp.path().join("skills/cairn");

    let receipt = install_agent_pack(&AgentInstallOpts {
        target_dir: target.clone(),
        project_dir: project.clone(),
        agents: vec![Agent::Codex],
        harness: Harness::Codex,
        force: false,
    })
    .expect("install codex agent pack");

    assert!(target.join("SKILL.md").exists());
    let agents = std::fs::read_to_string(project.join("AGENTS.md")).expect("read agents");
    assert!(agents.contains("# Project"));
    assert!(agents.contains("<!-- BEGIN CAIRN AGENT SKILL -->"));
    assert_eq!(receipt.integrations.len(), 1);
    assert_eq!(receipt.integrations[0].agent, Agent::Codex);
}

#[test]
fn install_agent_pack_all_writes_first_slice_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let target = tmp.path().join("skills/cairn");

    let receipt = install_agent_pack(&AgentInstallOpts {
        target_dir: target,
        project_dir: project.clone(),
        agents: Agent::ALL.to_vec(),
        harness: Harness::ClaudeCode,
        force: false,
    })
    .expect("install all agent packs");

    assert!(project.join(".claude/settings.json").exists());
    assert!(project.join("CLAUDE.md").exists());
    assert!(project.join("AGENTS.md").exists());
    assert!(project.join(".kiro/steering/cairn.md").exists());
    assert!(project.join(".cursor/rules/cairn.mdc").exists());
    assert_eq!(receipt.integrations.len(), 4);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p cairn-cli skill::tests::install_agent_pack_codex_writes_agents_md_and_bundle skill::tests::install_agent_pack_all_writes_first_slice_files
```

Expected: compile failure because `AgentInstallOpts`, `install_agent_pack`, and integration receipt types do not exist.

- [ ] **Step 3: Implement agent install receipt and writers**

Add public structs:

```rust
#[derive(Debug, Clone)]
pub struct AgentInstallOpts {
    pub target_dir: PathBuf,
    pub project_dir: PathBuf,
    pub agents: Vec<Agent>,
    pub harness: Harness,
    pub force: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct AgentInstallReceipt {
    pub bundle: InstallReceipt,
    pub integrations: Vec<AgentIntegrationReceipt>,
}

#[derive(Debug, serde::Serialize)]
pub struct AgentIntegrationReceipt {
    pub agent: Agent,
    pub files_created: Vec<PathBuf>,
    pub files_updated: Vec<PathBuf>,
    pub files_skipped: Vec<PathBuf>,
}
```

Implement `install_agent_pack` by calling existing `install`, then dispatching to:

- `install_claude_code_integration`
- `install_codex_integration`
- `install_kiro_integration`
- `install_cursor_integration`

Each writer should use `read_to_string` with empty fallback for missing files,
compare final content with existing content, and write only when changed.

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p cairn-cli skill::tests::install_agent_pack_codex_writes_agents_md_and_bundle skill::tests::install_agent_pack_all_writes_first_slice_files
```

Expected: both tests pass.

---

### Task 4: Claude Settings JSON Merge

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write failing unit test**

Add:

```rust
#[test]
fn claude_settings_merge_preserves_unrelated_keys() {
    let existing = serde_json::json!({
        "theme": "dark",
        "mcpServers": {
            "other": {"command": "other", "args": []}
        }
    });
    let merged = merge_claude_settings(existing).expect("merge settings");

    assert_eq!(merged["theme"], "dark");
    assert_eq!(merged["mcpServers"]["other"]["command"], "other");
    assert_eq!(merged["mcpServers"]["cairn"]["command"], "cairn");
    assert_eq!(merged["mcpServers"]["cairn"]["args"], serde_json::json!(["mcp"]));
    let rendered = serde_json::to_string(&merged).expect("json");
    assert!(rendered.contains("cairn ingest --folder . --mode keyword"));
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p cairn-cli skill::tests::claude_settings_merge_preserves_unrelated_keys
```

Expected: compile failure because `merge_claude_settings` does not exist.

- [ ] **Step 3: Implement JSON merge**

Implement:

```rust
fn merge_claude_settings(mut value: serde_json::Value) -> Result<serde_json::Value> {
    let root = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Claude Code settings root must be a JSON object"))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Claude Code mcpServers must be a JSON object"))?;
    servers.insert("cairn".to_owned(), serde_json::json!({"command": "cairn", "args": ["mcp"]}));
    root.insert("hooks".to_owned(), merged_hooks(root.get("hooks").cloned()));
    Ok(value)
}
```

`merged_hooks` should return a JSON value containing a `SessionStart` command with
`cairn ingest --folder . --mode keyword` while preserving existing hook keys when
they are an object.

- [ ] **Step 4: Run test to verify GREEN**

Run:

```bash
cargo test -p cairn-cli skill::tests::claude_settings_merge_preserves_unrelated_keys
```

Expected: test passes.

---

### Task 5: CLI Plumbing

**Files:**
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Test: `crates/cairn-cli/tests/skill_agent_pack.rs`

- [ ] **Step 1: Write failing CLI integration tests**

Create `crates/cairn-cli/tests/skill_agent_pack.rs` with tests that run the binary from a temp project:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn skill_install_agent_codex_writes_agents_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let target = tmp.path().join("skills/cairn");

    Command::cargo_bin("cairn")
        .expect("binary")
        .current_dir(&project)
        .args(["skill", "install", "--agent", "codex", "--target-dir"])
        .arg(&target)
        .assert()
        .success()
        .stdout(predicate::str::contains("Codex"));

    let agents = std::fs::read_to_string(project.join("AGENTS.md")).expect("AGENTS.md");
    assert!(agents.contains("cairn ingest --folder . --mode keyword"));
}

#[test]
fn skill_install_all_json_reports_integrations() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let target = tmp.path().join("skills/cairn");

    let output = Command::cargo_bin("cairn")
        .expect("binary")
        .current_dir(&project)
        .args(["skill", "install", "--all", "--json", "--target-dir"])
        .arg(&target)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).expect("json output");
    assert_eq!(json["integrations"].as_array().expect("integrations").len(), 4);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p cairn-cli --test skill_agent_pack
```

Expected: clap rejects `--agent`/`--all`.

- [ ] **Step 3: Add CLI args and dispatch**

In `command.rs`, make `--harness` optional and add:

```rust
.arg(
    clap::Arg::new("agent")
        .long("agent")
        .value_name("AGENT")
        .value_parser(clap::builder::EnumValueParser::<skill::Agent>::new())
        .conflicts_with_all(["harness", "all"])
)
.arg(
    clap::Arg::new("all")
        .long("all")
        .action(clap::ArgAction::SetTrue)
        .conflicts_with_all(["harness", "agent"])
)
.group(
    clap::ArgGroup::new("skill-install-target")
        .args(["harness", "agent", "all"])
        .required(true)
)
```

In `main.rs`, update `run_skill_install`:

- If `--agent`, call `skill::install_agent_pack`.
- If `--all`, call `skill::install_agent_pack` with `Agent::ALL`.
- If `--harness` maps to an `Agent`, call `install_agent_pack`; otherwise call existing `install`.
- Print `render_agent_human` for agent receipts.

- [ ] **Step 4: Run CLI tests to verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test skill_agent_pack
```

Expected: both tests pass.

---

### Task 6: Snapshots And Focused Verification

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`
- Generated snapshots under `crates/cairn-cli/src/snapshots/` or existing insta snapshot location

- [ ] **Step 1: Add snapshot tests**

Add:

```rust
#[test]
fn agent_markdown_block_snapshot() {
    insta::assert_snapshot!(render_agent_markdown_block(Agent::Codex));
}

#[test]
fn claude_settings_snapshot() {
    let merged = merge_claude_settings(serde_json::json!({})).expect("merge");
    insta::assert_json_snapshot!(merged);
}
```

- [ ] **Step 2: Run snapshots**

Run:

```bash
INSTA_UPDATE=always cargo test -p cairn-cli agent_markdown_block_snapshot claude_settings_snapshot
```

Expected: tests pass and snapshots are written.

- [ ] **Step 3: Run focused verification**

Run:

```bash
cargo test -p cairn-cli skill::tests::
cargo test -p cairn-cli --test skill_agent_pack
cargo fmt --check
```

Expected: all commands pass.

- [ ] **Step 4: Commit implementation**

Run:

```bash
git add crates/cairn-cli/src/skill.rs crates/cairn-cli/src/command.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/skill_agent_pack.rs docs/superpowers/specs/2026-05-19-issue-193-agent-skill-pack-design.md docs/superpowers/plans/2026-05-19-issue-193-agent-skill-pack.md
git commit -m "feat: add agent skill-pack install"
```
