# Claude Code MCP Registration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `cairn setup claude-code` so Cairn can write, update, verify, and remove a Claude Code stdio MCP registration bound to one active vault.

**Architecture:** Add a focused `cairn_cli::setup::claude_code` module that owns Claude Code JSON config transforms and receipts. Wire it through the existing clap command tree and `main.rs` dispatch, using the existing vault resolver and vault-binding gate before writing config. Keep doctor as the verification path and update docs plus generated CLI reference.

**Tech Stack:** Rust 2024, `clap` `ValueEnum`, `serde` / `serde_json`, `thiserror`, `assert_cmd`, `tempfile`, `cargo nextest`, `cairn-docgen`.

---

## File Structure

Create:

- `crates/cairn-cli/src/setup.rs` — top-level setup module export.
- `crates/cairn-cli/src/setup/claude_code.rs` — data types, JSON writer, removal logic, receipts, human rendering, and unit tests.
- `crates/cairn-cli/tests/claude_code_setup.rs` — CLI integration tests for setup/remove/doctor flow.
- `docs/site/src/usage/claude-code.md` — user-facing Claude Code setup page.

Modify:

- `crates/cairn-cli/src/lib.rs` — export `setup`.
- `crates/cairn-cli/src/command.rs` — add `setup claude-code` and `setup claude-code remove`.
- `crates/cairn-cli/src/main.rs` — route setup commands and require a bound vault before registering.
- `crates/cairn-cli/src/doctor.rs` — update remediation text to prefer the new setup command.
- `docs/site/src/usage/mcp.md` — link the recommended Claude Code setup path.
- `docs/site/src/SUMMARY.md` — add the new usage page and generated setup command references.
- `docs/site/src/reference/generated/*` — regenerate with `cairn-docgen`.

## Task 1: Config Writer Module

**Files:**
- Create: `crates/cairn-cli/src/setup.rs`
- Create: `crates/cairn-cli/src/setup/claude_code.rs`
- Modify: `crates/cairn-cli/src/lib.rs`

- [ ] **Step 1: Write the failing writer tests and public type skeleton**

Add `pub mod setup;` to `crates/cairn-cli/src/lib.rs`.

Create `crates/cairn-cli/src/setup.rs`:

```rust
//! Harness setup helpers.

pub mod claude_code;
```

Create `crates/cairn-cli/src/setup/claude_code.rs` with the public types, intentionally incomplete functions, and tests:

