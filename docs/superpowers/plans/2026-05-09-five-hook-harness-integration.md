# Five-Hook Harness Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the v0.1 `cairn hook <name>` CLI surface for `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, and `Stop`.

**Architecture:** Keep the public hook dispatcher in `cairn-cli`, with one shared result schema and one per-hook handler module. Because the current SQLite store and workflow crates are still stubs, hooks write durable JSON artifacts under the selected vault path as the first concrete trace/enqueue boundary that store/orchestrator issues can consume or replace.

**Tech Stack:** Rust 2024, `clap`, `serde`, `serde_json`, `ulid`, standard-library filesystem APIs, existing `cairn-cli` integration test style.

---

## File Structure

Create these files:

- `crates/cairn-cli/src/hooks/mod.rs` — hook name parsing, shared payload loading, `HookResult`, `HookError`, and command dispatch.
- `crates/cairn-cli/src/hooks/artifact.rs` — durable JSON artifact writer for trace, hot-memory, and queue artifacts.
- `crates/cairn-cli/src/hooks/session_start.rs` — `SessionStart` handler.
- `crates/cairn-cli/src/hooks/user_prompt_submit.rs` — `UserPromptSubmit` handler.
- `crates/cairn-cli/src/hooks/pre_tool_use.rs` — `PreToolUse` handler.
- `crates/cairn-cli/src/hooks/post_tool_use.rs` — `PostToolUse` handler.
- `crates/cairn-cli/src/hooks/stop.rs` — `Stop` handler.
- `crates/cairn-cli/src/hooks/queue.rs` — stop post-turn work request artifact.
- `crates/cairn-cli/tests/hook_cli.rs` — hook CLI contract, JSON shape, lifecycle, latency-boundary, and failure-mode integration tests.

Modify these files:

- `crates/cairn-cli/src/lib.rs` — export the new `hooks` module.
- `crates/cairn-cli/src/main.rs` — register and dispatch the `hook` subcommand.
- `crates/cairn-cli/tests/cli.rs` — assert top-level help lists `hook`.
- `docs/design/design-brief.md` — update the stale §9.3 `PreCompact` references to the canonical `PreToolUse` v0.1 hook set.

Do not edit generated files by hand.

---

### Task 1: Add Failing Hook CLI Contract Tests

**Files:**
- Create: `crates/cairn-cli/tests/hook_cli.rs`
- Modify: `crates/cairn-cli/tests/cli.rs`

- [ ] **Step 1: Write failing top-level help assertion**

In `crates/cairn-cli/tests/cli.rs`, extend `help_flag_lists_all_eight_verbs()` so it also requires `hook`.

```rust
#[test]
fn help_flag_lists_all_eight_verbs() {
    let out = cli().arg("--help").output().expect("cairn --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
        "hook",
    ] {
        assert!(
            stdout.contains(verb),
            "help output missing verb {verb}, got:\n{stdout}",
        );
    }
}
```

- [ ] **Step 2: Write failing canonical hook-set tests**

Create `crates/cairn-cli/tests/hook_cli.rs`:

```rust
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn hook_help_lists_canonical_five_hooks() {
    let out = cli().args(["hook", "--help"]).output().expect("cairn hook --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for hook in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ] {
        assert!(stdout.contains(hook), "hook help missing {hook}: {stdout}");
    }
}

#[test]
fn precompact_is_not_a_canonical_hook() {
    let out = cli()
        .args(["hook", "PreCompact", "--json"])
        .output()
        .expect("cairn hook PreCompact");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
}

#[test]
fn unknown_hook_name_exits_usage_64() {
    let out = cli()
        .args(["hook", "DefinitelyNotAHook", "--json"])
        .output()
        .expect("cairn hook unknown");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
}
```

- [ ] **Step 3: Run tests and verify they fail for the missing CLI surface**

Run:

```bash
cargo test -p cairn-cli --test cli help_flag_lists_all_eight_verbs
cargo test -p cairn-cli --test hook_cli hook_help_lists_canonical_five_hooks
```

Expected:

- First command fails because top-level help does not include `hook`.
- Second command fails because `crates/cairn-cli/tests/hook_cli.rs` exists but `cairn hook` is not registered.

