//! Integration tests for the cairn-cli config loader (brief §3.1, §6.5).

use cairn_cli::config::{CliOverrides, load, write_default};
use cairn_core::config::{CairnConfig, LlmProvider, StoreKind};

fn write_yaml(vault: &std::path::Path, content: &str) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yaml"), content).unwrap();
}

fn clean_env_vars(xdg_config_home: &std::path::Path) -> Vec<(String, Option<String>)> {
    let mut vars: Vec<(String, Option<String>)> = std::env::vars()
        .filter(|(key, _)| {
            key.starts_with("CAIRN_")
                || key.starts_with("OPENAI_")
                || key == "OLLAMA_HOST"
                || key == "XDG_CONFIG_HOME"
        })
        .map(|(key, _)| (key, None))
        .collect();
    vars.push((
        "XDG_CONFIG_HOME".to_owned(),
        Some(xdg_config_home.to_string_lossy().into_owned()),
    ));
    vars
}

fn with_clean_config_env<R>(extra: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
    let xdg = tempfile::tempdir().unwrap();
    let mut vars = clean_env_vars(xdg.path());
    vars.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).map(str::to_owned))),
    );
    temp_env::with_vars(vars, f)
}

// ── Loader ────────────────────────────────────────────────────────────────

#[test]
fn absent_config_file_gives_default() {
    with_clean_config_env(&[], || {
        let dir = tempfile::tempdir().unwrap();
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config, CairnConfig::default());
    });
}

#[test]
fn load_from_file_overrides_name() {
    with_clean_config_env(&[], || {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(dir.path(), "vault:\n  name: test-vault\n");
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config.vault.name, "test-vault");
        // Unset fields stay at default
        assert_eq!(config.store.kind, StoreKind::Sqlite);
    });
}

#[test]
fn env_var_interpolation_sets_api_key() {
    with_clean_config_env(&[], || {
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
    });
}

#[test]
fn missing_env_var_returns_error() {
    with_clean_config_env(&[], || {
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
    });
}

#[test]
fn cairn_env_override_wins_over_file() {
    // Use temp_env instead of set_var/remove_var (unsafe in edition 2024).
    with_clean_config_env(&[("CAIRN_STORE__KIND", Some("sqlite"))], || {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(dir.path(), "store:\n  kind: nexus-sandbox\n");
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        // CAIRN_STORE__KIND=sqlite overrides the file's nexus-sandbox
        assert_eq!(config.store.kind, StoreKind::Sqlite);
    });
}

#[test]
fn invalid_config_returns_error() {
    with_clean_config_env(&[], || {
        let dir = tempfile::tempdir().unwrap();
        // zero budget is invalid
        write_yaml(dir.path(), "vault:\n  hot_memory:\n    max_bytes: 0\n");
        let err = load(dir.path(), &CliOverrides::default()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("vault.hot_memory.max_bytes"),
            "error should mention the bad field: {msg}"
        );
    });
}

#[test]
fn unknown_config_key_returns_error() {
    with_clean_config_env(&[], || {
        let dir = tempfile::tempdir().unwrap();
        write_yaml(dir.path(), "vault:\n  typo_name: wrong\n");
        let err = load(dir.path(), &CliOverrides::default()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown field") && msg.contains("typo_name"),
            "error should reject and name the unknown key: {msg}"
        );
    });
}

#[test]
fn user_config_loads_below_vault_config() {
    let dir = tempfile::tempdir().unwrap();
    let xdg = tempfile::tempdir().unwrap();
    let user_dir = xdg.path().join("cairn");
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::write(user_dir.join("config.yaml"), "vault:\n  name: user-level\n").unwrap();
    write_yaml(dir.path(), "vault:\n  name: vault-level\n");

    let mut vars = clean_env_vars(xdg.path());
    vars.push(("HOME".to_owned(), None));
    temp_env::with_vars(vars, || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config.vault.name, "vault-level");
    });
}

#[test]
fn llm_alias_env_vars_map_to_nested_config() {
    with_clean_config_env(
        &[
            ("CAIRN_LLM_PROVIDER", Some("ollama")),
            ("CAIRN_LLM_BASE_URL", Some("http://localhost:1234/v1")),
            ("CAIRN_LLM_MODEL", Some("qwen2.5")),
            ("CAIRN_LLM_API_KEY", Some("local-key")),
        ],
        || {
            let dir = tempfile::tempdir().unwrap();
            let config = load(dir.path(), &CliOverrides::default()).unwrap();
            assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
            assert_eq!(
                config.llm.base_url.as_deref(),
                Some("http://localhost:1234/v1")
            );
            assert_eq!(config.llm.model.as_deref(), Some("qwen2.5"));
            assert_eq!(config.llm.api_key.as_deref(), Some("local-key"));
        },
    );
}

#[test]
fn openai_api_key_alone_does_not_configure_llm() {
    with_clean_config_env(&[("OPENAI_API_KEY", Some("ambient-key"))], || {
        let dir = tempfile::tempdir().unwrap();
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config.llm.provider, None);
        assert_eq!(config.llm.api_key, None);
    });
}

#[test]
fn openai_api_base_legacy_alias_configures_llm() {
    with_clean_config_env(
        &[
            ("OPENAI_API_BASE", Some("http://gateway.local/v1")),
            ("OPENAI_API_KEY", Some("ambient-key")),
        ],
        || {
            let dir = tempfile::tempdir().unwrap();
            let config = load(dir.path(), &CliOverrides::default()).unwrap();
            assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
            assert_eq!(
                config.llm.base_url.as_deref(),
                Some("http://gateway.local/v1")
            );
            assert_eq!(config.llm.api_key.as_deref(), Some("ambient-key"));
        },
    );
}

#[test]
fn openai_base_url_wins_over_legacy_alias_and_ollama_host() {
    with_clean_config_env(
        &[
            ("OPENAI_BASE_URL", Some("http://preferred.local/v1")),
            ("OPENAI_API_BASE", Some("http://legacy.local/v1")),
            ("OPENAI_API_KEY", Some("ambient-key")),
            ("OLLAMA_HOST", Some("localhost:11434")),
        ],
        || {
            let dir = tempfile::tempdir().unwrap();
            let config = load(dir.path(), &CliOverrides::default()).unwrap();
            assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
            assert_eq!(
                config.llm.base_url.as_deref(),
                Some("http://preferred.local/v1")
            );
            assert_eq!(config.llm.api_key.as_deref(), Some("ambient-key"));
        },
    );
}

#[test]
fn ollama_host_is_explicit_llm_intent() {
    with_clean_config_env(&[("OLLAMA_HOST", Some("localhost:11434"))], || {
        let dir = tempfile::tempdir().unwrap();
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config.llm.provider, Some(LlmProvider::OpenaiCompatible));
        assert_eq!(
            config.llm.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
    });
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
    with_clean_config_env(&[], || {
        let dir = tempfile::tempdir().unwrap();
        write_default(dir.path()).unwrap();
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