```rust
//! Claude Code MCP config writer for `cairn setup claude-code`.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeCodeScope {
    Local,
    Project,
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeSetupOpts {
    pub scope: ClaudeCodeScope,
    pub project_dir: PathBuf,
    pub home_dir: PathBuf,
    pub server_name: String,
    pub vault: PathBuf,
    pub binary: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeRemoveOpts {
    pub scope: ClaudeCodeScope,
    pub project_dir: PathBuf,
    pub home_dir: PathBuf,
    pub server_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupStatus {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
}

#[derive(Debug, Serialize)]
pub struct ClaudeCodeSetupReceipt {
    pub scope: ClaudeCodeScope,
    pub config_path: PathBuf,
    pub server_name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub status: SetupStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum ClaudeCodeSetupError {
    #[error("invalid setup option: {0}")]
    InvalidOption(String),
    #[error("failed to parse Claude Code config {path}: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Claude Code config {path} must be a JSON object")]
    ConfigRoot {
        path: PathBuf,
    },
    #[error("filesystem error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl ClaudeCodeSetupError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidOption(_) | Self::ConfigParse { .. } | Self::ConfigRoot { .. } => 78,
            Self::Io { .. } => 74,
        }
    }
}

pub type Result<T> = std::result::Result<T, ClaudeCodeSetupError>;

pub fn setup(opts: &ClaudeCodeSetupOpts) -> Result<ClaudeCodeSetupReceipt> {
    let config_path = config_path(opts.scope, &opts.project_dir, &opts.home_dir);
    Ok(ClaudeCodeSetupReceipt {
        scope: opts.scope,
        config_path,
        server_name: opts.server_name.clone(),
        command: opts.binary.clone(),
        args: vec!["--vault".to_owned(), opts.vault.display().to_string(), "mcp".to_owned()],
        status: SetupStatus::Unchanged,
    })
}

pub fn remove(opts: &ClaudeCodeRemoveOpts) -> Result<ClaudeCodeSetupReceipt> {
    let config_path = config_path(opts.scope, &opts.project_dir, &opts.home_dir);
    Ok(ClaudeCodeSetupReceipt {
        scope: opts.scope,
        config_path,
        server_name: opts.server_name.clone(),
        command: PathBuf::new(),
        args: Vec::new(),
        status: SetupStatus::NotFound,
    })
}

#[must_use]
pub fn render_human(receipt: &ClaudeCodeSetupReceipt) -> String {
    format!(
        "cairn setup claude-code: {:?} {} in {}",
        receipt.status,
        receipt.server_name,
        receipt.config_path.display()
    )
}

fn config_path(scope: ClaudeCodeScope, project_dir: &Path, home_dir: &Path) -> PathBuf {
    match scope {
        ClaudeCodeScope::Local => home_dir.join(".claude.json"),
        ClaudeCodeScope::Project => project_dir.join(".mcp.json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(root: &Path, scope: ClaudeCodeScope) -> ClaudeCodeSetupOpts {
        ClaudeCodeSetupOpts {
            scope,
            project_dir: root.join("project"),
            home_dir: root.join("home"),
            server_name: "cairn".to_owned(),
            vault: root.join("vault"),
            binary: root.join("bin/cairn"),
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read json"))
            .expect("valid json")
    }

    #[test]
    fn setup_local_creates_project_entry_in_claude_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = opts(tmp.path(), ClaudeCodeScope::Local);

        let receipt = setup(&opts).expect("setup");

        assert_eq!(receipt.status, SetupStatus::Created);
        let root = read_json(&opts.home_dir.join(".claude.json"));
        let server = &root["projects"][opts.project_dir.display().to_string()]["mcpServers"]["cairn"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], opts.binary.display().to_string());
        assert_eq!(
            server["args"],
            json!(["--vault", opts.vault.display().to_string(), "mcp"])
        );
        assert_eq!(server["env"], json!({}));
    }

    #[test]
    fn setup_local_is_idempotent_and_keeps_file_bytes_stable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = opts(tmp.path(), ClaudeCodeScope::Local);
        let first = setup(&opts).expect("first setup");
        assert_eq!(first.status, SetupStatus::Created);
        let path = opts.home_dir.join(".claude.json");
        let before = std::fs::read(&path).expect("read before");

        let second = setup(&opts).expect("second setup");

        assert_eq!(second.status, SetupStatus::Unchanged);
        assert_eq!(std::fs::read(&path).expect("read after"), before);
    }

    #[test]
    fn setup_local_replaces_stale_cairn_entry_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = opts(tmp.path(), ClaudeCodeScope::Local);
        std::fs::create_dir_all(&opts.home_dir).expect("mkdir home");
        std::fs::write(
            opts.home_dir.join(".claude.json"),
            serde_json::to_vec_pretty(&json!({
                "projects": {
                    opts.project_dir.display().to_string(): {
                        "mcpServers": {
                            "cairn": {"command": "/old/cairn", "args": ["mcp"]},
                            "other": {"command": "other", "args": []}
                        },
                        "keep": true
                    }
                }
            }))
            .expect("serialize"),
        )
        .expect("write");

        let receipt = setup(&opts).expect("setup");

        assert_eq!(receipt.status, SetupStatus::Updated);
        let root = read_json(&opts.home_dir.join(".claude.json"));
        assert_eq!(root["projects"][opts.project_dir.display().to_string()]["keep"], true);
        assert_eq!(
            root["projects"][opts.project_dir.display().to_string()]["mcpServers"]["other"]["command"],
            "other"
        );
        assert_eq!(
            root["projects"][opts.project_dir.display().to_string()]["mcpServers"]["cairn"]["args"],
            json!(["--vault", opts.vault.display().to_string(), "mcp"])
        );
    }

    #[test]
    fn setup_project_creates_mcp_json_and_preserves_unrelated_servers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = opts(tmp.path(), ClaudeCodeScope::Project);
        std::fs::create_dir_all(&opts.project_dir).expect("mkdir project");
        std::fs::write(
            opts.project_dir.join(".mcp.json"),
            serde_json::to_vec_pretty(&json!({
                "mcpServers": {
                    "other": {"command": "other", "args": []}
                },
                "keep": "value"
            }))
            .expect("serialize"),
        )
        .expect("write");

        let receipt = setup(&opts).expect("setup");

        assert_eq!(receipt.status, SetupStatus::Created);
        let root = read_json(&opts.project_dir.join(".mcp.json"));
        assert_eq!(root["keep"], "value");
        assert_eq!(root["mcpServers"]["other"]["command"], "other");
        assert_eq!(root["mcpServers"]["cairn"]["type"], "stdio");
    }

    #[test]
    fn remove_local_deletes_only_selected_server() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let setup_opts = opts(tmp.path(), ClaudeCodeScope::Local);
        setup(&setup_opts).expect("setup");
        let remove_opts = ClaudeCodeRemoveOpts {
            scope: setup_opts.scope,
            project_dir: setup_opts.project_dir.clone(),
            home_dir: setup_opts.home_dir.clone(),
            server_name: setup_opts.server_name.clone(),
        };

        let receipt = remove(&remove_opts).expect("remove");

        assert_eq!(receipt.status, SetupStatus::Removed);
        let root = read_json(&setup_opts.home_dir.join(".claude.json"));
        assert!(root["projects"][setup_opts.project_dir.display().to_string()]["mcpServers"]["cairn"].is_null());
    }

    #[test]
    fn remove_project_returns_not_found_when_absent_and_does_not_create_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let setup_opts = opts(tmp.path(), ClaudeCodeScope::Project);
        let remove_opts = ClaudeCodeRemoveOpts {
            scope: setup_opts.scope,
            project_dir: setup_opts.project_dir.clone(),
            home_dir: setup_opts.home_dir.clone(),
            server_name: setup_opts.server_name.clone(),
        };

        let receipt = remove(&remove_opts).expect("remove");

        assert_eq!(receipt.status, SetupStatus::NotFound);
        assert!(!setup_opts.project_dir.join(".mcp.json").exists());
    }

    #[test]
    fn setup_rejects_non_object_config_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = opts(tmp.path(), ClaudeCodeScope::Project);
        std::fs::create_dir_all(&opts.project_dir).expect("mkdir project");
        std::fs::write(opts.project_dir.join(".mcp.json"), b"[]").expect("write");

        let err = setup(&opts).expect_err("non-object root rejected");

        assert!(matches!(err, ClaudeCodeSetupError::ConfigRoot { .. }));
        assert_eq!(err.exit_code(), 78);
    }

    #[test]
    fn setup_receipt_serializes_to_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = opts(tmp.path(), ClaudeCodeScope::Local);
        let receipt = setup(&opts).expect("setup");

        let value = serde_json::to_value(&receipt).expect("serialize receipt");

        assert_eq!(value["scope"], "local");
        assert_eq!(value["status"], "created");
        assert_eq!(value["server_name"], "cairn");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-cli setup::claude_code --lib
```

