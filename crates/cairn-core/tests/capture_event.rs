//! Integration tests for the typed [`cairn_core::domain::CaptureEvent`].
//!
//! Covers the issue #71 acceptance criteria:
//! - Every captured event can be traced to a sensor / user / agent
//!   identity and source mode (attribution rule from §5.0.a).
//! - `CaptureEvent` serialization round-trips for replay and eval
//!   fixtures.
//! - Invalid or undeclared sensor labels are rejected before any pipeline
//!   stage observes them.
//!
//! Snapshot fixtures live under `fixtures/capture_events/`. They are
//! replayed here through `serde_json::from_str` so the same byte-for-byte
//! envelope can be consumed by the downstream extractor / filter / WAL
//! tests as they land.

#![allow(missing_docs)]

use std::path::Path;

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, DomainError, Identity, IdentityKind, PayloadHash, Rfc3339Timestamp, SensorLabel,
    SourceFamily, attribute, validate_label,
};
use proptest::prelude::*;

const FIXTURE_DIR: &str = "../../fixtures/capture_events";

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid")
}

fn ulid_a() -> CaptureEventId {
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ULID")
}

fn ulid_b() -> CaptureEventId {
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FB0").expect("valid ULID")
}

fn ulid_c() -> CaptureEventId {
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FCA").expect("valid ULID")
}

fn entry(role: ChainRole, id: &str) -> ActorChainEntry {
    ActorChainEntry {
        role,
        identity: Identity::parse(id).expect("valid"),
        at: ts(),
    }
}

fn hash() -> PayloadHash {
    PayloadHash::parse(format!("sha256:{}", "ab".repeat(32))).expect("valid")
}

fn auto_event() -> CaptureEvent {
    CaptureEvent {
        event_id: ulid_a(),
        sensor_id: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![entry(ChainRole::Author, "snr:local:hook:cc-session:v1")],
        refs: Some(CaptureRefs {
            session_id: Some("sess-42".into()),
            turn_id: Some("turn-7".into()),
            tool_id: Some("tool-1".into()),
        }),
        payload_hash: hash(),
        payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
        captured_at: ts(),
        payload: CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: Some("Read".into()),
        },
        source_family: SourceFamily::Hook,
    }
}

fn explicit_event() -> CaptureEvent {
    CaptureEvent {
        event_id: ulid_b(),
        sensor_id: Identity::parse("snr:local:cli:default:v1").expect("valid"),
        capture_mode: CaptureMode::Explicit,
        actor_chain: vec![
            entry(ChainRole::Delegator, "agt:claude-code:opus-4-7:main:v1"),
            entry(ChainRole::Author, "usr:tafeng"),
        ],
        refs: None,
        payload_hash: hash(),
        payload_ref: "sources/cli/01ARZ3NDEKTSV4RRFFQ69G5FB0.txt".into(),
        captured_at: ts(),
        payload: CapturePayload::Cli {
            kind_hint: "user".into(),
        },
        source_family: SourceFamily::Cli,
    }
}

fn proactive_event() -> CaptureEvent {
    CaptureEvent {
        event_id: ulid_c(),
        sensor_id: Identity::parse("snr:local:proactive:claude-code:v1").expect("valid"),
        capture_mode: CaptureMode::Proactive,
        actor_chain: vec![entry(
            ChainRole::Author,
            "agt:claude-code:opus-4-7:reviewer:v1",
        )],
        refs: Some(CaptureRefs {
            session_id: Some("sess-42".into()),
            turn_id: Some("turn-7".into()),
            tool_id: None,
        }),
        payload_hash: hash(),
        payload_ref: "sources/proactive/01ARZ3NDEKTSV4RRFFQ69G5FCA.json".into(),
        captured_at: ts(),
        payload: CapturePayload::Proactive {
            kind: "feedback".into(),
            rationale: "user corrected the agent — high-salience".into(),
        },
        source_family: SourceFamily::Proactive,
    }
}

#[test]
fn auto_event_validates_and_traces_to_sensor() {
    let ev = auto_event();
    ev.validate().expect("valid auto event");

    let author = attribute(ev.capture_mode, &ev.actor_chain).expect("attributed");
    assert_eq!(author.identity.kind(), IdentityKind::Sensor);
}

