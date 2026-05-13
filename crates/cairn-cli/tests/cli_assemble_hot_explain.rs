//! End-to-end CLI test for the `--explain` debug surface (issue #82).
//!
//! Bootstraps a tempdir vault, seeds the default identity via an
//! initial ingest (matching the pattern in `cli_assemble_hot.rs`), then
//! ingests a user / project / playbook record and asserts:
//!
//! 1. `cairn assemble_hot --json` emits no `debug` field by default
//!    (wire-compat with older clients).
//! 2. `cairn assemble_hot --json --explain` emits a `debug.steps`
//!    array of length 6 (one trace per recipe step).
//! 3. The pinned / project / playbook traces each include the
//!    `record_id` of the ingested record with a stable `note`.
//! 4. `--budget N` truncates the prefix to N bytes (`bytes <= N`).

use std::path::Path;
use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn seed_default_identity(vault: &Path) {
    let out = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "identity seed",
            "--json",
        ])
        .output()
        .expect("seed default identity");
    assert!(
        out.status.success(),
        "seed identity failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ingest(vault: &Path, kind: &str, body: &str, tags: &[&str]) {
    let mut cmd = cli();
    cmd.current_dir(vault)
        .args(["ingest", "--kind", kind, "--body", body]);
    for t in tags {
        cmd.args(["--tags", t]);
    }
    let out = cmd.output().expect("ingest");
    assert!(
        out.status.success(),
        "ingest --kind {kind} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assemble_hot_json(vault: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args: Vec<&str> = vec!["assemble_hot", "--json"];
    args.extend(extra);
    let out = cli()
        .current_dir(vault)
        .args(&args)
        .output()
        .expect("assemble_hot");
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    assert!(
        out.status.success(),
        "assemble_hot exit={:?} stdout={stdout} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(stdout.trim()).expect("valid JSON on stdout")
}

fn bootstrap_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(dir.path());
    dir
}

#[test]
fn debug_field_is_absent_without_explain_flag() {
    let vault = bootstrap_vault();
    let value = assemble_hot_json(vault.path(), &[]);
    assert!(
        value.pointer("/data/debug").is_none(),
        "debug field must be absent when --explain is not set: {value}"
    );
}

#[test]
fn explain_flag_populates_per_step_debug_trace() {
    let vault = bootstrap_vault();
    ingest(
        vault.path(),
        "user",
        "user prefers terse explanations",
        &["pinned"],
    );
    ingest(
        vault.path(),
        "project",
        "current project: cairn refactor",
        &[],
    );
    ingest(vault.path(), "playbook", "deploy via kubectl rollout", &[]);

    let value = assemble_hot_json(vault.path(), &["--explain"]);
    let steps = value
        .pointer("/data/debug/steps")
        .and_then(|v| v.as_array())
        .expect("debug.steps populated");
    assert_eq!(steps.len(), 6, "default recipe has 6 steps");

    // Pinned step must include the user record.
    let pinned = steps
        .iter()
        .find(|s| s.pointer("/step").and_then(|v| v.as_str()) == Some("pinned_feedback"))
        .expect("pinned_feedback trace");
    let pinned_included = pinned
        .pointer("/included")
        .and_then(|v| v.as_array())
        .expect("pinned included");
    assert_eq!(pinned_included.len(), 1, "user record must be pinned");
    assert_eq!(
        pinned_included[0]
            .pointer("/note")
            .and_then(|v| v.as_str())
            .expect("pinned note"),
        "salience × recency",
    );

    // Project step must include the project record.
    let project = steps
        .iter()
        .find(|s| s.pointer("/step").and_then(|v| v.as_str()) == Some("top_salience_project"))
        .expect("top_salience_project trace");
    assert_eq!(
        project
            .pointer("/included")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len),
        1,
        "project record must be included"
    );

    // Playbook step must include the playbook record.
    let playbook = steps
        .iter()
        .find(|s| s.pointer("/step").and_then(|v| v.as_str()) == Some("active_playbook"))
        .expect("active_playbook trace");
    assert_eq!(
        playbook
            .pointer("/included")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len),
        1,
        "playbook record must be included"
    );
}

#[test]
fn budget_flag_truncates_prefix() {
    let vault = bootstrap_vault();
    ingest(
        vault.path(),
        "user",
        "user prefers terse explanations",
        &["pinned"],
    );
    let value = assemble_hot_json(vault.path(), &["--budget", "64"]);
    let bytes = value
        .pointer("/data/bytes")
        .and_then(serde_json::Value::as_u64)
        .expect("bytes is u64");
    assert!(bytes <= 64, "budget=64 must cap bytes; got {bytes}");
    let prefix_len = value
        .pointer("/data/prefix")
        .and_then(|v| v.as_str())
        .map_or(0, str::len);
    let bytes_usize = usize::try_from(bytes).expect("bytes fit in usize on 64-bit targets");
    assert_eq!(
        bytes_usize, prefix_len,
        "data.bytes must equal prefix.len()",
    );
}
