//! Integration tests for `RegexExtractor`. Each test maps to an
//! acceptance-criterion bullet from spec §10.5 / §10.2.

#![allow(missing_docs)]

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, ChainRole,
    Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_core::pipeline::extract::regex::{RegexRule, RuleSet};
use cairn_core::pipeline::extract::{
    BodyResolution, BodyResolutionError, Confidence, ExtractBudget, ExtractInput, ExtractOutput,
    ExtractorWorker, ForgetMatchStrategy, KindHint, RegexExtractor, ResolvedBody, TruncationReason,
    UserIngestPayloadKind,
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
    let resolved = ResolvedBody::from_user_ingest(body, &event.payload, UserIngestPayloadKind::Cli)
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
async fn missing_body_for_cli_payload_is_typed_error() {
    use cairn_core::pipeline::extract::ExtractError;
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::NotApplicable,
    };
    let err = extractor.extract(&input).await.unwrap_err();
    assert!(matches!(err, ExtractError::MissingBody { .. }));
}

#[tokio::test]
async fn trigger_after_closing_quote_fires() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, r#"He said "done." forget my address"#);
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1, "expected forget after closing quote");
    assert!(matches!(res.outputs[0], ExtractOutput::Forget(_)));
}

#[tokio::test]
async fn trigger_after_closing_paren_fires() {
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "(see below). remember that I prefer cash");
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(
        res.outputs.len(),
        1,
        "expected remember after closing paren"
    );
    assert!(matches!(res.outputs[0], ExtractOutput::Draft(_)));
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
async fn quoted_punctuation_does_not_truncate_window() {
    // Punctuation inside balanced quotes must not end the phrase
    // window, otherwise the stored draft body is cut off mid-clause.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, r#"remember that she shouted "go!" and prefers tea"#);
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(d) = &res.outputs[0] else {
        panic!("expected draft");
    };
    assert!(
        d.body.contains("prefers tea"),
        "draft body must include text past quoted `!`: got {:?}",
        d.body
    );
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
async fn apostrophes_in_contractions_do_not_swallow_trigger() {
    // `don't forget John's address` must extract the forget intent —
    // contraction/possessive apostrophes must not be paired as a
    // quote span around the trigger.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, "don't worry. forget John's address");
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1, "expected forget output");
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert!(intent.target_text_normalized.contains("john's address"));
}

#[tokio::test]
async fn quoted_forget_target_strips_outer_quotes() {
    // `forget "my old address"` must emit `my old address`, not
    // `"my old address"`, so the resolver matches the stored record.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let input = body_input(&event, r#"forget "my old address""#);
    let res = extractor.extract(&input).await.expect("ok");
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert_eq!(intent.target_text_normalized, "my old address");
}

