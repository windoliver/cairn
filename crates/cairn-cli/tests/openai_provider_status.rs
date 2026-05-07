//! Verify `cairn status --json` capability advertisement is driven by the
//! embedding *provider* readiness, not just local model presence.
//!
//! Finding A (PR #303 round-2 review): `config.capabilities()` was receiving
//! `model_present` (a filesystem stat) instead of `embedding_provider_ready`
//! (which accounts for cloud providers). With `default_provider: openai` and
//! no local model on disk, the old code produced `semantic_search: false`
//! regardless of whether `OPENAI_API_KEY` was set.
//!
//! Finding B (same review): the CLI `search` verb's pre-dispatch capability
//! gate had the identical bug — it called `config.capabilities(model_present)`
//! and would reject `--mode semantic` with exit 69 before the OpenAI embedder
//! path ever ran.

use std::path::Path;
use std::process::Command;

fn cairn_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    // Ensure the local model-file probe never finds weights on disk so all
    // semantic/hybrid gates are driven purely by the configured provider.
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

/// `cairn status --json` with `default_provider: openai` but WITHOUT
/// `OPENAI_API_KEY` must NOT advertise semantic or hybrid.
///
/// This pins the fail-closed behaviour of `compute_embedding_provider_ready`:
/// absent key → provider not ready → capabilities suppressed.
#[test]
fn status_drops_semantic_with_openai_provider_without_key() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\nsearch:\n  local_embeddings: true\n  default_provider: openai\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args(["--vault", tmp.path().to_str().unwrap(), "status", "--json"])
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "cairn status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("status --json must emit valid JSON");
    let caps = v["capabilities"]
        .as_array()
        .expect("capabilities must be an array");
    let cap_strings: Vec<&str> = caps.iter().filter_map(|c| c.as_str()).collect();
    assert!(
        !cap_strings.contains(&"cairn.mcp.v1.search.semantic"),
        "without OPENAI_API_KEY, semantic must be suppressed; got {cap_strings:?}"
    );
    assert!(
        !cap_strings.contains(&"cairn.mcp.v1.search.hybrid"),
        "without OPENAI_API_KEY, hybrid must be suppressed; got {cap_strings:?}"
    );
}

/// `cairn status --json` with `default_provider: openai` AND
/// `OPENAI_API_KEY` set must advertise semantic and hybrid even when no
/// local model file is on disk.
///
/// This pins the positive path of Finding A: the provider-readiness flag
/// now drives `config.capabilities()`, so a valid API key is sufficient to
/// unlock semantic/hybrid advertisement regardless of local model absence.
///
/// Only runs when the `openai` Cargo feature is compiled in (otherwise the
/// embedder cannot be constructed at all and fail-closed is correct).
#[test]
#[cfg(feature = "openai")]
fn status_advertises_semantic_with_openai_provider_and_key() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\nsearch:\n  local_embeddings: true\n  default_provider: openai\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args(["--vault", tmp.path().to_str().unwrap(), "status", "--json"])
        .env("OPENAI_API_KEY", "sk-test-key-for-readiness-only")
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "cairn status must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("status --json must emit valid JSON");
    let caps = v["capabilities"]
        .as_array()
        .expect("capabilities must be an array");
    let cap_strings: Vec<&str> = caps.iter().filter_map(|c| c.as_str()).collect();
    assert!(
        cap_strings.contains(&"cairn.mcp.v1.search.semantic"),
        "OpenAI provider with key must advertise semantic; got {cap_strings:?}"
    );
    assert!(
        cap_strings.contains(&"cairn.mcp.v1.search.hybrid"),
        "OpenAI provider with key must advertise hybrid; got {cap_strings:?}"
    );
}

/// `cairn search --mode semantic` with `default_provider: openai` but WITHOUT
/// `OPENAI_API_KEY` must be rejected at the pre-dispatch capability gate
/// (exit 69, `CapabilityUnavailable`).
///
/// This pins the fail-closed behaviour of Finding B: the search verb's
/// pre-dispatch gate now correctly derives provider readiness via
/// `embedding_provider_ready()`, so OpenAI without a key → rejected.
#[test]
fn search_rejects_semantic_with_openai_provider_without_key() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\nsearch:\n  local_embeddings: true\n  default_provider: openai\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args([
            "--vault",
            tmp.path().to_str().unwrap(),
            "search",
            "--mode",
            "semantic",
            "--json",
            "anything",
        ])
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("spawn cairn");

    assert_eq!(
        out.status.code(),
        Some(69),
        "expected exit 69 (EX_UNAVAILABLE) for OpenAI without key; got {:?}\n  stdout: {}\n  stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected valid JSON on stdout; parse error: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(
        envelope["error"]["code"], "CapabilityUnavailable",
        "rejection must be CapabilityUnavailable; envelope: {envelope}"
    );
    assert_eq!(
        envelope["error"]["data"]["capability"], "cairn.mcp.v1.search.semantic",
        "rejection must name the semantic capability; envelope: {envelope}"
    );
}

/// `cairn search --mode semantic` with `default_provider: openai` AND
/// `OPENAI_API_KEY` set must NOT exit 69 from the pre-dispatch capability gate.
///
/// It may still fail for other reasons (e.g., no DB present, network error),
/// but the failure must not be a `CapabilityUnavailable` rejection for the
/// local-model path. This pins Finding B's positive path.
///
/// Only runs when the `openai` Cargo feature is compiled in.
#[test]
#[cfg(feature = "openai")]
fn search_not_rejected_by_capability_gate_with_openai_key() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\nsearch:\n  local_embeddings: true\n  default_provider: openai\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args([
            "--vault",
            tmp.path().to_str().unwrap(),
            "search",
            "--mode",
            "semantic",
            "--json",
            "anything",
        ])
        .env("OPENAI_API_KEY", "sk-test-key-for-readiness-only")
        .output()
        .expect("spawn cairn");

    // Must not be exit 69 (CapabilityUnavailable from pre-dispatch gate).
    assert_ne!(
        out.status.code(),
        Some(69),
        "with OPENAI_API_KEY, pre-dispatch capability gate must not reject; \
         this likely means the provider-readiness fix in search.rs is broken.\n  \
         stdout: {}\n  stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // If there is a JSON envelope on stdout and it IS CapabilityUnavailable,
    // that is doubly wrong — assert it's not that specific code.
    if let Ok(stdout) = String::from_utf8(out.stdout) {
        if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            assert_ne!(
                envelope["error"]["code"], "CapabilityUnavailable",
                "capability-unavailable rejection must not appear with OpenAI key; envelope: {envelope}"
            );
        }
    }
}
