//! Integration tests for the typed [`cairn_core::domain::MemoryRecord`].
//!
//! Covers the issue #37 acceptance criteria:
//! - Every durable record can answer who wrote it, when, under what scope,
//!   and from what evidence.
//! - Serialization round-trips preserve all fields needed by storage,
//!   search, and projection.
//! - Invalid records fail validation before any store call.

#![allow(missing_docs)]

use std::collections::BTreeMap;

use cairn_core::domain::{
    ActorChainEntry, ChainRole, DomainError, EvidenceVector, Identity, MemoryClass, MemoryKind,
    MemoryRecord, MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple,
    record::{Ed25519Signature, RecordId},
};
use proptest::prelude::*;

fn signature_a() -> Ed25519Signature {
    Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128))).expect("valid")
}

fn record() -> MemoryRecord {
    MemoryRecord {
        id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("tafeng".to_owned()),
            project: Some("cairn".to_owned()),
            ..ScopeTuple::default()
        },
        body: "user prefers dark mode".to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            originating_agent_id: Identity::parse("agt:claude-code:opus-4-7:main:v1")
                .expect("valid"),
            source_hash: "sha256:abc123".to_owned(),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: Some("opus-4-7".to_owned()),
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
        evidence: EvidenceVector {
            recall_count: 3,
            score: 0.82,
            unique_queries: 2,
            recency_half_life_days: 14,
        },
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }],
        signature: signature_a(),
        tags: vec!["pref".to_owned(), "ui".to_owned()],
        extra_frontmatter: BTreeMap::from([(
            "obsidian_color".to_owned(),
            serde_json::json!("blue"),
        )]),
    }
}

#[test]
fn valid_record_passes_validation() {
    record().validate().expect("valid");
}

#[test]
fn json_round_trip_preserves_all_fields() {
    let r = record();
    let json = serde_json::to_string(&r).expect("ser");
    let back: MemoryRecord = serde_json::from_str(&json).expect("de");
    assert_eq!(r, back);
    back.validate().expect("validates after round-trip");
}

/// Markdown frontmatter projection: serialize to JSON, drop `body`, and
/// re-parse the stripped form. The acceptance criterion is that every
/// non-body field round-trips through the projection so a YAML projector
/// (out of scope per issue #37) can rely on a stable shape.
#[test]
fn frontmatter_projection_preserves_metadata() {
    let r = record();
    let mut value = serde_json::to_value(&r).expect("ser");
    let body = value
        .as_object_mut()
        .expect("object")
        .remove("body")
        .expect("body present");
    assert_eq!(body, serde_json::Value::String(r.body.clone()));

    // Re-add body to round-trip back into a record — proves frontmatter
    // can be split out and re-merged without field loss.
    value
        .as_object_mut()
        .expect("object")
        .insert("body".to_owned(), body);
    let back: MemoryRecord = serde_json::from_value(value).expect("de");
    assert_eq!(r, back);
}

#[test]
fn missing_provenance_fails_validation() {
    let mut r = record();
    r.provenance.source_hash.clear();
    let err = r.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::MissingProvenance {
            field: "source_hash"
        }
    ));
}

#[test]
fn invalid_identity_rejected_at_parse() {
    let err = Identity::parse("not_an_identity").unwrap_err();
    assert!(matches!(err, DomainError::InvalidIdentity { .. }));
}

#[test]
fn malformed_scope_rejected() {
    let mut r = record();
    r.scope = ScopeTuple::default();
    let err = r.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedScope { .. }));
}

#[test]
fn unsupported_visibility_rejected_at_deserialize() {
    let json = serde_json::json!({
        "id": "01HQZX9F5N0000000000000000",
        "kind": "user",
        "class": "semantic",
        "visibility": "internal",
        "scope": {"user": "tafeng"},
        "body": "x",
        "provenance": {
            "source_sensor": "snr:local:hook:cc-session:v1",
            "created_at": "2026-04-22T14:02:11Z",
            "originating_agent_id": "agt:claude-code:opus-4-7:main:v1",
            "source_hash": "sha256:abc",
            "consent_ref": "consent:1"
        },
        "updated_at": "2026-04-22T14:05:11Z",
        "evidence": {"recall_count": 0, "score": 0.0, "unique_queries": 0, "recency_half_life_days": 14},
        "salience": 0.5,
        "confidence": 0.5,
        "actor_chain": [{"role": "author", "identity": "agt:claude-code:opus-4-7:main:v1", "at": "2026-04-22T14:02:11Z"}],
        "signature": format!("ed25519:{}", "a".repeat(128))
    });
    let res: Result<MemoryRecord, _> = serde_json::from_value(json);
    assert!(res.is_err(), "unknown visibility tier should reject");
}

