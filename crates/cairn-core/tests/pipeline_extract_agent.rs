//! Integration tests for agent extractor parser and worker plumbing.

use std::sync::{Arc, Mutex};

use cairn_core::config::{
    ExtractBudget as ConfigExtractBudget, ExtractConfig, ExtractorEntry, ExtractorWorkerKind,
};
use cairn_core::contract::agent_provider::{
    AgentBudgetConsumed, AgentOutput, AgentProvider, AgentProviderCapabilities, AgentProviderError,
    AgentRun, AgentRunStatus, AgentScope, AgentSpawnRequest, AgentToolAllowlist,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, ChainRole,
    Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::pipeline::extract::agent::{AgentExtractor, AgentParseError, parse_agent_response};
use cairn_core::pipeline::extract::{
    BodyResolution, ExtractBudget, ExtractBuildError, ExtractChain, ExtractError, ExtractInput,
    ExtractOutput, ExtractProviders, ExtractorWorker, RegexExtractor, ResolvedBody, TextSpan,
    UserIngestPayloadKind, build_extract_chain,
};

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid")
}

fn event_id() -> CaptureEventId {
    CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ULID")
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
        event_id: event_id(),
        sensor_id: Identity::parse("snr:local:cli:default:v1").expect("valid"),
        capture_mode: CaptureMode::Explicit,
        actor_chain: vec![
            entry(ChainRole::Delegator, "agt:claude-code:opus-4-7:main:v1"),
            entry(ChainRole::Author, "hmn:tafeng"),
        ],
        refs: None,
        payload_hash: hash(),
        payload_ref: "sources/cli/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
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
        eligible_spans: None,
    }
}

struct RecordingAgentProvider {
    last_request: Mutex<Option<AgentSpawnRequest>>,
    response: Mutex<Result<AgentRun, AgentProviderError>>,
}

impl RecordingAgentProvider {
    fn returning(run: AgentRun) -> Self {
        Self {
            last_request: Mutex::new(None),
            response: Mutex::new(Ok(run)),
        }
    }

    fn failing(err: AgentProviderError) -> Self {
        Self {
            last_request: Mutex::new(None),
            response: Mutex::new(Err(err)),
        }
    }

    fn last_request(&self) -> AgentSpawnRequest {
        self.last_request
            .lock()
            .expect("mutex not poisoned")
            .clone()
            .expect("provider was called")
    }
}

#[async_trait::async_trait]
impl AgentProvider for RecordingAgentProvider {
    fn name(&self) -> &str {
        "recording-agent"
    }

    fn capabilities(&self) -> &AgentProviderCapabilities {
        static CAPS: AgentProviderCapabilities = AgentProviderCapabilities {
            honors_cost_budget: true,
            scope_enforced: true,
            mcp_tools: false,
            cli_subprocess_tools: true,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
        *self.last_request.lock().expect("mutex not poisoned") = Some(request);
        self.response.lock().expect("mutex not poisoned").clone()
    }
}

fn successful_run() -> AgentRun {
    AgentRun {
        status: AgentRunStatus::Completed,
        abort_error: None,
        output: AgentOutput::Json(serde_json::json!({
            "drafts": [{
                "kind": "fact",
                "body": "Refund routing uses shard alpha.",
                "confidence": 0.91,
                "span": {"start": 4, "end": 27},
                "evidence": [{"tool": "retrieve", "claim": "source text says shard alpha"}]
            }],
            "discards": [],
            "evidence": [{"tool": "search", "claim": "matched refund routing note"}]
        })),
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 1,
            cost_units: 1,
        },
        tool_calls: vec![],
        policy_trace: vec![],
    }
}

#[tokio::test]
async fn agent_extractor_builds_read_only_request_and_returns_drafts() {
    let provider = Arc::new(RecordingAgentProvider::returning(successful_run()));
    let extractor = AgentExtractor::new(provider.clone()).with_budget(ExtractBudget {
        max_wall_ms: 1234,
        max_drafts: 16,
        max_prompt_bytes: Some(64 * 1024),
        max_response_tokens: Some(2048),
    });
    let event = cli_event();
    let input = body_input(&event, "Use shard alpha for refunds.");

    let result = extractor.extract(&input).await.expect("extract ok");

    assert_eq!(result.outputs.len(), 1);
    let ExtractOutput::Draft(draft) = &result.outputs[0] else {
        panic!("expected draft");
    };
    assert_eq!(draft.body, "Refund routing uses shard alpha.");

    let request = provider.last_request();
    assert_eq!(request.identity.as_str(), "agt:cairn-extractor:v1");
    assert_eq!(request.scope, AgentScope::read_only());
    assert_eq!(
        request.tool_allowlist,
        AgentToolAllowlist::read_only_cairn()
    );
    assert_eq!(request.cost_budget.max_turns, 4);
    assert_eq!(request.cost_budget.max_tool_calls, 4);
    assert_eq!(request.cost_budget.max_cost_units, 2048);
    assert_eq!(request.wall_clock_budget.max_millis, 1234);
    assert!(request.prompt.contains("Use shard alpha for refunds."));
    assert!(request.prompt.contains(event.event_id.as_str()));
}

