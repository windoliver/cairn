#![allow(missing_docs)]

use assert_cmd::Command;
use std::path::Path;

fn cli() -> Command {
    Command::cargo_bin("cairn").expect("cargo bin cairn")
}

fn isolated_cli(workspace: &Path) -> Command {
    let mut cmd = cli();
    cmd.env_remove("CAIRN_VAULT")
        .env("CAIRN_REGISTRY", workspace.join("registry.json"));
    cmd
}

fn assert_success(output: &std::process::Output, context: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn verify_pack(workspace: &Path, output_dir: &Path) {
    let output = isolated_cli(workspace)
        .args(["plugins", "verify", "--pack-path"])
        .arg(output_dir)
        .arg("--strict")
        .output()
        .expect("cairn plugins verify");

    assert_success(&output, "cairn plugins verify --strict");
}

fn skill_new_scaffold_verifies(harness: &str) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join(format!("{harness}-pack"));
    let output = isolated_cli(tmp.path())
        .args([
            "skill",
            "new",
            "sample-pack",
            "--harness",
            harness,
            "--output",
        ])
        .arg(&output_dir)
        .output()
        .expect("cairn skill new");

    assert_success(&output, "cairn skill new");
    verify_pack(tmp.path(), &output_dir);
}

#[test]
fn skill_new_rejects_unsafe_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().join("work");
    std::fs::create_dir(&cwd).expect("create cwd");
    let possible_output = tmp.path().join("bad");

    let output = isolated_cli(tmp.path())
        .current_dir(&cwd)
        .args(["skill", "new", "../bad", "--harness", "codex"])
        .output()
        .expect("cairn skill new");

    assert_ne!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid pack name"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!possible_output.exists());
}

#[test]
fn skill_new_fails_on_non_empty_output() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("existing");
    std::fs::create_dir(&output_dir).expect("create output dir");
    let keep = output_dir.join("keep.txt");
    std::fs::write(&keep, "preserve me\n").expect("write keep");

    let output = isolated_cli(tmp.path())
        .args([
            "skill",
            "new",
            "sample-pack",
            "--harness",
            "codex",
            "--output",
        ])
        .arg(&output_dir)
        .output()
        .expect("cairn skill new");

    assert_ne!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("output directory is not empty"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&keep).expect("read keep"),
        "preserve me\n"
    );
}

#[test]
fn skill_new_codex_scaffold_verifies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output_dir = tmp.path().join("sample-pack");
    let output = isolated_cli(tmp.path())
        .args([
            "skill",
            "new",
            "sample-pack",
            "--harness",
            "codex",
            "--output",
        ])
        .arg(&output_dir)
        .output()
        .expect("cairn skill new");

    assert_success(&output, "cairn skill new");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cairn plugins verify --pack-path"),
        "stdout:\n{stdout}"
    );
    verify_pack(tmp.path(), &output_dir);
}

#[test]
fn skill_new_claude_code_scaffold_verifies() {
    skill_new_scaffold_verifies("claude-code");
}

#[test]
fn skill_new_gemini_scaffold_verifies() {
    skill_new_scaffold_verifies("gemini");
}