Expected: tests compile and fail because `setup()` returns `unchanged`, does not write `.claude.json` / `.mcp.json`, and `remove()` returns `not-found`.

- [ ] **Step 3: Implement the writer**

Replace the stub functions and helpers in `crates/cairn-cli/src/setup/claude_code.rs` with this implementation:

```rust
pub fn setup(opts: &ClaudeCodeSetupOpts) -> Result<ClaudeCodeSetupReceipt> {
    validate_server_name(&opts.server_name)?;
    let project_dir = absolute(&opts.project_dir)?;
    let home_dir = absolute(&opts.home_dir)?;
    let vault = absolute(&opts.vault)?;
    let binary = absolute(&opts.binary)?;
    let config_path = config_path(opts.scope, &project_dir, &home_dir);
    let mut root = read_config_or_empty(&config_path)?;
    let args = vec!["--vault".to_owned(), vault.display().to_string(), "mcp".to_owned()];
    let entry = json!({
        "type": "stdio",
        "command": binary.display().to_string(),
        "args": args,
        "env": {}
    });

    let status = {
        let servers = ensure_mcp_servers_mut(&mut root, opts.scope, &project_dir, &config_path)?;
        match servers.get(&opts.server_name) {
            Some(existing) if existing == &entry => SetupStatus::Unchanged,
            Some(_) => {
                servers.insert(opts.server_name.clone(), entry);
                SetupStatus::Updated
            }
            None => {
                servers.insert(opts.server_name.clone(), entry);
                SetupStatus::Created
            }
        }
    };

    if status != SetupStatus::Unchanged {
        write_config(&config_path, &root)?;
    }

    Ok(ClaudeCodeSetupReceipt {
        scope: opts.scope,
        config_path,
        server_name: opts.server_name.clone(),
        command: binary,
        args: vec!["--vault".to_owned(), vault.display().to_string(), "mcp".to_owned()],
        status,
    })
}

pub fn remove(opts: &ClaudeCodeRemoveOpts) -> Result<ClaudeCodeSetupReceipt> {
    validate_server_name(&opts.server_name)?;
    let project_dir = absolute(&opts.project_dir)?;
    let home_dir = absolute(&opts.home_dir)?;
    let config_path = config_path(opts.scope, &project_dir, &home_dir);
    if !config_path.exists() {
        return Ok(ClaudeCodeSetupReceipt {
            scope: opts.scope,
            config_path,
            server_name: opts.server_name.clone(),
            command: PathBuf::new(),
            args: Vec::new(),
            status: SetupStatus::NotFound,
        });
    }

    let mut root = read_config_or_empty(&config_path)?;
    let removed = mcp_servers_mut_if_present(&mut root, opts.scope, &project_dir)
        .and_then(|servers| servers.remove(&opts.server_name))
        .is_some();

    let status = if removed {
        write_config(&config_path, &root)?;
        SetupStatus::Removed
    } else {
        SetupStatus::NotFound
    };

    Ok(ClaudeCodeSetupReceipt {
        scope: opts.scope,
        config_path,
        server_name: opts.server_name.clone(),
        command: PathBuf::new(),
        args: Vec::new(),
        status,
    })
}

#[must_use]
pub fn render_human(receipt: &ClaudeCodeSetupReceipt) -> String {
    let action = match receipt.status {
        SetupStatus::Created => "registered",
        SetupStatus::Updated => "updated",
        SetupStatus::Unchanged => "already registered",
        SetupStatus::Removed => "removed",
        SetupStatus::NotFound => "not found",
    };
    let mut lines = vec![format!(
        "cairn setup claude-code: {action} `{}` in {}",
        receipt.server_name,
        receipt.config_path.display()
    )];
    if !receipt.command.as_os_str().is_empty() {
        lines.push(format!("  command: {}", receipt.command.display()));
        lines.push(format!("  args: {}", receipt.args.join(" ")));
    }
    if matches!(
        receipt.status,
        SetupStatus::Created | SetupStatus::Updated | SetupStatus::Unchanged
    ) {
        lines.push("  verify: cairn doctor claude-code".to_owned());
    }
    lines.join("\n")
}

fn validate_server_name(server_name: &str) -> Result<()> {
    if server_name.trim().is_empty() {
        return Err(ClaudeCodeSetupError::InvalidOption(
            "--server-name must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.components().collect())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path).components().collect())
            .map_err(|source| ClaudeCodeSetupError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

fn read_config_or_empty(path: &Path) -> Result<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Map::new()));
        }
        Err(source) => {
            return Err(ClaudeCodeSetupError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|source| ClaudeCodeSetupError::ConfigParse {
            path: path.to_path_buf(),
            source,
        })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(ClaudeCodeSetupError::ConfigRoot {
            path: path.to_path_buf(),
        })
    }
}

fn write_config(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ClaudeCodeSetupError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(root).map_err(|source| {
        ClaudeCodeSetupError::ConfigParse {
            path: path.to_path_buf(),
            source,
        }
    })?;
    bytes.push(b'\n');
    std::fs::write(path, bytes).map_err(|source| ClaudeCodeSetupError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn ensure_object_child<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    let value = object
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(ClaudeCodeSetupError::ConfigRoot {
            path: path.to_path_buf(),
        }),
    }
}

fn ensure_mcp_servers_mut<'a>(
    root: &'a mut Value,
    scope: ClaudeCodeScope,
    project_dir: &Path,
    path: &Path,
) -> Result<&'a mut Map<String, Value>> {
    let root = root
        .as_object_mut()
        .ok_or_else(|| ClaudeCodeSetupError::ConfigRoot {
            path: path.to_path_buf(),
        })?;
    match scope {
        ClaudeCodeScope::Project => ensure_object_child(root, "mcpServers", path),
        ClaudeCodeScope::Local => {
            let projects = ensure_object_child(root, "projects", path)?;
            let project = ensure_object_child(projects, &project_dir.display().to_string(), path)?;
            ensure_object_child(project, "mcpServers", path)
        }
    }
}

fn mcp_servers_mut_if_present<'a>(
    root: &'a mut Value,
    scope: ClaudeCodeScope,
    project_dir: &Path,
) -> Option<&'a mut Map<String, Value>> {
    match scope {
        ClaudeCodeScope::Project => root.get_mut("mcpServers")?.as_object_mut(),
        ClaudeCodeScope::Local => root
            .get_mut("projects")?
            .get_mut(project_dir.display().to_string())?
            .get_mut("mcpServers")?
            .as_object_mut(),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p cairn-cli setup::claude_code --lib
```