#[tokio::test]
async fn agent_extractor_prompt_omits_text_outside_restricted_eligible_spans() {
    let provider = Arc::new(RecordingAgentProvider::returning(AgentRun {
        status: AgentRunStatus::Completed,
        abort_error: None,
        output: AgentOutput::Json(serde_json::json!({
            "drafts": [{
                "kind": "fact",
                "body": "Visible memory fact.",
                "confidence": 0.91,
                "span": {"start": 8, "end": 19}
            }],
            "discards": [],
            "evidence": []
        })),
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 0,
            cost_units: 1,
        },
        tool_calls: vec![],
        policy_trace: vec![],
    }));
    let extractor = AgentExtractor::new(provider.clone());
    let event = cli_event();
    let body = "VISIBLE memory fact. SECRET token must not leave.";
    let resolved = ResolvedBody::from_user_ingest(body, &event.payload, UserIngestPayloadKind::Cli)
        .expect("matching variant");
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(resolved),
        eligible_spans: Some(vec![TextSpan::new(0, 20)]),
    };

    let result = extractor.extract(&input).await.expect("extract ok");

    let ExtractOutput::Draft(draft) = &result.outputs[0] else {
        panic!("expected draft");
    };
    assert_eq!(draft.source_span, Some(TextSpan::new(8, 19)));

    let request = provider.last_request();
    assert!(request.prompt.contains("VISIBLE memory fact"));
    assert!(!request.prompt.contains("SECRET token"));
    assert!(request.prompt.contains("0..20"));
}

#[tokio::test]
async fn agent_extractor_aborted_run_surfaces_original_abort_error() {
    let provider = Arc::new(RecordingAgentProvider::returning(AgentRun {
        status: AgentRunStatus::Aborted,
        abort_error: Some(AgentProviderError::BudgetExceeded {
            limit: "turns".to_owned(),
        }),
        output: AgentOutput::Empty,
        budget_consumed: AgentBudgetConsumed {
            turns: 4,
            tool_calls: 0,
            cost_units: 10,
        },
        tool_calls: vec![],
        policy_trace: vec![],
    }));
    let extractor = AgentExtractor::new(provider);
    let event = cli_event();
    let input = body_input(&event, "Use shard alpha for refunds.");

    let err = extractor
        .extract(&input)
        .await
        .expect_err("aborted run should be provider error");

    assert!(matches!(
        err,
        ExtractError::AgentProvider {
            source: AgentProviderError::BudgetExceeded { ref limit },
            ..
        } if limit == "turns"
    ));
}

#[tokio::test]
async fn augmenting_agent_failure_is_chain_failure_not_gate_failure() {
    let provider = Arc::new(RecordingAgentProvider::failing(
        AgentProviderError::BudgetExceeded {
            limit: "turns".to_owned(),
        },
    ));
    let chain = ExtractChain::new(vec![
        Box::new(RegexExtractor::builtin()),
        Box::new(AgentExtractor::new(provider)),
    ])
    .expect("valid chain");
    let event = cli_event();
    let input = body_input(&event, "Use shard alpha for refunds.");

    let result = chain.run(&input).await.expect("augmenting failure is Ok");

    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].worker, "agent");
}

#[test]
fn build_chain_rejects_agent_entry_without_provider() {
    let config = ExtractConfig {
        chain: vec![
            ExtractorEntry {
                worker: ExtractorWorkerKind::Regex,
                kinds: vec![],
                trigger: None,
                budget: ConfigExtractBudget::default(),
            },
            ExtractorEntry {
                worker: ExtractorWorkerKind::Agent,
                kinds: vec![],
                trigger: None,
                budget: ConfigExtractBudget::default(),
            },
        ],
    };

    let err = match build_extract_chain(&config, ExtractProviders::default()) {
        Ok(_) => panic!("missing agent provider should fail"),
        Err(err) => err,
    };
    assert!(matches!(err, ExtractBuildError::MissingAgentProvider));
}

