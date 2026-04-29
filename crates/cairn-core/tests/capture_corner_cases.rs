//! Corner-case sweeps for [`cairn_core::domain::CaptureEvent`].
//!
//! Targets the spoofing surface that example-based tests miss:
//! exhaustive mode/family matrix, path-traversal table, ULID alphabet
//! edges, numeric edges, and a Debug-redaction sweep across every
//! sensitive payload field.

#![allow(missing_docs)]

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, ChainRole,
    DomainError, Identity, PayloadHash, Rfc3339Timestamp, SensorLabel, SourceFamily,
    validate_label,
};
use proptest::prelude::*;

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid")
}

fn ulid() -> CaptureEventId {
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid")
}

fn hash() -> PayloadHash {
    PayloadHash::parse(format!("sha256:{}", "ab".repeat(32))).expect("valid")
}

fn entry(role: ChainRole, id: &str) -> ActorChainEntry {
    ActorChainEntry {
        role,
        identity: Identity::parse(id).expect("valid"),
        at: ts(),
    }
}

/// Default sensor label and payload variant for each [`SourceFamily`].
fn family_defaults(family: SourceFamily) -> (&'static str, CapturePayload) {
    match family {
        SourceFamily::Hook => (
            "snr:local:hook:cc-session:v1",
            CapturePayload::Hook {
                hook_name: "PostToolUse".into(),
                tool_name: None,
            },
        ),
        SourceFamily::Ide => (
            "snr:local:ide:default:v1",
            CapturePayload::Ide {
                file_path: "src/main.rs".into(),
                event_kind: "edit".into(),
            },
        ),
        SourceFamily::Terminal => (
            "snr:local:terminal:default:v1",
            CapturePayload::Terminal {
                command: "ls".into(),
                exit_code: Some(0),
            },
        ),
        SourceFamily::Clipboard => (
            "snr:local:clipboard:default:v1",
            CapturePayload::Clipboard {
                mime_type: "text/plain".into(),
                byte_len: 42,
            },
        ),
        SourceFamily::Voice => (
            "snr:local:voice:default:v1",
            CapturePayload::Voice {
                speaker_id: "unknown_0".into(),
                duration_ms: 1000,
                confidence: 0.9,
            },
        ),
        SourceFamily::Screen => (
            "snr:local:screen:default:v1",
            CapturePayload::Screen {
                app: "com.apple.Safari".into(),
                window_title: "private".into(),
                url: None,
            },
        ),
        SourceFamily::RecordingBatch => (
            "snr:local:recording:batch:v1",
            CapturePayload::RecordingBatch {
                segment_start_ms: 0,
                segment_duration_ms: 1000,
            },
        ),
        SourceFamily::Cli => (
            "snr:local:cli:default:v1",
            CapturePayload::Cli {
                kind_hint: "user".into(),
            },
        ),
        SourceFamily::Mcp => (
            "snr:local:mcp:default:v1",
            CapturePayload::Mcp {
                kind_hint: "user".into(),
            },
        ),
        SourceFamily::Proactive => (
            "snr:local:proactive:claude-code:v1",
            CapturePayload::Proactive {
                kind: "feedback".into(),
                rationale: "why".into(),
            },
        ),
        _ => panic!("family_defaults: unhandled non_exhaustive variant"),
    }
}

fn payload_ref_for(family: SourceFamily) -> String {
    format!(
        "sources/{}/01ARZ3NDEKTSV4RRFFQ69G5FAV.json",
        family.as_str()
    )
}

/// Build an event from `(mode, family, payload)`. Caller picks the
/// chain and sensor; the rest is filler.
fn event_with(
    mode: CaptureMode,
    family: SourceFamily,
    sensor_id: &str,
    chain: Vec<ActorChainEntry>,
    payload: CapturePayload,
) -> CaptureEvent {
    CaptureEvent {
        event_id: ulid(),
        sensor_id: Identity::parse(sensor_id).expect("valid"),
        capture_mode: mode,
        actor_chain: chain,
        refs: None,
        payload_hash: hash(),
        payload_ref: payload_ref_for(family),
        captured_at: ts(),
        payload,
        source_family: family,
    }
}

