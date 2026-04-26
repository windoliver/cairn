//! Fixture deserialization and snapshot tests.
//!
//! Every fixture file in `fixtures/v0/` is loaded, deserialized into its
//! typed Rust form, validated, and snapshot-tested with `insta`. A CI run
//! that fails here means a schema change broke backward compatibility or
//! the wire form shifted without a deliberate snapshot update.

#![allow(clippy::unwrap_used, clippy::expect_used)]

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

// ── Records ─────────────────────────────────────────────────────────────────

use cairn_core::domain::record::MemoryRecord;

fn records_dir() -> std::path::PathBuf {
    v0().join("records")
}

#[test]
fn record_semantic_private_deserializes_and_validates() {
    let r: MemoryRecord = load_json(records_dir().join("semantic_private_user.json"));
    r.validate().expect("semantic_private_user must pass validate()");
    insta::assert_json_snapshot!("record_semantic_private_user", &r);
}

#[test]
fn record_episodic_session_deserializes_and_validates() {
    let r: MemoryRecord = load_json(records_dir().join("episodic_session_trace.json"));
    r.validate().expect("episodic_session_trace must pass validate()");
    insta::assert_json_snapshot!("record_episodic_session_trace", &r);
}

#[test]
fn record_procedural_project_deserializes_and_validates() {
    let r: MemoryRecord = load_json(records_dir().join("procedural_project_playbook.json"));
    r.validate().expect("procedural_project_playbook must pass validate()");
    insta::assert_json_snapshot!("record_procedural_project_playbook", &r);
}

#[test]
fn record_graph_team_deserializes_and_validates() {
    let r: MemoryRecord = load_json(records_dir().join("graph_team_entity.json"));
    r.validate().expect("graph_team_entity must pass validate()");
    insta::assert_json_snapshot!("record_graph_team_entity", &r);
}

// ── Config ───────────────────────────────────────────────────────────────────

use cairn_core::config::CairnConfig;

fn config_dir() -> std::path::PathBuf {
    v0().join("config")
}

#[test]
fn config_p0_defaults_deserializes_and_validates() {
    let c: CairnConfig = load_json(config_dir().join("p0-defaults.json"));
    c.validate().expect("p0-defaults must pass validate()");
    insta::assert_json_snapshot!("config_p0_defaults", &c);
}

#[test]
fn config_llm_enabled_deserializes_and_validates() {
    let c: CairnConfig = load_json(config_dir().join("llm-enabled.json"));
    c.validate().expect("llm-enabled must pass validate()");
    insta::assert_json_snapshot!("config_llm_enabled", &c);
}

#[test]
fn config_custom_store_deserializes_and_validates() {
    let c: CairnConfig = load_json(config_dir().join("custom-store.json"));
    c.validate().expect("custom-store must pass validate()");
    insta::assert_json_snapshot!("config_custom_store", &c);
}
