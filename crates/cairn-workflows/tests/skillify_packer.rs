#![allow(missing_docs)]

use cairn_core::pipeline::skillify::SkillifyGateStatus;
use cairn_workflows::skillify::materialize::{AuthoredSkillBundle, materialize_bundle};
use cairn_workflows::skillify::packer::{SkillPackBuilder, unpack_archive};
use serde_json::json;
use tempfile::TempDir;

fn authored(slug: &str) -> AuthoredSkillBundle {
    AuthoredSkillBundle {
        lane: format!("test.{slug}"),
        slug: slug.to_owned(),
        skill_markdown: format!(
            "---\nname: {slug}\nlane: test.{slug}\ntriggers:\n  - {slug}\nuses: scripts/{slug}.sh\nfiles_to: wiki/test/\n---\nSkill."
        ),
        script: format!("#!/usr/bin/env bash\necho {slug}\n"),
        unit_tests: json!({"cases": []}),
        integration_tests: json!({"cases": []}),
        llm_evals: json!({"rubric": []}),
        resolver_triggers: json!([slug]),
        resolver_eval: json!({"intents": []}),
        smoke: json!({"cases": []}),
        filing_rules: json!({"files_to": "wiki/test/"}),
    }
}

fn setup_candidate(temp: &TempDir, candidate_id: &str, slug: &str) {
    let a = authored(slug);
    materialize_bundle(
        temp.path(),
        candidate_id,
        &a,
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .unwrap();

    // Write a passing gate report so the packer accepts it.
    let root = temp
        .path()
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    let report = cairn_core::pipeline::skillify::SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: cairn_core::pipeline::skillify::SkillArtifactKind::required()
            .iter()
            .map(|kind| cairn_core::pipeline::skillify::SkillifyGate {
                name: kind.as_str().to_owned(),
                status: SkillifyGateStatus::Passed,
                message: None,
            })
            .collect(),
    };
    std::fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn pack_and_unpack_round_trip() {
    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_alpha", "alpha");
    setup_candidate(&temp, "skc_beta", "beta");

    let archive = SkillPackBuilder::new("test-pack", "0.1.0", ">=0.1.0", "Test pack")
        .add_candidate("skc_alpha")
        .add_candidate("skc_beta")
        .build(temp.path())
        .unwrap();

    assert!(archive.archive_path.exists());
    assert_eq!(archive.manifest.skills.len(), 2);
    assert!(archive.manifest.pack_id.starts_with("skp_"));

    // Unpack into a fresh vault.
    let install_temp = TempDir::new().unwrap();
    unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap();

    assert!(
        install_temp
            .path()
            .join(".cairn/evolution/skillify/skc_alpha/manifest.json")
            .exists()
    );
    assert!(
        install_temp
            .path()
            .join(".cairn/evolution/skillify/skc_beta/manifest.json")
            .exists()
    );
}

#[test]
fn pack_rejects_candidate_with_failing_gates() {
    let temp = TempDir::new().unwrap();
    let a = authored("gamma");
    materialize_bundle(
        temp.path(),
        "skc_gamma",
        &a,
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .unwrap();
    // Gate report is blocked (default from materialize_bundle) → packer should reject.

    let err = SkillPackBuilder::new("fail-pack", "0.1.0", ">=0.1.0", "Fail")
        .add_candidate("skc_gamma")
        .build(temp.path())
        .unwrap_err();

    assert!(err.to_string().contains("gate"));
}

#[test]
fn unpack_rejects_incompatible_version() {
    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_delta", "delta");

    let archive = SkillPackBuilder::new("version-pack", "0.1.0", ">=99.0.0", "Future pack")
        .add_candidate("skc_delta")
        .build(temp.path())
        .unwrap();

    let install_temp = TempDir::new().unwrap();
    let err = unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap_err();
    assert!(err.to_string().contains("Cairn"));
}
