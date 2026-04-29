//! Integration tests for `RegexExtractor`. Each test maps to an
//! acceptance-criterion bullet from spec §10.5 / §10.2.

#![allow(missing_docs)]

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, ChainRole,
    Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::pipeline::extract::{
    BodyResolution, BodyResolutionError, ExtractInput, ExtractOutput, ExtractorWorker,
    ForgetMatchStrategy, RegexExtractor, ResolvedBody, TruncationReason, UserIngestPayloadKind,
};

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid")
}

fn ulid() -> CaptureEventId {
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FB0").expect("valid ULID")
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

fn cli_event() -> CaptureEvent {
    CaptureEvent {
        event_id: ulid(),
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

fn body_input<'a>(event: &'a CaptureEvent, body: &'a str) -> ExtractInput<'a> {
    let resolved =
        ResolvedBody::from_user_ingest(body, &event.payload, UserIngestPayloadKind::Cli)
            .expect("matching variant");
    ExtractInput {
        event,
        body: BodyResolution::Resolved(resolved),
    }
}

#[tokio::test]
async fn explicit_remember_emits_user_draft() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "remember that I prefer dark mode");
    let res = extractor.extract(&input).await.expect("extract ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(draft) = &res.outputs[0] else {
        panic!("expected draft");
    };
    assert_eq!(draft.body, "I prefer dark mode");
    assert_eq!(draft.trigger_id.as_deref(), Some("remember.preference"));
}

#[tokio::test]
async fn remember_never_routes_to_rule_kind() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "remember never share API keys");
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(draft) = &res.outputs[0] else {
        panic!("expected draft");
    };
    assert_eq!(draft.trigger_id.as_deref(), Some("remember.rule"));
}

#[tokio::test]
async fn forget_emits_substring_intent() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "forget that I mentioned my address");
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert_eq!(intent.match_strategy, ForgetMatchStrategy::Substring);
    assert_eq!(intent.target_text_normalized, "i mentioned my address");
}

#[tokio::test]
async fn compound_utterance_emits_two_outputs() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(
        &event,
        "forget my old address and remember the new one is 1 Main St",
    );
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 2);
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    // Trailing conjunction must be trimmed from the captured target.
    assert_eq!(intent.target_text_normalized, "my old address");
    let ExtractOutput::Draft(draft) = &res.outputs[1] else {
        panic!("expected draft");
    };
    assert_eq!(draft.body, "the new one is 1 Main St");
}

#[tokio::test]
async fn abbreviation_does_not_split_sentence() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "remember that I live in the U.S. and prefer cash");
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(d) = &res.outputs[0] else {
        panic!();
    };
    assert_eq!(d.body, "I live in the U.S. and prefer cash");
}

#[tokio::test]
async fn empty_fallthrough_returns_no_outputs() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "hello world");
    let res = extractor.extract(&input).await.expect("ok");
    assert!(res.outputs.is_empty());
    assert_eq!(res.truncated, TruncationReason::None);
}

#[tokio::test]
async fn body_resolution_failure_surfaces_typed_error() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Failed(BodyResolutionError::HashMismatch {
            expected: "abc".into(),
            got: "def".into(),
        }),
    };
    let err = extractor.extract(&input).await.unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("body resolution failed"));
}

#[tokio::test]
async fn quoted_remember_is_not_extracted() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, r#"the user said "remember that" by mistake"#);
    let res = extractor.extract(&input).await.expect("ok");
    assert!(res.outputs.is_empty());
}

#[tokio::test]
async fn oversize_body_still_extracts_trigger() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let mut body_text = "lorem ipsum. ".repeat(20_000);
    body_text.push_str("forget my old address");
    let input = body_input(&event, &body_text);
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    assert!(matches!(res.outputs[0], ExtractOutput::Forget(_)));
    assert!(matches!(
        res.truncated,
        TruncationReason::BodyTooLarge { .. }
    ));
    assert_eq!(res.llm_eligible_spans.len(), 1);
}

#[tokio::test]
async fn single_period_abbreviation_does_not_fire_forget() {
    // `Dr. Forget at the clinic` must not be treated as a sentence
    // boundary that turns `forget` into a sentence-start trigger.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "Please contact Dr. Forget at the clinic");
    let res = extractor.extract(&input).await.expect("ok");
    assert!(
        res.outputs.is_empty(),
        "Dr. abbreviation must not fire forget; got {:?}",
        res.outputs
    );
}

#[tokio::test]
async fn multi_sentence_trigger_after_period() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "FYI old address is stale. forget my old address");
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    assert!(matches!(res.outputs[0], ExtractOutput::Forget(_)));
}