- [ ] **Step 4: Leave the red tests in the working tree for Task 2**

Do not commit yet. Task 2 adds the implementation that turns these tests green, then commits tests
and implementation together.

---

### Task 2: Register `cairn hook <name>` and Shared Hook Types

**Files:**
- Create: `crates/cairn-cli/src/hooks/mod.rs`
- Modify: `crates/cairn-cli/src/lib.rs`
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Add the hooks module export**

In `crates/cairn-cli/src/lib.rs`:

```rust
pub mod config;
pub mod hooks;
pub mod plugins;
pub mod verbs;
```

- [ ] **Step 2: Add shared hook types and command builder**

Create `crates/cairn-cli/src/hooks/mod.rs`:

```rust
//! Harness lifecycle hook command handlers.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::generated::common::Ulid;
use cairn_core::generated::errors::ErrorCode;
use clap::ArgMatches;
use serde::Serialize;
use serde_json::Value;

use crate::verbs::envelope::{emit_json, new_operation_id};

pub mod artifact;
pub mod post_tool_use;
pub mod pre_tool_use;
pub mod queue;
pub mod session_start;
pub mod stop;
pub mod user_prompt_submit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HookName {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

impl HookName {
    pub const ALL: [&'static str; 5] = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }

    fn parse(value: &str) -> Result<Self, HookError> {
        match value {
            "SessionStart" => Ok(Self::SessionStart),
            "UserPromptSubmit" => Ok(Self::UserPromptSubmit),
            "PreToolUse" => Ok(Self::PreToolUse),
            "PostToolUse" => Ok(Self::PostToolUse),
            "Stop" => Ok(Self::Stop),
            other => Err(HookError::invalid_args(format!(
                "unknown hook `{other}`; expected one of {}",
                Self::ALL.join(", ")
            ))),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HookResult {
    pub ok: bool,
    pub hook: HookName,
    pub operation_id: Ulid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<HookArtifacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<HookErrorBody>,
}

#[derive(Debug, Default, Serialize)]
pub struct HookArtifacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<Ulid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hot_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queued_jobs: Vec<Ulid>,
}

#[derive(Debug, Serialize)]
pub struct HookErrorBody {
    pub code: &'static str,
    pub message: String,
    pub retry_guidance: String,
}

#[derive(Debug)]
pub struct HookError {
    code: ErrorCode,
    message: String,
    retry_guidance: String,
}

impl HookError {
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidArgs,
            message: message.into(),
            retry_guidance: "fix the hook payload or hook name and retry the same command".to_owned(),
        }
    }

    pub fn internal(message: impl Into<String>, retry_guidance: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
            retry_guidance: retry_guidance.into(),
        }
    }

    fn into_body(self) -> HookErrorBody {
        HookErrorBody {
            code: self.code.as_str(),
            message: self.message,
            retry_guidance: self.retry_guidance,
        }
    }
}

pub fn command() -> clap::Command {
    clap::Command::new("hook")
        .about("Run a Cairn harness lifecycle hook")
        .arg(
            clap::Arg::new("name")
                .help("Hook name")
                .required(true)
                .value_parser(HookName::ALL),
        )
        .arg(
            clap::Arg::new("payload")
                .long("payload")
                .value_name("JSON")
                .help("Hook payload JSON object"),
        )
        .arg(
            clap::Arg::new("payload-file")
                .long("payload-file")
                .value_name("PATH")
                .value_parser(clap::builder::PathBufValueParser::new())
                .conflicts_with("payload")
                .help("Read hook payload JSON object from a file"),
        )
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .value_parser(clap::builder::PathBufValueParser::new())
                .help("Vault root directory used for hook artifacts"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON instead of human-readable output"),
        )
}

#[must_use]
pub fn run(matches: &ArgMatches) -> ExitCode {
    let json = matches.get_flag("json");
    let operation_id = new_operation_id();
    let hook = match matches.get_one::<String>("name").map(String::as_str) {
        Some(name) => match HookName::parse(name) {
            Ok(hook) => hook,
            Err(err) => return emit_failure(HookName::Stop, operation_id, err, json, 64),
        },
        None => {
            return emit_failure(
                HookName::Stop,
                operation_id,
                HookError::invalid_args("hook name is required"),
                json,
                64,
            );
        }
    };
    let payload = match load_payload(matches) {
        Ok(payload) => payload,
        Err(err) => return emit_failure(hook, operation_id, err, json, 1),
    };
    let vault_path = matches
        .get_one::<PathBuf>("vault-path")
        .map_or_else(|| PathBuf::from("."), Clone::clone);

    let outcome = match hook {
        HookName::SessionStart => session_start::run(&vault_path, operation_id.clone(), payload),
        HookName::UserPromptSubmit => user_prompt_submit::run(&vault_path, operation_id.clone(), payload),
        HookName::PreToolUse => pre_tool_use::run(&vault_path, operation_id.clone(), payload),
        HookName::PostToolUse => post_tool_use::run(&vault_path, operation_id.clone(), payload),
        HookName::Stop => stop::run(&vault_path, operation_id.clone(), payload),
    };

    match outcome {
        Ok(artifacts) => emit_success(hook, operation_id, artifacts, json),
        Err(err) => emit_failure(hook, operation_id, err, json, 1),
    }
}

fn load_payload(matches: &ArgMatches) -> Result<Value, HookError> {
    let value = if let Some(raw) = matches.get_one::<String>("payload") {
        serde_json::from_str(raw).map_err(|err| {
            HookError::invalid_args(format!("payload must be valid JSON: {err}"))
        })?
    } else if let Some(path) = matches.get_one::<PathBuf>("payload-file") {
        let raw = std::fs::read_to_string(path).map_err(|err| {
            HookError::internal(
                format!("failed to read payload file `{}`: {err}", path.display()),
                "restore access to the payload file and retry the same hook command",
            )
        })?;
        serde_json::from_str(&raw).map_err(|err| {
            HookError::invalid_args(format!("payload file must contain valid JSON: {err}"))
        })?
    } else {
        serde_json::json!({})
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(HookError::invalid_args("hook payload must be a JSON object"))
    }
}

pub fn require_string(payload: &Value, field: &'static str) -> Result<String, HookError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| HookError::invalid_args(format!("payload.{field} must be a non-empty string")))
}

pub fn payload_object(payload: &Value) -> serde_json::Map<String, Value> {
    payload.as_object().cloned().unwrap_or_default()
}

fn emit_success(
    hook: HookName,
    operation_id: Ulid,
    artifacts: HookArtifacts,
    json: bool,
) -> ExitCode {
    let result = HookResult {
        ok: true,
        hook,
        operation_id,
        artifacts: Some(artifacts),
        error: None,
    };
    if json {
        emit_json(&result);
    } else {
        println!("cairn hook {}: ok (operation_id: {})", hook.as_str(), result.operation_id.0);
    }
    ExitCode::SUCCESS
}

fn emit_failure(
    hook: HookName,
    operation_id: Ulid,
    err: HookError,
    json: bool,
    code: u8,
) -> ExitCode {
    let result = HookResult {
        ok: false,
        hook,
        operation_id,
        artifacts: None,
        error: Some(err.into_body()),
    };
    if json {
        emit_json(&result);
    } else if let Some(error) = &result.error {
        eprintln!(
            "cairn hook {}: {} - {} (operation_id: {}; retry: {})",
            hook.as_str(),
            error.code,
            error.message,
            result.operation_id.0,
            error.retry_guidance
        );
    }
    ExitCode::from(code)
}
```

