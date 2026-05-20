//! Latency gate comparator smoke test — verifies SLO + regression math
//! against a synthetic baseline. Does not invoke criterion.

use std::collections::BTreeMap;

use cairn_bench::gates::baseline::Baseline;
use cairn_bench::latency::harness::compare;

#[test]
fn comparator_emits_failure_when_measured_exceeds_baseline_plus_2pct() {
    let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 7.0)].into();
    let baseline = Baseline {
        schema_version: 1,
        runner: "test".into(),
        captured_at: "now".into(),
        commit: "x".into(),
        metrics: [("assemble_hot_p95".to_string(), 4.0)]
            .into_iter()
            .collect(),
        regression_pct: BTreeMap::new(),
    };
    let r = compare(&measured, &baseline);
    assert!(!r.ok);
    assert!(
        r.failures
            .iter()
            .any(|f| f.contains("baseline regression threshold"))
    );
}

#[test]
fn comparator_emits_failure_when_measured_exceeds_slo() {
    // SLO is 300 ms (subprocess proxy). 350 ms exceeds.
    let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 350.0)].into();
    let baseline = Baseline {
        schema_version: 1,
        runner: "test".into(),
        captured_at: "now".into(),
        commit: "x".into(),
        metrics: [("assemble_hot_p95".to_string(), 320.0)]
            .into_iter()
            .collect(),
        regression_pct: BTreeMap::new(),
    };
    let r = compare(&measured, &baseline);
    assert!(!r.ok);
    assert!(r.failures.iter().any(|f| f.contains("SLO")));
}

#[test]
fn comparator_passes_when_under_both_thresholds() {
    let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 100.0)].into();
    let baseline = Baseline {
        schema_version: 1,
        runner: "test".into(),
        captured_at: "now".into(),
        commit: "x".into(),
        metrics: [("assemble_hot_p95".to_string(), 100.0)]
            .into_iter()
            .collect(),
        regression_pct: BTreeMap::new(),
    };
    let r = compare(&measured, &baseline);
    assert!(r.ok, "expected pass; failures: {:?}", r.failures);
}
