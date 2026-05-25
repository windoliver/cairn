//! Integration coverage for the dev-only replay harness.

use std::collections::HashSet;

use cairn_test_fixtures::replay::{
    ReplayAction, ReplayExpectation, ReplaySearchAction, ReplaySearchMode, load_named_scenario,
    run_named_scenario,
};

const EXTENDED_DOMAIN_SUITES: [&str; 3] =
    ["research_domain", "engineering_domain", "support_domain"];

#[tokio::test(flavor = "multi_thread")]
async fn p0_stories_replay_passes_end_to_end() {
    let report = run_named_scenario("p0_stories")
        .await
        .expect("run scenario");
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.scenario_id, "p0_stories");
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.story == "US7" && check.verb == "search")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keyword_only_replay_reports_capability_rejections() {
    let report = run_named_scenario("p0_keyword_only")
        .await
        .expect("run scenario");
    assert!(report.passed(), "{report:#?}");
    let capabilities: Vec<_> = report
        .checks
        .iter()
        .filter(|check| check.actual["status"] == "capability_unavailable")
        .map(|check| {
            check.actual["capability"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(
        capabilities,
        vec![
            "cairn.mcp.v1.search.semantic".to_owned(),
            "cairn.mcp.v1.search.hybrid".to_owned(),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failure_report_identifies_scenario_verb_query_expected_and_actual() {
    let mut scenario = load_named_scenario("p0_stories").expect("load scenario");
    let search = scenario
        .actions
        .iter_mut()
        .find_map(|action| action.as_search_mut())
        .expect("search action");
    search.expected = ReplayExpectation::Hits {
        record_ids: vec!["01HQZX9F5N00000000000000A6".to_owned()],
    };

    let report = cairn_test_fixtures::replay::run_scenario(&scenario)
        .await
        .expect("run scenario");
    assert!(!report.passed(), "{report:#?}");
    let failure = report.failures().next().expect("one failure");
    assert_eq!(failure.scenario_id, "p0_stories");
    assert_eq!(failure.verb, "search");
    assert_eq!(failure.query.as_deref(), Some("ownership borrowing"));
    assert_eq!(
        failure.expected,
        serde_json::json!({
            "status": "hits",
            "record_ids": ["01HQZX9F5N00000000000000A6"]
        })
    );
    assert_ne!(failure.expected, failure.actual);
}

#[tokio::test(flavor = "multi_thread")]
async fn search_replay_hides_reasoning_by_default() {
    let mut scenario = load_named_scenario("p0_stories").expect("load scenario");
    scenario
        .actions
        .push(ReplayAction::Search(ReplaySearchAction {
            story: "US7_REASONING_PRIVACY".to_owned(),
            mode: ReplaySearchMode::Keyword,
            query: "rolling summary p0 session covers".to_owned(),
            limit: 5,
            expected: ReplayExpectation::Hits { record_ids: vec![] },
            metric_category: None,
            stale_record_ids: vec![],
        }));

    let report = cairn_test_fixtures::replay::run_scenario(&scenario)
        .await
        .expect("run scenario");
    let check = report
        .checks
        .iter()
        .find(|check| check.story == "US7_REASONING_PRIVACY")
        .expect("privacy check");
    assert!(check.passed, "{check:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn forget_replay_fails_when_followup_search_errors() {
    let mut scenario = load_named_scenario("p0_stories").expect("load scenario");
    let forget = scenario
        .actions
        .iter_mut()
        .find_map(|action| match action {
            ReplayAction::ForgetRecord { followup_query, .. } => Some(followup_query),
            _ => None,
        })
        .expect("forget action");
    forget.clear();

    let report = cairn_test_fixtures::replay::run_scenario(&scenario)
        .await
        .expect("run scenario");
    let failure = report
        .failures()
        .find(|check| check.verb == "forget_record")
        .expect("forget check should fail");
    assert_eq!(failure.actual["status"], "error");
}

#[test]
fn p0_replay_manifest_uses_canonical_trace_event_names() {
    let scenario = load_named_scenario("p0_stories").expect("load scenario");
    for record in &scenario.records {
        if let Some(event) = &record.trace_event {
            serde_json::from_value::<cairn_core::domain::trace::TraceEvent>(
                serde_json::Value::String(event.clone()),
            )
            .unwrap_or_else(|e| panic!("record {} trace_event {event:?}: {e}", record.id));
        }
    }
}

#[test]
fn replay_manifests_deserialize() {
    for name in [
        "p0_stories",
        "p0_keyword_only",
        "codex_consumer",
        EXTENDED_DOMAIN_SUITES[0],
        EXTENDED_DOMAIN_SUITES[1],
        EXTENDED_DOMAIN_SUITES[2],
    ] {
        let scenario = load_named_scenario(name).expect("load scenario");
        assert_eq!(scenario.id, name);
        assert!(!scenario.records.is_empty());
        assert!(!scenario.actions.is_empty());
    }
}

#[test]
fn extended_domain_replay_suites_document_goals_and_golden_expectations() {
    let readme_path = cairn_test_fixtures::fixture_v0_dir().join("replay/README.md");
    let readme = std::fs::read_to_string(&readme_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", readme_path.display()));
    for needle in [
        "research_domain",
        "engineering_domain",
        "support_domain",
        "long-horizon memory",
        "multi-session coherence",
        "privacy/forget",
        "search relevance",
        "Golden expectations",
    ] {
        assert!(
            readme.contains(needle),
            "missing {needle:?} in {}",
            readme_path.display()
        );
    }
}

#[test]
fn extended_domain_replay_suites_have_resolvable_goldens_and_private_forget_queries() {
    for name in EXTENDED_DOMAIN_SUITES {
        let scenario = load_named_scenario(name).expect("load scenario");
        let record_ids: HashSet<_> = scenario
            .records
            .iter()
            .map(|record| record.id.as_str())
            .collect();
        assert_eq!(
            record_ids.len(),
            scenario.records.len(),
            "{name} has duplicate record ids"
        );

        for action in &scenario.actions {
            match action {
                ReplayAction::Search(search) => {
                    assert_eq!(
                        search.mode,
                        ReplaySearchMode::Keyword,
                        "{name} must stay keyword-only for no-network replay"
                    );
                    if let ReplayExpectation::Hits {
                        record_ids: expected,
                    } = &search.expected
                    {
                        for id in expected {
                            assert!(record_ids.contains(id.as_str()), "{name} missing {id}");
                        }
                    }
                }
                ReplayAction::Summarize {
                    expected_record_ids,
                    ..
                }
                | ReplayAction::AssembleHot {
                    expected_record_ids,
                    ..
                } => {
                    for id in expected_record_ids {
                        assert!(record_ids.contains(id.as_str()), "{name} missing {id}");
                    }
                }
                ReplayAction::ForgetRecord {
                    record_id,
                    followup_query,
                    ..
                } => {
                    assert!(
                        record_ids.contains(record_id.as_str()),
                        "{name} forget target {record_id} missing"
                    );
                    let matching_records = scenario
                        .records
                        .iter()
                        .filter(|record| record.body.contains(followup_query))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        matching_records.len(),
                        1,
                        "{name} forget follow-up query must uniquely identify its target"
                    );
                    assert_eq!(matching_records[0].id, *record_id);
                }
                _ => {}
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_consumer_replay_passes_end_to_end() {
    let report = run_named_scenario("codex_consumer")
        .await
        .expect("run scenario");
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.scenario_id, "codex_consumer");
    for verb in [
        "assemble_hot",
        "capture_trace",
        "forget_record",
        "lint",
        "retrieve_session",
        "retrieve_turn",
        "search",
        "summarize",
    ] {
        assert!(
            report.checks.iter().any(|check| check.verb == verb),
            "missing check for {verb}: {report:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn extended_domain_replay_suites_pass_end_to_end_without_network_or_llm() {
    for name in EXTENDED_DOMAIN_SUITES {
        let scenario = load_named_scenario(name).expect("load scenario");
        assert!(
            !scenario.config.local_embeddings,
            "{name} must use deterministic keyword-only replay"
        );

        let report = cairn_test_fixtures::replay::run_scenario(&scenario)
            .await
            .unwrap_or_else(|e| panic!("run {name}: {e}"));
        assert!(report.passed(), "{report:#?}");

        for verb in [
            "capture_trace",
            "forget_record",
            "retrieve_session",
            "retrieve_turn",
            "search",
            "summarize",
        ] {
            assert!(
                report.checks.iter().any(|check| check.verb == verb),
                "{name} missing check for {verb}: {report:#?}"
            );
        }
    }
}