#[tokio::test]
async fn comma_separated_forget_strips_trailing_separator() {
    // `forget X, and remember Y` and `forget X, remember Y` must emit a
    // forget target without the trailing comma — otherwise the resolver
    // sees `my old address,` and misses the stored memory.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();

    let input = body_input(
        &event,
        "forget my old address, and remember the new one is 1 Main St",
    );
    let res = extractor.extract(&input).await.expect("ok");
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert_eq!(intent.target_text_normalized, "my old address");

    let input = body_input(
        &event,
        "forget my old address, remember the new one is 1 Main St",
    );
    let res = extractor.extract(&input).await.expect("ok");
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert_eq!(intent.target_text_normalized, "my old address");
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
async fn oversize_body_extracts_trigger_and_excludes_covered_span() {
    // The forget clause Phase A captures must not appear in
    // llm_eligible_spans on an oversize body, otherwise the LLM
    // extractor would re-emit a duplicate ForgetIntent for an
    // irreversible operation.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let mut body_text = "lorem ipsum. ".repeat(20_000);
    body_text.push_str("forget my old address");
    let input = body_input(&event, &body_text);
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert!(matches!(
        res.truncated,
        TruncationReason::BodyTooLarge { .. }
    ));
    let covered = intent.source_span;
    for span in &res.llm_eligible_spans {
        assert!(
            span.end <= covered.start || span.start >= covered.end,
            "llm_eligible_spans must not overlap covered forget span: \
             span={span:?} covered={covered:?}"
        );
    }
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

#[tokio::test]
async fn clause_cap_extracts_first_64_and_surfaces_tail_to_llm() {
    // 65 explicit `forget X.` clauses: regex extracts the first 64,
    // remaining triggers must appear in `llm_eligible_spans` (so the
    // chain's LLM extractor can recover them) and `truncated` reports
    // `ClauseCapExceeded`.
    let extractor = RegexExtractor::builtin();
    let event = cli_event();
    let body_text = (0..65)
        .map(|i| format!("forget item {i}."))
        .collect::<Vec<_>>()
        .join(" ");
    let input = body_input(&event, &body_text);
    let res = extractor.extract(&input).await.expect("ok");

    let forgets = res
        .outputs
        .iter()
        .filter(|o| matches!(o, ExtractOutput::Forget(_)))
        .count();
    assert_eq!(forgets, 64, "regex must extract exactly the first 64 clauses");
    assert!(matches!(
        res.truncated,
        TruncationReason::ClauseCapExceeded { processed: 64, .. }
    ));
    // Tail span must reach the body end and start somewhere within the
    // body so the 65th `forget` is recoverable by the LLM extractor.
    let body_len = u32::try_from(body_text.len()).unwrap();
    let tail_present = res
        .llm_eligible_spans
        .iter()
        .any(|s| s.end == body_len && s.start < body_len);
    assert!(
        tail_present,
        "expected llm_eligible_spans tail reaching body end; got {:?}",
        res.llm_eligible_spans
    );
}

#[tokio::test]
async fn hook_body_resolution_failure_does_not_abort_hook_extraction() {
    // PostToolUse is not text-bearing — a transient body-resolution
    // failure must still let the built-in hook rule fire.
    let extractor = RegexExtractor::builtin();
    let event = CaptureEvent {
        event_id: ulid(),
        sensor_id: Identity::parse("snr:local:hook:default:v1").expect("valid"),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1")],
        refs: None,
        payload_hash: hash(),
        payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FB0.json".into(),
        captured_at: ts(),
        payload: CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    };
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Failed(BodyResolutionError::NotFound("missing".into())),
    };
    let res = extractor.extract(&input).await.expect("must not abort");
    assert!(
        !res.outputs.is_empty(),
        "PostToolUse hook rule should still fire; got {:?}",
        res.outputs
    );
}

#[tokio::test]
async fn partial_match_user_rule_suppresses_only_its_bytes() {
    // A high-confidence user TriggerPhrase that matches a narrow prefix
    // of a phrase window must NOT mask trailing bytes in the same
    // window from the LLM extractor — only the matched span gets
    // suppressed.
    let confidence = Confidence::try_from(0.95_f32).expect("valid");
    let kind_hint = KindHint::from(MemoryKind::User);
    let user_rule = RegexRule::TriggerPhrase {
        id: "user.prefix.only".into(),
        // Anchored at sentence start; matches only `remember foo` even
        // when the clause continues. `\bfoo\b` keeps the match narrow.
        pattern: r"(?i)remember foo\b".into(),
        kind_hint,
        confidence,
        capture_group: None,
    };
    // Use an empty rule set + just the user rule so the built-in
    // `remember.*` rule does not pre-empt the user rule via Phase A
    // first-match-wins. This isolates partial-match suppression.
    let rules = RuleSet::from_config(&[user_rule]).expect("compile ok");
    let extractor = RegexExtractor::from_parts(rules, ExtractBudget::regex_default());
    let event = cli_event();
    // The window the prefilter builds for `remember` runs to the next
    // sentence terminator. Trailing text (`and bar baz quux`) must
    // remain LLM-eligible despite the high-confidence prefix match.
    let body_text = "remember foo and bar baz quux extra trailing text";
    let input = body_input(&event, body_text);
    let res = extractor.extract(&input).await.expect("ok");

    let user_match = res
        .outputs
        .iter()
        .find_map(|o| match o {
            ExtractOutput::Draft(d) if d.trigger_id.as_deref() == Some("user.prefix.only") => {
                d.source_span
            }
            _ => None,
        })
        .expect("user rule should fire");

    // The matched span covers only `remember foo` (12 bytes).
    let matched_text = &body_text[user_match.start as usize..user_match.end as usize];
    assert_eq!(matched_text.to_ascii_lowercase(), "remember foo");

    // Trailing bytes after the match must remain LLM-eligible — at
    // least one llm_eligible_span must overlap [match.end, body_end).
    let body_end = u32::try_from(body_text.len()).unwrap();
    let trailing_eligible = res
        .llm_eligible_spans
        .iter()
        .any(|s| s.end > user_match.end && s.start < body_end);
    assert!(
        trailing_eligible,
        "trailing bytes after partial match must stay LLM-eligible; \
         match={user_match:?} spans={:?}",
        res.llm_eligible_spans
    );
}