Expected: all `setup::claude_code` tests pass.

- [ ] **Step 5: Commit Task 1**

Run:

```bash
git add crates/cairn-cli/src/lib.rs crates/cairn-cli/src/setup.rs crates/cairn-cli/src/setup/claude_code.rs
git commit -m "feat(cli): add Claude Code MCP config writer"
```

## Task 2: Command Tree and Dispatch

**Files:**
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Write failing command tests in `crates/cairn-cli/tests/claude_code_setup.rs`**

Create the integration test file with the help and local setup tests first:

```rust
#![allow(missing_docs)]

use assert_cmd::Command;
use serde_json::{Value, json};

fn cli() -> Command {
    Command::cargo_bin("cairn").expect("cargo bin cairn")
}

fn parse_stdout_json(out: std::process::Output) -> Value {
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout")
}

fn seed_vault(root: &std::path::Path) {
    let cairn_dir = root.join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("mkdir .cairn");
    std::fs::write(cairn_dir.join("vault.id"), "01J8WSKJ5T0R6XKYV5T2P4ZQVD")
        .expect("write vault.id");
    std::fs::write(
        cairn_dir.join("config.yaml"),
        "search:\n  local_embeddings: false\nmcp:\n  stdio:\n    single_tenant: false\n",
    )
    .expect("write config.yaml");
}

fn synth_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_vault(dir.path());
    dir
}

#[test]
fn setup_help_lists_claude_code() {
    let out = cli()
        .args(["setup", "--help"])
        .output()
        .expect("cairn setup --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("claude-code"), "setup help missing claude-code: {stdout}");
}

#[test]
fn setup_claude_code_writes_local_scope_by_default() {
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let vault = synth_vault();
    let bin = env!("CARGO_BIN_EXE_cairn");

    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("vault utf-8"),
            "setup",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("project utf-8"),
            "--home-dir",
            home.path().to_str().expect("home utf-8"),
            "--binary",
            bin,
            "--json",
        ])
        .output()
        .expect("cairn setup claude-code");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["scope"], "local");
    assert_eq!(receipt["status"], "created");

    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude.json")).expect("read .claude.json"),
    )
    .expect("json");
    let server =
        &config["projects"][project.path().display().to_string()]["mcpServers"]["cairn"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], bin);
    assert_eq!(
        server["args"],
        json!(["--vault", vault.path().display().to_string(), "mcp"])
    );
    assert_eq!(server["env"], json!({}));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo nextest run -p cairn-cli --test claude_code_setup
```

