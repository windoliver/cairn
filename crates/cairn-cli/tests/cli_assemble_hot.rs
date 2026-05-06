//! End-to-end CLI snapshot for `cairn assemble_hot --json`. Pins the
//! JSON wire shape against a real binary invocation in a bootstrapped
//! tempdir vault.
//!
//! The CLI fails closed on a non-vault working directory (`CwdFallback`),
//! so the test bootstraps a real vault with `cairn_cli::vault::bootstrap`
//! before invoking the binary.

use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};

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

#[test]
fn cairn_assemble_hot_json_emits_segments() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

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
