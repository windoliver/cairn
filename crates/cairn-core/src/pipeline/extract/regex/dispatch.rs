//! Phase 0/A/B dispatch + budget enforcement + `llm_eligible_spans`.
//! Spec §6.

use std::time::Instant;

use crate::domain::{CaptureEvent, CapturePayload};

use super::super::{
    BodyResolution, CONFIDENCE_GATE_FOR_SUPPRESSION, ExtractBudget, ExtractError, ExtractInput,
    ExtractOutput, ExtractResult, ForgetIntent, MAX_BODY_LEN_FOR_REGEX, MAX_PHASE_A_WALL_MS,
    MAX_PHASE_A_WALL_MS_LARGE, MAX_PHRASE_WINDOWS, MemoryDraft, TextSpan, TruncationReason,
};
use super::prefilter::{PhraseWindow, TriggerPrefilter, build_phrase_windows};
use super::rule::{CompiledRule, CompiledRuleKind, RuleSet, ToolFrameFamily};

#[allow(clippy::too_many_lines)] // dispatch is the orchestrator; splitting it obscures the phase boundaries
#[allow(clippy::cast_possible_truncation)] // ms timing and body lengths are bounded
#[allow(clippy::unused_async)] // async matches ExtractorWorker::extract signature (Task 12)
pub(crate) async fn dispatch(
    rules: &RuleSet,
    prefilter: &TriggerPrefilter,
    budget: &ExtractBudget,
    input: &ExtractInput<'_>,
) -> Result<ExtractResult, ExtractError> {
    let body_text: Option<&str> = match &input.body {
        BodyResolution::Resolved(rb) => {
            let _ = rb.source();
            Some(rb.text())
        }
        BodyResolution::NotApplicable => {
            // Reject NotApplicable for payload variants that always
            // carry user text. Otherwise a caller bug (or version skew)
            // turns explicit user intents into silent empty results.
            if let Some(variant) = text_bearing_payload_variant(&input.event.payload) {
                return Err(ExtractError::MissingBody {
                    event_id: input.event.event_id.as_str().to_owned(),
                    payload_variant: variant,
                });
            }
            None
        }
        BodyResolution::Failed(err) => {
            // Text-bearing payloads must fail loudly — losing the body
            // means losing the user intent. Non-text payloads (Hook
            // tool-output, Terminal, Ide, …) still have structured
            // hook/tool-frame data the dispatcher can extract from, so
            // a transient body-resolution failure does not abort them.
            if text_bearing_payload_variant(&input.event.payload).is_some() {
                return Err(ExtractError::BodyResolution {
                    event_id: input.event.event_id.as_str().to_owned(),
                    source: err.clone(),
                });
            }
            tracing::warn!(
                worker = "regex",
                error = %err,
                "body resolution failed for non-text payload; continuing without body"
            );
            None
        }
    };

    let event = input.event;
    let mut outputs: Vec<ExtractOutput> = Vec::new();
    let mut llm_spans: Vec<TextSpan> = Vec::new();
    let mut truncated = TruncationReason::None;

    // Built-in hook + tool-frame rules are uncapped (first-party).
    run_hook_rules(&rules.builtin_hook, event, &mut outputs, None);
    run_tool_frame_rules(&rules.builtin_tool_frame, event, &mut outputs, None);

    // Single user-output budget shared across user hook, user tool-frame,
    // and user text rules. Tracks user-rule emissions explicitly so it
    // is robust against intervening built-in (Phase A) text outputs.
    let max_user_drafts = usize::from(budget.max_drafts);
    let baseline_pre_user_a = outputs.len();
    let user_a_cap = baseline_pre_user_a + max_user_drafts;
    if run_hook_rules(&rules.user_hook, event, &mut outputs, Some(user_a_cap))
        || run_tool_frame_rules(
            &rules.user_tool_frame,
            event,
            &mut outputs,
            Some(user_a_cap),
        )
    {
        truncated = TruncationReason::MaxDrafts;
    }
    let user_count_after_a = outputs.len() - baseline_pre_user_a;

    if let Some(text) = body_text {
        let scan = prefilter.scan(text);
        let windows = build_phrase_windows(text, &scan);

        let body_too_large = text.len() > MAX_BODY_LEN_FOR_REGEX;
        if body_too_large {
            tracing::warn!(
                body_len = text.len(),
                "body exceeds MAX_BODY_LEN_FOR_REGEX, skipping Phase B user rules"
            );
        }

        let wall_a = Instant::now();
        let mut covered: Vec<TextSpan> = Vec::new();
        for window in &windows {
            run_text_rules_first_match_wins(
                &rules.builtin_text,
                event,
                text,
                window,
                &mut outputs,
                &mut covered,
            );
        }
        let phase_a_ms = wall_a.elapsed().as_millis() as u32;
        let limit_a = if body_too_large {
            MAX_PHASE_A_WALL_MS_LARGE
        } else {
            MAX_PHASE_A_WALL_MS
        };
        if phase_a_ms > limit_a {
            tracing::warn!(
                worker = "regex",
                phase_a_ms,
                limit_a,
                "Phase A exceeded its wall-clock observability rail"
            );
        }

        if body_too_large {
            // Preserve an earlier `MaxDrafts` signal: if user
            // hook/tool-frame already exhausted the user-output budget
            // and dropped outputs, that data-loss reason must remain
            // visible rather than be silently replaced by the body-size
            // skip.
            if matches!(truncated, TruncationReason::None) {
                truncated = TruncationReason::BodyTooLarge {
                    body_len: u32::try_from(text.len()).unwrap_or(u32::MAX),
                };
            }
        } else {
            // Built-in (Phase A) outputs are first-party and not counted
            // against `max_user_drafts`. Phase B continues consuming the
            // user-output budget that hook/tool-frame already partially
            // drew from.
            let mut user_count = user_count_after_a;
            let wall_b = Instant::now();
            'phase_b: for window in &windows {
                if user_count >= max_user_drafts {
                    truncated = TruncationReason::MaxDrafts;
                    break 'phase_b;
                }
                for rule in &rules.user_text {
                    let elapsed_ms = wall_b.elapsed().as_millis() as u32;
                    if elapsed_ms > budget.max_wall_ms {
                        // Preserve any earlier outputs (Phase A built-ins,
                        // user hook/tool-frame, prior Phase B matches).
                        // Only return a hard error when no extractor
                        // output exists at all — built-in triggers must
                        // never be silently dropped under back-pressure.
                        if outputs.is_empty() {
                            return Err(ExtractError::BudgetExceeded {
                                worker: "regex",
                                elapsed_ms,
                            });
                        }
                        tracing::warn!(
                            worker = "regex",
                            elapsed_ms,
                            budget = budget.max_wall_ms,
                            "Phase B exceeded max_wall_ms"
                        );
                        truncated = TruncationReason::MaxWallMs { elapsed_ms };
                        break 'phase_b;
                    }
                    let before = outputs.len();
                    apply_text_rule(rule, event, text, window, &mut outputs, &mut covered);
                    let added = outputs.len() > before;
                    if added {
                        user_count += 1;
                        if user_count >= max_user_drafts {
                            tracing::warn!(
                                worker = "regex",
                                max_user_drafts,
                                "reached max_drafts cap on user rules"
                            );
                            truncated = TruncationReason::MaxDrafts;
                            break 'phase_b;
                        }
                        break;
                    }
                }
            }
        }

        if let Some(omitted_start) = scan.first_omitted_start {
            // Tail starts at the first omitted hit so triggers past the
            // window cap are always represented in `llm_eligible_spans`.
            let body_end = u32::try_from(text.len()).unwrap_or(u32::MAX);
            if omitted_start < body_end {
                llm_spans.push(TextSpan::new(omitted_start, body_end));
            }
            if matches!(truncated, TruncationReason::None) {
                truncated = TruncationReason::ClauseCapExceeded {
                    processed: u8::try_from(MAX_PHRASE_WINDOWS).unwrap_or(u8::MAX),
                    body_len: u32::try_from(text.len()).unwrap_or(u32::MAX),
                };
            }
        }

        if body_too_large {
            // The body skipped Phase B but Phase A still ran, so we
            // hand the LLM extractor the full body MINUS any high-
            // confidence regex coverage. Replacing with `0..body_len`
            // would re-expose explicit `remember`/`forget` clauses to
            // the LLM and risk duplicate or conflicting outputs.
            let body_len = u32::try_from(text.len()).unwrap_or(u32::MAX);
            let mut full = vec![TextSpan::new(0, body_len)];
            subtract_covered(&mut full, &covered);
            llm_spans.extend(full);
        } else {
            compute_llm_eligible_spans(text, &windows, &covered, &mut llm_spans);
        }
    }

    // Built-in outputs are uncapped: explicit user triggers must never
    // be silently dropped under back-pressure. Only Phase B (user-rule)
    // additions face `budget.max_drafts`, enforced inline above.

    // Normalise llm_eligible_spans once at the end: sort by start, then
    // merge overlapping/adjacent spans so callers receive a disjoint
    // span list. The clause-cap fallback and `compute_llm_eligible_spans`
    // both push into the same buffer, so without this final pass we can
    // hand the LLM extractor duplicate or overlapping ranges.
    normalise_spans(&mut llm_spans);

    Ok(ExtractResult {
        outputs,
        truncated,
        llm_eligible_spans: llm_spans,
    })
}

