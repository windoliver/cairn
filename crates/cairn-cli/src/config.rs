//! Config loading for the `cairn` binary (brief §3.1, §6.5).
//!
//! Precedence (highest to lowest):
//! 1. `CliOverrides` (parsed CLI flags / env forwarded by the verb layer)
//! 2. `CAIRN_*` environment variables (double-underscore nested keys)
//! 3. LLM explicit-intent environment aliases (`CAIRN_LLM_*`, `OPENAI_*`, `OLLAMA_HOST`)
//! 4. `.cairn/config.yaml` with `${VAR}` interpolation
//! 5. user config (`$XDG_CONFIG_HOME/cairn/config.yaml` or `~/.config/cairn/config.yaml`)
//! 6. `CairnConfig::default()` (P0 offline-local deployment)

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use cairn_core::config::{CairnConfig, ConfigError};

/// CLI-layer overrides. Sparse at P0 — extended as verbs land.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliOverrides {}

fn env_var_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").expect("invariant: env-var regex is valid")
    })
}

/// Replace every `${VAR}` in `src` with its environment variable value.
///
/// Only `[A-Z_][A-Z0-9_]*` variable names are recognized. Placeholders with
/// lowercase names (e.g. `${not_a_var}`) are left verbatim and never trigger
/// an error.
///
/// # Errors
/// [`ConfigError::UnresolvedEnvVar`] for the first unset variable found.
pub fn interpolate_env(src: &str) -> Result<String, ConfigError> {
    let re = env_var_re();
    let mut unresolved: Option<String> = None;
    let result = re.replace_all(src, |caps: &regex::Captures<'_>| {
        let name = &caps[1];
        if let Ok(val) = std::env::var(name) {
            val
        } else {
            if unresolved.is_none() {
                unresolved = Some(name.to_owned());
            }
            caps[0].to_owned()
        }
    });
    if let Some(name) = unresolved {
        return Err(ConfigError::UnresolvedEnvVar(name));
    }
    Ok(result.into_owned())
}

/// Load and validate the active `CairnConfig` for the given vault.
///
/// Applies the precedence described in the module doc. Missing user or vault
/// config files are skipped and defaults apply.
///
/// # Errors
/// Returns an error if the YAML file cannot be read, `${VAR}` placeholders
/// cannot be resolved, config extraction fails, or `CairnConfig::validate()`
/// rejects the resulting config.
pub fn load(vault_path: &Path, cli: &CliOverrides) -> Result<CairnConfig> {
    let config_path = vault_path.join(".cairn/config.yaml");
    let mut merged =
        serde_json::to_value(CairnConfig::default()).context("serializing default config")?;

    if let Some(user_path) = user_config_path()
        && let Some(user_config) = read_yaml_overlay(&user_path)?
    {
        merge_json(&mut merged, user_config);
    }
    if let Some(vault_config) = read_yaml_overlay(&config_path)? {
        merge_json(&mut merged, vault_config);
    }

    let explicit_llm_intent = has_llm_provider(&merged) || explicit_llm_env_present();
    merge_json(&mut merged, llm_env_overlay(explicit_llm_intent));
    merge_json(&mut merged, cairn_nested_env_overlay());
    merge_json(
        &mut merged,
        serde_json::to_value(cli).context("serializing CLI overrides")?,
    );

    let config: CairnConfig = serde_json::from_value(merged).context("parsing config")?;

    config
        .validate()
        .map_err(anyhow::Error::from)
        .context("validating config")?;

    Ok(config)
}

fn read_yaml_overlay(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let interpolated = interpolate_env(&raw)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("resolving ${{VAR}} placeholders in {}", path.display()))?;
    let value: Value = yaml_serde::from_str(&interpolated)
        .with_context(|| format!("parsing {}", path.display()))?;
    if value.is_null() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn user_config_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("cairn/config.yaml"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/cairn/config.yaml"))
}

fn merge_json(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn set_path(root: &mut Map<String, Value>, path: &[&str], value: Value) {
    if let Some((head, tail)) = path.split_first() {
        if tail.is_empty() {
            root.insert((*head).to_owned(), value);
        } else {
            let entry = root
                .entry((*head).to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            if let Value::Object(map) = entry {
                set_path(map, tail, value);
            }
        }
    }
}

fn env_value(raw: String) -> Value {
    match raw.as_str() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => raw.parse::<i64>().map_or_else(
            |_| Value::String(raw),
            |n| Value::Number(serde_json::Number::from(n)),
        ),
    }
}

fn explicit_llm_env_present() -> bool {
    [
        "CAIRN_LLM_PROVIDER",
        "CAIRN_LLM_BASE_URL",
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
        "OLLAMA_HOST",
    ]
    .iter()
    .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()))
}

fn has_llm_provider(config: &Value) -> bool {
    config
        .pointer("/llm/provider")
        .is_some_and(|value| !value.is_null())
}

