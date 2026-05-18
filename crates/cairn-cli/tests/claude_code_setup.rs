#![allow(missing_docs)]

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn cli() -> Command {
    Command::cargo_bin("cairn").expect("cargo bin cairn")
}

fn parse_stdout_json(out: std::process::Output) -> Value {
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout")
}

fn seed_vault(root: &Path) {
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

fn write_hook_settings(project: &Path) {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    let body = serde_json::json!({
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
fn setup_help_lists_claude_code() {
    cli()
        .args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code"));
}

#[test]
fn setup_claude_code_writes_local_scope_by_default() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["scope"], "local");
    assert_eq!(receipt["status"], "created");

    let config_path = home.path().join(".claude.json");
    let config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("read generated .claude.json"))
            .expect("parse generated .claude.json");
    let server = &config["projects"][project.path().display().to_string()]["mcpServers"]["cairn"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], binary);
    assert_eq!(
        server["args"],
        serde_json::json!(["--vault", vault.path().display().to_string(), "mcp"])
    );
    assert_eq!(server["env"], serde_json::json!({}));
}

#[test]
fn setup_claude_code_remove_rejects_invalid_explicit_vault() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let missing_vault = tempfile::tempdir()
        .expect("missing vault parent")
        .path()
        .join("definitely-not-a-vault");

    cli()
        .arg("--vault")
        .arg(&missing_vault)
        .args(["setup", "claude-code", "remove"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .arg("--json")
        .assert()
        .code(78)
        .stderr(
            predicate::str::contains("no active Cairn vault resolved")
                .or(predicate::str::contains("vault resolution")),
        );
}

#[test]
fn setup_claude_code_remove_honors_parent_common_options() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let config_path = project.path().join(".mcp.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "mcpServers": {
                "cairn": {
                    "type": "stdio",
                    "command": env!("CARGO_BIN_EXE_cairn"),
                    "args": ["--vault", vault.path().display().to_string(), "mcp"],
                    "env": {}
                },
                "other": {
                    "type": "stdio",
                    "command": "/usr/bin/true",
                    "args": [],
                    "env": {}
                }
            }
        }))
        .expect("serialize .mcp.json"),
    )
    .expect("write .mcp.json");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code", "--scope", "project"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--json", "remove"])
        .assert()
        .success()
        .get_output()
        .clone();

    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["scope"], "project");
    assert_eq!(receipt["status"], "removed");

    let config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("read generated .mcp.json"))
            .expect("parse generated .mcp.json");
    assert!(config["mcpServers"].get("cairn").is_none());
    assert_eq!(config["mcpServers"]["other"]["command"], "/usr/bin/true");
}

#[test]
fn setup_claude_code_is_idempotent() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success();

    let config_path = home.path().join(".claude.json");
    let before = std::fs::read(&config_path).expect("read generated .claude.json");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["status"], "unchanged");
    let after = std::fs::read(&config_path).expect("read unchanged .claude.json");
    assert_eq!(after, before);
}

#[test]
fn setup_claude_code_project_scope_writes_mcp_json() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code", "--scope", "project"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["scope"], "project");

    let config_path = project.path().join(".mcp.json");
    let config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("read generated .mcp.json"))
            .expect("parse generated .mcp.json");
    assert_eq!(config["mcpServers"]["cairn"]["command"], binary);
}

#[test]
fn setup_claude_code_remove_deletes_project_entry() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code", "--scope", "project"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success();

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code", "remove"])
        .arg("--scope")
        .arg("project")
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .clone();

    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["status"], "removed");

    let config_path = project.path().join(".mcp.json");
    let config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("read generated .mcp.json"))
            .expect("parse generated .mcp.json");
    assert!(config["mcpServers"].get("cairn").is_none());
}

#[test]
fn doctor_succeeds_after_setup_with_hook_settings() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");
    write_hook_settings(project.path());

    cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success();

    let out = cli()
        .args(["doctor", "claude-code"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .arg("--json")
        .output()
        .expect("cairn doctor claude-code --json");

    assert!(
        out.status.success(),
        "exit: {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["ok"], true);
}

#[test]
fn doctor_succeeds_after_setup_with_relative_project_dir() {
    let root = tempfile::tempdir().expect("root tempdir");
    let project = root.path().join("project");
    let home = root.path().join("home");
    std::fs::create_dir_all(&project).expect("mkdir project");
    std::fs::create_dir_all(&home).expect("mkdir home");
    write_hook_settings(&project);

    let vault = synth_vault();
    let binary = env!("CARGO_BIN_EXE_cairn");

    cli()
        .current_dir(root.path())
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "claude-code"])
        .args(["--project-dir", "project"])
        .arg("--home-dir")
        .arg(&home)
        .args(["--binary", binary, "--json"])
        .assert()
        .success();

    let out = cli()
        .current_dir(root.path())
        .args(["doctor", "claude-code"])
        .args(["--project-dir", "project"])
        .arg("--home-dir")
        .arg(&home)
        .arg("--json")
        .output()
        .expect("cairn doctor claude-code --json");

    assert!(
        out.status.success(),
        "exit: {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = parse_stdout_json(out);
    assert_eq!(receipt["ok"], true);
    let expected_project_dir = project
        .canonicalize()
        .expect("canonicalize project")
        .display()
        .to_string();
    assert_eq!(
        receipt["project_dir"].as_str(),
        Some(expected_project_dir.as_str())
    );
}
