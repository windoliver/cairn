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
        allow_coverage_shrink: false,
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
    let scenario = load_scenario_file(&workspace_root().join("fixtures/v0/replay/p0_stories.json"))
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
        allow_coverage_shrink: false,
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

/// Manifest whose `summary_quality` floor is just above the failing
/// synthetic cassette's score (0.0). All other categories accept the
/// vacuous-pass score of 1.0. Valid per the round-2 manifest validator.
fn failing_manifest_toml() -> &'static str {
    "schema_version = 1\n\
[recall_precision]\nbeta_min = 0.0\nrc_min = 0.0\nmax_drop_pct = 100.0\n\
[stale_avoidance]\nbeta_min = 0.0\nrc_min = 0.0\nmax_drop_pct = 100.0\n\
[summary_quality]\nbeta_min = 0.5\nrc_min = 0.5\nmax_drop_pct = 100.0\n\
[search_usefulness]\nbeta_min = 0.0\nrc_min = 0.0\nmax_drop_pct = 100.0\n\
[forget_completeness]\nbeta_min = 0.0\nrc_min = 0.0\nmax_drop_pct = 100.0\n"
}

/// Synthetic cassette that tags every canonical coherence category
/// (so the round-4 `IncompleteCoverage` guard passes) but the summary
/// action intentionally asserts a `record_id` that doesn't match the
/// seeded summary record. Result: `summary_quality` scores 0/1 = 0.0
/// while every other category passes — the gate fails on
/// `summary_quality` alone.
#[allow(clippy::too_many_lines)]
fn failing_cassette_json() -> &'static str {
    r#"{
  "id": "force_fail",
  "description": "synthetic cassette covering all 5 categories; summary fails",
  "config": { "local_embeddings": false },
  "records": [
    {
      "id": "01HQZX9F5N0000000000FFA001",
      "kind": "trace",
      "class": "episodic",
      "visibility": "private",
      "body": "force fail session one user turn",
      "session_id": "force-fail",
      "turn_id": "1",
      "sequence": 1,
      "trace_event": "user_message"
    },
    {
      "id": "01HQZX9F5N0000000000FFA002",
      "kind": "trace",
      "class": "episodic",
      "visibility": "private",
      "body": "force fail session one agent reply",
      "session_id": "force-fail",
      "turn_id": "1",
      "sequence": 2,
      "trace_event": "agent_message"
    },
    {
      "id": "01HQZX9F5N0000000000FFA003",
      "kind": "reasoning",
      "class": "semantic",
      "visibility": "private",
      "body": "force fail summary preserves discriminative tag",
      "session_id": "force-fail",
      "turn_id": "summary",
      "sequence": 3,
      "trace_event": "turn_summary"
    },
    {
      "id": "01HQZX9F5N0000000000FFA004",
      "kind": "fact",
      "class": "semantic",
      "visibility": "private",
      "body": "force fail discriminative target marker fact"
    },
    {
      "id": "01HQZX9F5N0000000000FFA005",
      "kind": "fact",
      "class": "semantic",
      "visibility": "private",
      "body": "force fail unrelated note distractor"
    },
    {
      "id": "01HQZX9F5N0000000000FFA006",
      "kind": "fact",
      "class": "semantic",
      "visibility": "private",
      "body": "force fail sensitive secret token"
    }
  ],
  "actions": [
    {
      "verb": "retrieve_session",
      "story": "FORCE_FAIL_RECALL",
      "session_id": "force-fail",
      "expected_turn_ids": ["1"],
      "expected_trace_events": ["user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "summarize",
      "story": "FORCE_FAIL_SUMMARY",
      "session_id": "force-fail",
      "expected_record_ids": ["01HQZX9F5N0000000000000XYZ"],
      "metric_category": "summary_quality"
    },
    {
      "verb": "search",
      "story": "FORCE_FAIL_SEARCH",
      "mode": "keyword",
      "query": "discriminative target marker",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N0000000000FFA004"]
      },
      "metric_category": "search_usefulness"
    },
    {
      "verb": "search",
      "story": "FORCE_FAIL_STALE",
      "mode": "keyword",
      "query": "discriminative target marker",
      "limit": 5,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N0000000000FFA004"]
      },
      "stale_record_ids": ["01HQZX9F5N0000000000FFA005"]
    },
    {
      "verb": "forget_record",
      "story": "FORCE_FAIL_FORGET",
      "record_id": "01HQZX9F5N0000000000FFA006",
      "followup_query": "sensitive secret token",
      "expected_absent_from_search": true,
      "metric_category": "forget_completeness"
    }
  ]
}"#
}

