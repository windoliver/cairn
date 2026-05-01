//! End-to-end coverage for issue #218 — `Terminal.context` persisted on
//! the `CapturePayload::Terminal` variant.
//!
//! Exercises three surfaces:
//! 1. Real on-disk JSON fixtures (legacy, post-#218 interactive,
//!    post-#218 structured) round-trip through `serde_json` and the
//!    permissive `validate()` / strict `validate_for_capture()` split.
//! 2. Wire-format negative cases — bad `context` shapes are rejected
//!    by deserialization.
//! 3. Property-test that `Option<TerminalContext>` round-trips through
//!    JSON for every variant.

#![allow(missing_docs)]

use std::path::PathBuf;

use cairn_core::domain::{CaptureEvent, CapturePayload, DomainError, TerminalContext};
use proptest::prelude::*;

const FIXTURE_DIR: &str = "../../fixtures/capture_events/terminal";

fn load(name: &str) -> String {
    let path = PathBuf::from(FIXTURE_DIR).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// -------- 1. Fixture round-trip ----------------------------------------

#[test]
fn legacy_fixture_parses_and_passes_read_validate_but_fails_capture_validate() {
    let raw = load("legacy_no_context.json");
    let ev: CaptureEvent = serde_json::from_str(&raw).expect("parse legacy fixture");

    // Read boundary: legacy events validate.
    ev.validate().expect("read-path validate accepts legacy");

    // Strict write boundary: rejected with the dedicated marker.
    let err = ev.validate_for_capture().unwrap_err();
    match err {
        DomainError::EmptyField { field } => assert_eq!(field, "context"),
        other => panic!("expected EmptyField{{context}}, got {other:?}"),
    }

    // Payload field really is None after deserialize.
    match ev.payload {
        CapturePayload::Terminal { context, .. } => assert_eq!(context, None),
        other => panic!("expected Terminal payload, got {other:?}"),
    }
}

#[test]
fn post_218_interactive_fixture_passes_both_validators() {
    let raw = load("post_218_interactive.json");
    let ev: CaptureEvent = serde_json::from_str(&raw).expect("parse post-#218 fixture");
    ev.validate().expect("validate ok");
    ev.validate_for_capture()
        .expect("validate_for_capture ok with InteractiveTty");
    match ev.payload {
        CapturePayload::Terminal { context, .. } => {
            assert_eq!(context, Some(TerminalContext::InteractiveTty));
        }
        other => panic!("expected Terminal, got {other:?}"),
    }
}

#[test]
fn post_218_structured_fixture_passes_both_validators() {
    let raw = load("post_218_structured.json");
    let ev: CaptureEvent = serde_json::from_str(&raw).expect("parse structured fixture");
    ev.validate().expect("validate ok");
    ev.validate_for_capture()
        .expect("validate_for_capture ok with NonInteractiveOrStructured");
    match ev.payload {
        CapturePayload::Terminal { context, .. } => {
            assert_eq!(context, Some(TerminalContext::NonInteractiveOrStructured));
        }
        other => panic!("expected Terminal, got {other:?}"),
    }
}

// -------- 2. Wire-format negative cases --------------------------------

/// `context` as an unknown enum tag must fail to deserialize. Catches
/// typos like `"interactive"` instead of `"interactive_tty"` at the wire
/// boundary instead of letting them masquerade as `None`.
#[test]
fn wire_rejects_unknown_context_string() {
    let raw = load("post_218_interactive.json").replace("interactive_tty", "interactive");
    let res: Result<CaptureEvent, _> = serde_json::from_str(&raw);
    assert!(
        res.is_err(),
        "wire must reject unknown context tag, got {res:?}"
    );
}

/// `context: null` should deserialize as `None` (Option semantics) —
/// the absence and the explicit-null shapes are equivalent at the wire.
#[test]
fn wire_accepts_null_context_as_none() {
    let raw = load("post_218_interactive.json").replace("\"interactive_tty\"", "null");
    let ev: CaptureEvent = serde_json::from_str(&raw).expect("null context parses as None");
    match ev.payload {
        CapturePayload::Terminal { context, .. } => assert_eq!(context, None),
        other => panic!("expected Terminal, got {other:?}"),
    }
}

/// `context` as a non-string scalar must fail.
#[test]
fn wire_rejects_non_string_context() {
    let raw = load("post_218_interactive.json").replace("\"interactive_tty\"", "42");
    let res: Result<CaptureEvent, _> = serde_json::from_str(&raw);
    assert!(
        res.is_err(),
        "wire must reject numeric context, got {res:?}"
    );
}

/// `deny_unknown_fields` still applies to the Terminal variant — a
/// bogus extra key fails parse. Locks down forward-compat: future field
/// additions must come from this codebase, not from typos by callers.
#[test]
fn wire_rejects_unknown_terminal_field() {
    let raw = load("post_218_interactive.json").replace(
        "\"context\": \"interactive_tty\"",
        "\"context\": \"interactive_tty\", \"extra\": 1",
    );
    let res: Result<CaptureEvent, _> = serde_json::from_str(&raw);
    assert!(
        res.is_err(),
        "deny_unknown_fields must reject extra Terminal keys, got {res:?}"
    );
}

// -------- 3. Property test: round-trip every variant -------------------

fn arb_terminal_context() -> impl Strategy<Value = Option<TerminalContext>> {
    prop_oneof![
        Just(None),
        Just(Some(TerminalContext::InteractiveTty)),
        Just(Some(TerminalContext::NonInteractiveOrStructured)),
    ]
}

proptest! {
    /// Every `Option<TerminalContext>` value round-trips through JSON
    /// without drift. Catches regressions in the snake_case rename or
    /// the `skip_serializing_if` setup.
    #[test]
    fn terminal_payload_round_trips_through_json(
        ctx in arb_terminal_context(),
        exit_code in proptest::option::of(any::<i32>()),
    ) {
        let original = CapturePayload::Terminal {
            command: "cmd".into(),
            exit_code,
            context: ctx,
        };
        let json = serde_json::to_string(&original).expect("ser");
        let parsed: CapturePayload = serde_json::from_str(&json).expect("de");
        prop_assert_eq!(parsed, original);
    }
}