/// Returns the payload variant name when the event's payload always
/// carries extractable user text. Hook payloads delegate to the single
/// trust-boundary helper [`super::super::is_user_utterance_hook`] so a
/// new entry to that allowlist automatically flows here — no duplicated
/// list to drift.
fn text_bearing_payload_variant(p: &CapturePayload) -> Option<&'static str> {
    match p {
        CapturePayload::Cli { .. } => Some("CapturePayload::Cli"),
        CapturePayload::Mcp { .. } => Some("CapturePayload::Mcp"),
        CapturePayload::Hook { hook_name, .. }
            if super::super::is_user_utterance_hook(hook_name) =>
        {
            Some("CapturePayload::Hook (user utterance)")
        }
        // Proactive (Mode C) payloads will be text-bearing once the
        // envelope grows a hashable message-body field and ResolvedBody
        // gains a `from_proactive_message` constructor. Until then they
        // are explicitly NOT classified as text-bearing here so dispatch
        // does not fail every Mode C event with `MissingBody`. Tracked
        // for the proactive-body follow-up issue.
        _ => None,
    }
}

/// Subtract `covered` spans from `spans` in place. Each input span is
/// split around any fully or partially covering range, dropping bytes
/// that a high-confidence regex output already claims.
fn subtract_covered(spans: &mut Vec<TextSpan>, covered: &[TextSpan]) {
    if covered.is_empty() {
        return;
    }
    let mut sorted = covered.to_vec();
    sorted.sort_by_key(|s| s.start);
    let mut out: Vec<TextSpan> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        let mut cur_start = s.start;
        let cur_end = s.end;
        for c in &sorted {
            if c.end <= cur_start || c.start >= cur_end {
                continue;
            }
            if c.start > cur_start {
                out.push(TextSpan::new(cur_start, c.start));
            }
            cur_start = cur_start.max(c.end);
            if cur_start >= cur_end {
                break;
            }
        }
        if cur_start < cur_end {
            out.push(TextSpan::new(cur_start, cur_end));
        }
    }
    *spans = out;
}