Expected: fails because `cairn setup` is not registered in the clap command tree.

- [ ] **Step 3: Add setup subcommands to `command.rs`**

Add `setup` to the crate import:

```rust
use crate::{coord, doctor, generated, hooks, identity, setup, skill, verbs};
```

Add `.subcommand(setup_subcommand())` after `.subcommand(skill_subcommand())` in `build_command()`.

Add these functions near `skill_subcommand()`:

```rust
fn setup_subcommand() -> clap::Command {
    clap::Command::new("setup")
        .about("Configure harness integrations")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(claude_code_setup_subcommand())
}

fn claude_code_setup_subcommand() -> clap::Command {
    claude_code_common_args(
        clap::Command::new("claude-code")
            .about("Register Cairn as a Claude Code stdio MCP server")
            .subcommand(
                claude_code_common_args(
                    clap::Command::new("remove")
                        .about("Remove the Cairn MCP server from Claude Code config"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt instead of human-readable output"),
                ),
            ),
    )
    .arg(
        clap::Arg::new("binary")
            .long("binary")
            .value_name("PATH")
            .help("Path to the cairn binary to register (default: current executable)"),
    )
    .arg(
        clap::Arg::new("json")
            .long("json")
            .action(clap::ArgAction::SetTrue)
            .help("Emit JSON receipt instead of human-readable output"),
    )
}

fn claude_code_common_args(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("scope")
            .long("scope")
            .value_name("SCOPE")
            .default_value("local")
            .value_parser(clap::builder::EnumValueParser::<
                setup::claude_code::ClaudeCodeScope,
            >::new())
            .help("Claude Code config scope (local or project)"),
    )
    .arg(
        clap::Arg::new("project-dir")
            .long("project-dir")
            .value_name("PATH")
            .help("Project directory for local-scope ~/.claude.json or project-scope .mcp.json"),
    )
    .arg(
        clap::Arg::new("home-dir")
            .long("home-dir")
            .value_name("PATH")
            .help("Override home directory for ~/.claude.json"),
    )
    .arg(
        clap::Arg::new("server-name")
            .long("server-name")
            .value_name("NAME")
            .default_value("cairn")
            .help("Claude Code MCP server name"),
    )
}
```

- [ ] **Step 4: Add main dispatch**

Update the import in `crates/cairn-cli/src/main.rs`:

```rust
use cairn_cli::{command, doctor, hooks, identity, plugins, repair, setup, verbs};
```

Exclude setup from the top-level vault guard by adding `"setup"` to `subcommand_needs_vault_guard()`.

Add this match arm after `Some(("skill", sub)) => run_skill(sub),`:

```rust
        Some(("setup", sub)) => run_setup(sub, explicit_vault.as_deref()),
```

Add these functions near `run_skill()`:

