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

#[test]
fn setup_help_lists_codex() {
    cli()
        .args(["setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex"));
}

#[test]
fn setup_codex_writes_home_config_and_project_hooks() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "codex"])
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

    let config_path = home.path().join(".codex/config.toml");
    let config = std::fs::read_to_string(&config_path).expect("read generated Codex config");
    let config: toml::Value = toml::from_str(&config).expect("parse generated Codex config");
    let server = &config["mcp_servers"]["cairn"];
    assert_eq!(server["type"].as_str(), Some("stdio"));
    assert_eq!(server["command"].as_str(), Some(binary));
    assert_eq!(
        server["args"].as_array().expect("args array"),
        &[
            toml::Value::String("--vault".to_owned()),
            toml::Value::String(vault.path().display().to_string()),
            toml::Value::String("mcp".to_owned()),
        ]
    );
    assert_eq!(
        config["hooks"].as_str(),
        Some(
            project
                .path()
                .canonicalize()
                .expect("canonical project path")
                .join(".codex/hooks.json")
                .display()
                .to_string()
                .as_str()
        )
    );

    let hooks_path = project.path().join(".codex/hooks.json");
    let hooks: Value =
        serde_json::from_slice(&std::fs::read(&hooks_path).expect("read Codex hooks"))
            .expect("parse Codex hooks");
    for hook in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ] {
        let command = hooks["hooks"][hook][0]["command"]
            .as_str()
            .unwrap_or_else(|| panic!("missing command for {hook}"));
        assert!(command.contains(binary), "{command}");
        assert!(command.contains("hook"), "{command}");
        assert!(command.contains(hook), "{command}");
    }
}

#[test]
fn setup_codex_is_idempotent() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "codex"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success();

    let config_path = home.path().join(".codex/config.toml");
    let before = std::fs::read(&config_path).expect("read generated Codex config");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "codex"])
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
    let after = std::fs::read(&config_path).expect("read unchanged Codex config");
    assert_eq!(after, before);
}

#[test]
fn setup_codex_reports_updated_when_repairing_hooks() {
    let vault = synth_vault();
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("home tempdir");
    let binary = env!("CARGO_BIN_EXE_cairn");

    cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "codex"])
        .arg("--project-dir")
        .arg(project.path())
        .arg("--home-dir")
        .arg(home.path())
        .args(["--binary", binary, "--json"])
        .assert()
        .success();

    let hooks_path = project.path().join(".codex/hooks.json");
    std::fs::remove_file(&hooks_path).expect("remove generated hooks");

    let out = cli()
        .arg("--vault")
        .arg(vault.path())
        .args(["setup", "codex"])
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
    assert_eq!(receipt["status"], "updated");
    assert!(hooks_path.exists(), "setup should recreate missing hooks");
}