#[test]
fn explicit_event_validates_and_traces_to_human() {
    let ev = explicit_event();
    ev.validate().expect("valid explicit event");

    let author = attribute(ev.capture_mode, &ev.actor_chain).expect("attributed");
    assert_eq!(author.identity.kind(), IdentityKind::Human);
    assert_eq!(author.identity.as_str(), "usr:tafeng");
}

#[test]
fn proactive_event_validates_and_traces_to_agent() {
    let ev = proactive_event();
    ev.validate().expect("valid proactive event");

    let author = attribute(ev.capture_mode, &ev.actor_chain).expect("attributed");
    assert_eq!(author.identity.kind(), IdentityKind::Agent);
}

#[test]
fn explicit_event_with_only_sensor_chain_is_rejected() {
    // Mode B requires a human author — a sensor-only chain must fail.
    let mut ev = explicit_event();
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:hook:cc-session:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::AttributionMismatch { .. }));
}

#[test]
fn auto_event_with_human_author_is_rejected() {
    let mut ev = auto_event();
    ev.actor_chain = vec![entry(ChainRole::Author, "usr:tafeng")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::AttributionMismatch { .. }));
}

#[test]
fn proactive_event_with_sensor_author_is_rejected() {
    let mut ev = proactive_event();
    ev.actor_chain = vec![entry(
        ChainRole::Author,
        "snr:local:proactive:claude-code:v1",
    )];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::AttributionMismatch { .. }));
}

#[test]
fn undeclared_sensor_label_rejected() {
    // `local:slack:` is not in the P0 manifest.
    let bad = SensorLabel::parse("remote:slack:default:v1").expect("syntactic");
    let err = validate_label(&bad).unwrap_err();
    assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
}

#[test]
fn validate_rejects_undeclared_sensor_in_event() {
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:remote:slack:default:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:remote:slack:default:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
}

#[test]
fn mode_family_mismatch_rejected() {
    // Mode A cannot carry an `cli` family (that's Mode B's surface).
    let mut ev = explicit_event();
    ev.capture_mode = CaptureMode::Auto;
    // Patch the chain so the attribution check would otherwise pass —
    // we want to prove the mode/family pairing is what catches this.
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:cli:default:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn proactive_with_screen_family_rejected() {
    let mut ev = auto_event();
    ev.capture_mode = CaptureMode::Proactive;
    ev.actor_chain = vec![entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn sensor_label_family_mismatch_rejected() {
    // CLI sensor declaring a screen payload — declared family ≠ event family.
    let mut ev = explicit_event();
    ev.source_family = SourceFamily::Screen;
    ev.payload = CapturePayload::Screen {
        app: "com.apple.Safari".into(),
        window_title: "spoof".into(),
        url: None,
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn auto_mode_author_must_equal_sensor_id() {
    // Sensor declared in `sensor_id` differs from the sensor that
    // appears as `Author` in the chain — Mode A authorship spoofing.
    let mut ev = auto_event();
    // Use a *different* canonical hook label to prove the author-vs-sensor
    // mismatch is what catches this, not a manifest miss.
    ev.sensor_id = Identity::parse("snr:local:hook:codex-session:v1").expect("valid");
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::AttributionMismatch { .. }));
}

#[test]
fn chain_sensor_entry_must_match_sensor_id() {
    // Mode B with a Sensor entry that points to a different sensor than
    // the one declared in `sensor_id`.
    let mut ev = explicit_event();
    ev.actor_chain.push(ActorChainEntry {
        role: ChainRole::Sensor,
        identity: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
        at: ts(),
    });
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::AttributionMismatch { .. }));
}

#[test]
fn payload_ref_rejects_uri_scheme() {
    // Any scheme (file://, https://, ...) is rejected — payload_ref is a
    // vault-relative path, not a URI.
    let mut ev = auto_event();
    ev.payload_ref = "file:///vault/sources/x.json".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));

    ev.payload_ref = "https://example.com/sources/x.json".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_must_be_under_sources() {
    let mut ev = auto_event();
    ev.payload_ref = "etc/passwd".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_rejects_absolute_path() {
    let mut ev = auto_event();
    ev.payload_ref = "/etc/passwd".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_rejects_dotdot_traversal() {
    let mut ev = auto_event();
    ev.payload_ref = "sources/../etc/passwd".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_rejects_double_slash() {
    let mut ev = auto_event();
    ev.payload_ref = "sources//hook/x.json".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_rejects_query_and_fragment() {
    let mut ev = auto_event();
    ev.payload_ref = "sources/x.json?evil=1".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_rejects_nul_byte() {
    let mut ev = auto_event();
    ev.payload_ref = "sources/hook/x\0.json".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn voice_confidence_out_of_range_rejected() {
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:voice:default:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:voice:default:v1")];
    ev.source_family = SourceFamily::Voice;
    ev.payload = CapturePayload::Voice {
        speaker_id: "alice".into(),
        duration_ms: 100,
        confidence: 42.0,
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::OutOfRange {
            field: "confidence",
            ..
        }
    ));
}

#[test]
fn voice_confidence_nan_rejected() {
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:voice:default:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:voice:default:v1")];
    ev.source_family = SourceFamily::Voice;
    ev.payload = CapturePayload::Voice {
        speaker_id: "alice".into(),
        duration_ms: 100,
        confidence: f32::NAN,
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::OutOfRange {
            field: "confidence",
            ..
        }
    ));
}

#[test]
fn empty_hook_name_rejected() {
    let mut ev = auto_event();
    ev.payload = CapturePayload::Hook {
        hook_name: String::new(),
        tool_name: None,
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::EmptyField { field: "hook_name" }
    ));
}

#[test]
fn empty_cli_kind_hint_rejected() {
    let mut ev = explicit_event();
    ev.payload = CapturePayload::Cli {
        kind_hint: String::new(),
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::EmptyField { field: "kind_hint" }
    ));
}

#[test]
fn empty_proactive_rationale_rejected() {
    let mut ev = proactive_event();
    ev.payload = CapturePayload::Proactive {
        kind: "feedback".into(),
        rationale: String::new(),
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::EmptyField { field: "rationale" }
    ));
}

#[test]
fn zero_recording_duration_rejected() {
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:recording:batch:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:recording:batch:v1")];
    ev.source_family = SourceFamily::RecordingBatch;
    ev.payload_ref = "sources/recording/x.mp4".into();
    ev.payload = CapturePayload::RecordingBatch {
        segment_start_ms: 0,
        segment_duration_ms: 0,
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::OutOfRange {
            field: "segment_duration_ms",
            ..
        }
    ));
}