fn normalise_spans(spans: &mut Vec<TextSpan>) {
    if spans.len() < 2 {
        return;
    }
    spans.sort_by_key(|s| s.start);
    let mut merged: Vec<TextSpan> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        if let Some(last) = merged.last_mut()
            && s.start <= last.end
        {
            last.end = last.end.max(s.end);
            continue;
        }
        merged.push(s);
    }
    *spans = merged;
}

/// Apply hook rules. If `cap` is `Some(n)`, stops pushing once
/// `outputs.len() >= n` and returns `true` (cap hit); otherwise returns
/// `false`.
fn run_hook_rules(
    rules: &[CompiledRule],
    event: &CaptureEvent,
    outputs: &mut Vec<ExtractOutput>,
    cap: Option<usize>,
) -> bool {
    let CapturePayload::Hook {
        hook_name,
        tool_name,
        ..
    } = &event.payload
    else {
        return false;
    };
    for rule in rules {
        let CompiledRuleKind::HookEvent {
            hook_name: rule_hook,
            tool_name: rule_tool,
            kind_hint,
            confidence,
        } = &rule.kind
        else {
            continue;
        };
        if rule_hook != hook_name {
            continue;
        }
        if rule_tool
            .as_ref()
            .is_some_and(|t| Some(t) != tool_name.as_ref())
        {
            continue;
        }
        if let Some(limit) = cap
            && outputs.len() >= limit
        {
            return true;
        }
        outputs.push(ExtractOutput::Draft(MemoryDraft {
            kind_hint: kind_hint.clone(),
            body: format!("hook:{hook_name}"),
            confidence: *confidence,
            source_event: event.event_id.clone(),
            source_span: None,
            trigger_id: Some(rule.id.clone()),
        }));
    }
    false
}