- [ ] **Step 3: Register `hook` in the binary**

In `crates/cairn-cli/src/main.rs`, add the subcommand:

```rust
.subcommand(cairn_cli::hooks::command())
```

Add dispatch:

```rust
Some(("hook", sub)) => cairn_cli::hooks::run(sub),
```

- [ ] **Step 4: Run the contract tests**

Run:

```bash
cargo test -p cairn-cli --test cli help_flag_lists_all_eight_verbs
cargo test -p cairn-cli --test hook_cli
```

Expected: all listed tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/lib.rs crates/cairn-cli/src/main.rs crates/cairn-cli/src/hooks/mod.rs crates/cairn-cli/tests/cli.rs crates/cairn-cli/tests/hook_cli.rs
git commit -m "feat(cli): add five-hook command dispatcher"
```

---

### Task 3: Add Durable Hook Artifact Writer

**Files:**
- Create: `crates/cairn-cli/src/hooks/artifact.rs`
- Test: `crates/cairn-cli/tests/hook_cli.rs`

- [ ] **Step 1: Write failing JSON artifact persistence tests**

Append these tests to `crates/cairn-cli/tests/hook_cli.rs`:

```rust
#[test]
fn user_prompt_submit_writes_trace_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let out = cli()
        .args([
            "hook",
            "UserPromptSubmit",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1","prompt":"remember this"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook UserPromptSubmit");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "UserPromptSubmit");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    let trace_path = vault
        .path()
        .join(".cairn/hooks/traces")
        .join(format!("{trace_id}.json"));
    assert!(trace_path.exists(), "missing trace artifact at {}", trace_path.display());
}

