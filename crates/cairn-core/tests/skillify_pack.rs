#![allow(missing_docs)]

use cairn_core::pipeline::skillify::{SkillPackEntry, SkillPackError, SkillPackManifest};

fn valid_entry(candidate_id: &str, lane: &str, slug: &str) -> SkillPackEntry {
    SkillPackEntry {
        candidate_id: candidate_id.to_owned(),
        lane: lane.to_owned(),
        slug: slug.to_owned(),
        bundle_version: 1,
        artifact_sha256: "sha256:aaaa".to_owned(),
    }
}

fn valid_manifest() -> SkillPackManifest {
    SkillPackManifest {
        pack_id: "skp_test".to_owned(),
        name: "test-pack".to_owned(),
        version: "0.1.0".to_owned(),
        cairn_compat: ">=0.1.0".to_owned(),
        description: "A test skill pack".to_owned(),
        skills: vec![
            valid_entry("skc_a", "deploy.hotfix", "deploy-hotfix"),
            valid_entry("skc_b", "test.smoke", "test-smoke"),
        ],
        requires: vec![],
        provides: vec!["deploy.hotfix".to_owned(), "test.smoke".to_owned()],
        content_sha256: "sha256:bbbb".to_owned(),
    }
}

#[test]
fn valid_manifest_passes() {
    assert!(valid_manifest().validate("0.1.0").is_ok());
}

#[test]
fn duplicate_lane_rejected() {
    let mut manifest = valid_manifest();
    manifest.skills[1].lane = "deploy.hotfix".to_owned();
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::DuplicateLane { .. }));
}

#[test]
fn incompatible_cairn_version_rejected() {
    let manifest = valid_manifest();
    let err = manifest.validate("0.0.1").unwrap_err();
    assert!(matches!(err, SkillPackError::IncompatibleCairn { .. }));
}

#[test]
fn missing_dependency_rejected() {
    let mut manifest = valid_manifest();
    manifest.requires = vec!["database.backup".to_owned()];
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::DependencyMissing { .. }));
}

#[test]
fn dependency_satisfied_by_provides() {
    let mut manifest = valid_manifest();
    manifest.requires = vec!["deploy.hotfix".to_owned()];
    assert!(manifest.validate("0.1.0").is_ok());
}

#[test]
fn empty_name_rejected() {
    let mut manifest = valid_manifest();
    manifest.name = String::new();
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::InvalidName { .. }));
}

#[test]
fn name_with_special_chars_rejected() {
    let mut manifest = valid_manifest();
    manifest.name = "test/../pack".to_owned();
    let err = manifest.validate("0.1.0").unwrap_err();
    assert!(matches!(err, SkillPackError::InvalidName { .. }));
}

#[test]
fn pack_id_derivation_is_deterministic() {
    let id1 = SkillPackManifest::derive_pack_id("test-pack", "0.1.0", &["skc_a", "skc_b"]);
    let id2 = SkillPackManifest::derive_pack_id("test-pack", "0.1.0", &["skc_b", "skc_a"]);
    assert_eq!(id1, id2);
    assert!(id1.starts_with("skp_"));
}

#[test]
fn serde_round_trip() {
    let manifest = valid_manifest();
    let json = serde_json::to_string(&manifest).unwrap();
    let parsed: SkillPackManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(manifest, parsed);
}

#[test]
fn higher_cairn_version_passes() {
    let manifest = valid_manifest();
    assert!(manifest.validate("1.0.0").is_ok());
}
