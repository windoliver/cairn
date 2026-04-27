//! Vault registry types for `~/.config/cairn/vaults.toml` (brief §3.3).

use serde::{Deserialize, Serialize};

/// One entry in the vault registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VaultEntry {
    /// Short human identifier, e.g. `"work"` or `"personal"`.
    pub name: String,
    /// Filesystem path to the vault root; may contain a leading `~`.
    pub path: String,
    /// Optional human label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// ISO 8601 date after which the vault is considered expired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl VaultEntry {
    /// Construct a new vault entry.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        path: impl Into<String>,
        label: Option<String>,
        expires_at: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            label,
            expires_at,
        }
    }
}

/// Parsed content of `~/.config/cairn/vaults.toml` (§3.3).
///
/// TOML shape:
/// ```toml
/// default = "work"
///
/// [[vault]]
/// name = "work"
/// path = "~/vaults/work"
/// label = "day job"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VaultRegistry {
    /// Name of the active default vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Known vaults. TOML key is `vault` (array of tables).
    #[serde(default, rename = "vault", skip_serializing_if = "Vec::is_empty")]
    pub vaults: Vec<VaultEntry>,
}

impl VaultRegistry {
    /// Parse from TOML text.
    ///
    /// # Errors
    /// Returns a `toml` deserialization error on malformed input.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Serialize to TOML text.
    ///
    /// # Errors
    /// Returns a `toml` serialization error (practically infallible for this type).
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Find a vault entry by name (exact match).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&VaultEntry> {
        self.vaults.iter().find(|v| v.name == name)
    }

    /// `true` if a vault with this name is already registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
default = "work"

[[vault]]
name = "work"
path = "~/vaults/work"
label = "day job"

[[vault]]
name = "personal"
path = "~/vaults/personal"
"#;

    #[test]
    fn parse_sample_toml() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        assert_eq!(reg.default.as_deref(), Some("work"));
        assert_eq!(reg.vaults.len(), 2);
        assert_eq!(reg.vaults[0].name, "work");
        assert_eq!(reg.vaults[0].path, "~/vaults/work");
        assert_eq!(reg.vaults[0].label.as_deref(), Some("day job"));
        assert_eq!(reg.vaults[1].name, "personal");
        assert!(reg.vaults[1].label.is_none());
    }

    #[test]
    fn empty_registry_round_trips() {
        let reg = VaultRegistry::default();
        let toml = reg.to_toml().unwrap();
        let restored = VaultRegistry::from_toml(&toml).unwrap();
        assert_eq!(reg, restored);
    }

    #[test]
    fn get_returns_entry_by_name() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        assert!(reg.get("work").is_some());
        assert!(reg.get("personal").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn contains_works() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        assert!(reg.contains("work"));
        assert!(!reg.contains("ghost"));
    }

    #[test]
    fn round_trip_preserves_entries() {
        let reg = VaultRegistry::from_toml(SAMPLE).unwrap();
        let toml = reg.to_toml().unwrap();
        let restored = VaultRegistry::from_toml(&toml).unwrap();
        assert_eq!(reg, restored);
    }

    #[test]
    fn parse_expires_at() {
        let toml = r#"
[[vault]]
name = "research"
path = "~/vaults/research"
expires_at = "2026-07-01"
"#;
        let reg = VaultRegistry::from_toml(toml).unwrap();
        assert_eq!(reg.vaults[0].expires_at.as_deref(), Some("2026-07-01"));
    }
}