#[test]
fn malformed_non_object_payload_returns_typed_error() {
    let out = cli()
        .args([
            "hook",
            "UserPromptSubmit",
            "--payload",
            r#"[]"#,
            "--json",
        ])
        .output()
        .expect("cairn hook malformed");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert!(v["operation_id"].is_string());
    assert!(v["error"]["retry_guidance"].as_str().unwrap_or("").contains("retry"));
}
```

- [ ] **Step 2: Run tests and verify the trace persistence test fails**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected:

- `malformed_non_object_payload_returns_typed_error` passes if Task 2 is complete.
- `user_prompt_submit_writes_trace_artifact` fails until `artifact.rs` and the handler exist.

- [ ] **Step 3: Implement atomic JSON artifact writing**

Create `crates/cairn-cli/src/hooks/artifact.rs`:

```rust
use std::io::Write;
use std::path::{Path, PathBuf};

use cairn_core::generated::common::Ulid;
use serde::Serialize;

use super::HookError;
use crate::verbs::envelope::new_operation_id;

#[derive(Debug, Clone, Copy)]
pub enum ArtifactKind {
    Hot,
    Trace,
    Queue,
}

impl ArtifactKind {
    const fn dir_name(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Trace => "traces",
            Self::Queue => "queue",
        }
    }
}

pub struct ArtifactWrite {
    pub id: Ulid,
    pub path: PathBuf,
}

