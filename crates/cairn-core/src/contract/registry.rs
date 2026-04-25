//! Plugin registry: typed, in-memory, host-assembled at startup.
//!
//! Brief §4.1: registration is explicit. Hosts call each plugin crate's
//! `register(&mut PluginRegistry)` (emitted by `register_plugin!`) in a
//! deterministic order, then assemble the active set from config.

use crate::contract::version::{ContractVersion, VersionRange};

/// Stable identifier for a plugin instance. Lowercase ASCII alnum + `-`
/// (matches crates.io naming), 3..=64 chars. Examples: `cairn-store-sqlite`,
/// `cairn-llm-openai-compat`, `acme-store-qdrant`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginName(String);

impl PluginName {
    /// Construct a `PluginName`, validating shape.
    ///
    /// # Errors
    /// [`PluginError::InvalidName`] when `raw` violates the naming rule.
    pub fn new(raw: impl Into<String>) -> Result<Self, PluginError> {
        let raw = raw.into();
        let valid = raw.len() >= 3
            && raw.len() <= 64
            && raw
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !raw.starts_with('-')
            && !raw.ends_with('-');
        if valid {
            Ok(Self(raw))
        } else {
            Err(PluginError::InvalidName(raw))
        }
    }

    /// Returns the plugin name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PluginName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors produced by the plugin registry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// The supplied string is not a valid plugin name.
    #[error("invalid plugin name: {0:?}")]
    InvalidName(String),

    /// A plugin with this name was already registered for this contract.
    #[error("duplicate plugin name {name} for contract {contract}")]
    DuplicateName {
        /// The contract slot that already holds this name.
        contract: &'static str,
        /// The conflicting plugin name.
        name: PluginName,
    },

    /// The plugin's accepted version range does not include the host's contract version.
    #[error(
        "plugin {plugin} for contract {contract} accepts {plugin_range:?} \
         but host is {host}"
    )]
    UnsupportedContractVersion {
        /// The contract whose version is mismatched.
        contract: &'static str,
        /// The plugin that declared incompatible version support.
        plugin: PluginName,
        /// The version range the plugin declared it supports.
        plugin_range: VersionRange,
        /// The host's current contract version.
        host: ContractVersion,
    },

    /// The plugin manifest contains invalid or missing fields.
    #[error("invalid plugin manifest: {0}")]
    InvalidManifest(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_accepts_kebab_alnum() {
        assert!(PluginName::new("cairn-store-sqlite").is_ok());
        assert!(PluginName::new("acme-llm-2").is_ok());
        assert!(PluginName::new("a1b").is_ok());
    }

    #[test]
    fn name_rejects_uppercase() {
        assert!(matches!(
            PluginName::new("Cairn-Store"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_rejects_underscore() {
        assert!(matches!(
            PluginName::new("cairn_store"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_rejects_too_short() {
        assert!(matches!(
            PluginName::new("ab"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_rejects_leading_hyphen() {
        assert!(matches!(
            PluginName::new("-cairn"),
            Err(PluginError::InvalidName(_))
        ));
    }

    #[test]
    fn name_display_matches_input() {
        let n = PluginName::new("cairn-store-sqlite").expect("valid");
        assert_eq!(n.to_string(), "cairn-store-sqlite");
    }
}

// PluginRegistry struct + impl arrives in Task 10 (Batch E) once contract traits exist.
