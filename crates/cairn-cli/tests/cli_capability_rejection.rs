//! Verify that capability-gated args are rejected with the full
//! `CapabilityUnavailable` envelope (including `data.remediation`) when the
//! runtime does not advertise the capability.
//!
//! The JSON envelope is emitted to **stdout**; exit code is 69 (`EX_UNAVAILABLE`).
//! The envelope shape is:
//! ```json
//! {
//!   "status": "rejected",
//!   "error": {
//!     "code": "CapabilityUnavailable",
//!     "data": { "capability": "...", "remediation": "..." }
//!   }
//! }
//! ```

use std::path::Path;
use std::process::Command;

fn cairn_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    // Ensure the embedding model probe never finds a model on disk
    // (avoids false capability grant on a developer machine with cached
    // BGE weights). The CAIRN_MOCK_EMBEDDER escape hatch is NOT set here —
    // the whole point is that semantic mode is NOT available.
    cmd.env_remove("CAIRN_MOCK_EMBEDDER");
    cmd
}

fn write_vault_id(root: &Path) {
    std::fs::create_dir_all(root.join(".cairn")).unwrap();
    std::fs::write(
        root.join(".cairn").join("vault.id"),
        b"01HZZ0000000000000000000AB\n",
    )
    .unwrap();
}

/// `cairn search --mode semantic` against a vault whose config explicitly
/// sets `local_embeddings: false` must:
///
/// 1. Exit with code 69 (`EX_UNAVAILABLE`).
/// 2. Emit a `status=rejected` JSON envelope to stdout.
/// 3. Carry `error.code = CapabilityUnavailable`.
/// 4. Carry `error.data.capability = cairn.mcp.v1.search.semantic`.
/// 5. Carry `error.data.remediation` that mentions `local_embeddings`.
///
/// The remediation text is sourced from
/// `cairn_core::status::remediation_for("cairn.mcp.v1.search.semantic")` so
/// this test pins the end-to-end wiring introduced in Task 10.
#[test]
fn search_semantic_rejects_with_remediation_when_local_embeddings_off() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    // Write a minimal config that disables local_embeddings. This ensures
    // the capability gate fires even on a developer machine where BGE
    // weights happen to be present (the config is the gate, not just
    // model presence).
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\nsearch:\n  local_embeddings: false\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args([
            "--vault",
            tmp.path().to_str().unwrap(),
            "search",
            "--mode",
            "semantic",
            "anything",
            "--json",
        ])
        .output()
        .expect("spawn cairn");

    // sysexit 69 = EX_UNAVAILABLE — CapabilityUnavailable.
    assert_eq!(
        out.status.code(),
        Some(69),
        "expected exit 69 (EX_UNAVAILABLE); got {:?}\n  stdout: {}\n  stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // The JSON envelope is on stdout (emit_json writes to stdout).
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected valid JSON on stdout; parse error: {e}\nstdout: {stdout:?}")
    });

    assert_eq!(
        envelope["status"], "rejected",
        "CapabilityUnavailable must produce status=rejected; envelope: {envelope}"
    );
    assert_eq!(
        envelope["error"]["code"], "CapabilityUnavailable",
        "error.code must be CapabilityUnavailable; envelope: {envelope}"
    );
    assert_eq!(
        envelope["error"]["data"]["capability"], "cairn.mcp.v1.search.semantic",
        "error.data.capability must be the semantic capability id; envelope: {envelope}"
    );
    let remediation = envelope["error"]["data"]["remediation"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("error.data.remediation must be a non-empty string; envelope: {envelope}")
        });
    assert!(
        remediation.contains("local_embeddings"),
        "remediation must mention the config toggle 'local_embeddings'; got: {remediation:?}"
    );
}
