//! `FolderPolicy`, `EffectivePolicy`, parse + resolve.

use serde::{Deserialize, Serialize};

use crate::domain::folder::FolderError;
use crate::domain::{MemoryKind, MemoryVisibility};

/// Per-folder configuration deserialized from `_policy.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderPolicy {
    /// Single-line per-folder purpose; echoed into `_index.md` frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Kinds permitted in this folder. `None` = inherit; `Some(empty)` = forbid all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_kinds: Option<Vec<MemoryKind>>,
    /// Visibility default when `None` chosen at ingest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_default: Option<MemoryVisibility>,
    /// Override for the global consolidation cadence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consolidation_cadence: Option<ConsolidationCadence>,
    /// Agent that owns summary regeneration for this folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent: Option<String>,
    /// Retention policy override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionPolicy>,
    /// Cap for `_summary.md` regeneration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_max_tokens: Option<u32>,
}

/// Cadence on which `_summary.md` is regenerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ConsolidationCadence {
    /// Hourly cadence.
    Hourly,
    /// Daily cadence (default).
    Daily,
    /// Weekly cadence.
    Weekly,
    /// Monthly cadence.
    Monthly,
    /// Manual (no automatic regeneration).
    Manual,
}

/// Retention policy override for a folder.
///
/// Serializes as a plain integer for `Days(n)` and as the string `"unlimited"`
/// for `Unlimited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetentionPolicy {
    /// Keep records for `Days(n)` since their last update.
    Days(u32),
    /// Keep records indefinitely.
    Unlimited,
}

impl Serialize for RetentionPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Days(n) => serializer.serialize_u32(*n),
            Self::Unlimited => serializer.serialize_str("unlimited"),
        }
    }
}

impl<'de> Deserialize<'de> for RetentionPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RetentionVisitor;

        impl serde::de::Visitor<'_> for RetentionVisitor {
            type Value = RetentionPolicy;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, r#"a positive integer or the string "unlimited""#)
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<RetentionPolicy, E> {
                u32::try_from(v)
                    .map(RetentionPolicy::Days)
                    .map_err(|_| E::custom(format!("day count {v} overflows u32")))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<RetentionPolicy, E> {
                if v.eq_ignore_ascii_case("unlimited") {
                    Ok(RetentionPolicy::Unlimited)
                } else {
                    Err(E::unknown_variant(v, &["unlimited"]))
                }
            }
        }

        deserializer.deserialize_any(RetentionVisitor)
    }
}

/// Parse a `_policy.yaml` content string.
///
/// # Errors
///
/// Returns [`FolderError::PolicyParse`] if YAML is malformed or contains
/// unknown keys (the struct is `deny_unknown_fields`).
pub fn parse_policy(yaml: &str) -> Result<FolderPolicy, FolderError> {
    if yaml.trim().is_empty() {
        return Ok(FolderPolicy::default());
    }
    serde_yaml::from_str(yaml).map_err(|source| FolderError::PolicyParse { source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_every_field() {
        let yaml = "
purpose: people Cairn knows about
allowed_kinds: [user, feedback]
visibility_default: private
consolidation_cadence: weekly
owner_agent: agt:cairn-librarian:v2
retention: 90
summary_max_tokens: 300
";
        let policy = parse_policy(yaml).expect("parse");
        assert_eq!(policy.purpose.as_deref(), Some("people Cairn knows about"));
        assert_eq!(policy.allowed_kinds.as_ref().map(Vec::len), Some(2));
        assert_eq!(policy.visibility_default, Some(MemoryVisibility::Private));
        assert_eq!(
            policy.consolidation_cadence,
            Some(ConsolidationCadence::Weekly),
        );
        assert_eq!(policy.owner_agent.as_deref(), Some("agt:cairn-librarian:v2"));
        assert_eq!(policy.retention, Some(RetentionPolicy::Days(90)));
        assert_eq!(policy.summary_max_tokens, Some(300));
    }

    #[test]
    fn parse_unknown_key_returns_policy_parse() {
        let yaml = "unknown_key: 42\n";
        let err = parse_policy(yaml).unwrap_err();
        assert!(matches!(err, FolderError::PolicyParse { .. }));
    }

    #[test]
    fn parse_malformed_yaml_returns_policy_parse() {
        let yaml = "purpose: [unclosed";
        let err = parse_policy(yaml).unwrap_err();
        assert!(matches!(err, FolderError::PolicyParse { .. }));
    }

    #[test]
    fn parse_empty_yaml_returns_default() {
        let policy = parse_policy("").expect("parse empty");
        assert_eq!(policy, FolderPolicy::default());
        let policy = parse_policy("   \n\n").expect("parse whitespace");
        assert_eq!(policy, FolderPolicy::default());
    }

    #[test]
    fn retention_unlimited_round_trip() {
        let yaml = "retention: unlimited\n";
        let policy = parse_policy(yaml).expect("parse");
        assert_eq!(policy.retention, Some(RetentionPolicy::Unlimited));
    }
}
