//! `LLMExtractor` and supporting modules. See spec §4–§5 of
//! `docs/superpowers/specs/2026-05-01-issue-74-llm-extractor-design.md`.

pub mod parse;
pub mod prompt;
pub mod schema;

pub use parse::ParsedResponse;
pub use prompt::Region;
pub use schema::SCHEMA_KIND_ENUM;

use std::sync::Arc;

use crate::contract::llm_provider::{CompletionOutput, CompletionRequest, LLMProvider, LlmError};
use crate::pipeline::extract::body::BodyResolution;
use crate::pipeline::extract::{
    ExtractBudget, ExtractError, ExtractInput, ExtractOutput, ExtractResult, ExtractorWorker,
    TruncationReason, WorkerRole,
};
use crate::pipeline::filter::fence::fence;

use crate::pipeline::filter::fence::FenceMark;
use parse::parse_response;
use prompt::{Region as PromptRegion, build_regions, render_prompt};

/// LLM-backed extractor; spec §4.2.
pub struct LLMExtractor {
    provider: Arc<dyn LLMProvider>,
    budget: ExtractBudget,
}

impl LLMExtractor {
    /// Construct with the LLM-default budget.
    pub fn new(provider: Arc<dyn LLMProvider>) -> Self {
        Self {
            provider,
            budget: ExtractBudget::llm_default(),
        }
    }

    /// Override the budget.
    #[must_use]
    pub fn with_budget(mut self, budget: ExtractBudget) -> Self {
        self.budget = budget;
        self
    }
}

/// Helper: empty `ExtractResult` with no truncation.
fn empty_result() -> ExtractResult {
    ExtractResult {
        outputs: vec![],
        discards: vec![],
        truncated: TruncationReason::None,
        llm_eligible_spans: vec![],
    }
}

/// Helper: empty `ExtractResult` with a truncation reason.
fn empty_with_truncation(t: TruncationReason) -> ExtractResult {
    ExtractResult {
        truncated: t,
        ..empty_result()
    }
}

/// Map an `LlmError` discriminant to a stable string code for metrics /
/// `ExtractError::Provider.code`.
fn err_code(e: &LlmError) -> &'static str {
    match e {
        LlmError::NotConfigured { .. } => "not_configured",
        LlmError::ProviderUnreachable { .. } => "unreachable",
        LlmError::AuthDenied => "auth_denied",
        LlmError::CapabilityMissing { .. } => "capability_missing",
        LlmError::InvalidJsonOutput { .. } => "invalid_json_output",
        LlmError::BudgetExceeded => "budget_exceeded",
    }
}

/// Outcome of a single `run_with_retry` call.
enum InnerOutcome {
    /// The model returned a parseable, valid response.
    Parsed(ParsedResponse),
    /// Soft-empty: provider is not configured or signalled budget exceeded.
    SoftEmpty(TruncationReason),
    /// Hard error: the caller should propagate this as `Err`.
    HardError(ExtractError),
}

/// Prompt suffix to append on the retry attempt when the first response
/// was invalid JSON.
const RETRY_SUFFIX: &str = "\n\nYour previous response was not valid JSON matching the required schema. Reply with valid JSON only — no prose, no code fences.";

