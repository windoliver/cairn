//! Unit tests for the `AutoUserProfile` synthesizer (issue #81).
//!
//! Coverage map (against issue acceptance criteria + verification list):
//! - **Profile materialization fixture** — `materializes_static_and_dynamic_split`.
//! - **Evidence-link** — `evidence_lists_source_record_ids`.
//! - **Forget propagation** — `forget_drops_line_when_evidence_removed`.
//! - **Low-confidence exclusion** — `excludes_uncertain_band`.
//! - **Privacy-blocked exclusion** — adapter responsibility (asserted in
//!   `caller_filters_tombstoned_before_synthesize` doc-test); the
//!   synthesizer's contract is "trust the projection".
//! - **Dynamic facts can expire without deleting static prefs** —
//!   `dynamic_records_disappearing_does_not_touch_static_half`.
//! - **Subject validation** — `errors_when_subject_empty`,
//!   `errors_when_subject_field_blank`.

use super::*;
use crate::domain::{RecordId, Rfc3339Timestamp};
use crate::generated::common::Ulid;

fn ts(s: &str) -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse(s).expect("valid rfc3339")
}

fn rid(c: char) -> RecordId {
    // Construct a 26-char ULID by repeating the first character. ULID
    // first char must be in `[0..=7]`, rest are Crockford base32. We
    // enforce that by constraining the call sites.
    assert!(matches!(c, '0'..='7'), "ULID first char must be [0..=7]");
    let s: String = std::iter::repeat_n(c, 26).collect();
    RecordId::parse(s).expect("valid ulid")
}

fn user_subject() -> ProfileSubject {
    ProfileSubject {
        user: Some("hmn:alice".to_owned()),
        agent: None,
    }
}

fn rec(
    first: char,
    is_static: bool,
    confidence: f32,
    facet: KeyFactFacet,
    value: &str,
    when: &str,
) -> ProfileSourceRecord {
    ProfileSourceRecord {
        record_id: rid(first),
        is_static,
        confidence,
        facet,
        value: value.to_owned(),
        updated_at: ts(when),
    }
}

