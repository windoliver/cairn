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

/// Result of walking up `_policy.yaml` files and merging deepest-wins per key.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectivePolicy {
    /// Purpose echoed from the deepest policy that set one.
    pub purpose: Option<String>,
    /// Allowed kinds from the deepest policy that set them.
    pub allowed_kinds: Option<Vec<crate::domain::MemoryKind>>,
    /// Visibility default; falls back to `Private`.
    pub visibility_default: crate::domain::MemoryVisibility,
    /// Consolidation cadence; falls back to `Daily`.
    pub consolidation_cadence: ConsolidationCadence,
    /// Owning agent; `None` if unset anywhere in the chain.
    pub owner_agent: Option<String>,
    /// Retention; falls back to `Unlimited`.
    pub retention: RetentionPolicy,
    /// Summary token cap; falls back to 200.
    pub summary_max_tokens: u32,
    /// Folder paths that contributed, shallowest first, deepest last.
    pub source_chain: Vec<std::path::PathBuf>,
}

impl Default for EffectivePolicy {
    fn default() -> Self {
        Self {
            purpose: None,
            allowed_kinds: None,
            visibility_default: crate::domain::MemoryVisibility::Private,
            consolidation_cadence: ConsolidationCadence::Daily,
            owner_agent: None,
            retention: RetentionPolicy::Unlimited,
            summary_max_tokens: 200,
            source_chain: Vec::new(),
        }
    }
}

/// Walk from `target`'s parent up to the vault root, merging `_policy.yaml`
/// entries deepest-wins per key. Defaults from [`EffectivePolicy::default`]
/// fill in fields that no policy set.
#[must_use]
pub fn resolve_policy(
    target: &std::path::Path,
    policies_by_dir: &std::collections::BTreeMap<std::path::PathBuf, FolderPolicy>,
) -> EffectivePolicy {
    // Build the chain shallowest → deepest.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut cur = target.parent();
    while let Some(d) = cur {
        if d.as_os_str().is_empty() {
            break;
        }
        dirs.push(d.to_path_buf());
        cur = d.parent();
    }
    dirs.reverse();

    let mut effective = EffectivePolicy::default();
    for dir in dirs {
        let Some(p) = policies_by_dir.get(&dir) else {
            continue;
        };
        effective.source_chain.push(dir);
        if let Some(v) = &p.purpose {
            effective.purpose = Some(v.clone());
        }
        if let Some(v) = &p.allowed_kinds {
            effective.allowed_kinds = Some(v.clone());
        }
        if let Some(v) = p.visibility_default {
            effective.visibility_default = v;
        }
        if let Some(v) = p.consolidation_cadence {
            effective.consolidation_cadence = v;
        }
        if let Some(v) = &p.owner_agent {
            effective.owner_agent = Some(v.clone());
        }
        if let Some(v) = p.retention {
            effective.retention = v;
        }
        if let Some(v) = p.summary_max_tokens {
            effective.summary_max_tokens = v;
        }
    }
    effective
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

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn empty_chain() -> BTreeMap<PathBuf, FolderPolicy> {
        BTreeMap::new()
    }

    #[test]
    fn resolve_with_no_policies_returns_defaults() {
        let target = Path::new("raw/projects/koi/rfc.md");
        let resolved = resolve_policy(target, &empty_chain());
        assert_eq!(resolved.visibility_default, MemoryVisibility::Private);
        assert_eq!(resolved.consolidation_cadence, ConsolidationCadence::Daily);
        assert_eq!(resolved.retention, RetentionPolicy::Unlimited);
        assert_eq!(resolved.summary_max_tokens, 200);
        assert!(resolved.purpose.is_none());
        assert!(resolved.allowed_kinds.is_none());
        assert!(resolved.owner_agent.is_none());
        assert!(resolved.source_chain.is_empty());
    }

    #[test]
    fn resolve_single_root_policy_is_echoed() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("root".into()),
                consolidation_cadence: Some(ConsolidationCadence::Weekly),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/x.md");
        let resolved = resolve_policy(target, &chain);
        assert_eq!(resolved.purpose.as_deref(), Some("root"));
        assert_eq!(resolved.consolidation_cadence, ConsolidationCadence::Weekly);
        assert_eq!(resolved.source_chain, vec![PathBuf::from("raw")]);
    }

    #[test]
    fn resolve_child_overrides_parent_per_key() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("root".into()),
                consolidation_cadence: Some(ConsolidationCadence::Daily),
                summary_max_tokens: Some(100),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/projects"),
            FolderPolicy {
                consolidation_cadence: Some(ConsolidationCadence::Weekly),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/projects/koi.md");
        let resolved = resolve_policy(target, &chain);
        // Inherited from root:
        assert_eq!(resolved.purpose.as_deref(), Some("root"));
        assert_eq!(resolved.summary_max_tokens, 100);
        // Overridden by child:
        assert_eq!(resolved.consolidation_cadence, ConsolidationCadence::Weekly);
    }

    #[test]
    fn resolve_three_deep_chain_deepest_wins() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("root".into()),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/projects"),
            FolderPolicy {
                purpose: Some("projects".into()),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/projects/koi"),
            FolderPolicy {
                purpose: Some("koi".into()),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/projects/koi/rfc.md");
        let resolved = resolve_policy(target, &chain);
        assert_eq!(resolved.purpose.as_deref(), Some("koi"));
        assert_eq!(
            resolved.source_chain,
            vec![
                PathBuf::from("raw"),
                PathBuf::from("raw/projects"),
                PathBuf::from("raw/projects/koi"),
            ],
        );
    }

    #[test]
    fn resolve_source_chain_skips_dirs_without_policy() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("r".into()),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/a/b/c"),
            FolderPolicy {
                purpose: Some("c".into()),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/a/b/c/d/e.md");
        let resolved = resolve_policy(target, &chain);
        assert_eq!(
            resolved.source_chain,
            vec![PathBuf::from("raw"), PathBuf::from("raw/a/b/c")],
        );
    }
}