#[test]
fn build_chain_accepts_regex_and_agent_with_provider() {
    let config = ExtractConfig {
        chain: vec![
            ExtractorEntry {
                worker: ExtractorWorkerKind::Regex,
                kinds: vec![MemoryKind::User],
                trigger: None,
                budget: ConfigExtractBudget::default(),
            },
            ExtractorEntry {
                worker: ExtractorWorkerKind::Agent,
                kinds: vec![],
                trigger: None,
                budget: ConfigExtractBudget {
                    max_tokens: Some(1024),
                    max_wall_ms: Some(2000),
                    max_turns: Some(2),
                },
            },
        ],
    };
    let provider = Arc::new(RecordingAgentProvider::returning(successful_run()));

    let chain = build_extract_chain(
        &config,
        ExtractProviders {
            llm: None,
            agent: Some(provider),
        },
    );

    assert!(chain.is_ok());
}

#[test]
fn parser_accepts_drafts_discards_and_evidence() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let source = "Use shard alpha for refunds. Ignore the earlier typo.";
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "Refund routing uses shard alpha.",
            "confidence": 0.91,
            "span": {"start": 4, "end": 27},
            "evidence": [{"tool": "retrieve", "claim": "source text says shard alpha"}]
        }],
        "discards": [{
            "reason": "earlier typo is explicitly superseded",
            "span": {"start": 29, "end": 53}
        }],
        "evidence": [{"tool": "search", "claim": "matched prior refund routing note"}]
    });

    let parsed = parse_agent_response(&event_id, source, value).expect("valid agent output");
    assert_eq!(parsed.drafts.len(), 1);
    assert_eq!(parsed.discards.len(), 1);
    assert_eq!(parsed.evidence.len(), 1);
}

#[test]
fn parser_rejects_out_of_bounds_spans() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "bad span",
            "confidence": 0.8,
            "span": {"start": 0, "end": 99}
        }],
        "discards": [],
        "evidence": []
    });

    let err = parse_agent_response(&event_id, "short", value).expect_err("span must be checked");
    assert!(matches!(err, AgentParseError::SpanOutOfBounds { .. }));
}

#[test]
fn parser_rejects_invalid_confidence() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "bad confidence",
            "confidence": 1.2,
            "span": {"start": 0, "end": 3}
        }],
        "discards": [],
        "evidence": []
    });

    let err =
        parse_agent_response(&event_id, "short", value).expect_err("confidence must be checked");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "drafts.confidence",
            ..
        }
    ));
}

#[test]
fn parser_rejects_confidence_that_only_rounds_into_range_as_f32() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "rounded confidence",
            "confidence": 1.00000001,
            "span": {"start": 0, "end": 3}
        }],
        "discards": [],
        "evidence": []
    });

    let err =
        parse_agent_response(&event_id, "short", value).expect_err("confidence must be checked");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "drafts.confidence",
            ..
        }
    ));
}

#[test]
fn parser_rejects_unknown_top_level_fields() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [],
        "unexpected": true
    });

    let err = parse_agent_response(&event_id, "source", value)
        .expect_err("top-level object must reject unknown fields");
    assert!(matches!(err, AgentParseError::InvalidField { .. }));
}

#[test]
fn parser_rejects_unknown_draft_fields() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "extra field",
            "confidence": 0.9,
            "span": {"start": 0, "end": 3},
            "unexpected": true
        }],
        "discards": [],
        "evidence": []
    });

    let err = parse_agent_response(&event_id, "source", value)
        .expect_err("draft object must reject unknown fields");
    assert!(matches!(err, AgentParseError::InvalidField { .. }));
}

#[test]
fn parser_accepts_empty_record_id_but_rejects_empty_tool_and_claim() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let valid = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [{"tool": "retrieve", "record_id": "", "claim": "matched note"}]
    });

    let parsed = parse_agent_response(&event_id, "source", valid).expect("valid evidence");
    assert_eq!(parsed.evidence[0].record_id.as_deref(), Some(""));

    let empty_tool = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [{"tool": "", "claim": "matched note"}]
    });
    let err =
        parse_agent_response(&event_id, "source", empty_tool).expect_err("tool must be non-empty");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "evidence",
            ..
        }
    ));

    let empty_claim = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [{"tool": "retrieve", "claim": ""}]
    });
    let err = parse_agent_response(&event_id, "source", empty_claim)
        .expect_err("claim must be non-empty");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "evidence",
            ..
        }
    ));
}
