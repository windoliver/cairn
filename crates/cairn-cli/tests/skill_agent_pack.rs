//! CLI coverage for `cairn skill install --agent` and `--all`.

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
    assert_eq!(
        json["integrations"].as_array().expect("integrations").len(),
        4
    );
}