#[test]
fn payload_ref_rejects_backslash_traversal() {
    // sources/..\..\Windows\win.ini — backslash-separated parent hops
    // would slip past a forward-slash-only segment scan.
    let mut ev = auto_event();
    ev.payload_ref = "sources/..\\..\\Windows\\win.ini".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_ref_rejects_pure_backslash() {
    let mut ev = auto_event();
    ev.payload_ref = "sources\\hook\\x.json".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn deserialize_runs_validate() {
    // A JSON event whose mode/family pair would violate §5.0.a must be
    // rejected at `serde_json::from_str` time, not silently materialized
    // into a `CaptureEvent`. This proves the `try_from` gate works.
    let mut ev = explicit_event();
    ev.capture_mode = CaptureMode::Auto;
    let json = serde_json::to_string(&ev).expect("ser");
    let res: Result<CaptureEvent, _> = serde_json::from_str(&json);
    assert!(res.is_err(), "deserialization must run validate()");
}

#[test]
fn deserialize_rejects_undeclared_sensor() {
    let mut ev = auto_event();
    // Construct an envelope whose sensor label does not match the
    // structural rule (no version segment).
    ev.sensor_id = Identity::parse("snr:local:hook:bare").expect("syntactic");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:hook:bare")];
    let json = serde_json::to_string(&ev).expect("ser");
    let res: Result<CaptureEvent, _> = serde_json::from_str(&json);
    assert!(res.is_err());
}

#[test]
fn debug_redacts_sensitive_payload_fields() {
    // Any of `Terminal.command`, `Screen.window_title/url`, or
    // `Proactive.rationale` reaching a `Debug` dump would be a
    // user-data leak. The Debug impl must not contain the secret.
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:terminal:default:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:terminal:default:v1")];
    ev.source_family = SourceFamily::Terminal;
    ev.payload = CapturePayload::Terminal {
        command: "echo SUPER_SECRET_TOKEN_42".into(),
        exit_code: Some(0),
    };
    let dump = format!("{ev:?}");
    assert!(
        !dump.contains("SUPER_SECRET_TOKEN_42"),
        "Debug must redact terminal command: {dump}"
    );
}

#[test]
fn debug_redacts_payload_ref() {
    // `payload_ref` is a vault-relative path whose filename portion is
    // chosen by the producer and may carry user-derived content (e.g.
    // `sources/cli/remember-my-ssn.txt`). The `Debug` impl must not
    // leak it into tracing/panic dumps.
    let mut ev = auto_event();
    ev.payload_ref = "sources/hook/remember-my-ssn-123-45-6789.json".into();
    let dump = format!("{ev:?}");
    assert!(
        !dump.contains("remember-my-ssn"),
        "Debug must redact payload_ref: {dump}"
    );
    assert!(
        !dump.contains("123-45-6789"),
        "Debug must redact payload_ref: {dump}"
    );
}

#[test]
fn proactive_sensor_agent_must_match_author_agent() {
    // sensor_id = snr:local:proactive:claude-code:v1 but author is a
    // codex agent — must be rejected to block cross-agent spoofing.
    let mut ev = proactive_event();
    ev.actor_chain = vec![entry(ChainRole::Author, "agt:codex:gpt-5:main:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::AttributionMismatch { .. }));
}

#[test]
fn proactive_sensor_agent_match_passes() {
    // Aligned agent — claude-code label, claude-code author.
    let mut ev = proactive_event();
    ev.actor_chain = vec![entry(
        ChainRole::Author,
        "agt:claude-code:opus-4-7:reviewer:v2",
    )];
    ev.validate().expect("agent slugs match");
}

#[test]
fn captureevent_rejects_structurally_valid_but_unregistered_sensor() {
    // `snr:local:hook:any-instance:v1` is structurally well-formed but
    // is not in `P0_CANONICAL_LABELS`. `CaptureEvent::validate` must
    // close that gap pre-#50 — otherwise a producer can mint trusted
    // events under fabricated sensor identities.
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:hook:any-instance:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:hook:any-instance:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
}

#[test]
fn captureevent_rejects_unregistered_proactive_agent() {
    let mut ev = proactive_event();
    ev.sensor_id = Identity::parse("snr:local:proactive:rogue-agent:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "agt:rogue-agent:m:role:v1")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
}

#[test]
fn arbitrary_suffix_under_declared_family_rejected() {
    // `snr:local:hook:anything` has the right family prefix but no
    // version segment — must not pass manifest validation.
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:hook:anything").expect("valid syntactic");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:hook:anything")];
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
}

#[test]
fn captureeventid_rejects_overflow_first_char() {
    // ULID first char must be `0`-`7`. `8...` overflows 128 bits.
    let err = CaptureEventId::parse("8ARZ3NDEKTSV4RRFFQ69G5FAVZ").unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn captureeventid_accepts_max_valid_first_char() {
    CaptureEventId::parse("7ZZZZZZZZZZZZZZZZZZZZZZZZZ").expect("first char `7` is the max");
}

#[test]
fn recording_payload_ref_must_be_vault_relative() {
    // The source recording is now bound to the envelope-level
    // `payload_ref` — its trust boundary is the same shared check.
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:recording:batch:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:recording:batch:v1")];
    ev.source_family = SourceFamily::RecordingBatch;
    ev.payload = CapturePayload::RecordingBatch {
        segment_start_ms: 0,
        segment_duration_ms: 1000,
    };
    ev.payload_ref = "/tmp/attacker/x.mp4".into();
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn empty_string_session_ref_rejected() {
    // CaptureRefs absences are `None`; `Some("")` is malformed because
    // downstream ordering/dedup keys off these values.
    let mut ev = auto_event();
    ev.refs = Some(CaptureRefs {
        session_id: Some(String::new()),
        ..Default::default()
    });
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::EmptyField {
            field: "refs.session_id"
        }
    ));
}

#[test]
fn whitespace_only_turn_ref_rejected() {
    let mut ev = auto_event();
    ev.refs = Some(CaptureRefs {
        turn_id: Some("   ".into()),
        ..Default::default()
    });
    let err = ev.validate().unwrap_err();
    assert!(matches!(
        err,
        DomainError::EmptyField {
            field: "refs.turn_id"
        }
    ));
}

#[test]
fn try_new_runs_validate() {
    // Smart constructor enforces every invariant — caller cannot skip
    // it the way field-literal construction would.
    let res = CaptureEvent::try_new(
        ulid_a(),
        Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
        CaptureMode::Auto,
        vec![entry(ChainRole::Author, "snr:local:hook:cc-session:v1")],
        None,
        hash(),
        // Off-vault path — must be rejected.
        "/etc/passwd".into(),
        ts(),
        CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: None,
        },
        SourceFamily::Hook,
    );
    assert!(matches!(res, Err(DomainError::MalformedCapture { .. })));
}

#[test]
fn screen_window_title_can_be_empty_for_redaction() {
    // Privacy-redacted screen captures legitimately ship empty titles.
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:screen:default:v1").expect("valid");
    ev.actor_chain = vec![entry(ChainRole::Author, "snr:local:screen:default:v1")];
    ev.source_family = SourceFamily::Screen;
    ev.payload = CapturePayload::Screen {
        app: "com.apple.Safari".into(),
        window_title: String::new(),
        url: None,
    };
    ev.validate().expect("redacted title must not block ingest");
}

#[test]
fn neuroskill_sensor_pinned_to_hook_family() {
    // local:neuroskill:* is now pinned to Hook — emitting any other
    // family from a neuroskill sensor must fail.
    let mut ev = auto_event();
    ev.sensor_id = Identity::parse("snr:local:neuroskill:cc-session:v1").expect("valid");
    ev.actor_chain = vec![entry(
        ChainRole::Author,
        "snr:local:neuroskill:cc-session:v1",
    )];
    // Hook payload — should pass.
    ev.validate().expect("neuroskill emitting Hook is valid");

    // Now flip to a Voice payload + family. Neuroskill must be rejected.
    ev.source_family = SourceFamily::Voice;
    ev.payload = CapturePayload::Voice {
        speaker_id: "x".into(),
        duration_ms: 0,
        confidence: 1.0,
    };
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn payload_family_mismatch_rejected() {
    let mut ev = auto_event();
    ev.source_family = SourceFamily::Voice;
    let err = ev.validate().unwrap_err();
    assert!(matches!(err, DomainError::MalformedCapture { .. }));
}

#[test]
fn round_trips_each_mode_through_json() {
    for ev in [auto_event(), explicit_event(), proactive_event()] {
        let s = serde_json::to_string(&ev).expect("ser");
        let back: CaptureEvent = serde_json::from_str(&s).expect("de");
        assert_eq!(back, ev);
    }
}

#[test]
fn snapshot_each_mode() {
    insta::assert_json_snapshot!("capture_event_auto", auto_event());
    insta::assert_json_snapshot!("capture_event_explicit", explicit_event());
    insta::assert_json_snapshot!("capture_event_proactive", proactive_event());
}

#[test]
fn fixtures_replay_and_revalidate() {
    let dir = Path::new(FIXTURE_DIR);
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("fixture dir `{}` should exist: {e}", dir.display()));
    let mut count = 0;
    for entry in entries {
        let entry = entry.expect("readdir");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture `{}`: {e}", path.display()));
        let ev: CaptureEvent = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse fixture `{}`: {e}", path.display()));
        ev.validate()
            .unwrap_or_else(|e| panic!("validate fixture `{}`: {e}", path.display()));
        count += 1;
    }
    assert!(count >= 3, "expected at least 3 fixtures, found {count}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any event we hand-construct round-trips byte-for-byte through JSON.
    #[test]
    fn serde_round_trip_is_total(seed in 0u32..1000) {
        let ev = match seed % 3 {
            0 => auto_event(),
            1 => explicit_event(),
            _ => proactive_event(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: CaptureEvent = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(back, ev);
    }

    /// Any 64-char lowercase-hex tail is a valid `PayloadHash`; mutating
    /// any byte to an invalid one (uppercase or non-hex) is rejected.
    #[test]
    fn payload_hash_invariants(
        idx in 0usize..64,
        bad in prop::char::range('g', 'z')
    ) {
        let good = format!("sha256:{}", "a".repeat(64));
        PayloadHash::parse(&good).expect("baseline valid");

        let mut bytes = good.into_bytes();
        bytes[7 + idx] = bad as u8;
        let mutated = String::from_utf8(bytes).unwrap();
        let res = PayloadHash::parse(mutated);
        prop_assert!(res.is_err());
    }
}