/// Apply tool-frame rules. If `cap` is `Some(n)`, stops pushing once
/// `outputs.len() >= n` and returns `true` (cap hit); otherwise returns
/// `false`.
fn run_tool_frame_rules(
    rules: &[CompiledRule],
    event: &CaptureEvent,
    outputs: &mut Vec<ExtractOutput>,
    cap: Option<usize>,
) -> bool {
    for rule in rules {
        let CompiledRuleKind::ToolFrame {
            family,
            kind_hint,
            confidence,
        } = &rule.kind
        else {
            continue;
        };
        let fired = match (family, &event.payload) {
            (
                ToolFrameFamily::Terminal {
                    exit_code_nonzero: true,
                },
                CapturePayload::Terminal {
                    exit_code: Some(code),
                    ..
                },
            ) => *code != 0,
            (
                ToolFrameFamily::Terminal {
                    exit_code_nonzero: false,
                },
                CapturePayload::Terminal { .. },
            ) => true,
            (ToolFrameFamily::Ide { event_kind }, CapturePayload::Ide { event_kind: ek, .. }) => {
                event_kind == ek
            }
            _ => false,
        };
        if fired {
            if let Some(limit) = cap
                && outputs.len() >= limit
            {
                return true;
            }
            outputs.push(ExtractOutput::Draft(MemoryDraft {
                kind_hint: kind_hint.clone(),
                body: format!("tool-frame:{}", rule.id),
                confidence: *confidence,
                source_event: event.event_id.clone(),
                source_span: None,
                trigger_id: Some(rule.id.clone()),
            }));
        }
    }
    false
}

fn run_text_rules_first_match_wins(
    rules: &[CompiledRule],
    event: &CaptureEvent,
    body: &str,
    window: &PhraseWindow,
    outputs: &mut Vec<ExtractOutput>,
    covered_high_conf: &mut Vec<TextSpan>,
) {
    for rule in rules {
        let before = outputs.len();
        apply_text_rule(rule, event, body, window, outputs, covered_high_conf);
        if outputs.len() > before {
            return;
        }
    }
}

