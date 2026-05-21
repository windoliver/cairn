//! SRE report DTO serialization and classifier coverage.

use cairn_core::domain::sre::{
    SreGateResult, SreGateSummary, SrePrivacySummary, SreProjectionSummary, SreRehydrationSummary,
    SreReport, SreSearchSummary, SreStatus, SreVaultSummary, SreWorkflowKindSummary,
    SreWorkflowSummary, classify_count_status, scrub_detail,
};

#[test]
fn sre_report_serializes_body_free_shape() {
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
            p95_latency_ms: Some(2_210.0),
            slo_ms: 3_000.0,
            sample_count: 12,
            last_gate: Some(SreGateResult {
                name: "cold_rehydrate_p95".into(),
                status: SreStatus::Ok,
                measured: Some(2_210.0),
                threshold: Some(3_000.0),
                unit: "ms".into(),
                detail: None,
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
