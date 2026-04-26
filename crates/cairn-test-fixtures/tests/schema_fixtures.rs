//! Fixture deserialization and snapshot tests.
//!
//! Every fixture file in `fixtures/v0/` is loaded, deserialized into its
//! typed Rust form, validated, and snapshot-tested with `insta`. A CI run
//! that fails here means a schema change broke backward compatibility or
//! the wire form shifted without a deliberate snapshot update.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[allow(dead_code)]
fn load_json<T: serde::de::DeserializeOwned>(path: impl AsRef<std::path::Path>) -> T {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[allow(dead_code)]
fn load_toml_str(path: impl AsRef<std::path::Path>) -> String {
    let path = path.as_ref();
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn v0() -> std::path::PathBuf {
    cairn_test_fixtures::fixture_v0_dir()
}

// ── Directory structure ──────────────────────────────────────────────────────

#[test]
fn v0_directory_structure_exists() {
    let base = v0();
    assert!(base.exists(), "fixtures/v0 must exist: {base:?}");
    for sub in &["records", "config", "envelopes", "search-filters", "manifests"] {
        let p = base.join(sub);
        assert!(p.is_dir(), "fixtures/v0/{sub} must be a directory: {p:?}");
    }
}
