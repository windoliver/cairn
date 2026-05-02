//! Config loading for the `cairn` binary (brief §3.1, §6.5).
//!
//! Precedence (highest to lowest):
//! 1. `CliOverrides` (parsed CLI flags / env forwarded by the verb layer)
//! 2. Documented LLM environment aliases (`CAIRN_LLM_PROVIDER`, `OLLAMA_HOST`, etc.)
//! 3. `CAIRN_*` environment variables (double-underscore nested keys)
//! 4. `.cairn/config.yaml` with `${VAR}` interpolation
//! 5. `CairnConfig::default()` (P0 offline-local deployment)

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use cairn_core::config::{CairnConfig, ConfigError, LlmProvider};

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
/// Applies the four-layer precedence described in the module doc. If no
/// `.cairn/config.yaml` exists the file layer is skipped and defaults apply.
///
/// # Errors
/// Returns an error if the YAML file cannot be read, `${VAR}` placeholders
/// cannot be resolved, figment extraction fails, or `CairnConfig::validate()`
/// rejects the resulting config.
pub fn load(vault_path: &Path, cli: &CliOverrides) -> Result<CairnConfig> {
    use figment::Figment;
    use figment::providers::{Env, Format, Serialized, Yaml};

    let config_path = vault_path.join(".cairn/config.yaml");

    let yaml_content: String = if config_path.exists() {
        let raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        interpolate_env(&raw)
            .map_err(anyhow::Error::from)
            .with_context(|| "resolving ${VAR} placeholders in config")?
    } else {
        String::new()
    };

    let documented_env =
        documented_llm_env_config().context("applying documented LLM environment aliases")?;

    let mut config: CairnConfig = Figment::new()
        .merge(Serialized::defaults(CairnConfig::default()))
        .merge(Yaml::string(&yaml_content))
        .merge(Env::prefixed("CAIRN_").split("__"))
        .merge(Serialized::globals(documented_env))
        .merge(Serialized::globals(cli))
        .extract()
        .context("parsing config")?;

    apply_openai_api_key_for_explicit_intent(&mut config);

    config
        .validate()
        .map_err(anyhow::Error::from)
        .context("validating config")?;

    Ok(config)
}

#[derive(Debug, Clone, Default, Serialize)]
struct DocumentedEnvConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    llm: Option<DocumentedLlmEnvConfig>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct DocumentedLlmEnvConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<LlmProvider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
}

fn documented_llm_env_config() -> Result<DocumentedEnvConfig> {
    let mut llm = DocumentedLlmEnvConfig::default();

    if let Some(provider) = env_value("CAIRN_LLM_PROVIDER") {
        llm.provider = Some(parse_documented_llm_provider(&provider)?);
    }

    let endpoint_from_env = if let Some(base_url) = env_value("CAIRN_LLM_BASE_URL") {
        llm.base_url = Some(base_url);
        true
    } else if let Some(base_url) = env_value("OPENAI_BASE_URL") {
        llm.base_url = Some(base_url);
        llm.provider = Some(LlmProvider::OpenaiCompatible);
        true
    } else if let Some(base_url) = env_value("OPENAI_API_BASE") {
        llm.base_url = Some(base_url);
        llm.provider = Some(LlmProvider::OpenaiCompatible);
        true
    } else if let Some(host) = env_value("OLLAMA_HOST") {
        llm.base_url = Some(ollama_host_to_openai_base_url(&host));
        llm.provider = Some(LlmProvider::OpenaiCompatible);
        true
    } else {
        false
    };

    if let Some(model) = env_value("CAIRN_LLM_MODEL") {
        llm.model = Some(model);
    }

    if let Some(api_key) = env_value("CAIRN_LLM_API_KEY") {
        llm.api_key = Some(api_key);
    } else if (endpoint_from_env || llm.provider.is_some())
        && let Some(api_key) = env_value("OPENAI_API_KEY")
    {
        llm.api_key = Some(api_key);
    }

    if endpoint_from_env && llm.provider.is_none() {
        llm.provider = Some(LlmProvider::OpenaiCompatible);
    }

    let has_any = llm.provider.is_some()
        || llm.base_url.is_some()
        || llm.model.is_some()
        || llm.api_key.is_some();
    Ok(DocumentedEnvConfig {
        llm: has_any.then_some(llm),
    })
}

fn apply_openai_api_key_for_explicit_intent(config: &mut CairnConfig) {
    if env_value("CAIRN_LLM_API_KEY").is_some() {
        return;
    }

    let has_explicit_intent = config.llm.provider.is_some() || config.llm.base_url.is_some();
    if has_explicit_intent && let Some(api_key) = env_value("OPENAI_API_KEY") {
        config.llm.api_key = Some(api_key);
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn parse_documented_llm_provider(raw: &str) -> Result<LlmProvider> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openai-compatible" | "ollama" => Ok(LlmProvider::OpenaiCompatible),
        other => anyhow::bail!(
            "unsupported CAIRN_LLM_PROVIDER {other:?}; expected openai-compatible or ollama"
        ),
    }
}

fn ollama_host_to_openai_base_url(raw: &str) -> String {
    let host = raw.trim().trim_end_matches('/');
    let base = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("http://{host}")
    };
    if base.ends_with("/v1") {
        base
    } else {
        format!("{base}/v1")
    }
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

    let yaml = serde_yaml::to_string(&CairnConfig::default())
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