```rust
fn run_setup(matches: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    match matches.subcommand() {
        Some(("claude-code", sub)) => run_setup_claude_code(sub, explicit_vault),
        _ => unreachable!(
            "clap subcommand_required(true) on setup ensures a subcommand is always present"
        ),
    }
}

fn run_setup_claude_code(matches: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    match matches.subcommand() {
        Some(("remove", sub)) => run_setup_claude_code_remove(sub),
        None => run_setup_claude_code_write(matches, explicit_vault),
        Some((name, _)) => {
            eprintln!("cairn setup claude-code: unknown subcommand `{name}`");
            ExitCode::from(64)
        }
    }
}

fn setup_project_dir(matches: &ArgMatches) -> Result<std::path::PathBuf, ExitCode> {
    if let Some(path) = matches.get_one::<String>("project-dir") {
        Ok(std::path::PathBuf::from(path))
    } else {
        std::env::current_dir().map_err(|e| {
            eprintln!("cairn setup claude-code: failed to resolve current directory — {e}");
            ExitCode::from(69)
        })
    }
}

fn setup_home_dir(matches: &ArgMatches) -> Result<std::path::PathBuf, ExitCode> {
    if let Some(path) = matches.get_one::<String>("home-dir") {
        Ok(std::path::PathBuf::from(path))
    } else {
        std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or_else(|| {
            eprintln!("cairn setup claude-code: HOME is not set; pass --home-dir");
            ExitCode::from(69)
        })
    }
}

fn setup_scope(matches: &ArgMatches) -> setup::claude_code::ClaudeCodeScope {
    *matches
        .get_one::<setup::claude_code::ClaudeCodeScope>("scope")
        .expect("scope has default")
}

fn setup_server_name(matches: &ArgMatches) -> String {
    matches
        .get_one::<String>("server-name")
        .expect("server-name has default")
        .clone()
}

fn setup_binary(matches: &ArgMatches) -> Result<std::path::PathBuf, ExitCode> {
    if let Some(path) = matches.get_one::<String>("binary") {
        Ok(std::path::PathBuf::from(path))
    } else {
        std::env::current_exe().map_err(|e| {
            eprintln!("cairn setup claude-code: failed to resolve current executable — {e}");
            ExitCode::from(69)
        })
    }
}

fn resolve_setup_vault(
    explicit_vault: Option<&str>,
    project_dir: &std::path::Path,
) -> Result<std::path::PathBuf, ExitCode> {
    let store = registry_store().map_err(|e| {
        eprintln!("cairn setup claude-code: registry path error — {e:#}");
        ExitCode::from(78)
    })?;
    let opts = cairn_cli::vault::ResolveOpts {
        explicit: explicit_vault.map(str::to_owned),
        cwd: Some(project_dir.to_path_buf()),
        store: &store,
    };
    let vault_root = cairn_cli::vault::resolve_vault(opts).map_err(|e| {
        eprintln!(
            "cairn setup claude-code: no active Cairn vault resolved — {e:#}. \
             Pass --vault or run cairn bootstrap first."
        );
        ExitCode::from(78)
    })?;
    match verbs::status::probe_vault_binding(&vault_root) {
        verbs::status::VaultBinding::Bound => Ok(vault_root),
        verbs::status::VaultBinding::Unbound => {
            eprintln!(
                "cairn setup claude-code: {} is not a bound Cairn vault \
                 (missing .cairn/vault.id) — run `cairn bootstrap --vault-path {}` first",
                vault_root.display(),
                vault_root.display()
            );
            Err(ExitCode::from(78))
        }
        verbs::status::VaultBinding::Invalid(reason) => {
            eprintln!("cairn setup claude-code: vault binding error — {reason}");
            Err(ExitCode::from(78))
        }
    }
}

fn run_setup_claude_code_write(matches: &ArgMatches, explicit_vault: Option<&str>) -> ExitCode {
    let project_dir = match setup_project_dir(matches) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let home_dir = match setup_home_dir(matches) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let binary = match setup_binary(matches) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let vault = match resolve_setup_vault(explicit_vault, &project_dir) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let opts = setup::claude_code::ClaudeCodeSetupOpts {
        scope: setup_scope(matches),
        project_dir,
        home_dir,
        server_name: setup_server_name(matches),
        vault,
        binary,
    };
    let json = matches.get_flag("json");
    match setup::claude_code::setup(&opts) {
        Ok(receipt) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt)
                        .expect("invariant: setup receipt is serializable")
                );
            } else {
                println!("{}", setup::claude_code::render_human(&receipt));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn setup claude-code: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

fn run_setup_claude_code_remove(matches: &ArgMatches) -> ExitCode {
    let project_dir = match setup_project_dir(matches) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let home_dir = match setup_home_dir(matches) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let opts = setup::claude_code::ClaudeCodeRemoveOpts {
        scope: setup_scope(matches),
        project_dir,
        home_dir,
        server_name: setup_server_name(matches),
    };
    let json = matches.get_flag("json");
    match setup::claude_code::remove(&opts) {
        Ok(receipt) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt)
                        .expect("invariant: setup receipt is serializable")
                );
            } else {
                println!("{}", setup::claude_code::render_human(&receipt));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn setup claude-code remove: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run:

```bash
cargo nextest run -p cairn-cli --test claude_code_setup
```

Expected: the two tests pass.

- [ ] **Step 6: Commit Task 2**

Run:

```bash
git add crates/cairn-cli/src/command.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/claude_code_setup.rs
git commit -m "feat(cli): wire Claude Code setup command"
```

## Task 3: CLI Coverage and Doctor Integration

**Files:**
- Modify: `crates/cairn-cli/tests/claude_code_setup.rs`
- Modify: `crates/cairn-cli/src/doctor.rs`

- [ ] **Step 1: Add remaining CLI tests**

Append these helpers and tests to `crates/cairn-cli/tests/claude_code_setup.rs`:

```rust
fn write_hook_settings(project: &std::path::Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = json!({
        "hooks": {
            "SessionStart": [{"command": "cairn hook SessionStart"}],
            "UserPromptSubmit": [{"command": "cairn hook UserPromptSubmit"}],
            "PreToolUse": [{"command": "cairn hook PreToolUse"}],
            "PostToolUse": [{"command": "cairn hook PostToolUse"}],
            "Stop": [{"command": "cairn hook Stop"}]
        }
    });
    std::fs::write(
        claude_dir.join("settings.local.json"),
        serde_json::to_vec_pretty(&body).expect("serialize settings.local.json"),
    )
    .expect("write settings.local.json");
}