#[test]
fn materializes_static_and_dynamic_split() {
    let records = vec![
        rec(
            '0',
            true,
            0.9,
            KeyFactFacet::Preferences,
            "prefers terse explanations",
            "2026-04-22T14:00:00Z",
        ),
        rec(
            '1',
            true,
            0.85,
            KeyFactFacet::Devices,
            "M2 MacBook Pro",
            "2026-04-22T14:05:00Z",
        ),
        rec(
            '2',
            false,
            0.7,
            KeyFactFacet::CurrentIssues,
            "shipping retrieve --profile dispatch",
            "2026-04-22T14:10:00Z",
        ),
    ];
    let now = ts("2026-04-22T14:11:00Z");
    let profile = synthesize(&records, &user_subject(), &now).expect("synthesize");

    assert_eq!(profile.subject.user.as_deref(), Some("hmn:alice"));
    assert_eq!(profile.r#static.key_facts.preferences.len(), 1);
    assert_eq!(profile.r#static.key_facts.devices.len(), 1);
    assert!(profile.r#static.key_facts.current_issues.is_empty());
    assert_eq!(profile.dynamic.key_facts.current_issues.len(), 1);
    assert!(profile.dynamic.key_facts.preferences.is_empty());

    // updated_at is the latest of the contributing records, not `now`.
    assert_eq!(profile.updated_at, "2026-04-22T14:10:00Z");

    // P0 narrative bodies are stub-empty per brief §7.1.
    assert_eq!(profile.r#static.summary, "");
    assert_eq!(profile.r#static.historical_summary, "");
    assert_eq!(profile.dynamic.summary, "");
    assert_eq!(profile.dynamic.historical_summary, "");
}

#[test]
fn evidence_lists_source_record_ids() {
    let records = vec![rec(
        '3',
        true,
        0.95,
        KeyFactFacet::Preferences,
        "prefers terse explanations",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");

    let line = &profile.r#static.key_facts.preferences[0];
    assert_eq!(line.value, "prefers terse explanations");
    assert!((line.confidence - 0.95).abs() < 1e-6);
    assert_eq!(line.evidence, vec![Ulid(rid('3').as_str().to_owned())]);
}

#[test]
fn merges_duplicate_value_with_max_confidence_and_union_evidence() {
    let records = vec![
        rec(
            '0',
            true,
            0.6,
            KeyFactFacet::Preferences,
            "prefers terse explanations",
            "2026-04-22T14:00:00Z",
        ),
        rec(
            '1',
            true,
            0.85,
            KeyFactFacet::Preferences,
            "prefers terse explanations",
            "2026-04-22T14:01:00Z",
        ),
        // Same value but a different facet is *not* merged.
        rec(
            '2',
            true,
            0.9,
            KeyFactFacet::Software,
            "prefers terse explanations",
            "2026-04-22T14:02:00Z",
        ),
    ];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:02:01Z")).expect("synthesize");

    let prefs = &profile.r#static.key_facts.preferences;
    assert_eq!(prefs.len(), 1);
    assert!((prefs[0].confidence - 0.85).abs() < 1e-6);
    assert_eq!(prefs[0].evidence.len(), 2);
    assert_eq!(profile.r#static.key_facts.software.len(), 1);
}

#[test]
fn excludes_uncertain_band() {
    let records = vec![
        rec(
            '0',
            true,
            0.29,
            KeyFactFacet::Preferences,
            "low confidence trait",
            "2026-04-22T14:00:00Z",
        ),
        rec(
            '1',
            true,
            0.3,
            KeyFactFacet::Preferences,
            "boundary trait",
            "2026-04-22T14:00:00Z",
        ),
    ];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");

    let prefs = &profile.r#static.key_facts.preferences;
    // 0.29 was filtered (< 0.3); 0.3 stays (>= 0.3 admits the line).
    assert_eq!(prefs.len(), 1);
    assert_eq!(prefs[0].value, "boundary trait");
}

#[test]
fn excludes_nan_confidence() {
    let records = vec![rec(
        '0',
        true,
        f32::NAN,
        KeyFactFacet::Preferences,
        "broken record",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    assert!(profile.r#static.key_facts.preferences.is_empty());
}

#[test]
fn excludes_blank_value() {
    let records = vec![rec(
        '0',
        true,
        0.95,
        KeyFactFacet::Preferences,
        "",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    assert!(profile.r#static.key_facts.preferences.is_empty());
}

#[test]
fn forget_drops_line_when_evidence_removed() {
    // Brief §7.1: "Profile lines can be removed by record-level forget
    // of their source evidence." Here we simulate forget by removing the
    // source record from the input slice and re-synthesizing.
    let pref = rec(
        '0',
        true,
        0.9,
        KeyFactFacet::Preferences,
        "prefers terse explanations",
        "2026-04-22T14:00:00Z",
    );
    let device = rec(
        '1',
        true,
        0.85,
        KeyFactFacet::Devices,
        "M2 MacBook Pro",
        "2026-04-22T14:05:00Z",
    );

    let before = synthesize(
        &[pref.clone(), device.clone()],
        &user_subject(),
        &ts("2026-04-22T14:06:00Z"),
    )
    .expect("synthesize");
    assert_eq!(before.r#static.key_facts.preferences.len(), 1);

    let after = synthesize(&[device], &user_subject(), &ts("2026-04-22T14:06:00Z"))
        .expect("re-synthesize after forget");
    assert!(after.r#static.key_facts.preferences.is_empty());
    assert_eq!(after.r#static.key_facts.devices.len(), 1);
}

#[test]
fn dynamic_records_disappearing_does_not_touch_static_half() {
    // Brief §7.1: "Dynamic facts can expire without deleting stable user
    // preferences." Static records survive when only dynamic ones are
    // pruned.
    let pref = rec(
        '0',
        true,
        0.9,
        KeyFactFacet::Preferences,
        "prefers terse explanations",
        "2026-04-22T14:00:00Z",
    );
    let issue = rec(
        '1',
        false,
        0.7,
        KeyFactFacet::CurrentIssues,
        "shipping retrieve --profile dispatch",
        "2026-04-22T14:05:00Z",
    );

    let with_dynamic = synthesize(
        &[pref.clone(), issue],
        &user_subject(),
        &ts("2026-04-22T14:06:00Z"),
    )
    .expect("synthesize");
    assert_eq!(with_dynamic.dynamic.key_facts.current_issues.len(), 1);
    assert_eq!(with_dynamic.r#static.key_facts.preferences.len(), 1);

    // Expirer drops the dynamic fact only.
    let without_dynamic = synthesize(&[pref], &user_subject(), &ts("2026-04-22T14:10:00Z"))
        .expect("synthesize after expiration");
    assert!(without_dynamic.dynamic.key_facts.current_issues.is_empty());
    assert_eq!(without_dynamic.r#static.key_facts.preferences.len(), 1);
}

#[test]
fn errors_when_subject_empty() {
    let subject = ProfileSubject {
        user: None,
        agent: None,
    };
    let err = synthesize(&[], &subject, &ts("2026-04-22T14:00:00Z")).unwrap_err();
    assert_eq!(err, SynthesizeError::EmptySubject);
}

#[test]
fn errors_when_subject_field_blank() {
    let subject = ProfileSubject {
        user: Some(String::new()),
        agent: None,
    };
    let err = synthesize(&[], &subject, &ts("2026-04-22T14:00:00Z")).unwrap_err();
    assert_eq!(err, SynthesizeError::BlankSubjectField { field: "user" });
}

#[test]
fn empty_input_returns_empty_profile_with_now_timestamp() {
    let now = ts("2026-04-22T14:00:00Z");
    let profile = synthesize(&[], &user_subject(), &now).expect("synthesize");
    assert_eq!(profile.updated_at, "2026-04-22T14:00:00Z");
    assert!(profile.r#static.key_facts.preferences.is_empty());
    assert!(profile.dynamic.key_facts.current_issues.is_empty());
}

#[test]
fn output_round_trips_through_json() {
    // The synthesizer's output must survive a wire round-trip through the
    // generated `DataProfile` Deserialize — that's the trust-boundary
    // validator the IDL emits. If the synthesizer ever produced a value
    // the validator would reject (e.g., empty `evidence`), this test
    // catches it before the bug reaches the response envelope.
    let records = vec![rec(
        '0',
        true,
        0.9,
        KeyFactFacet::Preferences,
        "prefers terse explanations",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    let json = serde_json::to_string(&profile).expect("serialize");
    let back: crate::generated::verbs::retrieve::DataProfile =
        serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, profile);
}

#[test]
fn deterministic_ordering_within_facet() {
    let records = vec![
        rec(
            '2',
            true,
            0.9,
            KeyFactFacet::KnownEntities,
            "zebra org",
            "2026-04-22T14:00:00Z",
        ),
        rec(
            '0',
            true,
            0.9,
            KeyFactFacet::KnownEntities,
            "alpha org",
            "2026-04-22T14:00:00Z",
        ),
        rec(
            '1',
            true,
            0.9,
            KeyFactFacet::KnownEntities,
            "mid org",
            "2026-04-22T14:00:00Z",
        ),
    ];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    let entities = &profile.r#static.key_facts.known_entities;
    assert_eq!(
        entities
            .iter()
            .map(|l| l.value.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha org", "mid org", "zebra org"]
    );
}

// ── Confidence-range boundary checks ─────────────────────────────────
//
// `MemoryRecord.validate` clamps confidence to `[0.0, 1.0]` upstream;
// the synthesizer re-checks at the trust boundary because the
// generated `ProfileLine` deserializer emits the same `[0.0, 1.0]`
// validator. A confidence of `1.5` slipping through here would produce
// a `DataProfile` that deserializes-to-error in its own
// schema round-trip.

#[test]
fn admits_confidence_at_lower_bound() {
    let records = vec![rec(
        '0',
        true,
        0.0_f32,
        KeyFactFacet::Preferences,
        "zero confidence",
        "2026-04-22T14:00:00Z",
    )];
    // 0.0 is below the Uncertain floor — should be dropped.
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    assert!(profile.r#static.key_facts.preferences.is_empty());
}

#[test]
fn admits_confidence_at_upper_bound() {
    let records = vec![rec(
        '0',
        true,
        1.0_f32,
        KeyFactFacet::Preferences,
        "max confidence",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    let line = &profile.r#static.key_facts.preferences[0];
    assert!((line.confidence - 1.0_f64).abs() < f64::EPSILON);
}

#[test]
fn excludes_confidence_above_one() {
    // Out-of-range input — must not produce un-serializable output.
    let records = vec![rec(
        '0',
        true,
        1.5_f32,
        KeyFactFacet::Preferences,
        "broken extractor",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    assert!(profile.r#static.key_facts.preferences.is_empty());
}

#[test]
fn excludes_negative_confidence() {
    let records = vec![rec(
        '0',
        true,
        -0.5_f32,
        KeyFactFacet::Preferences,
        "broken extractor",
        "2026-04-22T14:00:00Z",
    )];
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    assert!(profile.r#static.key_facts.preferences.is_empty());
}

// ── IDL deserializer negative tests ──────────────────────────────────
//
// Direct exercise of the codegen-emitted `ProfileLine` TryFrom path —
// guards the validator we hand-rolled in
// `cairn-idl::codegen::emit_sdk::write_retrieve_data_extra_checks`.
// Without these the codegen could regress (drop the validator) and
// only failing wire-compat snapshots elsewhere would catch it.

#[test]
fn profile_line_deserialize_rejects_empty_value() {
    let json = r#"{"value":"","confidence":0.5,"evidence":["00000000000000000000000000"]}"#;
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::ProfileLine>(json)
        .expect_err("empty value must reject");
    assert!(err.to_string().contains("value"), "{err}");
}

#[test]
fn profile_line_deserialize_rejects_confidence_above_one() {
    let json = r#"{"value":"x","confidence":1.5,"evidence":["00000000000000000000000000"]}"#;
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::ProfileLine>(json)
        .expect_err("confidence > 1 must reject");
    assert!(err.to_string().contains("confidence"), "{err}");
}

#[test]
fn profile_line_deserialize_rejects_empty_evidence() {
    let json = r#"{"value":"x","confidence":0.5,"evidence":[]}"#;
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::ProfileLine>(json)
        .expect_err("empty evidence must reject");
    assert!(err.to_string().contains("evidence"), "{err}");
}

#[test]
fn profile_line_deserialize_rejects_duplicate_evidence() {
    let json = r#"{"value":"x","confidence":0.5,"evidence":["00000000000000000000000000","00000000000000000000000000"]}"#;
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::ProfileLine>(json)
        .expect_err("duplicate evidence must reject");
    assert!(err.to_string().contains("evidence"), "{err}");
}

#[test]
fn data_profile_subject_deserialize_rejects_neither_user_nor_agent() {
    let json = r#"{
        "subject": {},
        "static": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "dynamic": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "updated_at": "2026-04-22T14:00:00Z"
    }"#;
    serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(json)
        .expect_err("subject without user or agent must reject");
}

#[test]
fn data_profile_subject_deserialize_rejects_empty_user_string() {
    // Codex review caught this: the IDL says `minLength: 1` on
    // `subject.user`, but the only `anyOf` check would let `{"user": ""}`
    // through. The codegen-emitted DataProfileSubject TryFrom is what
    // closes that hole.
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfileSubject>(
        r#"{"user": ""}"#,
    )
    .expect_err("empty user must reject");
    assert!(err.to_string().contains("user"), "{err}");
}

#[test]
fn data_profile_subject_deserialize_rejects_empty_agent_string() {
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfileSubject>(
        r#"{"agent": ""}"#,
    )
    .expect_err("empty agent must reject");
    assert!(err.to_string().contains("agent"), "{err}");
}

#[test]
fn data_profile_deserialize_rejects_short_updated_at() {
    // RFC3339 minLength: 20. `2026-04-22Z` is too short — would be
    // silently accepted before this PR.
    let json = r#"{
        "subject": {"user": "hmn:alice"},
        "static": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "dynamic": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "updated_at": "2026-04-22Z"
    }"#;
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(json)
        .expect_err("short updated_at must reject");
    assert!(err.to_string().contains("updated_at"), "{err}");
}

#[test]
fn data_profile_deserialize_rejects_malformed_updated_at_anchors() {
    let json = r#"{
        "subject": {"user": "hmn:alice"},
        "static": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "dynamic": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "updated_at": "2026/04/22T14:00:00Z"
    }"#;
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(json)
        .expect_err("`/` anchors must reject");
    assert!(err.to_string().contains("updated_at"), "{err}");
}

#[test]
fn data_profile_deserialize_accepts_lowercase_t_and_z() {
    // RFC3339 §5.6 says `T` and `Z` are case-insensitive, and the
    // domain `Rfc3339Timestamp::parse` accepts lowercase. The wire
    // validator must match — otherwise the synthesizer's output for a
    // lowercase-input timestamp would round-trip-fail.
    let json = r#"{
        "subject": {"user": "hmn:alice"},
        "static": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "dynamic": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "updated_at": "2026-04-22t14:00:00z"
    }"#;
    let p = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(json)
        .expect("lowercase t/z must parse");
    assert_eq!(p.updated_at, "2026-04-22t14:00:00z");
}

#[test]
fn synthesize_round_trips_lowercase_t_z_timestamp() {
    // End-to-end check that a lowercase RFC3339 timestamp accepted by
    // the domain parser also survives the synthesizer → JSON →
    // generated DataProfile deserializer round-trip.
    let records = vec![ProfileSourceRecord {
        record_id: rid('0'),
        is_static: true,
        confidence: 0.9,
        facet: KeyFactFacet::Preferences,
        value: "lowercase-t".to_owned(),
        updated_at: ts("2026-04-22t14:00:00z"),
    }];
    let now = ts("2026-04-22t14:00:01z");
    let p = synthesize(&records, &user_subject(), &now).expect("synthesize");
    let json = serde_json::to_string(&p).expect("serialize");
    let back: crate::generated::verbs::retrieve::DataProfile =
        serde_json::from_str(&json).expect("deserialize round-trip with lowercase t/z");
    assert_eq!(back, p);
}

#[test]
fn data_profile_deserialize_rejects_oversized_updated_at() {
    // Pathological fractional: 100 digits. Without a maxLength cap the
    // String is allocated then dropped; with the cap (`len <= 64`) it's
    // rejected at the validator boundary.
    let big_frac: String = "1".repeat(100);
    let json = format!(
        r#"{{
        "subject": {{"user": "hmn:alice"}},
        "static": {{"summary":"","historical_summary":"","key_facts":{{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}}}},
        "dynamic": {{"summary":"","historical_summary":"","key_facts":{{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}}}},
        "updated_at": "2026-04-22T14:00:00.{big_frac}Z"
    }}"#
    );
    let err = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(&json)
        .expect_err("oversized updated_at must reject");
    assert!(err.to_string().contains("updated_at"), "{err}");
}

#[test]
fn data_profile_deserialize_rejects_out_of_range_components() {
    // Each example targets one component in `Rfc3339Timestamp::parse`'s
    // range table so any future drift is caught field-by-field.
    let cases = [
        ("2026-13-01T00:00:00Z", "month"),
        ("2026-04-32T00:00:00Z", "day"),
        ("2026-04-22T24:00:00Z", "hour"),
        ("2026-04-22T00:60:00Z", "minute"),
        ("2026-04-22T00:00:60Z", "second"),
        ("2026-04-22T00:00:00+24:00", "offset hour"),
        ("2026-04-22T00:00:00+00:60", "offset minute"),
        ("2026-04-22T14:00:00+ab:cd", "offset"),
    ];
    for (ts, label) in cases {
        let json = format!(
            r#"{{
            "subject": {{"user": "hmn:alice"}},
            "static": {{"summary":"","historical_summary":"","key_facts":{{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}}}},
            "dynamic": {{"summary":"","historical_summary":"","key_facts":{{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}}}},
            "updated_at": "{ts}"
        }}"#
        );
        let err = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(&json)
            .err()
            .unwrap_or_else(|| panic!("`{ts}` ({label}) parsed but should have rejected"));
        assert!(
            err.to_string().contains("updated_at"),
            "expected updated_at error, got: {err} (case: {label})"
        );
    }
}

#[test]
fn data_profile_updated_at_validator_agrees_with_domain_parser_on_corpus() {
    // Cross-check: the wire validator and the domain parser must
    // converge on the same accept/reject decision for a corpus of
    // edge-case timestamps. Without this, a future codegen edit can
    // silently drift the two and break the response-envelope round-trip.
    let corpus = [
        // Accepted by both.
        ("2026-04-22T14:00:00Z", true),
        ("2026-04-22T14:00:00.000Z", true),
        ("2026-04-22T14:00:00+00:00", true),
        ("2026-04-22T14:00:00-05:30", true),
        ("2026-04-22t14:00:00z", true),
        ("2026-04-22T14:00:00.123456789Z", true), // exactly 9 fractional digits (ns)
        ("2024-02-29T00:00:00Z", true),           // leap-year Feb 29
        ("2000-02-29T00:00:00Z", true),           // 2000 is leap (400-rule)
        // Rejected by both.
        ("not a date", false),
        ("2026-04-22T14:00:00", false),             // no zone
        ("2026-13-01T00:00:00Z", false),            // bad month
        ("2026-04-22T24:00:00Z", false),            // bad hour
        ("2026-04-22T14:00:00+24:00", false),       // bad offset
        ("2026-02-30T00:00:00Z", false),            // Feb 30: invalid for any year
        ("2026-04-31T00:00:00Z", false),            // Apr 31: 30-day month
        ("2026-02-29T00:00:00Z", false),            // 2026 not leap
        ("2100-02-29T00:00:00Z", false),            // 2100 not leap (century non-400)
        ("2026-04-22T14:00:00.1234567890Z", false), // 10 fractional digits > ns
    ];
    for (ts, expected) in corpus {
        let domain_ok = crate::domain::Rfc3339Timestamp::parse(ts).is_ok();
        let wire_ok = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(
            &format!(
                r#"{{
                "subject": {{"user": "hmn:alice"}},
                "static": {{"summary":"","historical_summary":"","key_facts":{{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}}}},
                "dynamic": {{"summary":"","historical_summary":"","key_facts":{{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}}}},
                "updated_at": "{ts}"
            }}"#
            ),
        )
        .is_ok();
        assert_eq!(
            domain_ok, expected,
            "domain parser disagreed with corpus expectation for `{ts}`"
        );
        assert_eq!(
            wire_ok, expected,
            "wire validator disagreed with corpus expectation for `{ts}`"
        );
        assert_eq!(domain_ok, wire_ok, "wire/domain divergence for `{ts}`");
    }
}

#[test]
fn data_profile_deserialize_accepts_well_formed_updated_at() {
    let json = r#"{
        "subject": {"user": "hmn:alice"},
        "static": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "dynamic": {"summary":"","historical_summary":"","key_facts":{"devices":[],"software":[],"preferences":[],"current_issues":[],"addressed_issues":[],"recurring_issues":[],"known_entities":[]}},
        "updated_at": "2026-04-22T14:00:00+00:00"
    }"#;
    let p = serde_json::from_str::<crate::generated::verbs::retrieve::DataProfile>(json)
        .expect("well-formed updated_at must parse");
    assert_eq!(p.updated_at, "2026-04-22T14:00:00+00:00");
}

#[test]
fn updated_at_picks_chronologically_latest_irrespective_of_input_order() {
    // Codex review: the proptest's permutation invariance was vacuous
    // for `updated_at` because every record carried the same timestamp.
    // This test pins the latest-timestamp picker explicitly.
    let early = rec(
        '0',
        true,
        0.9,
        KeyFactFacet::Preferences,
        "early",
        "2026-04-22T14:00:00Z",
    );
    let late = rec(
        '1',
        true,
        0.9,
        KeyFactFacet::Preferences,
        "late",
        "2026-04-22T15:00:00Z",
    );
    let mid = rec(
        '2',
        true,
        0.9,
        KeyFactFacet::Preferences,
        "mid",
        "2026-04-22T14:30:00Z",
    );
    let now = ts("2026-04-22T16:00:00Z");

    for permutation in [
        vec![early.clone(), late.clone(), mid.clone()],
        vec![late.clone(), early.clone(), mid.clone()],
        vec![mid.clone(), late.clone(), early.clone()],
        vec![late.clone(), mid.clone(), early.clone()],
    ] {
        let p = synthesize(&permutation, &user_subject(), &now).expect("synthesize");
        assert_eq!(
            p.updated_at,
            "2026-04-22T15:00:00Z",
            "permutation: {:?}",
            permutation
                .iter()
                .map(|r| r.value.as_str())
                .collect::<Vec<_>>()
        );
    }
}

// ── Performance sanity ───────────────────────────────────────────────
//
// Synthesizer is O(n) over input + O(b log b) per facet bucket where
// `b` is the bucket cardinality. For 10K records spread across 7 facets
// the BTreeMap insert/walk dominates and stays well under 100ms on
// commodity hardware. This test guards against an accidental
// regression to O(n²) (e.g., a future "merge across facets" refactor).

#[test]
fn synthesizes_ten_thousand_records_under_budget() {
    let mut records: Vec<ProfileSourceRecord> = Vec::with_capacity(10_000);
    let facets = [
        KeyFactFacet::Devices,
        KeyFactFacet::Software,
        KeyFactFacet::Preferences,
        KeyFactFacet::CurrentIssues,
        KeyFactFacet::AddressedIssues,
        KeyFactFacet::RecurringIssues,
        KeyFactFacet::KnownEntities,
    ];
    // Crockford base32, no I/L/O/U.
    let alphabet: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    for i in 0..10_000_u64 {
        // ULID first char must be 0..=7 (top 5 bits zero in a 128-bit
        // ULID). Mix `i` into a 25-char suffix using a 64-bit Weyl
        // sequence so we don't shift past the type's width.
        let mut s = String::with_capacity(26);
        s.push(char::from(b'0' + u8::try_from(i % 8).expect("0..8")));
        let mut state = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        for _ in 0..25 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let idx = (state >> 33) as usize % alphabet.len();
            s.push(alphabet[idx] as char);
        }
        let Ok(record_id) = RecordId::parse(&s) else {
            // Crockford alphabet is closed under our index map, so this
            // path is unreachable — keep the guard rather than panic.
            continue;
        };
        records.push(ProfileSourceRecord {
            record_id,
            is_static: i % 2 == 0,
            confidence: 0.5,
            // `i` is bounded < 10_000 so the cast is lossless.
            facet: facets[usize::try_from(i).expect("i < 10000") % facets.len()],
            // Limit value cardinality so the merge step exercises the
            // BTreeMap collision path; otherwise every line is unique
            // and we'd only stress the linear scan.
            value: format!("fact-{}", i % 256),
            updated_at: ts("2026-04-22T14:00:00Z"),
        });
    }
    assert!(
        records.len() >= 9_000,
        "ULID generator dropped too many: got {}",
        records.len()
    );

    let start = std::time::Instant::now();
    let profile =
        synthesize(&records, &user_subject(), &ts("2026-04-22T14:00:01Z")).expect("synthesize");
    let elapsed = start.elapsed();

    // Generous bound: synthesizes in ~5–15ms locally; CI can be 5×
    // slower under load. 200ms keeps us well clear of an O(n²)
    // regression (which would push 10K records into seconds).
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "synthesizer over budget: {elapsed:?}"
    );

    // Sanity: output is non-empty and every line is wire-valid.
    let json = serde_json::to_string(&profile).expect("serialize");
    let back: crate::generated::verbs::retrieve::DataProfile =
        serde_json::from_str(&json).expect("deserialize round-trip");
    assert_eq!(back, profile);
}

// ── Property: order-invariance ───────────────────────────────────────
//
// `synthesize` is documented as "same inputs always produce the same
// profile" — the forget-propagation contract leans on that. Permuting
// the input slice must not change the output. This is the strongest
// claim a property test can make about a pure aggregation function;
// without it the brief's "byte-identical wire output across calls"
// guarantee (§8.0.a) cannot hold.

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 64,
        ..proptest::prelude::ProptestConfig::default()
    })]

    #[test]
    fn output_invariant_under_input_permutation(
        seed in 0u64..1_000_000,
        len in 1usize..32,
    ) {
        // Build a deterministic batch from the seed so failures are
        // reproducible without proptest's own minimizer.
        let facets = [
            KeyFactFacet::Devices,
            KeyFactFacet::Software,
            KeyFactFacet::Preferences,
            KeyFactFacet::CurrentIssues,
            KeyFactFacet::AddressedIssues,
            KeyFactFacet::RecurringIssues,
            KeyFactFacet::KnownEntities,
        ];
        let mut records: Vec<ProfileSourceRecord> = Vec::with_capacity(len);
        for i in 0..len {
            let mix = seed
                .wrapping_add(u64::try_from(i).expect("len < 32"))
                .wrapping_mul(2_654_435_761);
            let first = char::from(b'0' + u8::try_from(mix % 8).expect("0..8"));
            // 26-char ULID: leading 0..=7 char + 25-char Crockford suffix
            // derived from `mix`. Forming a hex tail then padding keeps
            // the alphabet legal for the ULID parser (all hex digits are
            // in the Crockford set).
            let id = format!("{first}{:0>25X}", mix % (1u64 << 25));
            let Ok(record_id) = RecordId::parse(&id) else {
                continue;
            };
            // Confidence ∈ [0.3, 1.0] so admission isn't probabilistic.
            // (mix >> 8) % 700 fits in u16 — divide as u32 before
            // converting to f32 to avoid precision warnings.
            let bucket = u16::try_from((mix >> 8) % 700).expect("< 700");
            let conf = 0.3_f32 + f32::from(bucket) / 1000.0_f32;
            // Vary timestamps so the proptest also covers the
            // `updated_at = max(contributing.updated_at)` rule. Without
            // this, a regression where the synthesizer kept the *last*
            // record's timestamp instead of the chronologically latest
            // would still pass the permutation invariance check.
            let minute = u8::try_from((mix >> 32) % 60).expect("< 60");
            let updated_at = ts(&format!("2026-04-22T14:{minute:02}:00Z"));
            records.push(ProfileSourceRecord {
                record_id,
                is_static: (mix >> 16) & 1 == 0,
                confidence: conf,
                facet: facets[usize::try_from(mix >> 24).expect("u64 fits usize on 64-bit") % facets.len()],
                value: format!("v-{}", mix % 8),
                updated_at,
            });
        }
        if records.is_empty() {
            return Ok(());
        }

        let mut shuffled = records.clone();
        // Deterministic Fisher-Yates using the seed.
        let mut s = seed;
        for i in (1..shuffled.len()).rev() {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let j = usize::try_from(s).expect("u64 fits usize on 64-bit") % (i + 1);
            shuffled.swap(i, j);
        }

        let now = ts("2026-04-22T14:00:01Z");
        let a = synthesize(&records,  &user_subject(), &now).expect("synthesize a");
        let b = synthesize(&shuffled, &user_subject(), &now).expect("synthesize b");
        proptest::prop_assert_eq!(a, b);
    }
}