fn llm_env_overlay(explicit_llm_intent: bool) -> Value {
    let mut root = Map::new();

    if let Ok(host) = std::env::var("OLLAMA_HOST")
        && !host.is_empty()
    {
        set_llm_provider(&mut root);
        set_path(
            &mut root,
            &["llm", "base_url"],
            Value::String(ollama_base_url(&host)),
        );
    }
    if let Ok(base_url) = std::env::var("OPENAI_API_BASE")
        && !base_url.is_empty()
    {
        set_llm_provider(&mut root);
        set_path(&mut root, &["llm", "base_url"], Value::String(base_url));
    }
    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL")
        && !base_url.is_empty()
    {
        set_llm_provider(&mut root);
        set_path(&mut root, &["llm", "base_url"], Value::String(base_url));
    }
    if explicit_llm_intent
        && let Ok(api_key) = std::env::var("OPENAI_API_KEY")
        && !api_key.is_empty()
    {
        set_path(&mut root, &["llm", "api_key"], Value::String(api_key));
    }
    if let Ok(base_url) = std::env::var("CAIRN_LLM_BASE_URL")
        && !base_url.is_empty()
    {
        set_llm_provider(&mut root);
        set_path(&mut root, &["llm", "base_url"], Value::String(base_url));
    }
    if let Ok(provider) = std::env::var("CAIRN_LLM_PROVIDER")
        && !provider.is_empty()
    {
        set_path(&mut root, &["llm", "provider"], Value::String(provider));
    }
    if let Ok(model) = std::env::var("CAIRN_LLM_MODEL")
        && !model.is_empty()
    {
        set_path(&mut root, &["llm", "model"], Value::String(model));
    }
    if let Ok(api_key) = std::env::var("CAIRN_LLM_API_KEY")
        && !api_key.is_empty()
    {
        set_path(&mut root, &["llm", "api_key"], Value::String(api_key));
    }

    Value::Object(root)
}

fn set_llm_provider(root: &mut Map<String, Value>) {
    set_path(
        root,
        &["llm", "provider"],
        Value::String("openai-compatible".to_owned()),
    );
}

fn ollama_base_url(host: &str) -> String {
    let base = if host.starts_with("http://") || host.starts_with("https://") {
        host.trim_end_matches('/').to_owned()
    } else {
        format!("http://{}", host.trim_end_matches('/'))
    };
    if base.ends_with("/v1") {
        base
    } else {
        format!("{base}/v1")
    }
}

fn cairn_nested_env_overlay() -> Value {
    let mut root = Map::new();
    for (key, value) in std::env::vars() {
        let Some(tail) = key.strip_prefix("CAIRN_") else {
            continue;
        };
        if !tail.contains("__") {
            continue;
        }
        let parts: Vec<String> = tail.split("__").map(str::to_ascii_lowercase).collect();
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        set_path(&mut root, &refs, env_value(value));
    }
    Value::Object(root)
}

/// Write the serialized default config to `<vault_path>/.cairn/config.yaml`.
///
/// Creates `.cairn/` if it does not exist. Fails if the file already exists
/// so that re-running bootstrap never silently overwrites user edits.
///
/// # Errors
/// Returns an error if the config file already exists, the directory cannot be
/// created, YAML serialization fails, or the file cannot be written.
pub fn write_default(vault_path: &Path) -> Result<()> {
    let config_dir = vault_path.join(".cairn");
    let config_path = config_dir.join("config.yaml");

    anyhow::ensure!(
        !config_path.exists(),
        "{} already exists; delete it first to re-bootstrap",
        config_path.display()
    );

    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("creating {}", config_dir.display()))?;

    let yaml = yaml_serde::to_string(&CairnConfig::default())
        .context("serializing default config to YAML")?;

    std::fs::write(&config_path, yaml)
        .with_context(|| format!("writing {}", config_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_no_vars_unchanged() {
        let input = "vault:\n  name: my-vault\n";
        assert_eq!(interpolate_env(input).unwrap(), input);
    }

    #[test]
    fn interpolate_substitutes_set_var() {
        // Use a variable that the test runner guarantees is set.
        // `HOME` is always present on Unix; nextest also injects `CARGO_MANIFEST_DIR`.
        let home = std::env::var("HOME").expect("HOME must be set in test environment");
        let result = interpolate_env("home: ${HOME}").unwrap();
        assert_eq!(result, format!("home: {home}"));
    }

    #[test]
    fn interpolate_errors_on_unset_var() {
        // A var with this exact name is guaranteed absent by construction
        // (no CI system or shell sets it).
        const ABSENT: &str = "CAIRN_UNIT_TEST_GUARANTEED_ABSENT_7F3A";
        assert!(
            std::env::var(ABSENT).is_err(),
            "test precondition: {ABSENT} must not be set"
        );
        let err = interpolate_env(&format!("key: ${{{ABSENT}}}")).unwrap_err();
        assert!(matches!(err, ConfigError::UnresolvedEnvVar(ref v) if v == ABSENT));
    }

    #[test]
    fn interpolate_ignores_lowercase_placeholder() {
        // Only uppercase+underscore names are recognized; lowercase passes through.
        let input = "note: ${not_a_var}";
        assert_eq!(interpolate_env(input).unwrap(), input);
    }
}
