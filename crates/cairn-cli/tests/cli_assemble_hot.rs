//! End-to-end CLI snapshot for `cairn assemble_hot --json`. Pins the
//! JSON wire shape against a real binary invocation in a bootstrapped
//! tempdir vault.
//!
//! The CLI fails closed on a non-vault working directory (`CwdFallback`),
//! so the test bootstraps a real vault with `cairn_cli::vault::bootstrap`
//! before invoking the binary.

use std::path::Path;
use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};
use cairn_core::config::{CairnConfig, HotMemoryRecipeStep};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

/// Replace the per-call `operation_id` ULID with a fixed placeholder so the
/// golden snapshot stays deterministic across runs.
fn redact_operation_id(json: &str) -> String {
    const KEY: &str = "\"operation_id\":\"";
    const ULID_LEN: usize = 26;
    const PLACEHOLDER: &str = "01XXXXXXXXXXXXXXXXXXXXXXXX";
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(idx) = rest.find(KEY) {
        out.push_str(&rest[..idx + KEY.len()]);
        let value_start = idx + KEY.len();
        if rest[value_start..].len() >= ULID_LEN {
            out.push_str(PLACEHOLDER);
            rest = &rest[value_start + ULID_LEN..];
        } else {
            rest = &rest[value_start..];
        }
    }
    out.push_str(rest);
    out
}

fn seed_default_identity(vault: &Path) {
    let output = cli()
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
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cairn_assemble_hot_json_emits_segments() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run cairn");

    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");

    // Sanity: segments present and length 6.
    let segments = value.pointer("/data/segments").expect("segments present");
    assert!(
        segments.is_array(),
        "segments should be array, got {segments}"
    );
    assert_eq!(
        segments.as_array().unwrap().len(),
        6,
        "default recipe has 6 steps"
    );

    // Redact the volatile operation_id before snapshotting.
    let redacted = redact_operation_id(stdout.trim());
    insta::assert_snapshot!(redacted);
}

#[test]
fn cairn_assemble_hot_honors_budget_and_session_args() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--budget")
        .arg("16")
        .arg("--session")
        .arg("01H00000000000000000000000")
        .arg("--json")
        .output()
        .expect("run cairn");

    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    let bytes = value
        .pointer("/data/bytes")
        .and_then(serde_json::Value::as_u64)
        .expect("bytes");
    let prefix = value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix");

    assert!(bytes <= 16, "bytes should respect budget, got {bytes}");
    assert!(
        prefix.len() <= 16,
        "prefix should respect budget, got {}",
        prefix.len()
    );
    assert!(value["policy_trace"].is_array());
}

#[test]
fn cairn_assemble_hot_rejects_budget_above_wire_max() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--budget")
        .arg("4194305")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(64),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["error"]["code"], "InvalidArgs");
}

#[test]
fn cairn_assemble_hot_rejects_empty_session_id() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--session")
        .arg("")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(64),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["error"]["code"], "InvalidArgs");
}

#[test]
fn cairn_assemble_hot_loads_project_memory_from_store() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let ingest = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "project",
            "--body",
            "project hot body",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert!(
        ingest.status.success(),
        "exit={:?} stdout={} stderr={}",
        ingest.status.code(),
        String::from_utf8_lossy(&ingest.stdout),
        String::from_utf8_lossy(&ingest.stderr)
    );

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run assemble_hot");
    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    let prefix = value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix");
    let trace = value
        .pointer("/policy_trace")
        .and_then(serde_json::Value::as_array)
        .expect("policy_trace");

    assert!(
        prefix.contains("project hot body"),
        "prefix missing ingested project memory: {prefix:?}"
    );
    assert!(trace.iter().any(|entry| {
        entry.get("gate").and_then(serde_json::Value::as_str) == Some("read.scope")
            && entry
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|detail| detail.contains("records=1"))
    }));
}

#[test]
fn cairn_assemble_hot_caps_store_backed_sections_to_budget() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let body = "project hot body ".repeat(64);
    let ingest = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "project", "--body", &body, "--json"])
        .output()
        .expect("run ingest");
    assert!(
        ingest.status.success(),
        "exit={:?} stdout={} stderr={}",
        ingest.status.code(),
        String::from_utf8_lossy(&ingest.stdout),
        String::from_utf8_lossy(&ingest.stderr)
    );

    let output = cli()
        .current_dir(vault.path())
        .args(["assemble_hot", "--budget", "80", "--json"])
        .output()
        .expect("run assemble_hot");
    assert!(
        output.status.success(),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    let bytes = value
        .pointer("/data/bytes")
        .and_then(serde_json::Value::as_u64)
        .expect("bytes");
    let prefix = value
        .pointer("/data/prefix")
        .and_then(serde_json::Value::as_str)
        .expect("prefix");

    assert!(bytes <= 80, "bytes should respect budget, got {bytes}");
    assert!(
        prefix.len() <= 80,
        "prefix should respect budget, got {}",
        prefix.len()
    );
}

#[test]
fn cairn_assemble_hot_rejects_oversized_recipe_before_loading_sources() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());

    let config_path = vault.path().join(".cairn/config.yaml");
    let raw_config = std::fs::read_to_string(&config_path).expect("read config");
    let mut config: CairnConfig = yaml_serde::from_str(&raw_config).expect("parse config");
    config.vault.hot_memory.recipe =
        vec![HotMemoryRecipeStep::Purpose; cairn_core::verbs::assemble_hot::MAX_SEGMENTS + 1];
    std::fs::write(
        &config_path,
        yaml_serde::to_string(&config).expect("serialize config"),
    )
    .expect("write config");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(78),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "aborted");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("recipe exceeds"))
    );
}

#[test]
fn cairn_assemble_hot_rejects_unknown_issuer_before_loading_sources() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let output = cli()
        .current_dir(vault.path())
        .env("CAIRN_ISSUER", "agt:cairn-cli:missing:writer:v1")
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(64),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "rejected");
    assert!(value["data"].is_null());
    assert_eq!(value["error"]["code"], "Unauthorized");
}

#[test]
fn cairn_assemble_hot_validates_later_markdown_sources_after_budget_exhaustion() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(vault.path());
    std::fs::remove_file(vault.path().join("index.md")).expect("remove index.md");

    let output = cli()
        .current_dir(vault.path())
        .arg("assemble_hot")
        .arg("--budget")
        .arg("1")
        .arg("--json")
        .output()
        .expect("run assemble_hot");

    assert_eq!(
        output.status.code(),
        Some(78),
        "exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert_eq!(value["status"], "aborted");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("read index.md"))
    );
}
