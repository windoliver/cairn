//! Integration tests for the cairn-cli config loader (brief §3.1, §6.5).

use cairn_cli::config::{CliOverrides, load, write_default};
use cairn_core::config::{CairnConfig, LlmProvider, StoreKind};

fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yaml"), content).unwrap();
}

fn with_clean_llm_env<R>(overrides: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
    let mut vars = vec![
        ("CAIRN_LLM_PROVIDER", None),
        ("CAIRN_LLM_BASE_URL", None),
        ("CAIRN_LLM_MODEL", None),
        ("CAIRN_LLM_API_KEY", None),
        ("CAIRN_LLM__PROVIDER", None),
        ("CAIRN_LLM__BASE_URL", None),
        ("CAIRN_LLM__MODEL", None),
        ("CAIRN_LLM__API_KEY", None),
        ("OPENAI_BASE_URL", None),
        ("OPENAI_API_BASE", None),
        ("OPENAI_API_KEY", None),
        ("OLLAMA_HOST", None),
    ];
    vars.extend_from_slice(overrides);
    temp_env::with_vars(vars, f)
}

// ── Loader ────────────────────────────────────────────────────────────────

#[test]
fn absent_config_file_gives_default() {
    let dir = tempfile::tempdir().unwrap();
    with_clean_llm_env(&[], || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config, CairnConfig::default());
    });
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
    with_clean_llm_env(&[], || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(
            config.llm.api_key,
            Some(std::env::var("HOME").expect("HOME must be set in test environment"))
        );
    });
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
fn documented_flat_cairn_llm_env_vars_are_loaded() {
    let dir = tempfile::tempdir().unwrap();
    with_clean_llm_env(
        &[
            ("CAIRN_LLM_PROVIDER", Some("openai-compatible")),
            ("CAIRN_LLM_BASE_URL", Some("http://localhost:11434/v1")),
            ("CAIRN_LLM_MODEL", Some("llama3.2")),
            ("CAIRN_LLM_API_KEY", Some("ollama")),
        ],
        || {
            let config = load(dir.path(), &CliOverrides::default()).unwrap();
            assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
            assert_eq!(
                config.llm.base_url.as_deref(),
                Some("http://localhost:11434/v1")
            );
            assert_eq!(config.llm.model.as_deref(), Some("llama3.2"));
            assert_eq!(config.llm.api_key.as_deref(), Some("ollama"));
        },
    );
}

#[test]
fn documented_openai_base_env_vars_are_loaded() {
    let dir = tempfile::tempdir().unwrap();
    with_clean_llm_env(
        &[
            ("OPENAI_API_BASE", Some("https://legacy.example/v1")),
            ("OPENAI_BASE_URL", Some("https://openai.example/v1")),
            ("OPENAI_API_KEY", Some("sk-test")),
        ],
        || {
            let config = load(dir.path(), &CliOverrides::default()).unwrap();
            assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
            assert_eq!(
                config.llm.base_url.as_deref(),
                Some("https://openai.example/v1")
            );
            assert_eq!(config.llm.api_key.as_deref(), Some("sk-test"));
        },
    );
}

#[test]
fn documented_ollama_host_env_var_is_loaded() {
    let dir = tempfile::tempdir().unwrap();
    with_clean_llm_env(&[("OLLAMA_HOST", Some("localhost:11434"))], || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
        assert_eq!(
            config.llm.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
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
        msg.contains("vault.hot_memory.max_bytes"),
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
    with_clean_llm_env(&[], || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config, CairnConfig::default());
    });
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
