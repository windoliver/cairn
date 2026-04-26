//! Integration tests for the cairn-cli config loader (brief §3.1, §6.5).

use cairn_cli::config::{CliOverrides, load, write_default};
use cairn_core::config::{CairnConfig, StoreKind};

fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yaml"), content).unwrap();
}

// ── Loader ────────────────────────────────────────────────────────────────

#[test]
fn absent_config_file_gives_default() {
    let dir = tempfile::tempdir().unwrap();
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config, CairnConfig::default());
}

#[test]
fn load_from_file_overrides_name() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "vault:\n  name: test-vault\n");
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config.vault.name, "test-vault");
    // Unset fields stay at default
    assert_eq!(config.store.kind, StoreKind::Sqlite);
}

#[test]
fn env_var_interpolation_sets_api_key() {
    // Use HOME instead of set_var (set_var is unsafe in Rust edition 2024).
    // HOME is guaranteed to be set in any Unix test environment.
    let dir = tempfile::tempdir().unwrap();
    write_yaml(
        dir.path(),
        "llm:\n  provider: openai-compatible\n  api_key: ${HOME}\n",
    );
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(
        config.llm.api_key,
        Some(std::env::var("HOME").expect("HOME must be set in test environment"))
    );
}

#[test]
fn missing_env_var_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    // CAIRN_IT_MISSING_VAR_TEST is not set in any test environment
    write_yaml(
        dir.path(),
        "llm:\n  api_key: ${CAIRN_IT_MISSING_VAR_TEST}\n",
    );
    let err = load(dir.path(), &CliOverrides::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("CAIRN_IT_MISSING_VAR_TEST"),
        "error should name the unresolved var: {msg}"
    );
}

#[test]
fn cairn_env_override_wins_over_file() {
    // Use temp_env::with_var instead of set_var/remove_var (unsafe in edition 2024).
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "store:\n  kind: nexus-sandbox\n");
    temp_env::with_var("CAIRN_STORE__KIND", Some("sqlite"), || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        // CAIRN_STORE__KIND=sqlite overrides the file's nexus-sandbox
        assert_eq!(config.store.kind, StoreKind::Sqlite);
    });
}

#[test]
fn invalid_config_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    // zero budget is invalid
    write_yaml(dir.path(), "vault:\n  hot_memory:\n    max_bytes: 0\n");
    let err = load(dir.path(), &CliOverrides::default()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("max_bytes"),
        "error should mention the bad field: {msg}"
    );
}

// ── Bootstrap ─────────────────────────────────────────────────────────────

#[test]
fn bootstrap_writes_config_file() {
    let dir = tempfile::tempdir().unwrap();
    write_default(dir.path()).unwrap();
    assert!(dir.path().join(".cairn/config.yaml").exists());
}

#[test]
fn bootstrap_round_trips_to_default() {
    let dir = tempfile::tempdir().unwrap();
    write_default(dir.path()).unwrap();
    let config = load(dir.path(), &CliOverrides::default()).unwrap();
    assert_eq!(config, CairnConfig::default());
}

#[test]
fn bootstrap_fails_if_file_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    write_yaml(dir.path(), "vault:\n  name: existing\n");
    let err = write_default(dir.path()).unwrap_err();
    assert!(
        format!("{err}").contains("already exists"),
        "should describe the conflict: {err}"
    );
}
