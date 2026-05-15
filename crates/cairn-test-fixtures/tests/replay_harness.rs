//! Integration coverage for the dev-only replay harness.

use cairn_test_fixtures::replay::{
    ReplayAction, ReplayExpectation, ReplaySearchAction, ReplaySearchMode, load_named_scenario,
    run_named_scenario,
};

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
    for name in ["p0_stories", "p0_keyword_only"] {
        let scenario = load_named_scenario(name).expect("load scenario");
        assert_eq!(scenario.id, name);
        assert!(!scenario.records.is_empty());
        assert!(!scenario.actions.is_empty());
    }
}
