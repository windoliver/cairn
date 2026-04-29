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
        BodyResolution::NotApplicable => None,
        BodyResolution::Failed(err) => {
            return Err(ExtractError::BodyResolution {
                event_id: input.event.event_id.as_str().to_owned(),
                source: err.clone(),
            });
        }
    };

    let event = input.event;
    let mut outputs: Vec<ExtractOutput> = Vec::new();
    let mut llm_spans: Vec<TextSpan> = Vec::new();
    let mut truncated = TruncationReason::None;

    // Built-in hook + tool-frame rules are uncapped (first-party).
    run_hook_rules(&rules.builtin_hook, event, &mut outputs, None);
    run_tool_frame_rules(&rules.builtin_tool_frame, event, &mut outputs, None);

    // User hook + tool-frame rules share the user-rule budget with
    // Phase B text rules. Track the per-event user-output count so a
    // misconfigured config cannot amplify a single hot event into
    // unbounded writes. Cap is `outputs.len()` baseline + max_user_drafts.
    let max_user_drafts = usize::from(budget.max_drafts);
    let user_cap = outputs.len() + max_user_drafts;
    let hook_capped = run_hook_rules(&rules.user_hook, event, &mut outputs, Some(user_cap));
    let tool_capped =
        run_tool_frame_rules(&rules.user_tool_frame, event, &mut outputs, Some(user_cap));
    if hook_capped || tool_capped {
        truncated = TruncationReason::MaxDrafts;
    }

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
            truncated = TruncationReason::BodyTooLarge {
                body_len: u32::try_from(text.len()).unwrap_or(u32::MAX),
            };
        } else {
            // Built-in outputs are first-party and always preserved. Only
            // user-rule additions face `budget.max_drafts`, so explicit
            // built-in triggers like `remember`/`forget` are never lost
            // to back-pressure.
            let builtin_count = outputs.len();
            let wall_b = Instant::now();
            let max_user_drafts = usize::from(budget.max_drafts);
            'phase_b: for window in &windows {
                for rule in &rules.user_text {
                    let elapsed_ms = wall_b.elapsed().as_millis() as u32;
                    if elapsed_ms > budget.max_wall_ms {
                        if outputs.len() == builtin_count {
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
                    let user_added = outputs.len() - builtin_count;
                    if added && user_added >= max_user_drafts {
                        tracing::warn!(
                            worker = "regex",
                            max_user_drafts,
                            "reached max_drafts cap on user rules"
                        );
                        truncated = TruncationReason::MaxDrafts;
                        break 'phase_b;
                    }
                    if added {
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

        compute_llm_eligible_spans(text, &windows, &covered, &mut llm_spans);

        if body_too_large {
            llm_spans.clear();
            llm_spans.push(TextSpan::new(
                0,
                u32::try_from(text.len()).unwrap_or(u32::MAX),
            ));
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
    let slice = &body[window.span.start as usize..window.span.end as usize];
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
                outputs.push(ExtractOutput::Draft(MemoryDraft {
                    kind_hint: kind_hint.clone(),
                    body: group_text.to_owned(),
                    confidence: *confidence,
                    source_event: event.event_id.clone(),
                    source_span: Some(window.span),
                    trigger_id: Some(rule.id.clone()),
                }));
                if confidence.as_f32() >= CONFIDENCE_GATE_FOR_SUPPRESSION {
                    covered_high_conf.push(window.span);
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
                outputs.push(ExtractOutput::Forget(ForgetIntent {
                    target_text_normalized: normalize_target(target),
                    match_strategy: *match_strategy,
                    kind_filter: None,
                    source_span: window.span,
                    confidence: *confidence,
                    source_event: event.event_id.clone(),
                    trigger_id: rule.id.clone(),
                }));
                if confidence.as_f32() >= CONFIDENCE_GATE_FOR_SUPPRESSION {
                    covered_high_conf.push(window.span);
                }
            }
        }
        _ => {}
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
    out.trim().to_owned()
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
    // Drop spans that a high-confidence output has fully covered.
    let eligible: Vec<TextSpan> = eligible
        .into_iter()
        .filter(|s| {
            !covered_high_conf
                .iter()
                .any(|c| c.overlaps(*s) && c.start <= s.start && c.end >= s.end)
        })
        .collect();
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
        };
        let result = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .expect("ok");
        assert!(result.outputs.is_empty());
        assert!(matches!(result.truncated, TruncationReason::None));
        assert!(result.llm_eligible_spans.is_empty());
    }

    #[tokio::test]
    async fn body_resolution_failure_propagates() {
        use crate::pipeline::extract::body::BodyResolutionError;
        let rules = RuleSet::empty();
        let prefilter = TriggerPrefilter::new();
        let budget = ExtractBudget::regex_default();
        let event = make_event(CapturePayload::Hook {
            hook_name: "PostToolUse".to_owned(),
            tool_name: None,
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Failed(BodyResolutionError::NotFound("missing".to_owned())),
        };
        let err = dispatch(&rules, &prefilter, &budget, &input)
            .await
            .unwrap_err();
        assert!(matches!(err, ExtractError::BodyResolution { .. }));
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
        let resolved = ResolvedBody::from_hook_utterance(body_text, &event.payload, "UserPromptSubmit")
            .expect("matching hook");
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Resolved(resolved),
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
        };
        let event = make_event(CapturePayload::Hook {
            hook_name: "PostToolUse".to_owned(),
            tool_name: None,
        });
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
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
