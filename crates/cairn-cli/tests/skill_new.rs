#![allow(missing_docs)]

use assert_cmd::Command;
use serde_json::Value;
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
    assert_eq!(output.status.code(), Some(74));
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
    assert_eq!(output.status.code(), Some(74));
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
fn skill_new_help_lists_only_supported_harnesses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = isolated_cli(tmp.path())
        .args(["skill", "new", "--help"])
        .output()
        .expect("cairn skill new --help");

    assert_success(&output, "cairn skill new --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for supported in ["claude-code", "codex", "gemini"] {
        assert!(stdout.contains(supported), "stdout:\n{stdout}");
    }
    for unsupported in ["opencode", "cursor", "custom"] {
        assert!(!stdout.contains(unsupported), "stdout:\n{stdout}");
    }
}

#[test]
fn skill_new_rejects_unsupported_harness_at_clap_boundary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = isolated_cli(tmp.path())
        .args(["skill", "new", "sample-pack", "--harness", "opencode"])
        .output()
        .expect("cairn skill new");

    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid value") && stderr.contains("possible values"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn skill_new_json_emits_receipt_only() {
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
        .arg("--json")
        .output()
        .expect("cairn skill new --json");

    assert_success(&output, "cairn skill new --json");
    assert!(
        output.stderr.is_empty(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let receipt: Value = serde_json::from_str(stdout.trim()).expect("stdout JSON receipt");
    assert_eq!(receipt["pack_id"], "sample-pack");
}

#[test]
fn skill_pack_authoring_guide_has_required_anchors() {
    let guide_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/skill-pack-authoring.md");
    let guide = std::fs::read_to_string(&guide_path).expect("docs/skill-pack-authoring.md");
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

    for required_text in [
        "cairn-pack/v1",
        ".cairnpack",
        "cairn skillpack",
        "manual.md",
        "CLAUDE.md",
        "AGENTS.md",
        "GEMINI.md",
        "hooks/settings.json",
        "hooks/hooks.json",
        "CAIRN PACK MANUAL",
        "BEGIN CAIRN PACK my-pack",
        "--vault-path",
        "--payload-file - --json",
        "manifest hook command stays at `cairn hook SessionStart`; the concrete Codex/Gemini installed hook payload lives in `hooks/hooks.json`",
        "manifest hooks validate event names; concrete installed hook payloads live in harness hook files",
        "cairn plugins verify --pack-path . --strict",
        "bash tests/smoke.sh",
        "temporary install round-trip and does not install into the current project",
    ] {
        assert!(
            guide.contains(required_text),
            "missing guide text {required_text}"
        );
    }
}

#[test]
fn generated_templates_include_ci_and_smoke() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root");
    for harness in ["claude-code", "codex", "gemini"] {
        let root = repo_root.join("packs/templates").join(harness);
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
