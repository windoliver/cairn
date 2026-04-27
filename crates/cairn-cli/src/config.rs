//! Config loading for the `cairn` binary (brief §3.1, §6.5).
//!
//! Precedence (highest to lowest):
//! 1. `CliOverrides` (parsed CLI flags / env forwarded by the verb layer)
//! 2. `CAIRN_*` environment variables (double-underscore nested keys)
//! 3. `.cairn/config.yaml` with `${VAR}` interpolation
//! 4. `CairnConfig::default()` (P0 offline-local deployment)

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

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

    let config: CairnConfig = Figment::new()
        .merge(Serialized::defaults(CairnConfig::default()))
        .merge(Yaml::string(&yaml_content))
        .merge(Env::prefixed("CAIRN_").split("__"))
        .merge(Serialized::globals(cli))
        .extract()
        .context("parsing config")?;

    config
        .validate()
        .map_err(anyhow::Error::from)
        .context("validating config")?;

    Ok(config)
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