#[tokio::test]
async fn gate_outcome_69_on_failing_gate() {
    let dir = tempdir().unwrap();
    let fake_manifest = dir.path().join("coherence.toml");
    std::fs::write(&fake_manifest, failing_manifest_toml()).unwrap();
    let cassettes_dir = dir.path().to_path_buf();
    std::fs::write(
        cassettes_dir.join("force_fail.json"),
        failing_cassette_json(),
    )
    .unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir,
        include: vec!["force_fail".to_owned()],
        manifest_path: fake_manifest,
        baseline_path: None,
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let outcome = run_coherence_gate(opts).await.expect("gate run");
    assert!(!outcome.gate_passed, "gate should have failed");
    assert!(outcome.report.failures.contains(&"summary_quality"));
}

#[tokio::test]
async fn update_baseline_rejects_coverage_shrink_without_override() {
    // `--gate none --update-baseline --include research_domain`
    // (instead of all three cassettes) would otherwise overwrite the
    // committed 3-cassette baseline with a smaller-denominator one,
    // and future runs would be allowed to shed actions down to that
    // lower floor. Without --allow-coverage-shrink this must fail.
    use cairn_bench::coherence::GateError;
    let dir = tempdir().unwrap();
    let target_baseline = dir.path().join("baseline.json");
    std::fs::copy(baseline_path(), &target_baseline).unwrap();
    let opts = GateOptions {
        mode: GateMode::None,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: Some(target_baseline.clone()),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: true,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let err = run_coherence_gate(opts).await.expect_err("must reject");
    assert!(
        matches!(err, GateError::BaselineInvalid { ref reason, .. } if reason.contains("shrink")),
        "expected BaselineInvalid(shrink), got {err:?}"
    );
    assert_eq!(err.exit_code(), 78);
}

#[tokio::test]
async fn enforced_gate_rejects_coverage_regression() {
    // Even when every category has *some* coverage, dropping the
    // denominator below the baseline must fail closed. We simulate
    // this by including only research_domain (1/3 of the trusted
    // cassette corpus). Baseline records totals from all 3 cassettes,
    // so research-only run shows a coverage shrink and trips
    // CoverageRegression.
    use cairn_bench::coherence::GateError;
    let dir = tempdir().unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: Some(baseline_path()),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let err = run_coherence_gate(opts).await.expect_err("must reject");
    assert!(
        matches!(err, GateError::CoverageRegression { .. }),
        "expected CoverageRegression, got {err:?}"
    );
    assert_eq!(err.exit_code(), 78);
}

#[tokio::test]
async fn enforced_gate_rejects_zero_coverage_cassettes() {
    // The new IncompleteCoverage guard makes beta/rc fail closed when
    // any canonical category has 0 actions. p0_stories carries no
    // metric_category tags, so every category buckets at 0/0 — the
    // gate must refuse to run instead of passing vacuously.
    use cairn_bench::coherence::GateError;
    let dir = tempdir().unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["p0_stories".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: None,
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let err = run_coherence_gate(opts).await.expect_err("must reject");
    assert!(
        matches!(err, GateError::IncompleteCoverage { mode: "beta", .. }),
        "expected IncompleteCoverage(beta), got {err:?}"
    );
    assert_eq!(err.exit_code(), 78);
}

#[tokio::test]
async fn missing_baseline_path_fails_closed() {
    // Configuring a baseline path that doesn't exist must NOT silently
    // disable the regression delta check — it must surface as EX_CONFIG.
    use cairn_bench::coherence::GateError;
    let dir = tempdir().unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: Some(dir.path().join("does-not-exist.json")),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let err = run_coherence_gate(opts).await.expect_err("must fail");
    assert!(
        matches!(err, GateError::BaselineIo { .. }),
        "expected BaselineIo, got {err:?}"
    );
    assert_eq!(err.exit_code(), 78);
}

#[tokio::test]
async fn update_baseline_rejects_zero_coverage_run() {
    // --update-baseline + a run that doesn't actually exercise every
    // category would otherwise lock in 0/0 vacuous-pass scores and
    // destroy the prior real measurement.
    use cairn_bench::coherence::GateError;
    let dir = tempdir().unwrap();
    // Use the P0 cassette which has NO metric_category tags, so every
    // category bucket comes back empty (total=0). With --gate none the
    // gate "passes" (GateNone), so previously the orchestrator would
    // proceed to write a vacuous baseline.
    let target_baseline = dir.path().join("baseline.json");
    std::fs::copy(baseline_path(), &target_baseline).unwrap();
    let before = std::fs::read_to_string(&target_baseline).unwrap();
    let opts = GateOptions {
        mode: GateMode::None,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["p0_stories".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: Some(target_baseline.clone()),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: true,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let err = run_coherence_gate(opts).await.expect_err("must reject");
    assert!(
        matches!(err, GateError::BaselineInvalid { .. }),
        "expected BaselineInvalid, got {err:?}"
    );
    assert_eq!(err.exit_code(), 78);
    // Baseline file untouched.
    let after = std::fs::read_to_string(&target_baseline).unwrap();
    assert_eq!(before, after);
}

/// Synthetic baseline whose totals match `failing_cassette_json` (1
/// action per category). Lets failure-path tests run against a custom
/// baseline without tripping the round-7 `CoverageRegression` guard,
/// which would otherwise fire because the live baseline records
/// totals from all three 3-domain cassettes.
fn force_fail_baseline_json() -> &'static str {
    r#"{
  "schema_version": 1,
  "captured_at": "2026-05-24T00:00:00Z",
  "cairn_version": "0.0.0",
  "git_sha": "force-fail-test",
  "metrics": {
    "recall_precision":    { "score": 1.0, "passed": 1, "total": 1 },
    "stale_avoidance":     { "score": 1.0, "passed": 1, "total": 1 },
    "summary_quality":     { "score": 1.0, "passed": 1, "total": 1 },
    "search_usefulness":   { "score": 1.0, "passed": 1, "total": 1 },
    "forget_completeness": { "score": 1.0, "passed": 1, "total": 1 }
  }
}"#
}

#[tokio::test]
async fn failed_gate_does_not_update_baseline() {
    // A regression that trips --gate beta must not be normalised into the
    // committed baseline. `--update-baseline` is honoured only when the
    // gate actually passes (or when --gate none is in use).
    let dir = tempdir().unwrap();
    let fake_manifest = dir.path().join("coherence.toml");
    std::fs::write(&fake_manifest, failing_manifest_toml()).unwrap();
    let cassettes_dir = dir.path().to_path_buf();
    std::fs::write(
        cassettes_dir.join("force_fail.json"),
        failing_cassette_json(),
    )
    .unwrap();
    let target_baseline = dir.path().join("baseline.json");
    // Use a synthetic baseline whose totals match force_fail's coverage
    // (1 action per category). The live committed baseline records the
    // 3-cassette extended totals, which would trip the CoverageRegression
    // guard before the failure path runs.
    std::fs::write(&target_baseline, force_fail_baseline_json()).unwrap();
    let before = std::fs::read_to_string(&target_baseline).unwrap();

    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir,
        include: vec!["force_fail".to_owned()],
        manifest_path: fake_manifest,
        baseline_path: Some(target_baseline.clone()),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: true,
        allow_coverage_shrink: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let outcome = run_coherence_gate(opts).await.expect("gate run");
    assert!(!outcome.gate_passed);
    let after = std::fs::read_to_string(&target_baseline).unwrap();
    assert_eq!(before, after, "baseline must not be rewritten on gate fail");
}