impl LLMExtractor {
    /// Run the provider call with up to one retry on `InvalidJsonOutput`.
    async fn run_with_retry(
        &self,
        base_prompt: &str,
        regions: &[PromptRegion],
        marks: &[FenceMark],
        event_id: &crate::domain::CaptureEventId,
    ) -> InnerOutcome {
        for attempt in 0..2_u8 {
            let prompt = if attempt == 0 {
                base_prompt.to_owned()
            } else {
                format!("{base_prompt}{RETRY_SUFFIX}")
            };

            let req = CompletionRequest::builder()
                .prompt(prompt)
                .schema(schema::schema_value().clone())
                .build();

            match self.provider.complete(&req).await {
                Ok(CompletionOutput::Json(v)) => {
                    match parse_response(&v, regions, marks, event_id) {
                        Ok(parsed) => return InnerOutcome::Parsed(parsed),
                        Err(ExtractError::Provider {
                            code: "invalid_json_output",
                            ..
                        }) if attempt == 0 => {
                            tracing::warn!(reason = "llm.invalid_json_output_retry");
                            // loop continues to attempt 1
                        }
                        Err(e) => return InnerOutcome::HardError(e),
                    }
                }
                Ok(CompletionOutput::Text(s)) => {
                    match serde_json::from_str::<serde_json::Value>(&s) {
                        Ok(v) => match parse_response(&v, regions, marks, event_id) {
                            Ok(parsed) => return InnerOutcome::Parsed(parsed),
                            Err(ExtractError::Provider {
                                code: "invalid_json_output",
                                ..
                            }) if attempt == 0 => {
                                tracing::warn!(reason = "llm.invalid_json_output_retry");
                                // loop continues to attempt 1
                            }
                            Err(e) => return InnerOutcome::HardError(e),
                        },
                        Err(_parse_err) if attempt == 0 => {
                            tracing::warn!(reason = "llm.invalid_json_output_retry");
                            // loop continues to attempt 1
                        }
                        Err(parse_err) => {
                            return InnerOutcome::HardError(ExtractError::Provider {
                                worker: "llm",
                                code: "invalid_json_output",
                                source: LlmError::InvalidJsonOutput {
                                    detail: parse_err.to_string(),
                                    raw: s,
                                },
                            });
                        }
                    }
                }
                Err(LlmError::NotConfigured { .. }) => {
                    tracing::info!(reason = "llm.not_configured");
                    return InnerOutcome::SoftEmpty(TruncationReason::None);
                }
                Err(LlmError::BudgetExceeded) => {
                    return InnerOutcome::SoftEmpty(TruncationReason::MaxWallMs { elapsed_ms: 0 });
                }
                Err(LlmError::InvalidJsonOutput { .. }) if attempt == 0 => {
                    tracing::warn!(reason = "llm.invalid_json_output_retry");
                    // loop continues to attempt 1
                }
                Err(e) => {
                    let code = err_code(&e);
                    return InnerOutcome::HardError(ExtractError::Provider {
                        worker: "llm",
                        code,
                        source: e,
                    });
                }
            }
        }

        // Both attempts exhausted by InvalidJsonOutput.
        InnerOutcome::HardError(ExtractError::Provider {
            worker: "llm",
            code: "invalid_json_output",
            source: LlmError::InvalidJsonOutput {
                detail: "response failed schema validation after retry".to_owned(),
                raw: String::new(),
            },
        })
    }
}

