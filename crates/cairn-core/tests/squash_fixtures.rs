//! Golden-file snapshot tests for `cairn_core::pipeline::squash`.
//!
//! Each test feeds a fixture file from `fixtures/v0/squash/` through `squash()`
//! with the default [`SquashConfig`] and snapshots the compacted output. Snapshots
//! live in `tests/snapshots/` and are committed alongside the code.

use cairn_core::{
    domain::{
        actor_chain::{ActorChainEntry, ChainRole},
        capture::{
            CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
            SourceFamily,
        },
        identity::Identity,
        timestamp::Rfc3339Timestamp,
    },
    pipeline::squash::{SquashConfig, TerminalContext, UnstructuredTextBytes, squash},
};
use sha2::{Digest, Sha256};

// ── helpers ──────────────────────────────────────────────────────────────────

fn workspace_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is `crates/cairn-core`; two levels up is the workspace root.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("invariant: crates/ parent exists")
        .parent()
        .expect("invariant: workspace root exists")
        .to_path_buf()
}

fn fixture(name: &str) -> Vec<u8> {
    let path = workspace_root().join("fixtures/v0/squash").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn payload_hash_of(bytes: &[u8]) -> PayloadHash {
    let digest = Sha256::digest(bytes);
    PayloadHash::parse(format!("sha256:{digest:x}"))
        .expect("invariant: sha256 hex is always a valid PayloadHash")
}

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("invariant: valid RFC-3339 literal")
}

/// Build a minimal terminal `CaptureEvent` bound to `payload_bytes`.
/// Mirrors the `terminal_event` helper in `squash.rs`'s `wrapper_tests` module.
fn terminal_event(payload_bytes: &[u8]) -> CaptureEvent {
    CaptureEvent {
        event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("invariant: valid ULID literal"),
        sensor_id: Identity::parse("snr:local:terminal:cli:v1")
            .expect("invariant: valid sensor identity"),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("snr:local:terminal:cli:v1")
                .expect("invariant: valid sensor identity"),
            at: ts(),
        }],
        refs: Some(CaptureRefs {
            session_id: Some("sess".into()),
            turn_id: Some("turn".into()),
            tool_id: None,
        }),
        payload_hash: payload_hash_of(payload_bytes),
        payload_ref: "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
        captured_at: ts(),
        payload: CapturePayload::Terminal {
            command: "echo hi".into(),
            exit_code: Some(0),
        },
        source_family: SourceFamily::Terminal,
    }
}

fn run_squash(name: &str, cfg: &SquashConfig) -> String {
    let raw = fixture(name);
    let evt = terminal_event(&raw);
    let wrapper =
        UnstructuredTextBytes::try_from_terminal_event(&evt, &raw, TerminalContext::InteractiveTty)
            .unwrap_or_else(|e| panic!("bind {name}: {e:?}"));
    let out = squash(wrapper, cfg);
    String::from_utf8_lossy(&out.compacted_bytes).into_owned()
}

// ── snapshot tests ────────────────────────────────────────────────────────────

#[test]
fn snapshot_short_ls() {
    insta::assert_snapshot!(run_squash("short_ls.txt", &SquashConfig::default()));
}

#[test]
fn snapshot_cargo_build() {
    insta::assert_snapshot!(run_squash("cargo_build.txt", &SquashConfig::default()));
}

#[test]
fn snapshot_npm_test() {
    insta::assert_snapshot!(run_squash("npm_test.txt", &SquashConfig::default()));
}

#[test]
fn snapshot_binary_junk() {
    insta::assert_snapshot!(run_squash("binary_junk.bin", &SquashConfig::default()));
}