fn apply_text_rule(
    rule: &CompiledRule,
    event: &CaptureEvent,
    body: &str,
    window: &PhraseWindow,
    outputs: &mut Vec<ExtractOutput>,
    covered_high_conf: &mut Vec<TextSpan>,
) {
    let win_start = window.span.start as usize;
    let slice = &body[win_start..window.span.end as usize];
    match &rule.kind {
        CompiledRuleKind::TriggerPhrase {
            re,
            kind_hint,
            confidence,
            capture_group,
        } => {
            if let Some(caps) = re.captures(slice) {
                let group_text = caps
                    .get(*capture_group as usize)
                    .map_or(slice, |m| m.as_str());
                // Use the full match span (group 0) for suppression so
                // a narrow user rule that only matches a prefix does
                // not silently mask trailing text in the same window
                // from the LLM extractor.
                let match_span = match_span_in_body(&caps, win_start, window.span);
                outputs.push(ExtractOutput::Draft(MemoryDraft {
                    kind_hint: kind_hint.clone(),
                    body: group_text.to_owned(),
                    confidence: *confidence,
                    source_event: event.event_id.clone(),
                    source_span: Some(match_span),
                    trigger_id: Some(rule.id.clone()),
                }));
                if confidence.as_f32() >= CONFIDENCE_GATE_FOR_SUPPRESSION {
                    covered_high_conf.push(match_span);
                }
            }
        }
        CompiledRuleKind::ForgetPhrase {
            re,
            target_group,
            confidence,
            match_strategy,
        } => {
            if let Some(caps) = re.captures(slice) {
                let target = caps.get(*target_group as usize).map_or("", |m| m.as_str());
                let match_span = match_span_in_body(&caps, win_start, window.span);
                outputs.push(ExtractOutput::Forget(ForgetIntent {
                    target_text_normalized: normalize_target(target),
                    match_strategy: *match_strategy,
                    kind_filter: None,
                    source_span: match_span,
                    confidence: *confidence,
                    source_event: event.event_id.clone(),
                    trigger_id: rule.id.clone(),
                }));
                if confidence.as_f32() >= CONFIDENCE_GATE_FOR_SUPPRESSION {
                    covered_high_conf.push(match_span);
                }
            }
        }
        _ => {}
    }
}

/// Translate the regex group-0 match (which is in `slice` coordinates,
/// i.e. relative to `win_start`) into absolute body coordinates. Falls
/// back to `window_span` if the regex backend returns no group 0.
fn match_span_in_body(
    caps: &::regex::Captures<'_>,
    win_start: usize,
    window_span: TextSpan,
) -> TextSpan {
    if let Some(m) = caps.get(0) {
        let abs_start = u32::try_from(win_start + m.start()).unwrap_or(u32::MAX);
        let abs_end = u32::try_from(win_start + m.end()).unwrap_or(u32::MAX);
        TextSpan::new(abs_start, abs_end)
    } else {
        window_span
    }
}

fn normalize_target(s: &str) -> String {
    // Unicode-aware lowercase + whitespace collapse. ASCII-only folding
    // would silently mismatch on non-ASCII text (e.g., `Åsa` vs `åsa`),
    // turning a privacy-sensitive forget request into a locale-dependent
    // miss. Must stay aligned with the delete resolver's body
    // normalisation.
    let mut out = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_was_space = false;
        }
    }
    // Trim trailing clause separators (commas, semicolons) plus
    // whitespace. Without this, windows like `forget my old address,`
    // emit `my old address,` and miss the stored record on exact
    // selectors.
    let trimmed = out.trim_matches(|c: char| c.is_whitespace() || c == ',' || c == ';');
    // Strip a single balanced outer quote pair so `forget "my old address"`
    // emits `my old address`, not `"my old address"`. Without this the
    // resolver compares a quoted selector against unquoted record bodies
    // and silently misses the stored memory.
    let unquoted = strip_balanced_outer_quotes(trimmed);
    unquoted.to_owned()
}

