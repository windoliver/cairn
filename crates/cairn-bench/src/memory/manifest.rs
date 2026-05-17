//! Memory-gate manifest parser. Reads `manifests/memory.toml`, applies profile inheritance.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// A single asset entry in the manifest.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct AssetSpec {
    /// Source identifier used by the sizer to resolve to a filesystem path.
    pub source: String,
    /// Expected size in MiB.
    pub expected_mb: f64,
    /// Whether this asset is enforced at P0. Non-enforced assets are tracked
    /// but do not count toward budget or trigger per-asset failures.
    #[serde(default = "default_enforced")]
    pub enforced: bool,
}

fn default_enforced() -> bool {
    true
}

/// Raw profile as read from TOML (before inheritance resolution).
#[derive(Debug, Deserialize, Clone)]
pub struct RawProfile {
    /// Path to the release binary relative to workspace root.
    pub binary: Option<String>,
    /// Name of the parent profile to inherit from.
    #[serde(default)]
    pub extends: Option<String>,
    /// Cargo feature flags to pass to `cargo build --release`.
    #[serde(default)]
    pub features: Vec<String>,
    /// Asset list (overrides parent's list when non-empty).
    #[serde(default)]
    pub assets: Vec<AssetSpec>,
    /// Assets to append after inheritance.
    #[serde(default)]
    pub assets_add: Vec<AssetSpec>,
    /// Total budget in MiB (enforced assets only).
    pub budget_mb: f64,
}

/// Raw manifest as read from TOML.
#[derive(Debug, Deserialize)]
pub struct RawManifest {
    /// Named profiles.
    pub profile: HashMap<String, RawProfile>,
}

/// A profile after inheritance has been resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    /// Profile name.
    pub name: String,
    /// Path to the release binary.
    pub binary: String,
    /// Cargo features for this profile.
    pub features: Vec<String>,
    /// Resolved asset list.
    pub assets: Vec<AssetSpec>,
    /// Total enforced-asset budget in MiB.
    pub budget_mb: f64,
}

/// Errors that can occur while loading or resolving the manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// I/O error reading the manifest file.
    #[error("manifest read error: {0}")]
    Read(#[from] std::io::Error),
    /// TOML parse error.
    #[error("manifest parse error: {0}")]
    Parse(#[from] toml::de::Error),
    /// Requested profile not found in the manifest.
    #[error("profile not found: {0}")]
    UnknownProfile(String),
    /// Cyclic profile inheritance detected.
    #[error("profile cycle detected at: {0}")]
    Cycle(String),
}

/// Load and parse a `memory.toml` file.
///
/// # Errors
/// Returns [`ManifestError`] if the file cannot be read or the TOML is invalid.
pub fn load(path: &Path) -> Result<RawManifest, ManifestError> {
    let bytes = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&bytes)?)
}

/// Resolve a profile by name, applying `extends` inheritance.
///
/// # Errors
/// Returns [`ManifestError::UnknownProfile`] if the profile name is not in the manifest,
/// or [`ManifestError::Cycle`] if profile inheritance is cyclic.
pub fn resolve(manifest: &RawManifest, profile: &str) -> Result<ResolvedProfile, ManifestError> {
    let mut seen = Vec::new();
    resolve_inner(manifest, profile, &mut seen)
}

fn resolve_inner(
    manifest: &RawManifest,
    profile: &str,
    seen: &mut Vec<String>,
) -> Result<ResolvedProfile, ManifestError> {
    if seen.iter().any(|p| p == profile) {
        return Err(ManifestError::Cycle(profile.into()));
    }
    seen.push(profile.into());
    let raw = manifest
        .profile
        .get(profile)
        .ok_or_else(|| ManifestError::UnknownProfile(profile.into()))?;

    let (base_binary, base_features, mut assets) = if let Some(parent) = &raw.extends {
        let parent_resolved = resolve_inner(manifest, parent, seen)?;
        (
            parent_resolved.binary,
            parent_resolved.features,
            parent_resolved.assets,
        )
    } else {
        (String::new(), Vec::new(), Vec::new())
    };

    let binary = raw.binary.clone().unwrap_or(base_binary);
    let mut features = base_features;
    features.extend(raw.features.iter().cloned());

    if !raw.assets.is_empty() {
        assets.clone_from(&raw.assets);
    }
    assets.extend(raw.assets_add.iter().cloned());

    Ok(ResolvedProfile {
        name: profile.into(),
        binary,
        features,
        assets,
        budget_mb: raw.budget_mb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> RawManifest {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn resolves_default_profile() {
        let m = parse(
            r#"
            [profile.default]
            binary = "bin"
            assets = [ { source = "a", expected_mb = 10, enforced = true } ]
            budget_mb = 100
        "#,
        );
        let p = resolve(&m, "default").unwrap();
        assert_eq!(p.binary, "bin");
        assert_eq!(p.assets.len(), 1);
        assert!((p.budget_mb - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn enforced_defaults_to_true_when_absent() {
        let m = parse(
            r#"
            [profile.default]
            binary = "bin"
            assets = [ { source = "a", expected_mb = 10 } ]
            budget_mb = 100
        "#,
        );
        let p = resolve(&m, "default").unwrap();
        assert!(p.assets[0].enforced);
    }

    #[test]
    fn profile_extends_inherits_assets_then_adds() {
        let m = parse(
            r#"
            [profile.default]
            binary = "bin"
            assets = [ { source = "a", expected_mb = 10, enforced = true } ]
            budget_mb = 100

            [profile.heavy]
            extends = "default"
            assets_add = [ { source = "b", expected_mb = 50, enforced = false } ]
            budget_mb = 200
        "#,
        );
        let p = resolve(&m, "heavy").unwrap();
        assert_eq!(p.binary, "bin");
        assert_eq!(p.assets.len(), 2);
        assert!((p.budget_mb - 200.0).abs() < f64::EPSILON);
        assert!(p.assets[0].enforced);
        assert!(!p.assets[1].enforced);
    }

    #[test]
    fn unknown_profile_errs() {
        let m = parse(
            r#"[profile.default]
            binary = "b"
            assets = []
            budget_mb = 10
        "#,
        );
        let err = resolve(&m, "nope").unwrap_err();
        assert!(matches!(err, ManifestError::UnknownProfile(_)));
    }

    #[test]
    fn extend_cycle_errs() {
        let m = parse(
            r#"
            [profile.a]
            extends = "b"
            assets = []
            budget_mb = 1

            [profile.b]
            extends = "a"
            assets = []
            budget_mb = 1
        "#,
        );
        let err = resolve(&m, "a").unwrap_err();
        assert!(matches!(err, ManifestError::Cycle(_)));
    }
}
