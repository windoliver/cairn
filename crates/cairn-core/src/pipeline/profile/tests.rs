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
//!   `dynamic_records_disappearing_does_not_touch_static_section`.
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
    assert_eq!(profile.static_section.key_facts.preferences.len(), 1);
    assert_eq!(profile.static_section.key_facts.devices.len(), 1);
    assert!(profile.static_section.key_facts.current_issues.is_empty());
    assert_eq!(profile.dynamic_section.key_facts.current_issues.len(), 1);
    assert!(profile.dynamic_section.key_facts.preferences.is_empty());

    // updated_at is the latest of the contributing records, not `now`.
    assert_eq!(profile.updated_at, "2026-04-22T14:10:00Z");

    // P0 narrative bodies are stub-empty per brief §7.1.
    assert_eq!(profile.static_section.summary, "");
    assert_eq!(profile.static_section.historical_summary, "");
    assert_eq!(profile.dynamic_section.summary, "");
    assert_eq!(profile.dynamic_section.historical_summary, "");
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

    let line = &profile.static_section.key_facts.preferences[0];
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

    let prefs = &profile.static_section.key_facts.preferences;
    assert_eq!(prefs.len(), 1);
    assert!((prefs[0].confidence - 0.85).abs() < 1e-6);
    assert_eq!(prefs[0].evidence.len(), 2);
    assert_eq!(profile.static_section.key_facts.software.len(), 1);
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

    let prefs = &profile.static_section.key_facts.preferences;
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
    assert!(profile.static_section.key_facts.preferences.is_empty());
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
    assert!(profile.static_section.key_facts.preferences.is_empty());
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
    assert_eq!(before.static_section.key_facts.preferences.len(), 1);

    let after = synthesize(&[device], &user_subject(), &ts("2026-04-22T14:06:00Z"))
        .expect("re-synthesize after forget");
    assert!(after.static_section.key_facts.preferences.is_empty());
    assert_eq!(after.static_section.key_facts.devices.len(), 1);
}

#[test]
fn dynamic_records_disappearing_does_not_touch_static_section() {
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
    assert_eq!(
        with_dynamic.dynamic_section.key_facts.current_issues.len(),
        1
    );
    assert_eq!(with_dynamic.static_section.key_facts.preferences.len(), 1);

    // Expirer drops the dynamic fact only.
    let without_dynamic = synthesize(&[pref], &user_subject(), &ts("2026-04-22T14:10:00Z"))
        .expect("synthesize after expiration");
    assert!(
        without_dynamic
            .dynamic_section
            .key_facts
            .current_issues
            .is_empty()
    );
    assert_eq!(
        without_dynamic.static_section.key_facts.preferences.len(),
        1
    );
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
    assert!(profile.static_section.key_facts.preferences.is_empty());
    assert!(profile.dynamic_section.key_facts.current_issues.is_empty());
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
    let entities = &profile.static_section.key_facts.known_entities;
    assert_eq!(
        entities
            .iter()
            .map(|l| l.value.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha org", "mid org", "zebra org"]
    );
}