fn strip_balanced_outer_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return s;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if (first == b'"' || first == b'\'' || first == b'`') && first == last {
        // ASCII single-byte quotes only — safe to slice on byte indices.
        return &s[1..s.len() - 1];
    }
    s
}

fn compute_llm_eligible_spans(
    body: &str,
    windows: &[PhraseWindow],
    covered_high_conf: &[TextSpan],
    out: &mut Vec<TextSpan>,
) {
    // Seed eligible spans from the phrase windows themselves, then add
    // any uncovered inter-window gaps and the tail.
    let mut eligible: Vec<TextSpan> = windows.iter().map(|w| w.span).collect();
    let mut last_end: u32 = 0;
    for w in windows {
        if w.span.start > last_end {
            eligible.push(TextSpan::new(last_end, w.span.start));
        }
        last_end = last_end.max(w.span.end);
    }
    let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    if last_end < body_len {
        eligible.push(TextSpan::new(last_end, body_len));
    }
    // Carve high-confidence covered subspans out of each eligible
    // span instead of only dropping fully-covered windows. This keeps
    // the suppression contract honest for narrow user rules: a
    // high-confidence prefix match still removes its own bytes, but
    // unrelated trailing text in the same clause stays LLM-eligible.
    let mut eligible_vec = eligible;
    subtract_covered(&mut eligible_vec, covered_high_conf);
    let eligible = eligible_vec;
    // Sort and merge overlapping/adjacent spans.
    let mut sorted = eligible;
    sorted.sort_by_key(|s| s.start);
    let mut merged: Vec<TextSpan> = Vec::new();
    for s in sorted {
        match merged.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    out.extend(merged);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ActorChainEntry, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, ChainRole,
        Identity, PayloadHash, Rfc3339Timestamp,
    };
    use crate::pipeline::extract::{BodyResolution, ExtractBudget, body::ResolvedBody};

    fn make_event(payload: CapturePayload) -> CaptureEvent {
        let source_family = payload.source_family();
        let ts = Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").expect("valid ts");
        let sensor = Identity::parse("snr:local:hook:test:v1").expect("valid sensor");
        CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid"),
            sensor_id: sensor.clone(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor,
                at: ts.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess-test".into()),
                turn_id: None,
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(format!("sha256:{}", "ab".repeat(32)))
                .expect("valid hash"),
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
            captured_at: ts,
            payload,
            source_family,
        }
    }

    #[tokio::test]
    async fn empty_ruleset_no_body_returns_empty() {
        let rules = RuleSet::empty();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Hook {
            hook_name: "SessionStart".to_owned(),
            tool_name: None,
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("ok");
        assert!(result.outputs.is_empty());
        assert!(matches!(result.truncated, TruncationReason::None));
        assert!(result.llm_eligible_spans.is_empty());
    }

    #[tokio::test]
    async fn body_resolution_failure_propagates_for_text_payload() {
        use crate::pipeline::extract::body::BodyResolutionError;
        let rules = RuleSet::empty();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Cli {
            kind_hint: "user".into(),
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Failed(BodyResolutionError::NotFound("missing".to_owned())),
            eligible_spans: vec![],
        };
        let err = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .unwrap_err();
        assert!(matches!(err, ExtractError::BodyResolution { .. }));
    }

    #[tokio::test]
    async fn proactive_payload_runs_hook_tool_frame_rules_only() {
        // Until the wire envelope grows a hashable message-body field
        // and ResolvedBody gains a proactive constructor, proactive
        // events run hook/tool-frame rules only. They must NOT fail
        // every event closed (would break Mode C traffic) and must
        // NOT silently absorb a bogus Resolved text body (no
        // constructor to verify provenance).
        let rules = RuleSet::builtin();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Proactive {
            kind: "feedback".into(),
            rationale: "test".into(),
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("proactive event must not hard-fail");
        assert!(matches!(result.truncated, TruncationReason::None));
    }

    #[tokio::test]
    async fn body_resolution_failure_does_not_abort_non_text_payload() {
        // PostToolUse hook does not need a text body — a transient
        // resolver error must not suppress hook/tool-frame extraction.
        use crate::pipeline::extract::body::BodyResolutionError;
        let rules = RuleSet::builtin();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Hook {
            hook_name: "PostToolUse".to_owned(),
            tool_name: None,
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Failed(BodyResolutionError::NotFound("missing".to_owned())),
            eligible_spans: vec![],
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("non-text payload must continue");
        assert!(
            !result.outputs.is_empty(),
            "PostToolUse hook should still fire"
        );
    }

    #[tokio::test]
    async fn builtin_hook_rule_fires() {
        let rules = RuleSet::builtin();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Hook {
            hook_name: "PostToolUse".to_owned(),
            tool_name: None,
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("ok");
        assert!(!result.outputs.is_empty(), "PostToolUse hook should fire");
    }

    #[tokio::test]
    async fn trigger_phrase_produces_draft() {
        let rules = RuleSet::builtin();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Hook {
            hook_name: "UserPromptSubmit".to_owned(),
            tool_name: None,
        });
        let body_text = "remember that I prefer dark mode";
        let resolved =
            ResolvedBody::from_hook_utterance(body_text, &event.payload, "UserPromptSubmit")
                .expect("matching hook");
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Resolved(resolved),
            eligible_spans: vec![],
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("ok");
        let has_draft = result.outputs.iter().any(|o| {
            if let ExtractOutput::Draft(d) = o {
                d.body.contains("prefer dark mode")
            } else {
                false
            }
        });
        assert!(has_draft, "expected draft from trigger phrase");
    }

    #[test]
    fn normalize_target_lowercases_and_collapses_spaces() {
        assert_eq!(normalize_target("  Hello   World  "), "hello world");
        assert_eq!(normalize_target("ABC"), "abc");
        assert_eq!(normalize_target(""), "");
    }

    #[test]
    fn normalize_target_handles_non_ascii_lowercase() {
        // Unicode-aware folding: Åsa, ÄPFEL, İ all need to fold.
        assert_eq!(normalize_target("Åsa's address"), "åsa's address");
        assert_eq!(normalize_target("ÄPFEL"), "äpfel");
        assert_eq!(normalize_target("CAFÉ"), "café");
    }

    #[tokio::test]
    async fn builtin_outputs_are_uncapped() {
        // A body with many independent built-in trigger clauses must
        // produce all matching outputs even when max_drafts is small.
        // Built-in extractor outputs are first-party and never dropped.
        let rules = RuleSet::builtin();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget {
            max_wall_ms: 100,
            max_drafts: 3,
            max_prompt_bytes: None,
            max_prompt_tokens: None,
            max_response_tokens: None,
        };
        let body_text = (0..20)
            .map(|i| format!("remember thing {i}"))
            .collect::<Vec<_>>()
            .join(". ");
        let event = make_event(CapturePayload::Cli {
            kind_hint: "user".into(),
        });
        let resolved = ResolvedBody::from_user_ingest(
            &body_text,
            &event.payload,
            crate::pipeline::extract::UserIngestPayloadKind::Cli,
        )
        .expect("matching variant");
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Resolved(resolved),
            eligible_spans: vec![],
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("ok");
        // All 20 built-in matches survive even though max_drafts=3.
        assert_eq!(result.outputs.len(), 20);
        assert_eq!(result.truncated, TruncationReason::None);
    }

    #[tokio::test]
    async fn user_hook_rules_respect_max_drafts_cap() {
        use crate::domain::taxonomy::MemoryKind;
        use crate::pipeline::extract::regex::rule::{RegexRule, RuleSet};
        use crate::pipeline::extract::{Confidence, KindHint};

        // 20 user hook rules all matching the same hook; max_drafts=3 ⇒
        // only 3 user outputs emitted, truncation flagged MaxDrafts.
        let confidence = Confidence::try_from(0.5_f32).expect("valid");
        let kind_hint = KindHint::from(MemoryKind::Feedback);
        let mut rules = Vec::new();
        for i in 0..20u32 {
            rules.push(RegexRule::HookEvent {
                id: format!("user.hook.{i}"),
                hook_name: "PostToolUse".into(),
                tool_name: None,
                kind_hint: kind_hint.clone(),
                confidence,
            });
        }
        let ruleset = RuleSet::from_config(&rules).expect("compile ok");
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget {
            max_wall_ms: 100,
            max_drafts: 3,
            max_prompt_bytes: None,
            max_prompt_tokens: None,
            max_response_tokens: None,
        };
        let event = make_event(CapturePayload::Hook {
            hook_name: "PostToolUse".to_owned(),
            tool_name: None,
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let result = dispatch(&ruleset, &prefilter, &budget, &input)
            .await
            .expect("ok");
        assert_eq!(
            result.outputs.len(),
            3,
            "user hook rules must respect max_drafts"
        );
        assert!(matches!(result.truncated, TruncationReason::MaxDrafts));
    }

    #[tokio::test]
    async fn user_hook_and_text_rules_share_max_drafts_budget() {
        // A mixed-family event: 2 matching user hook rules + a user text
        // rule that fires once. With max_drafts=3 the total user output
        // count must be capped at 3 across hook+tool+text.
        use crate::domain::taxonomy::MemoryKind;
        use crate::pipeline::extract::regex::rule::{RegexRule, RuleSet};
        use crate::pipeline::extract::{Confidence, KindHint};

        let confidence = Confidence::try_from(0.5_f32).expect("valid");
        let kind_hint = KindHint::from(MemoryKind::Feedback);
        let mut rules = Vec::new();
        for i in 0..5u32 {
            rules.push(RegexRule::HookEvent {
                id: format!("user.hook.{i}"),
                hook_name: "UserPromptSubmit".into(),
                tool_name: None,
                kind_hint: kind_hint.clone(),
                confidence,
            });
        }
        for i in 0..5u32 {
            rules.push(RegexRule::TriggerPhrase {
                id: format!("user.text.{i}"),
                pattern: r"(?i)\bnoteworthy\b".into(),
                kind_hint: kind_hint.clone(),
                confidence,
                capture_group: None,
            });
        }
        let ruleset = RuleSet::from_config(&rules).expect("compile ok");
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget {
            max_wall_ms: 100,
            max_drafts: 3,
            max_prompt_bytes: None,
            max_prompt_tokens: None,
            max_response_tokens: None,
        };
        let event = make_event(CapturePayload::Hook {
            hook_name: "UserPromptSubmit".to_owned(),
            tool_name: None,
        });
        let body_text = "this is noteworthy. also noteworthy. and noteworthy too.";
        let resolved =
            ResolvedBody::from_hook_utterance(body_text, &event.payload, "UserPromptSubmit")
                .expect("matching hook");
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Resolved(resolved),
            eligible_spans: vec![],
        };
        let result = dispatch(&ruleset, &prefilter, &budget, &input)
            .await
            .expect("ok");
        assert!(
            result.outputs.len() <= 3,
            "user rule outputs across hook+text must respect max_drafts; got {}",
            result.outputs.len()
        );
        assert!(matches!(result.truncated, TruncationReason::MaxDrafts));
    }

    #[test]
    fn normalise_spans_merges_overlaps_and_dedupes() {
        let mut spans = vec![
            TextSpan::new(10, 20),
            TextSpan::new(0, 5),
            TextSpan::new(15, 25),
            TextSpan::new(5, 10),
        ];
        normalise_spans(&mut spans);
        assert_eq!(spans, vec![TextSpan::new(0, 25)]);
    }
}