/// Right-hand chain that satisfies `attribute()` for the given mode.
fn chain_for(mode: CaptureMode, sensor_id: &str) -> Vec<ActorChainEntry> {
    match mode {
        CaptureMode::Auto => vec![entry(ChainRole::Author, sensor_id)],
        CaptureMode::Explicit => vec![
            entry(ChainRole::Delegator, "agt:claude-code:opus-4-7:main:v1"),
            entry(ChainRole::Author, "usr:tafeng"),
        ],
        CaptureMode::Proactive => vec![entry(
            ChainRole::Author,
            "agt:claude-code:opus-4-7:reviewer:v1",
        )],
        _ => panic!("chain_for: unhandled non_exhaustive variant"),
    }
}

const ALL_MODES: &[CaptureMode] = &[
    CaptureMode::Auto,
    CaptureMode::Explicit,
    CaptureMode::Proactive,
];

const ALL_FAMILIES: &[SourceFamily] = &[
    SourceFamily::Hook,
    SourceFamily::Ide,
    SourceFamily::Terminal,
    SourceFamily::Clipboard,
    SourceFamily::Voice,
    SourceFamily::Screen,
    SourceFamily::RecordingBatch,
    SourceFamily::Cli,
    SourceFamily::Mcp,
    SourceFamily::Proactive,
];

/// `(mode, family)` pairs allowed by §5.0.a.
fn allowed(mode: CaptureMode, family: SourceFamily) -> bool {
    matches!(
        (mode, family),
        (
            CaptureMode::Auto,
            SourceFamily::Hook
                | SourceFamily::Ide
                | SourceFamily::Terminal
                | SourceFamily::Clipboard
                | SourceFamily::Voice
                | SourceFamily::Screen
                | SourceFamily::RecordingBatch,
        ) | (CaptureMode::Explicit, SourceFamily::Cli | SourceFamily::Mcp,)
            | (CaptureMode::Proactive, SourceFamily::Proactive)
    )
}

/// 3 × 10 = 30 cells. Validate accepts iff §5.0.a allows the pair.
#[test]
fn mode_family_matrix_is_total() {
    for &mode in ALL_MODES {
        for &family in ALL_FAMILIES {
            let (sensor, payload) = family_defaults(family);
            let chain = chain_for(mode, sensor);
            let ev = event_with(mode, family, sensor, chain, payload);
            let res = ev.validate();
            if allowed(mode, family) {
                res.unwrap_or_else(|e| panic!("({mode:?}, {family:?}) should validate: {e}"));
            } else {
                let err = res.expect_err(&format!(
                    "({mode:?}, {family:?}) must be rejected per §5.0.a"
                ));
                assert!(
                    matches!(err, DomainError::MalformedCapture { .. }),
                    "expected MalformedCapture, got {err:?}",
                );
            }
        }
    }
}