#[async_trait::async_trait]
impl ExtractorWorker for LLMExtractor {
    fn name(&self) -> &'static str {
        "llm"
    }

    fn role(&self) -> WorkerRole {
        WorkerRole::Augmenting
    }

    fn budget(&self) -> ExtractBudget {
        self.budget
    }

    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
        // Step 0: body resolution
        let body = match &input.body {
            BodyResolution::NotApplicable => return Ok(empty_result()),
            BodyResolution::Failed(e) => {
                return Err(ExtractError::BodyResolution {
                    event_id: input.event.event_id.as_str().to_owned(),
                    source: e.clone(),
                });
            }
            BodyResolution::Resolved(rb) => rb.text(),
        };

        if body.is_empty() {
            return Ok(empty_result());
        }

        if input.eligible_spans.is_empty() {
            return Ok(empty_result());
        }

        // Step 1: fence + render
        let payload = fence(body);
        let regions = build_regions(&payload.text, &input.eligible_spans, &payload.marks);
        if regions.is_empty() {
            return Ok(empty_result());
        }
        let prompt = render_prompt(&regions);

        // Step 2: byte cap
        if self
            .budget
            .max_prompt_bytes
            .is_some_and(|cap| prompt.len() > cap as usize)
        {
            tracing::warn!(
                reason = "llm.prompt_size_byte_cap_skip",
                prompt_bytes = prompt.len(),
                cap = self.budget.max_prompt_bytes,
            );
            return Ok(empty_with_truncation(TruncationReason::MaxWallMs {
                elapsed_ms: 0,
            }));
        }

        // Step 3+4: run with timeout + retry
        let timeout_dur = std::time::Duration::from_millis(u64::from(self.budget.max_wall_ms));
        let started = std::time::Instant::now();

        let outcome = tokio::time::timeout(
            timeout_dur,
            self.run_with_retry(&prompt, &regions, &payload.marks, &input.event.event_id),
        )
        .await;

        match outcome {
            Ok(InnerOutcome::Parsed(parsed)) => {
                if parsed.all_dropped {
                    return Err(ExtractError::SpanOutOfBounds { worker: "llm" });
                }
                let outputs: Vec<ExtractOutput> = parsed
                    .drafts
                    .into_iter()
                    .map(ExtractOutput::Draft)
                    .collect();
                Ok(ExtractResult {
                    outputs,
                    discards: parsed.discards,
                    truncated: TruncationReason::None,
                    llm_eligible_spans: vec![],
                })
            }
            Ok(InnerOutcome::SoftEmpty(reason)) => Ok(empty_with_truncation(reason)),
            Ok(InnerOutcome::HardError(e)) => Err(e),
            Err(_timeout) => {
                let elapsed = started.elapsed();
                // Clamp to u32::MAX then cast: the `.min()` makes the truncation safe.
                let elapsed_ms_u128 = elapsed.as_millis().min(u128::from(u32::MAX));
                #[allow(clippy::cast_possible_truncation)] // clamped above
                let elapsed_ms = elapsed_ms_u128 as u32;
                Ok(empty_with_truncation(TruncationReason::MaxWallMs {
                    elapsed_ms,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::llm_provider::{CompletionRequest, LLMProvider, LLMProviderCapabilities};
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::pipeline::extract::TextSpan;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubProvider {
        responses: Mutex<Vec<Result<CompletionOutput, LlmError>>>,
        call_count: std::sync::atomic::AtomicUsize,
    }

    impl StubProvider {
        fn with_responses(rs: Vec<Result<CompletionOutput, LlmError>>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(rs.into_iter().rev().collect()),
                call_count: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.call_count.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl LLMProvider for StubProvider {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn capabilities(&self) -> &LLMProviderCapabilities {
            static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
                json_mode: true,
                streaming: false,
                tool_calls: false,
            };
            &CAPS
        }

        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }

        async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.responses
                .lock()
                .expect("mutex not poisoned")
                .pop()
                .unwrap_or_else(|| panic!("StubProvider: more calls than configured responses"))
        }
    }

    // ── Shared helpers ────────────────────────────────────────────────────────

    fn fixture_event() -> crate::domain::CaptureEvent {
        use crate::domain::{
            ActorChainEntry, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, ChainRole,
            Identity, PayloadHash, Rfc3339Timestamp,
        };
        let payload = CapturePayload::Cli {
            kind_hint: "user".to_owned(),
        };
        let source_family = payload.source_family();
        let ts = Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").expect("valid ts");
        let sensor = Identity::parse("snr:local:hook:test:v1").expect("valid sensor");
        crate::domain::CaptureEvent {
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
            payload_ref: "sources/cli/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
            captured_at: ts,
            payload,
            source_family,
        }
    }

    /// Build an `ExtractInput` with a resolved body covering `body_text`.
    fn fixture_input_with_body(
        event: &crate::domain::CaptureEvent,
        body_text: &str,
    ) -> (Vec<u8>, Vec<TextSpan>) {
        // Returns the eligible spans (whole body). Caller holds body_text.
        let body_len = u32::try_from(body_text.len()).expect("test: body fits in u32");
        let spans = if body_text.is_empty() {
            vec![]
        } else {
            vec![TextSpan::new(0, body_len)]
        };
        let _ = event; // used for context; lifetime tied to caller
        (body_text.as_bytes().to_vec(), spans)
    }

    fn make_input<'a>(
        event: &'a crate::domain::CaptureEvent,
        body: &'a str,
        spans: Vec<TextSpan>,
    ) -> ExtractInput<'a> {
        use crate::pipeline::extract::body::{ResolvedBody, UserIngestPayloadKind};
        let resolved =
            ResolvedBody::from_user_ingest(body, &event.payload, UserIngestPayloadKind::Cli)
                .expect("Cli payload matches");
        ExtractInput {
            event,
            body: BodyResolution::Resolved(resolved),
            eligible_spans: spans,
        }
    }

    fn good_response_json() -> CompletionOutput {
        CompletionOutput::Json(serde_json::json!({
            "items": [{
                "type": "draft",
                "kind": "user",
                "body": "tabs preferred",
                "confidence": 0.9,
                "source": {
                    "region_id": 0,
                    "text_excerpt": "user prefers tabs over spaces"
                }
            }]
        }))
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn happy_path_returns_drafts() {
        let provider = StubProvider::with_responses(vec![Ok(good_response_json())]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "user prefers tabs over spaces";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert_eq!(res.outputs.len(), 1);
        assert_eq!(provider.calls(), 1);
    }

    #[tokio::test]
    async fn not_configured_returns_ok_empty() {
        let provider = StubProvider::with_responses(vec![Err(LlmError::NotConfigured {
            remediation: "set llm".into(),
        })]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "body content here please";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert!(res.outputs.is_empty());
    }

    #[tokio::test]
    async fn budget_exceeded_returns_ok_empty_truncation() {
        let provider = StubProvider::with_responses(vec![Err(LlmError::BudgetExceeded)]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "body content here please";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert!(res.outputs.is_empty());
        assert!(matches!(res.truncated, TruncationReason::MaxWallMs { .. }));
    }

    #[tokio::test]
    async fn invalid_json_first_retried_then_recovers() {
        let provider = StubProvider::with_responses(vec![
            Err(LlmError::InvalidJsonOutput {
                detail: "bad".into(),
                raw: "{".into(),
            }),
            Ok(good_response_json()),
        ]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "user prefers tabs over spaces";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert_eq!(res.outputs.len(), 1);
        assert_eq!(provider.calls(), 2);
    }

    #[tokio::test]
    async fn invalid_json_twice_returns_provider_err() {
        let provider = StubProvider::with_responses(vec![
            Err(LlmError::InvalidJsonOutput {
                detail: "bad1".into(),
                raw: "{".into(),
            }),
            Err(LlmError::InvalidJsonOutput {
                detail: "bad2".into(),
                raw: "{}".into(),
            }),
        ]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "body content here please";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let err = extractor.extract(&input).await.unwrap_err();
        assert!(
            matches!(
                err,
                ExtractError::Provider {
                    code: "invalid_json_output",
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn unreachable_returns_provider_err_no_retry() {
        let provider = StubProvider::with_responses(vec![Err(LlmError::ProviderUnreachable {
            detail: "no route".into(),
        })]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "body content here please";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let err = extractor.extract(&input).await.unwrap_err();
        assert!(
            matches!(
                err,
                ExtractError::Provider {
                    code: "unreachable",
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
        assert_eq!(provider.calls(), 1);
    }

    #[tokio::test]
    async fn auth_denied_returns_provider_err() {
        let provider = StubProvider::with_responses(vec![Err(LlmError::AuthDenied)]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let body = "body content here please";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let err = extractor.extract(&input).await.unwrap_err();
        assert!(
            matches!(
                err,
                ExtractError::Provider {
                    code: "auth_denied",
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn empty_body_does_not_call_provider() {
        let provider = StubProvider::with_responses(vec![]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        // For empty / not-applicable body, use NotApplicable so eligible_spans = []
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let res = extractor.extract(&input).await.expect("should succeed");
        assert!(res.outputs.is_empty());
        assert_eq!(provider.calls(), 0);
    }

    #[tokio::test]
    async fn body_over_byte_cap_does_not_call_provider() {
        // Default LLM budget cap is 64 KiB for the prompt. A 200 KiB body
        // will produce a prompt larger than that.
        let huge_body = "a".repeat(200_000);
        let provider = StubProvider::with_responses(vec![]);
        let extractor = LLMExtractor::new(provider.clone());
        let event = fixture_event();
        let (_, spans) = fixture_input_with_body(&event, &huge_body);
        let input = make_input(&event, &huge_body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert!(res.outputs.is_empty());
        assert!(
            matches!(res.truncated, TruncationReason::MaxWallMs { elapsed_ms: 0 }),
            "expected MaxWallMs{{0}} truncation, got {:?}",
            res.truncated
        );
        assert_eq!(provider.calls(), 0);
    }

    // Wall-clock timeout test. We use a 50 ms budget and a provider that sleeps
    // 500 ms so the test runs in ~50 ms on any reasonable machine. `start_paused`
    // is intentionally omitted — it requires `tokio/test-util` which is not yet
    // in the workspace. A real-time sleep of 500 ms (capped at 50 ms by the budget)
    // is acceptable for a determinism-safe integration test.
    #[tokio::test]
    async fn wall_clock_timeout_returns_ok_empty_truncation() {
        struct SlowProvider;

        #[async_trait::async_trait]
        impl LLMProvider for SlowProvider {
            fn name(&self) -> &'static str {
                "slow"
            }

            fn capabilities(&self) -> &LLMProviderCapabilities {
                static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
                    json_mode: true,
                    streaming: false,
                    tool_calls: false,
                };
                &CAPS
            }

            fn supported_contract_versions(&self) -> VersionRange {
                VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
            }

            async fn complete(
                &self,
                _req: &CompletionRequest,
            ) -> Result<CompletionOutput, LlmError> {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                unreachable!("should have timed out")
            }
        }

        let extractor = LLMExtractor::new(Arc::new(SlowProvider)).with_budget(ExtractBudget {
            max_wall_ms: 50,
            ..ExtractBudget::llm_default()
        });

        let event = fixture_event();
        let body = "body content here please";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert!(res.outputs.is_empty());
        assert!(
            matches!(res.truncated, TruncationReason::MaxWallMs { .. }),
            "expected MaxWallMs truncation, got {:?}",
            res.truncated
        );
    }

    #[tokio::test]
    async fn discards_surface_in_extract_result() {
        let provider =
            StubProvider::with_responses(vec![Ok(CompletionOutput::Json(serde_json::json!({
                "items": [{
                    "type": "discard",
                    "reason": "low_salience",
                    "evidence": "noise",
                    "source": {
                        "region_id": 0,
                        "text_excerpt": "user prefers tabs over spaces"
                    }
                }]
            })))]);
        let extractor = LLMExtractor::new(provider);
        let event = fixture_event();
        let body = "user prefers tabs over spaces";
        let (_, spans) = fixture_input_with_body(&event, body);
        let input = make_input(&event, body, spans);
        let res = extractor.extract(&input).await.expect("should succeed");
        assert!(res.outputs.is_empty());
        assert_eq!(res.discards.len(), 1);
    }

    #[tokio::test]
    async fn body_resolution_failed_returns_body_error() {
        use crate::pipeline::extract::body::BodyResolutionError;
        let provider = StubProvider::with_responses(vec![]);
        let extractor = LLMExtractor::new(provider);
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::Failed(BodyResolutionError::NotFound("nope".into())),
            eligible_spans: vec![TextSpan::new(0, 10)],
        };
        let err = extractor.extract(&input).await.unwrap_err();
        assert!(
            matches!(err, ExtractError::BodyResolution { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn llm_extractor_role_is_augmenting() {
        let provider = StubProvider::with_responses(vec![]);
        let extractor = LLMExtractor::new(provider);
        assert_eq!(extractor.role(), WorkerRole::Augmenting);
    }

    #[test]
    fn llm_extractor_name_is_llm() {
        let provider = StubProvider::with_responses(vec![]);
        let extractor = LLMExtractor::new(provider);
        assert_eq!(extractor.name(), "llm");
    }
}