pub fn write_json<T: Serialize>(
    vault_path: &Path,
    kind: ArtifactKind,
    id: Option<Ulid>,
    value: &T,
) -> Result<ArtifactWrite, HookError> {
    let id = id.unwrap_or_else(new_operation_id);
    let dir = vault_path
        .join(".cairn")
        .join("hooks")
        .join(kind.dir_name());
    std::fs::create_dir_all(&dir).map_err(|err| {
        HookError::internal(
            format!("failed to create hook artifact directory `{}`: {err}", dir.display()),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    let final_path = dir.join(format!("{}.json", id.0));
    let tmp_path = dir.join(format!(".{}.tmp", id.0));
    let mut file = std::fs::File::create(&tmp_path).map_err(|err| {
        HookError::internal(
            format!("failed to create hook artifact `{}`: {err}", tmp_path.display()),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    let bytes = serde_json::to_vec(value).map_err(|err| {
        HookError::internal(
            format!("failed to encode hook artifact: {err}"),
            "retry the hook command; report this operation_id if encoding fails again",
        )
    })?;
    file.write_all(&bytes).map_err(|err| {
        HookError::internal(
            format!("failed to write hook artifact `{}`: {err}", tmp_path.display()),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    file.write_all(b"\n").map_err(|err| {
        HookError::internal(
            format!("failed to finish hook artifact `{}`: {err}", tmp_path.display()),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    file.sync_all().map_err(|err| {
        HookError::internal(
            format!("failed to sync hook artifact `{}`: {err}", tmp_path.display()),
            "restore durable storage for the vault path and retry the same hook command",
        )
    })?;
    std::fs::rename(&tmp_path, &final_path).map_err(|err| {
        HookError::internal(
            format!("failed to publish hook artifact `{}`: {err}", final_path.display()),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    Ok(ArtifactWrite { id, path: final_path })
}
```

- [ ] **Step 4: Run artifact tests**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: the malformed payload test passes; the trace artifact test still fails until Task 5 wires `UserPromptSubmit`.

- [ ] **Step 5: Leave artifact changes in the working tree for Task 5**

Do not commit yet. Task 5 wires the first trace hook that makes the artifact-persistence test pass,
then commits `artifact.rs`, the trace handlers, and the green tests together.

---

### Task 4: Implement `SessionStart`

**Files:**
- Create: `crates/cairn-cli/src/hooks/session_start.rs`
- Test: `crates/cairn-cli/tests/hook_cli.rs`

- [ ] **Step 1: Write failing `SessionStart` test**

Append this test:

```rust
#[test]
fn session_start_returns_hot_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let out = cli()
        .args([
            "hook",
            "SessionStart",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook SessionStart");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "SessionStart");
    let hot_path = v["artifacts"]["hot_path"].as_str().expect("hot_path");
    assert!(vault.path().join(hot_path).exists(), "missing hot artifact {hot_path}");
}
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
cargo test -p cairn-cli --test hook_cli session_start_returns_hot_artifact
```

Expected: fails because `SessionStart` is not implemented.

- [ ] **Step 3: Implement `SessionStart`**

Create `crates/cairn-cli/src/hooks/session_start.rs`:

```rust
use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{require_string, HookArtifacts, HookError};

#[derive(Serialize)]
struct HotArtifact {
    operation_id: Ulid,
    session_id: String,
    prefix: String,
    note: &'static str,
}

pub fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(&payload, "session_id")?;
    let artifact = HotArtifact {
        operation_id: operation_id.clone(),
        session_id,
        prefix: String::new(),
        note: "assemble_hot store path is not wired yet; empty prefix is the P0 hook boundary",
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Hot, Some(operation_id), &artifact)?;
    let hot_path = written
        .path
        .strip_prefix(vault_path)
        .unwrap_or(&written.path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_owned();
    Ok(HookArtifacts {
        trace_id: None,
        hot_path: Some(hot_path),
        queued_jobs: Vec::new(),
    })
}
```

- [ ] **Step 4: Run test**

Run:

```bash
cargo test -p cairn-cli --test hook_cli session_start_returns_hot_artifact
```

Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/hooks/session_start.rs crates/cairn-cli/tests/hook_cli.rs
git commit -m "feat(cli): handle SessionStart hook"
```

---

### Task 5: Implement Trace-Oriented Hooks

**Files:**
- Create: `crates/cairn-cli/src/hooks/user_prompt_submit.rs`
- Create: `crates/cairn-cli/src/hooks/pre_tool_use.rs`
- Create: `crates/cairn-cli/src/hooks/post_tool_use.rs`
- Test: `crates/cairn-cli/tests/hook_cli.rs`

- [ ] **Step 1: Write failing tests for trace hooks**

Append:

```rust
fn run_hook_with_payload(name: &str, payload: &str, vault: &tempfile::TempDir) -> serde_json::Value {
    let out = cli()
        .args([
            "hook",
            name,
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            payload,
            "--json",
        ])
        .output()
        .unwrap_or_else(|err| panic!("cairn hook {name}: {err}"));
    assert!(out.status.success(), "{name} exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).expect("hook JSON")
}

#[test]
fn pre_tool_use_writes_trace_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_payload(
        "PreToolUse",
        r#"{"session_id":"sess-1","tool_call_id":"call-1","tool_name":"shell"}"#,
        &vault,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "PreToolUse");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    assert!(vault.path().join(".cairn/hooks/traces").join(format!("{trace_id}.json")).exists());
}

#[test]
fn post_tool_use_writes_trace_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_payload(
        "PostToolUse",
        r#"{"session_id":"sess-1","tool_call_id":"call-1","tool_name":"shell","status":"ok"}"#,
        &vault,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "PostToolUse");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    assert!(vault.path().join(".cairn/hooks/traces").join(format!("{trace_id}.json")).exists());
}

#[test]
fn missing_required_trace_field_returns_invalid_args() {
    let out = cli()
        .args([
            "hook",
            "PreToolUse",
            "--payload",
            r#"{"session_id":"sess-1","tool_call_id":"call-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook PreToolUse invalid");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert!(v["error"]["retry_guidance"].as_str().unwrap_or("").contains("retry"));
}
```

- [ ] **Step 2: Run and verify failures**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: tests fail until handlers are implemented.

- [ ] **Step 3: Implement `UserPromptSubmit`**

Create `crates/cairn-cli/src/hooks/user_prompt_submit.rs`:

```rust
use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{payload_object, require_string, HookArtifacts, HookError};

#[derive(Serialize)]
struct TraceArtifact {
    operation_id: Ulid,
    hook: &'static str,
    session_id: String,
    event: serde_json::Map<String, Value>,
}

pub fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(&payload, "session_id")?;
    let _prompt = require_string(&payload, "prompt")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id,
        hook: "UserPromptSubmit",
        session_id,
        event: payload_object(&payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: Vec::new(),
    })
}
```

- [ ] **Step 4: Implement `PreToolUse`**

Create `crates/cairn-cli/src/hooks/pre_tool_use.rs`:

```rust
use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{payload_object, require_string, HookArtifacts, HookError};

#[derive(Serialize)]
struct TraceArtifact {
    operation_id: Ulid,
    hook: &'static str,
    session_id: String,
    tool_call_id: String,
    tool_name: String,
    event: serde_json::Map<String, Value>,
}

pub fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(&payload, "session_id")?;
    let tool_call_id = require_string(&payload, "tool_call_id")?;
    let tool_name = require_string(&payload, "tool_name")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id,
        hook: "PreToolUse",
        session_id,
        tool_call_id,
        tool_name,
        event: payload_object(&payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: Vec::new(),
    })
}
```

- [ ] **Step 5: Implement `PostToolUse`**

Create `crates/cairn-cli/src/hooks/post_tool_use.rs`:

```rust
use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{payload_object, require_string, HookArtifacts, HookError};

#[derive(Serialize)]
struct TraceArtifact {
    operation_id: Ulid,
    hook: &'static str,
    session_id: String,
    tool_call_id: String,
    tool_name: String,
    status: String,
    event: serde_json::Map<String, Value>,
}

pub fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(&payload, "session_id")?;
    let tool_call_id = require_string(&payload, "tool_call_id")?;
    let tool_name = require_string(&payload, "tool_name")?;
    let status = require_string(&payload, "status")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id,
        hook: "PostToolUse",
        session_id,
        tool_call_id,
        tool_name,
        status,
        event: payload_object(&payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: Vec::new(),
    })
}
```

- [ ] **Step 6: Run trace hook tests**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/hooks/artifact.rs crates/cairn-cli/src/hooks/user_prompt_submit.rs crates/cairn-cli/src/hooks/pre_tool_use.rs crates/cairn-cli/src/hooks/post_tool_use.rs crates/cairn-cli/tests/hook_cli.rs
git commit -m "feat(cli): persist trace hook artifacts"
```

---

### Task 6: Implement `Stop` Enqueue Boundary

**Files:**
- Create: `crates/cairn-cli/src/hooks/queue.rs`
- Create: `crates/cairn-cli/src/hooks/stop.rs`
- Test: `crates/cairn-cli/tests/hook_cli.rs`

- [ ] **Step 1: Write failing stop enqueue test**

Append:

```rust
#[test]
fn stop_writes_trace_and_queue_artifacts() {
    let vault = tempfile::tempdir().expect("temp vault");
    let out = cli()
        .args([
            "hook",
            "Stop",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook Stop");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "Stop");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    let job_id = v["artifacts"]["queued_jobs"][0].as_str().expect("queued job id");
    assert!(vault.path().join(".cairn/hooks/traces").join(format!("{trace_id}.json")).exists());
    assert!(vault.path().join(".cairn/hooks/queue").join(format!("{job_id}.json")).exists());
}

#[test]
fn stop_queue_failure_returns_retry_guidance() {
    let vault = tempfile::tempdir().expect("temp vault");
    let hooks_dir = vault.path().join(".cairn/hooks");
    std::fs::create_dir_all(&hooks_dir).expect("hooks dir");
    std::fs::write(hooks_dir.join("queue"), b"not a directory").expect("queue blocker");
    let out = cli()
        .args([
            "hook",
            "Stop",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook Stop");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], false);
    assert_eq!(v["hook"], "Stop");
    assert_eq!(v["error"]["code"], "Internal");
    assert!(v["operation_id"].is_string());
    assert!(
        v["error"]["retry_guidance"]
            .as_str()
            .unwrap_or("")
            .contains("retry cairn hook Stop")
    );
}
```

- [ ] **Step 2: Run and verify failure**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: tests fail until `Stop` and queue writing are implemented.

- [ ] **Step 3: Implement queue writer**

Create `crates/cairn-cli/src/hooks/queue.rs`:

```rust
use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;

use super::artifact::{self, ArtifactKind};
use super::HookError;

#[derive(Serialize)]
struct QueueArtifact {
    operation_id: Ulid,
    job_id: Ulid,
    session_id: String,
    trace_id: Ulid,
    kind: &'static str,
    status: &'static str,
}

pub fn enqueue_post_turn(
    vault_path: &Path,
    operation_id: Ulid,
    session_id: String,
    trace_id: Ulid,
) -> Result<Ulid, HookError> {
    let job_id = crate::verbs::envelope::new_operation_id();
    let artifact = QueueArtifact {
        operation_id,
        job_id: job_id.clone(),
        session_id,
        trace_id,
        kind: "post_turn",
        status: "pending",
    };
    artifact::write_json(vault_path, ArtifactKind::Queue, Some(job_id), &artifact)
        .map(|written| written.id)
        .map_err(|err| {
            HookError::internal(
                format!("{err:?}"),
                "retry cairn hook Stop for the same session after restoring queue write access",
            )
        })
}
```

- [ ] **Step 4: Implement `Stop`**

Create `crates/cairn-cli/src/hooks/stop.rs`:

```rust
use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{payload_object, queue, require_string, HookArtifacts, HookError};

#[derive(Serialize)]
struct TraceArtifact {
    operation_id: Ulid,
    hook: &'static str,
    session_id: String,
    event: serde_json::Map<String, Value>,
}

pub fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(&payload, "session_id")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id: operation_id.clone(),
        hook: "Stop",
        session_id: session_id.clone(),
        event: payload_object(&payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    let job_id = queue::enqueue_post_turn(vault_path, operation_id, session_id, written.id.clone())?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: vec![job_id],
    })
}
```

- [ ] **Step 5: Run stop tests**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/hooks/queue.rs crates/cairn-cli/src/hooks/stop.rs crates/cairn-cli/tests/hook_cli.rs
git commit -m "feat(cli): enqueue post-turn work on Stop hook"
```

---

### Task 7: Add Lifecycle, Latency-Boundary, and Failure Tests

**Files:**
- Modify: `crates/cairn-cli/tests/hook_cli.rs`

- [ ] **Step 1: Add full lifecycle test**

Append:

```rust
#[test]
fn full_hook_lifecycle_writes_expected_artifacts() {
    let vault = tempfile::tempdir().expect("temp vault");
    let session = r#""sess-1""#;
    let cases = [
        ("SessionStart", format!(r#"{{"session_id":{session}}}"#)),
        ("UserPromptSubmit", format!(r#"{{"session_id":{session},"prompt":"hello"}}"#)),
        ("PreToolUse", format!(r#"{{"session_id":{session},"tool_call_id":"call-1","tool_name":"shell"}}"#)),
        ("PostToolUse", format!(r#"{{"session_id":{session},"tool_call_id":"call-1","tool_name":"shell","status":"ok"}}"#)),
        ("Stop", format!(r#"{{"session_id":{session}}}"#)),
    ];
    for (hook, payload) in cases {
        let v = run_hook_with_payload(hook, &payload, &vault);
        assert_eq!(v["ok"], true, "{hook} did not succeed: {v}");
    }
    let trace_dir = vault.path().join(".cairn/hooks/traces");
    let trace_count = std::fs::read_dir(trace_dir).expect("trace dir").count();
    assert_eq!(trace_count, 4, "prompt, pre-tool, post-tool, and stop traces");
    let queue_count = std::fs::read_dir(vault.path().join(".cairn/hooks/queue"))
        .expect("queue dir")
        .count();
    assert_eq!(queue_count, 1, "Stop enqueues exactly one post-turn job");
}
```

- [ ] **Step 2: Add latency-boundary smoke test**

Append:

```rust
#[test]
fn stop_returns_after_enqueue_boundary() {
    let vault = tempfile::tempdir().expect("temp vault");
    let started = std::time::Instant::now();
    let v = run_hook_with_payload("Stop", r#"{"session_id":"sess-1"}"#, &vault);
    assert_eq!(v["ok"], true);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "Stop hook should not wait on downstream workflow execution"
    );
    assert_eq!(v["artifacts"]["queued_jobs"].as_array().unwrap().len(), 1);
}
```

- [ ] **Step 3: Add trace-write failure test**

Append:

```rust
#[test]
fn trace_write_failure_returns_operation_id_and_retry_guidance() {
    let blocked_vault = tempfile::NamedTempFile::new().expect("vault blocker");
    let out = cli()
        .args([
            "hook",
            "UserPromptSubmit",
            "--vault-path",
            blocked_vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1","prompt":"hello"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook trace failure");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("hook JSON");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "Internal");
    assert!(v["operation_id"].is_string());
    assert!(v["error"]["retry_guidance"].as_str().unwrap_or("").contains("retry"));
}
```

- [ ] **Step 4: Run focused test suite**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: all hook CLI tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/tests/hook_cli.rs
git commit -m "test(cli): cover hook lifecycle and failures"
```

---

### Task 8: Document the Canonical Hook Contract

**Files:**
- Modify: `docs/design/design-brief.md`

- [ ] **Step 1: Write failing documentation check**

Run:

```bash
rg -n "PreCompact|SessionStart.*UserPromptSubmit.*PostToolUse.*PreCompact.*Stop" docs/design/design-brief.md
```

Expected before edit: at least one stale `PreCompact` reference appears in the v0.1 hook lifecycle sections.

- [ ] **Step 2: Update §9.3 hook table**

In `docs/design/design-brief.md`, change the hook sensor row and §9.3 table so the five-hook set is:

```markdown
| `SessionStart` | startup / resume | `assemble_hot` builds the prefix; semantic re-index runs in background |
| `UserPromptSubmit` | every message | lightweight classifier emits routing hints |
| `PreToolUse` | before tool execution | record the planned tool call as a trace event |
| `PostToolUse` | after tool execution | record the tool result and validate markdown writes when applicable |
| `Stop` | end of session | persist the stop trace event and enqueue post-turn work |
```

Keep the existing sentence that hooks are plain scripts executed via `cairn hook <name>`.

- [ ] **Step 3: Run documentation check**

Run:

```bash
rg -n "PreCompact" docs/design/design-brief.md
```

Expected: any remaining `PreCompact` references are outside the v0.1 five-hook lifecycle contract and describe older or non-v0.1 compatibility context. If the command still reports the §9.3 lifecycle or hook sensor row, fix those references.

- [ ] **Step 4: Commit**

```bash
git add docs/design/design-brief.md
git commit -m "docs: document canonical five-hook lifecycle"
```

---

### Task 9: Final Verification

**Files:**
- No code edits expected.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all
```

Expected: no formatting errors.

- [ ] **Step 2: Run focused CLI tests**

Run:

```bash
cargo test -p cairn-cli
```

Expected: all `cairn-cli` tests pass.

- [ ] **Step 3: Run hook lifecycle tests explicitly**

Run:

```bash
cargo test -p cairn-cli --test hook_cli
```

Expected: all hook lifecycle, failure-mode, and contract tests pass.

- [ ] **Step 4: Run boundary check required by repo instructions**

Run:

```bash
scripts/check-core-boundary.sh
```

Expected: passes; `cairn-core` still has no dependency on workspace adapter crates.

- [ ] **Step 5: Inspect git diff**

Run:

```bash
git status --short
git diff --stat HEAD
```

Expected:

- only scoped hook CLI, hook tests, and design brief edits are present
- generated files are not manually modified

- [ ] **Step 6: Final commit if verification required edits**

If `cargo fmt --all` or documentation cleanup changed files:

```bash
git add crates/cairn-cli docs/design/design-brief.md
git commit -m "chore: finalize hook integration verification"
```

Use an exact `git add` path list matching the files that changed; do not stage unrelated files.