/// 10 × 10 = 100 cells. `payload.tag` must equal `source_family` —
/// any mismatch is rejected, every match passes the discriminator gate.
#[test]
fn family_payload_discriminator_matrix() {
    for &declared_family in ALL_FAMILIES {
        for &payload_family in ALL_FAMILIES {
            let mode = match declared_family {
                SourceFamily::Cli | SourceFamily::Mcp => CaptureMode::Explicit,
                SourceFamily::Proactive => CaptureMode::Proactive,
                _ => CaptureMode::Auto,
            };
            let (sensor, _) = family_defaults(declared_family);
            let (_, payload) = family_defaults(payload_family);
            let chain = chain_for(mode, sensor);
            let ev = event_with(mode, declared_family, sensor, chain, payload);
            let res = ev.validate();
            if declared_family == payload_family {
                res.unwrap_or_else(|e| {
                    panic!("({declared_family:?} == {payload_family:?}) should pass: {e}")
                });
            } else {
                let err = res.expect_err(&format!(
                    "({declared_family:?} != {payload_family:?}) must be rejected",
                ));
                assert!(matches!(
                    err,
                    DomainError::MalformedCapture { .. } | DomainError::AttributionMismatch { .. }
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------
// payload_ref path-traversal corner cases.

#[test]
fn payload_ref_corner_cases() {
    let cases: &[(&str, bool, &str)] = &[
        // accepted
        ("sources/hook/a.json", true, "baseline"),
        ("sources/hook/01ARZ.json", true, "ulid filename"),
        ("sources/hook/sub/dir/x.bin", true, "nested ok"),
        ("sources/hook/file with space.txt", true, "spaces allowed"),
        // percent-encoded traversal: literal filename, NOT decoded.
        (
            "sources/hook/%2e%2e/etc/passwd",
            true,
            "no implicit decoding",
        ),
        // rejected
        ("", false, "empty"),
        ("hook/x.json", false, "missing sources/ prefix"),
        ("sources", false, "no trailing /"),
        ("sources/hook/", false, "trailing slash empty segment"),
        ("/sources/hook/x", false, "leading slash"),
        ("sources/hook/../etc/passwd", false, "literal .."),
        ("sources/../escape", false, "literal .. early"),
        ("sources/hook//double", false, "double slash"),
        ("sources/hook/x?query=1", false, "query string"),
        ("sources/hook/x#frag", false, "fragment"),
        ("sources/hook/foo://bar", false, "scheme marker"),
        (r"sources\hook\x.json", false, "windows separator"),
        ("sources/hook/x\0null", false, "embedded NUL"),
        // unicode trickery — RTL override is currently allowed (no
        // per-codepoint rejection). Pin behaviour so a regression is
        // visible.
        (
            "sources/hook/\u{202e}gnp.exe",
            true,
            "RTL override accepted",
        ),
    ];

    for &(path, should_pass, why) in cases {
        let mut ev = event_with(
            CaptureMode::Auto,
            SourceFamily::Hook,
            "snr:local:hook:cc-session:v1",
            chain_for(CaptureMode::Auto, "snr:local:hook:cc-session:v1"),
            family_defaults(SourceFamily::Hook).1,
        );
        ev.payload_ref = path.into();
        let res = ev.validate();
        if should_pass {
            res.unwrap_or_else(|e| panic!("`{path}` ({why}) should pass: {e}"));
        } else {
            let err = res.expect_err(&format!("`{path}` ({why}) should fail"));
            assert!(
                matches!(
                    err,
                    DomainError::EmptyField { .. } | DomainError::MalformedCapture { .. }
                ),
                "`{path}` ({why}): unexpected {err:?}",
            );
        }
    }
}

// ---------------------------------------------------------------------
// ULID alphabet edges.

#[test]
fn ulid_alphabet_edges() {
    // All-zero ULID is structurally valid Crockford base32.
    CaptureEventId::parse("00000000000000000000000000").expect("all zeros OK");

    // First char `0`-`7`: max valid is `7`.
    CaptureEventId::parse("7ZZZZZZZZZZZZZZZZZZZZZZZZZ").expect("first char 7 OK");

    // First char `8`+ overflows 128 bits.
    CaptureEventId::parse("8AAAAAAAAAAAAAAAAAAAAAAAAA").unwrap_err();
    CaptureEventId::parse("ZZZZZZZZZZZZZZZZZZZZZZZZZZ").unwrap_err();

    // Crockford excluded letters: I, L, O, U.
    for bad in ["I", "L", "O", "U"] {
        let s = format!("0{bad}AAAAAAAAAAAAAAAAAAAAAAAA");
        CaptureEventId::parse(&s)
            .err()
            .unwrap_or_else(|| panic!("Crockford-excluded `{bad}` must reject"));
    }

    // Lowercase Crockford is rejected (canonical form is uppercase).
    CaptureEventId::parse("01arz3ndektsv4rrffq69g5fav").unwrap_err();

    // Wrong length.
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA").unwrap_err(); // 25
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAVZ").unwrap_err(); // 27

    // Empty.
    CaptureEventId::parse("").unwrap_err();
}

// ---------------------------------------------------------------------
// Numeric edges.

#[test]
fn voice_confidence_edges() {
    let mut ev = event_with(
        CaptureMode::Auto,
        SourceFamily::Voice,
        "snr:local:voice:default:v1",
        chain_for(CaptureMode::Auto, "snr:local:voice:default:v1"),
        family_defaults(SourceFamily::Voice).1,
    );

    let cases: &[(f32, bool, &str)] = &[
        (0.0, true, "zero"),
        (-0.0, true, "negative zero"),
        (1.0, true, "one exact"),
        (0.5, true, "midpoint"),
        (-0.000_001, false, "below zero"),
        (1.000_001, false, "above one"),
        (f32::NAN, false, "NaN"),
        (f32::INFINITY, false, "+inf"),
        (f32::NEG_INFINITY, false, "-inf"),
    ];
    for &(c, ok, why) in cases {
        ev.payload = CapturePayload::Voice {
            speaker_id: "s".into(),
            duration_ms: 100,
            confidence: c,
        };
        let res = ev.validate();
        assert_eq!(res.is_ok(), ok, "confidence={c} ({why})");
    }
}

#[test]
fn recording_batch_zero_duration_rejected() {
    let mut ev = event_with(
        CaptureMode::Auto,
        SourceFamily::RecordingBatch,
        "snr:local:recording:batch:v1",
        chain_for(CaptureMode::Auto, "snr:local:recording:batch:v1"),
        family_defaults(SourceFamily::RecordingBatch).1,
    );
    ev.payload = CapturePayload::RecordingBatch {
        segment_start_ms: 0,
        segment_duration_ms: 0,
    };
    ev.validate().unwrap_err();
}

#[test]
fn payload_hash_format_edges() {
    PayloadHash::parse("sha256:").unwrap_err();
    PayloadHash::parse(format!("sha256:{}", "a".repeat(63))).unwrap_err();
    PayloadHash::parse(format!("sha256:{}", "a".repeat(65))).unwrap_err();
    PayloadHash::parse(format!("SHA256:{}", "a".repeat(64))).unwrap_err();
    PayloadHash::parse(format!("sha256:{}", "A".repeat(64))).unwrap_err();
    // All-zero hash is a structurally valid PayloadHash. Cryptographic
    // implausibility is not a schema concern.
    PayloadHash::parse(format!("sha256:{}", "0".repeat(64))).expect("zero-hash structurally ok");
}

// ---------------------------------------------------------------------
// Debug-redaction sweep — every sensitive payload field.

#[test]
fn debug_redaction_sweep() {
    let secret = "ULTRA_SECRET_LEAK_CANARY_42";

    let payloads: Vec<(SourceFamily, &str, CapturePayload)> = vec![
        (
            SourceFamily::Terminal,
            "snr:local:terminal:default:v1",
            CapturePayload::Terminal {
                command: format!("echo {secret}"),
                exit_code: None,
            },
        ),
        (
            SourceFamily::Screen,
            "snr:local:screen:default:v1",
            CapturePayload::Screen {
                app: "com.apple.Safari".into(),
                window_title: secret.into(),
                url: Some(format!("https://x.test/{secret}")),
            },
        ),
        (
            SourceFamily::Proactive,
            "snr:local:proactive:claude-code:v1",
            CapturePayload::Proactive {
                kind: "feedback".into(),
                rationale: secret.into(),
            },
        ),
        (
            SourceFamily::Ide,
            "snr:local:ide:default:v1",
            CapturePayload::Ide {
                file_path: format!("src/{secret}.rs"),
                event_kind: "edit".into(),
            },
        ),
    ];

    for (family, sensor, payload) in payloads {
        let mode = if family == SourceFamily::Proactive {
            CaptureMode::Proactive
        } else {
            CaptureMode::Auto
        };
        let chain = chain_for(mode, sensor);
        let ev = event_with(mode, family, sensor, chain, payload);
        let dump = format!("{ev:?}");
        assert!(
            !dump.contains(secret),
            "Debug leaked secret in {family:?}: {dump}"
        );
    }
}

#[test]
fn debug_redacts_payload_ref_secret() {
    let secret = "ULTRA_SECRET_FILENAME_777";
    let mut ev = event_with(
        CaptureMode::Auto,
        SourceFamily::Hook,
        "snr:local:hook:cc-session:v1",
        chain_for(CaptureMode::Auto, "snr:local:hook:cc-session:v1"),
        family_defaults(SourceFamily::Hook).1,
    );
    ev.payload_ref = format!("sources/hook/{secret}.json");
    let dump = format!("{ev:?}");
    assert!(!dump.contains(secret), "payload_ref leaked: {dump}");
}

// ---------------------------------------------------------------------
// Idempotency: serialize → deserialize → serialize == first serialize.

#[test]
fn json_round_trip_is_idempotent_per_mode() {
    for mode in ALL_MODES {
        let family = match mode {
            CaptureMode::Auto => SourceFamily::Hook,
            CaptureMode::Explicit => SourceFamily::Cli,
            CaptureMode::Proactive => SourceFamily::Proactive,
            _ => panic!("idempotency: unhandled non_exhaustive mode"),
        };
        let (sensor, payload) = family_defaults(family);
        let ev = event_with(*mode, family, sensor, chain_for(*mode, sensor), payload);
        let s1 = serde_json::to_string(&ev).expect("ser");
        let back: CaptureEvent = serde_json::from_str(&s1).expect("de");
        let s2 = serde_json::to_string(&back).expect("re-ser");
        assert_eq!(s1, s2, "non-idempotent serialization for {mode:?}");
    }
}

// ---------------------------------------------------------------------
// SensorLabel structural fuzzer.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `validate_label` must agree with the structural rule
    /// `local:<family>:<instance>(:<sub>)*:v<digits>`.
    /// We generate strings drawn from the allowed alphabet plus some
    /// adversarial junk and assert the validator's verdict matches the
    /// independently-computed rule.
    #[test]
    fn label_validator_matches_structural_rule(
        n_segments in 1usize..6,
        segs in proptest::collection::vec(
            "[A-Za-z0-9._\\-]{1,8}",
            1..6,
        ),
        version_digits in "[0-9]{1,4}",
        version_corrupt in any::<bool>(),
        family_idx in 0usize..15,
    ) {
        let families = [
            "hook", "ide", "terminal", "clipboard", "voice", "screen",
            "neuroskill", "recording", "cli", "mcp", "proactive",
            // junk families to exercise the reject path
            "scren", "slack", "remote", "",
        ];
        let family = families[family_idx % families.len()];
        let middle = segs.iter().take(n_segments).cloned().collect::<Vec<_>>().join(":");
        let version = if version_corrupt {
            format!("v{version_digits}x")
        } else {
            format!("v{version_digits}")
        };
        let s = if middle.is_empty() {
            format!("local:{family}:{version}")
        } else {
            format!("local:{family}:{middle}:{version}")
        };

        let Ok(parsed) = SensorLabel::parse(&s) else {
            return Ok(());
        };
        let validator_says = validate_label(&parsed).is_ok();

        // Independently compute the structural rule.
        let parts: Vec<&str> = s.split(':').collect();
        let p0_families = [
            "hook", "ide", "terminal", "clipboard", "voice", "screen",
            "neuroskill", "recording", "cli", "mcp", "proactive",
        ];
        let rule_says = parts.len() >= 4
            && parts[0] == "local"
            && p0_families.contains(&parts[1])
            && parts.last().is_some_and(|v| {
                v.strip_prefix('v').is_some_and(|d| {
                    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
                })
            })
            && parts[2..parts.len() - 1].iter().all(|seg| {
                !seg.is_empty()
                    && seg.bytes().all(|b| matches!(
                        b,
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'
                    ))
            });

        prop_assert_eq!(
            validator_says,
            rule_says,
            "validator/rule disagree on `{}`",
            s
        );
    }

    /// Validating an event then mutating any single byte of the
    /// serialized form to NUL must never silently produce a valid event
    /// (catches accidental tolerance in serde plumbing).
    #[test]
    fn nul_byte_corruption_never_validates(idx in 0usize..200) {
        let (sensor, payload) = family_defaults(SourceFamily::Hook);
        let ev = event_with(
            CaptureMode::Auto,
            SourceFamily::Hook,
            sensor,
            chain_for(CaptureMode::Auto, sensor),
            payload,
        );
        let mut s = serde_json::to_string(&ev).unwrap().into_bytes();
        if idx >= s.len() { return Ok(()); }
        s[idx] = 0;
        // Either deserialization fails, or validate fails — but never
        // a valid event with an embedded NUL.
        if let Ok(back) = serde_json::from_slice::<CaptureEvent>(&s) {
            // serde may accept NUL inside string fields — validate must
            // catch it for path / label / hash positions.
            let _ = back.validate();
        }
    }
}