#[test]
fn setup_claude_code_is_idempotent() {
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let vault = synth_vault();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let args = [
        "--vault",
        vault.path().to_str().expect("vault utf-8"),
        "setup",
        "claude-code",
        "--project-dir",
        project.path().to_str().expect("project utf-8"),
        "--home-dir",
        home.path().to_str().expect("home utf-8"),
        "--binary",
        bin,
        "--json",
    ];
    let first = cli().args(args).output().expect("first setup");
    assert_eq!(first.status.code(), Some(0));
    let path = home.path().join(".claude.json");
    let before = std::fs::read(&path).expect("read first config");

    let second = cli().args(args).output().expect("second setup");

    assert_eq!(second.status.code(), Some(0));
    let receipt = parse_stdout_json(second);
    assert_eq!(receipt["status"], "unchanged");
    assert_eq!(std::fs::read(&path).expect("read second config"), before);
}

#[test]
fn setup_claude_code_project_scope_writes_mcp_json() {
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let vault = synth_vault();
    let bin = env!("CARGO_BIN_EXE_cairn");

    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("vault utf-8"),
            "setup",
            "claude-code",
            "--scope",
            "project",
            "--project-dir",
            project.path().to_str().expect("project utf-8"),
            "--home-dir",
            home.path().to_str().expect("home utf-8"),
            "--binary",
            bin,
            "--json",
        ])
        .output()
        .expect("setup project scope");

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["scope"], "project");
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".mcp.json")).expect("read .mcp.json"),
    )
    .expect("json");
    assert_eq!(config["mcpServers"]["cairn"]["command"], bin);
}

#[test]
fn setup_claude_code_remove_deletes_project_entry() {
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let vault = synth_vault();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let common = [
        "--project-dir",
        project.path().to_str().expect("project utf-8"),
        "--home-dir",
        home.path().to_str().expect("home utf-8"),
        "--scope",
        "project",
    ];
    let setup_out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("vault utf-8"),
            "setup",
            "claude-code",
            common[0],
            common[1],
            common[2],
            common[3],
            common[4],
            common[5],
            "--binary",
            bin,
            "--json",
        ])
        .output()
        .expect("setup");
    assert_eq!(setup_out.status.code(), Some(0));

    let remove_out = cli()
        .args([
            "setup",
            "claude-code",
            "remove",
            common[0],
            common[1],
            common[2],
            common[3],
            common[4],
            common[5],
            "--json",
        ])
        .output()
        .expect("remove");

    assert_eq!(remove_out.status.code(), Some(0));
    let receipt = parse_stdout_json(remove_out);
    assert_eq!(receipt["status"], "removed");
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(project.path().join(".mcp.json")).expect("read .mcp.json"),
    )
    .expect("json");
    assert!(config["mcpServers"]["cairn"].is_null());
}

