//! SRE report DTO serialization and classifier coverage.

use cairn_core::domain::metrics::MetricEvent;
use cairn_core::domain::sre::{
    SreDetail, SreGateResult, SreGateSummary, SreMeasurement, SrePrivacySummary,
    SreProjectionSummary, SreRehydrationSummary, SreReport, SreSearchSummary, SreStatus,
    SreVaultSummary, SreWorkflowKindSummary, SreWorkflowSummary, classify_count_status,
    classify_threshold, scrub_detail,
};

#[test]
fn sre_report_serializes_body_free_shape() {
    let raw_detail = "record body SECRET_PRIVATE_TOKEN from /Users/alice/vault/raw.md query text";
    let report = SreReport {
        schema_version: 1,
        captured_at_ms: 1_700_000_000_000,
        vault: SreVaultSummary {
            id_hash: "sha256:vault".into(),
            name: "Fixture Vault".into(),
        },
        workflow: SreWorkflowSummary {
            status: SreStatus::Warning,
            oldest_queued_age_ms: Some(742_000),
            longest_held_lease_ms: None,
            dead_letter_count: 1,
            kinds: vec![SreWorkflowKindSummary {
                kind: "expire.tier".into(),
                queued: 2,
                leased: 1,
                done_recent: 3,
                failed_recent: 1,
                oldest_queued_age_ms: Some(742_000),
                last_success_age_ms: Some(50_000),
                backlog_threshold_ms: 600_000,
                status: SreStatus::Warning,
            }],
        },
        rehydration: SreRehydrationSummary {
            status: SreStatus::Ok,
            latest_latency_ms: Some(2_100),
            p95_latency_ms: SreMeasurement::new(2_210.0),
            slo_ms: SreMeasurement::new(3_000.0).expect("finite SLO"),
            sample_count: 12,
            last_gate: Some(SreGateResult {
                name: "cold_rehydrate_p95".into(),
                status: SreStatus::Ok,
                measured: SreMeasurement::new(2_210.0),
                threshold: SreMeasurement::new(3_000.0),
                unit: "ms".into(),
                detail: Some(SreDetail::from_raw(raw_detail)),
            }),
        },
        projection: SreProjectionSummary {
            status: SreStatus::Unknown,
            nexus_state: "disabled".into(),
            nexus_reason: None,
            targets: Vec::new(),
        },
        search: SreSearchSummary {
            status: SreStatus::Ok,
            modes: Vec::new(),
        },
        gates: SreGateSummary {
            status: SreStatus::Ok,
            gates: Vec::new(),
        },
        privacy: SrePrivacySummary {
            scrubbed: true,
            forbidden_field_count: 0,
        },
    };

    let json = serde_json::to_string(&report).expect("serialize SRE report");
    assert!(json.contains("\"schema_version\":1"));
    assert!(json.contains("\"status\":\"warning\""));
    assert!(json.contains("\"detail\":\"redacted\""));
    assert!(!json.contains("SECRET_PRIVATE_TOKEN"));
    assert!(!json.contains("/Users/alice"));
    assert!(!json.contains("private body"));
    assert!(!json.contains("query text"));
}

#[test]
fn status_classification_warns_when_count_is_positive() {
    assert_eq!(classify_count_status(0), SreStatus::Ok);
    assert_eq!(classify_count_status(1), SreStatus::Warning);
}

#[test]
fn scrub_detail_maps_raw_text_to_stable_class() {
    let raw = "record body SECRET_PRIVATE_TOKEN from /Users/alice/vault/raw.md";
    assert_eq!(scrub_detail(raw), "redacted");
}

#[test]
fn stable_detail_rejects_privacy_risk_text() {
    assert_eq!(
        SreDetail::stable("latency.ok-1")
            .expect("stable detail class")
            .as_str(),
        "latency.ok-1"
    );
    assert!(SreDetail::stable("").is_none());
    assert!(SreDetail::stable("private_body").is_none());
    assert!(SreDetail::stable("query.text").is_none());
    assert!(SreDetail::stable("/Users/alice").is_none());
}

#[test]
fn sre_detail_deserialization_scrubs_raw_text() {
    let raw = r#"{
        "name":"privacy_gate",
        "status":"fail",
        "measured":1.0,
        "threshold":0.0,
        "unit":"count",
        "detail":"SECRET_PRIVATE_TOKEN from /Users/alice/private body query text"
    }"#;

    let gate: SreGateResult = serde_json::from_str(raw).expect("deserialize gate");
    assert_eq!(
        gate.detail.as_ref().map(SreDetail::as_str),
        Some("redacted")
    );

    let json = serde_json::to_string(&gate).expect("serialize gate");
    assert!(json.contains("\"detail\":\"redacted\""));
    assert!(!json.contains("SECRET_PRIVATE_TOKEN"));
    assert!(!json.contains("/Users/alice"));
    assert!(!json.contains("private body"));
    assert!(!json.contains("query text"));
}

#[test]
fn measurements_reject_non_finite_values() {
    assert!(SreMeasurement::new(1.0).is_some());
    assert!(SreMeasurement::new(f64::NAN).is_none());
    assert!(SreMeasurement::new(f64::INFINITY).is_none());
    assert!(SreMeasurement::new(f64::NEG_INFINITY).is_none());
}

#[test]
fn threshold_classification_rejects_non_finite_inputs() {
    assert_eq!(classify_threshold(Some(1.0), 2.0), SreStatus::Ok);
    assert_eq!(classify_threshold(Some(2.0), 2.0), SreStatus::Ok);
    assert_eq!(classify_threshold(Some(3.0), 2.0), SreStatus::Fail);
    assert_eq!(classify_threshold(None, 2.0), SreStatus::Unknown);
    assert_eq!(classify_threshold(Some(f64::NAN), 2.0), SreStatus::Unknown);
    assert_eq!(classify_threshold(Some(1.0), f64::NAN), SreStatus::Unknown);
}

#[test]
fn rehydration_completed_metric_is_body_free() {
    let event = MetricEvent::RehydrationCompleted {
        ts_ms: 1_700_000_000_000,
        target: "session".into(),
        source_tier: "cold".into(),
        restored_tier: "warm".into(),
        status: "committed".into(),
        latency_ms: 2_900,
        bytes_restored: 9_500_000,
        record_count: 240,
        error: None,
    };

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.contains("\"event\":\"rehydration_completed\""));
    assert!(json.contains("\"target\":\"session\""));
    assert!(!json.contains("session_id"));
    assert!(!json.contains("body"));
}
