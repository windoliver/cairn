//! Integration smoke tests for the coherence release gate.

use std::path::{Path, PathBuf};

use cairn_bench::coherence::{
    ALL_CATEGORIES, GateMode, GateOptions, MetricCategory, aggregate, classify, run_coherence_gate,
};
use cairn_test_fixtures::replay::{load_scenario_file, run_scenario};
use jsonschema::Validator;
use tempfile::tempdir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn manifest_path() -> PathBuf {
    workspace_root().join("crates/cairn-bench/manifests/coherence.toml")
}

fn baseline_path() -> PathBuf {
    workspace_root().join("crates/cairn-bench/baselines/coherence.json")
}

fn schema(path: &str) -> Validator {
    let raw = std::fs::read_to_string(workspace_root().join(path)).expect("read schema");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse schema");
    Validator::new(&value).expect("compile schema")
}

#[tokio::test]
async fn extended_cassettes_pass_beta_gate() {
    let dir = tempdir().unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec![
            "research_domain".to_owned(),
            "engineering_domain".to_owned(),
            "support_domain".to_owned(),
        ],
        manifest_path: manifest_path(),
        baseline_path: Some(baseline_path()),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        write_trend: true,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let outcome = run_coherence_gate(opts).await.expect("gate run");
    assert!(
        outcome.gate_passed,
        "beta gate must pass against extended cassettes: {}",
        outcome.human
    );
}

#[tokio::test]
async fn untagged_actions_excluded_from_scoring() {
    let scenario = load_scenario_file(
        &workspace_root().join("fixtures/v0/replay/p0_stories.json"),
    )
    .expect("load p0_stories");
    let report = run_scenario(&scenario).await.expect("run p0_stories");
    let scores = aggregate(&scenario.actions, &report.checks).expect("aggregate");
    for category in ALL_CATEGORIES {
        let s = scores[&category];
        assert_eq!(
            s.total, 0,
            "category {category:?} should be empty for untagged cassette",
        );
    }
}

#[tokio::test]
async fn extended_cassettes_cover_all_five_categories() {
    let mut covered = std::collections::BTreeSet::<MetricCategory>::new();
    for cassette in ["research_domain", "engineering_domain", "support_domain"] {
        let scenario = load_scenario_file(
            &workspace_root().join(format!("fixtures/v0/replay/{cassette}.json")),
        )
        .expect("load cassette");
        for action in &scenario.actions {
            if let Some(c) = classify(action) {
                covered.insert(c);
            }
        }
    }
    for category in ALL_CATEGORIES {
        assert!(
            covered.contains(&category),
            "extended cassettes must cover {category:?}; got {covered:?}",
        );
    }
}

#[test]
fn live_manifest_validates_against_schema() {
    let validator = schema("crates/cairn-bench/schemas/coherence-threshold.schema.json");
    let raw_toml = std::fs::read_to_string(manifest_path()).expect("read manifest");
    let parsed: toml::Value = toml::from_str(&raw_toml).expect("parse toml");
    let as_json = serde_json::to_value(&parsed).expect("toml->json");
    validator
        .validate(&as_json)
        .expect("manifest schema validation");
}

#[test]
fn live_baseline_validates_against_schema() {
    let validator = schema("crates/cairn-bench/schemas/coherence-baseline.schema.json");
    let raw = std::fs::read_to_string(baseline_path()).expect("read baseline");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse baseline json");
    validator
        .validate(&value)
        .expect("baseline schema validation");
}

#[tokio::test]
async fn trend_line_validates_against_schema() {
    let dir = tempdir().unwrap();
    let trend = dir.path().join("trend.jsonl");
    let opts = GateOptions {
        mode: GateMode::None,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: None,
        trend_path: trend.clone(),
        update_baseline: false,
        write_trend: true,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let _ = run_coherence_gate(opts).await.expect("gate run");
    let body = std::fs::read_to_string(&trend).expect("read trend");
    let line = body.lines().next().expect("at least one trend line");
    let value: serde_json::Value = serde_json::from_str(line).expect("parse trend line");
    let validator = schema("crates/cairn-bench/schemas/coherence-trend.schema.json");
    validator
        .validate(&value)
        .expect("trend line schema validation");
}

#[tokio::test]
async fn gate_outcome_69_on_failing_gate() {
    let dir = tempdir().unwrap();
    let fake_manifest = dir.path().join("coherence.toml");
    std::fs::write(
        &fake_manifest,
        "schema_version = 1\n\
[recall_precision]\n\
beta_min = 1.001\n\
rc_min = 1.001\n\
max_drop_pct = 0.0\n\
[stale_avoidance]\n\
beta_min = 0.0\n\
rc_min = 0.0\n\
max_drop_pct = 100.0\n\
[summary_quality]\n\
beta_min = 0.0\n\
rc_min = 0.0\n\
max_drop_pct = 100.0\n\
[search_usefulness]\n\
beta_min = 0.0\n\
rc_min = 0.0\n\
max_drop_pct = 100.0\n\
[forget_completeness]\n\
beta_min = 0.0\n\
rc_min = 0.0\n\
max_drop_pct = 100.0\n",
    )
    .unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: fake_manifest,
        baseline_path: None,
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let outcome = run_coherence_gate(opts).await.expect("gate run");
    assert!(!outcome.gate_passed, "gate should have failed");
    assert!(outcome.report.failures.contains(&"recall_precision"));
}