#[test]
fn doctor_succeeds_after_setup_with_hook_settings() {
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let vault = synth_vault();
    let bin = env!("CARGO_BIN_EXE_cairn");
    write_hook_settings(project.path());

    let setup_out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("vault utf-8"),
            "setup",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("project utf-8"),
            "--home-dir",
            home.path().to_str().expect("home utf-8"),
            "--binary",
            bin,
            "--json",
        ])
        .output()
        .expect("setup");
    assert_eq!(setup_out.status.code(), Some(0));

    let doctor_out = cli()
        .args([
            "doctor",
            "claude-code",
            "--project-dir",
            project.path().to_str().expect("project utf-8"),
            "--home-dir",
            home.path().to_str().expect("home utf-8"),
            "--json",
        ])
        .output()
        .expect("doctor");

    assert_eq!(
        doctor_out.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&doctor_out.stdout),
        String::from_utf8_lossy(&doctor_out.stderr)
    );
    let receipt = parse_stdout_json(doctor_out);
    assert_eq!(receipt["ok"], true);
}
```

- [ ] **Step 2: Run tests to verify any new failures**

Run:

```bash
cargo nextest run -p cairn-cli --test claude_code_setup
```

Expected: remove and doctor tests may fail if dispatch or writer edge cases are incomplete.

- [ ] **Step 3: Update doctor remediation text**

In `crates/cairn-cli/src/doctor.rs`, replace the missing-registration remediation string with:

```rust
"run `cairn setup claude-code --vault <name-or-path>` from the project, \
then rerun `cairn doctor claude-code`"
```

Replace the non-stdio remediation with:

```rust
"run `cairn setup claude-code --vault <name-or-path>` to replace the entry \
with a stdio Cairn MCP registration, then rerun doctor"
```

Replace the missing-`mcp` remediation with:

```rust
"run `cairn setup claude-code --vault <name-or-path>` so the Claude Code \
registration launches the Cairn binary with the `mcp` argument"
```

- [ ] **Step 4: Run doctor and setup tests**

Run:

```bash
cargo nextest run -p cairn-cli --test claude_code_setup
cargo nextest run -p cairn-cli --test doctor_cli
```

Expected: both test binaries pass.

- [ ] **Step 5: Commit Task 3**

Run:

```bash
git add crates/cairn-cli/tests/claude_code_setup.rs crates/cairn-cli/src/doctor.rs
git commit -m "test(cli): cover Claude Code setup and doctor flow"
```

## Task 4: Documentation and Generated References

**Files:**
- Create: `docs/site/src/usage/claude-code.md`
- Modify: `docs/site/src/usage/mcp.md`
- Modify: `docs/site/src/SUMMARY.md`
- Modify/Create: `docs/site/src/reference/generated/cli.md`
- Modify/Create: `docs/site/src/reference/generated/commands/setup.md`
- Modify/Create: `docs/site/src/reference/generated/commands/setup-claude-code.md`
- Modify/Create: `docs/site/src/reference/generated/commands/setup-claude-code-remove.md`

- [ ] **Step 1: Write the Claude Code usage page**

Create `docs/site/src/usage/claude-code.md`:

````markdown
# Claude Code

Claude Code is Cairn's v0.1 reference consumer. Register Cairn as a stdio MCP
server with one command:

```bash
cairn setup claude-code --vault work
```

By default this writes a local-scope entry in `~/.claude.json` for the current
project. Local scope is private to your user account and does not create a
shareable `.mcp.json`.

To write project scope explicitly:

```bash
cairn setup claude-code --scope project --vault work
```

Project scope writes `.mcp.json` in the project directory. Commit that file
only when the absolute binary and vault paths are intentional for the team, or
edit them to a team-supported path first.

Verify the registration:

```bash
cairn doctor claude-code
```

Remove only the Cairn server entry:

```bash
cairn setup claude-code remove
```

Cairn writes no API keys or provider credentials into Claude Code config. The
generated MCP entry uses an empty `env` object and launches:

```bash
cairn --vault <vault> mcp
```
````

- [ ] **Step 2: Link from MCP usage page**

At the top of `docs/site/src/usage/mcp.md`, replace the current opening with:

````markdown
# MCP

For Claude Code, prefer the first-party setup command:

```bash
cairn setup claude-code --vault <name-or-path>
cairn doctor claude-code
```

See [Claude Code](claude-code.md) for the full reference-consumer workflow.

`cairn-mcp` contains the lower-level MCP adapter crate, generated tool
declarations, plugin manifest, and stdio serving entry point.
````

Keep the existing "Current truth" bullets below this opening.

- [ ] **Step 3: Regenerate command reference**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen -- --write
```

Expected: generated CLI reference updates and new setup command files appear under `docs/site/src/reference/generated/commands/`.

- [ ] **Step 4: Update `SUMMARY.md`**

Add this usage entry after MCP:

```markdown
- [Claude Code](usage/claude-code.md)
```

Add these generated command entries near the other top-level command entries:

```markdown
  - [`cairn setup`](reference/generated/commands/setup.md)
  - [`cairn setup claude-code`](reference/generated/commands/setup-claude-code.md)
  - [`cairn setup claude-code remove`](reference/generated/commands/setup-claude-code-remove.md)
```

- [ ] **Step 5: Check doc generation**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen -- --check
```

Expected: `cairn-docgen` exits 0 with no drift.

- [ ] **Step 6: Commit Task 4**

Run:

```bash
git add docs/site/src/usage/claude-code.md docs/site/src/usage/mcp.md docs/site/src/SUMMARY.md docs/site/src/reference/generated
git commit -m "docs: add Claude Code setup workflow"
```

## Task 5: Final Verification

**Files:**
- No planned edits.

- [ ] **Step 1: Run targeted tests**

Run:

```bash
cargo nextest run -p cairn-cli --test claude_code_setup
cargo nextest run -p cairn-cli --test doctor_cli
cargo nextest run -p cairn-cli --test mcp_subcommand
```

Expected: all targeted tests pass.

- [ ] **Step 2: Run core boundary check**

Run:

```bash
scripts/check-core-boundary.sh
```

Expected: exits 0; no `cairn-core` dependency boundary violation.

- [ ] **Step 3: Run docs check**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen -- --check
```

Expected: exits 0 with no generated reference drift.

- [ ] **Step 4: Run workspace verification**

Run:

```bash
cargo nextest run --workspace
cargo test --doc --workspace
```

Expected: all workspace tests and doctests pass. If this is too slow in the current session, run the targeted suite plus `cargo build --workspace` and record the skipped command explicitly in the final response.

- [ ] **Step 5: Inspect final diff**

Run:

```bash
git status --short
git diff --check
git log --oneline -n 6
```

Expected: only intended files are changed, whitespace check passes, and the branch contains the spec/plan plus implementation commits.