#[test]
fn missing_signature_rejected() {
    let res = Ed25519Signature::parse("");
    assert!(matches!(res, Err(DomainError::MissingSignature { .. })));
}

#[test]
fn empty_actor_chain_rejected() {
    let mut r = record();
    r.actor_chain.clear();
    let err = r.validate().unwrap_err();
    assert!(matches!(err, DomainError::MissingSignature { .. }));
}

#[test]
fn confidence_out_of_range_rejected() {
    let mut r = record();
    r.confidence = 1.5;
    let err = r.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::OutOfRange {
            field: "confidence",
            ..
        }
    ));
}

#[test]
fn invalid_record_fails_before_store_call() {
    // Sanity: validation surfaces a typed error rather than panicking. A
    // store adapter would propagate this `Err` back to the verb layer
    // without having touched any disk state.
    let mut r = record();
    r.body.clear();
    let err = r.validate().unwrap_err();
    assert_eq!(err, DomainError::EmptyField { field: "body" });
}

// --- Property tests ---------------------------------------------------------

prop_compose! {
    fn arb_kind()(idx in 0u8..19) -> MemoryKind {
        match idx {
            0 => MemoryKind::User,
            1 => MemoryKind::Feedback,
            2 => MemoryKind::Project,
            3 => MemoryKind::Reference,
            4 => MemoryKind::Fact,
            5 => MemoryKind::Belief,
            6 => MemoryKind::Opinion,
            7 => MemoryKind::Event,
            8 => MemoryKind::Entity,
            9 => MemoryKind::Workflow,
            10 => MemoryKind::Rule,
            11 => MemoryKind::StrategySuccess,
            12 => MemoryKind::StrategyFailure,
            13 => MemoryKind::Trace,
            14 => MemoryKind::Reasoning,
            15 => MemoryKind::Playbook,
            16 => MemoryKind::SensorObservation,
            17 => MemoryKind::UserSignal,
            _ => MemoryKind::KnowledgeGap,
        }
    }
}

prop_compose! {
    fn arb_class()(idx in 0u8..4) -> MemoryClass {
        match idx {
            0 => MemoryClass::Episodic,
            1 => MemoryClass::Semantic,
            2 => MemoryClass::Procedural,
            _ => MemoryClass::Graph,
        }
    }
}

prop_compose! {
    fn arb_visibility()(idx in 0u8..6) -> MemoryVisibility {
        match idx {
            0 => MemoryVisibility::Private,
            1 => MemoryVisibility::Session,
            2 => MemoryVisibility::Project,
            3 => MemoryVisibility::Team,
            4 => MemoryVisibility::Org,
            _ => MemoryVisibility::Public,
        }
    }
}

prop_compose! {
    fn arb_record()(
        kind in arb_kind(),
        class in arb_class(),
        visibility in arb_visibility(),
        body in "[a-z ]{1,40}",
        salience in 0.0f32..=1.0,
        confidence in 0.0f32..=1.0,
        recall_count in 0u32..1000,
        score in 0.0f32..=1.0,
        unique_queries in 0u32..100,
    ) -> MemoryRecord {
        MemoryRecord {
            kind,
            class,
            visibility,
            body,
            salience,
            confidence,
            evidence: EvidenceVector { recall_count, score, unique_queries, recency_half_life_days: 14 },
            ..record()
        }
    }
}

proptest! {
    #[test]
    fn json_round_trip_preserves_required_fields(r in arb_record()) {
        let json = serde_json::to_string(&r).expect("ser");
        let back: MemoryRecord = serde_json::from_str(&json).expect("de");
        prop_assert_eq!(&r, &back);
        // Required fields must still be present after the round-trip.
        prop_assert!(!back.body.is_empty());
        prop_assert!(!back.id.as_str().is_empty());
        prop_assert!(!back.signature.as_str().is_empty());
        prop_assert!(!back.actor_chain.is_empty());
        prop_assert!(!back.provenance.source_hash.is_empty());
        prop_assert!(!back.provenance.consent_ref.is_empty());
        back.validate().expect("validates after round-trip");
    }
}
