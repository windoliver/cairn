# Agent Extractor And Dream Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Keep this checklist updated as work proceeds.

**Goal:** Implement issue #125 full scope: `AgentExtractor` for high-stakes or low-confidence capture, agent-mode `DreamWorker`, read-only tool-policy enforcement, budget/evidence metadata, and fallbacks to existing regex/LLM paths.

**Architecture:** Build on the merged #124 `AgentProvider` contract. `cairn-core` owns config validation, extraction schemas, parsing, chain building, and policy-safe request construction. `cairn-agent-core` is a bundled minimal provider runtime over `LLMProvider` plus bounded read-only Cairn CLI tools. `cairn-workflows` routes dream distillation through a worker planning seam that returns `FlushPlan` before side effects. `cairn-cli` wires configured LLM and agent providers into MCP workflow startup and plugin discovery.

**Tech Stack:** Rust 2024, `async-trait`, `tokio`, `serde`, `serde_json`, `thiserror`, existing `cairn-core` extraction pipeline, existing `AgentProvider` contract, existing workflow `FlushPlan`/drainer surfaces, and `cargo test`/`cargo nextest` where available.

---

## Source References

- Issue: `https://github.com/windoliver/cairn/issues/125`
- Dependency: PR #403 / issue #124 merged `AgentProvider` contract.
- Design spec: `docs/superpowers/specs/2026-05-21-agent-extractor-dream-worker-design.md`
- Current agent contract: `crates/cairn-core/src/contract/agent_provider.rs`
- Current extraction chain: `crates/cairn-core/src/pipeline/extract/chain.rs`
- Current LLM extractor patterns: `crates/cairn-core/src/pipeline/extract/llm/`
- Current dream worker: `crates/cairn-workflows/src/dream/handler.rs`
- Current plugin host: `crates/cairn-cli/src/plugins/host.rs`

## File Structure

Create:

- `crates/cairn-core/src/pipeline/extract/agent/mod.rs`
- `crates/cairn-core/src/pipeline/extract/agent/parse.rs`
- `crates/cairn-core/src/pipeline/extract/agent/prompt.rs`
- `crates/cairn-core/src/pipeline/extract/agent/schema.rs`
- `crates/cairn-core/src/pipeline/extract/build.rs`
- `crates/cairn-core/tests/pipeline_extract_agent.rs`
- `crates/cairn-agent-core/Cargo.toml`
- `crates/cairn-agent-core/src/lib.rs`
- `crates/cairn-agent-core/src/action.rs`
- `crates/cairn-agent-core/src/provider.rs`
- `crates/cairn-agent-core/src/tool.rs`
- `crates/cairn-agent-core/tests/provider.rs`
- `crates/cairn-workflows/src/dream/plan.rs`

Modify:

- `Cargo.toml`
- `crates/cairn-cli/Cargo.toml`
- `crates/cairn-cli/src/mcp.rs`
- `crates/cairn-cli/src/plugins/host.rs`
- `crates/cairn-cli/src/plugins/list.rs`
- `crates/cairn-core/src/config/mod.rs`
- `crates/cairn-core/src/config/dream.rs`
- `crates/cairn-core/src/pipeline/extract/mod.rs`
- `crates/cairn-core/src/pipeline/extract/chain.rs`
- `crates/cairn-core/src/status/mod.rs`
- `crates/cairn-core/src/status/tests.rs`
- `crates/cairn-workflows/Cargo.toml`
- `crates/cairn-workflows/src/dream/handler.rs`
- `crates/cairn-workflows/src/dream/mod.rs`
- `crates/cairn-workflows/tests/dream.rs`

## Task 1 - Config Gates And Capability Truthfulness

- [ ] Add failing config tests in `crates/cairn-core/src/config/mod.rs`.

Use these test names:

```rust
#[test]
fn agent_extractor_requires_agent_provider_config() {
    let mut config = CairnConfig::default();
    config.pipeline.extract.chain.push(ExtractorEntry {
        worker: ExtractorWorkerKind::Agent,
        kinds: vec![],
        trigger: None,
        budget: ExtractBudget {
            max_tokens: Some(2048),
            max_wall_ms: Some(30_000),
            max_turns: Some(4),
        },
    });

    let err = config.validate().expect_err("agent extractor needs provider");
    assert!(matches!(err, ConfigError::AgentModeWithoutProvider { field }
        if field == "pipeline.extract.chain[].worker"));
}

#[test]
fn agent_dream_requires_provider_and_tool_budget() {
    let mut config = CairnConfig::default();
    config.llm.provider = Some(LlmProvider::OpenaiCompatible);
    config.dream.enabled = true;
    config.dream.deep_dreaming.worker = DreamWorkerMode::Agent;
    config.dream.deep_dreaming.max_tool_calls = 0;

    let err = config.validate().expect_err("agent dream needs provider first");
    assert!(matches!(err, ConfigError::AgentModeWithoutProvider { field }
        if field == "dream.deep_dreaming.worker"));

    config.agent_provider.kind = Some(AgentProviderKind::CairnCore);
    let err = config.validate().expect_err("agent dream needs tool budget");
    assert!(matches!(err, ConfigError::InvalidDream { .. }));
}
```

- [ ] Add failing dream enum/validation tests in `crates/cairn-core/src/config/dream.rs`.

```rust
#[test]
fn agent_worker_round_trips() {
    let json = serde_json::to_string(&DreamWorkerMode::Agent).expect("serialize");
    assert_eq!(json, "\"agent\"");
    let back: DreamWorkerMode = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, DreamWorkerMode::Agent);
}

#[test]
fn agent_worker_requires_nonzero_tool_budget() {
    let mut cfg = DreamConfig::default();
    cfg.enabled = true;
    cfg.deep_dreaming.worker = DreamWorkerMode::Agent;
    cfg.deep_dreaming.max_tool_calls = 0;

    let err = cfg.validate().expect_err("agent mode must budget tools");
    assert!(matches!(err, DreamConfigError::AgentToolBudgetZero { tier }
        if tier == DreamTier::DeepDreaming));
}
```

- [ ] Run the red tests.

```bash
cargo test -p cairn-core agent_extractor_requires_agent_provider_config
cargo test -p cairn-core agent_dream_requires_provider_and_tool_budget
cargo test -p cairn-core agent_worker_round_trips
cargo test -p cairn-core agent_worker_requires_nonzero_tool_budget
```

Expected failure before implementation: missing `agent_provider`, missing `AgentProviderKind`, missing `DreamWorkerMode::Agent`, or missing `DreamConfigError::AgentToolBudgetZero`.

- [ ] Implement config types in `crates/cairn-core/src/config/mod.rs`.

```rust
string_enum! {
    /// Which bundled or named agent provider is active.
    #[non_exhaustive]
    pub enum AgentProviderKind {
        /// Bundled bounded provider runtime.
        CairnCore => "cairn-core",
    }
    unknown_msg: "expected cairn-core | custom:<name>",
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentProviderConfig {
    pub kind: Option<AgentProviderKind>,
    pub command: String,
}

impl Default for AgentProviderConfig {
    fn default() -> Self {
        Self {
            kind: None,
            command: "cairn".to_owned(),
        }
    }
}
```

Add `agent_provider: AgentProviderConfig` to `CairnConfig`.

Extend `ConfigError`:

```rust
#[error("{field} uses agent mode but agent_provider.kind is not configured")]
AgentModeWithoutProvider { field: &'static str },
```

Extend `CairnConfig::validate()` after the existing LLM-worker provider gate:

```rust
let agent_configured = self.agent_provider.kind.is_some();
for entry in &self.pipeline.extract.chain {
    if matches!(entry.worker, ExtractorWorkerKind::Agent) && !agent_configured {
        return Err(ConfigError::AgentModeWithoutProvider {
            field: "pipeline.extract.chain[].worker",
        });
    }
}
for (field, tier) in [
    ("dream.light_sleep.worker", self.dream.light_sleep),
    ("dream.rem_sleep.worker", self.dream.rem_sleep),
    ("dream.deep_dreaming.worker", self.dream.deep_dreaming),
] {
    if matches!(tier.worker, DreamWorkerMode::Agent) && !agent_configured {
        return Err(ConfigError::AgentModeWithoutProvider { field });
    }
}
```

- [ ] Implement `DreamWorkerMode::Agent` and `DreamConfigError::AgentToolBudgetZero` in `crates/cairn-core/src/config/dream.rs`.

```rust
pub enum DreamWorkerMode {
    Llm,
    Hybrid,
    Agent,
}

#[error("dream.{tier}.max_tool_calls must be >= 1 for agent worker")]
AgentToolBudgetZero { tier: DreamTier },
```

In `DreamConfig::validate()`:

```rust
if matches!(tier.worker, DreamWorkerMode::Agent) && tier.max_tool_calls == 0 {
    return Err(DreamConfigError::AgentToolBudgetZero { tier: tier.tier });
}
```

- [ ] Add `agent_provider_config_round_trips` test to protect config compatibility.

```rust
#[test]
fn agent_provider_config_round_trips() {
    let json = r#"{"kind":"cairn-core","command":"cairn"}"#;
    let cfg: AgentProviderConfig = serde_json::from_str(json).expect("deserialize");
    assert_eq!(cfg.kind, Some(AgentProviderKind::CairnCore));
    assert_eq!(serde_json::to_value(&cfg).expect("serialize")["command"], "cairn");
}
```

- [ ] Update `CapabilitySet` and status gates only where runtime can honor the feature.

Add `agent_dream: bool` to the pure config capability set if it already has feature-level fields nearby. Set it to true only when a dream tier uses `DreamWorkerMode::Agent` and `agent_provider.kind.is_some()`.

Add `agent_configured: bool` to `crates/cairn-core/src/status/mod.rs::CapabilityGates` only if status needs to distinguish LLM dream from agent dream. The MCP dream capability remains withheld unless the required provider for the selected dream mode is wired.

- [ ] Run the task tests.

```bash
cargo test -p cairn-core agent_
cargo test -p cairn-core status::tests
```

Expected pass: config rejects agent modes without `agent_provider.kind`, accepts configured agent extraction, accepts configured agent dream with nonzero `max_tool_calls`, and status does not advertise dream when its selected provider is unavailable.

## Task 2 - Agent Extraction Schema, Prompt, And Parser

- [ ] Add parser tests in `crates/cairn-core/tests/pipeline_extract_agent.rs`.

Use a test-only event builder that mirrors existing extraction tests:

```rust
fn cli_event(body: &str) -> CaptureEvent {
    CaptureEvent {
        id: CaptureEventId::new("evt-agent-1").expect("valid id"),
        payload: CapturePayload::Cli { text: body.to_owned() },
        captured_at_ms: 1_700_000_000_000,
        scope: ScopeTuple::for_test(),
    }
}
```

Add these tests:

```rust
#[test]
fn parser_accepts_drafts_discards_and_evidence() {
    let event = cli_event("Use shard alpha for refunds. Ignore the earlier typo.");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "insight",
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

    let parsed = parse_agent_response(&event, value).expect("valid agent output");
    assert_eq!(parsed.drafts.len(), 1);
    assert_eq!(parsed.discards.len(), 1);
    assert_eq!(parsed.evidence.len(), 1);
}

#[test]
fn parser_rejects_out_of_bounds_spans() {
    let event = cli_event("short");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "insight",
            "body": "bad span",
            "confidence": 0.8,
            "span": {"start": 0, "end": 99}
        }],
        "discards": [],
        "evidence": []
    });

    let err = parse_agent_response(&event, value).expect_err("span must be checked");
    assert!(matches!(err, AgentParseError::SpanOutOfBounds { .. }));
}
```

- [ ] Run the red parser tests.

```bash
cargo test -p cairn-core --test pipeline_extract_agent parser_
```

Expected failure: module and parser types do not exist.

- [ ] Create `crates/cairn-core/src/pipeline/extract/agent/schema.rs`.

```rust
pub const AGENT_EXTRACTOR_OUTPUT_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["drafts", "discards", "evidence"],
  "properties": {
    "drafts": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["kind", "body", "confidence", "span"],
        "properties": {
          "kind": {"type": "string"},
          "body": {"type": "string", "minLength": 1},
          "confidence": {"type": "number", "minimum": 0, "maximum": 1},
          "span": {"$ref": "#/$defs/span"},
          "evidence": {
            "type": "array",
            "items": {"$ref": "#/$defs/evidence"},
            "default": []
          }
        }
      }
    },
    "discards": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["reason", "span"],
        "properties": {
          "reason": {"type": "string", "minLength": 1},
          "span": {"$ref": "#/$defs/span"}
        }
      }
    },
    "evidence": {
      "type": "array",
      "items": {"$ref": "#/$defs/evidence"}
    }
  },
  "$defs": {
    "span": {
      "type": "object",
      "required": ["start", "end"],
      "properties": {
        "start": {"type": "integer", "minimum": 0},
        "end": {"type": "integer", "minimum": 0}
      }
    },
    "evidence": {
      "type": "object",
      "required": ["tool", "claim"],
      "properties": {
        "tool": {"type": "string"},
        "record_id": {"type": ["string", "null"]},
        "claim": {"type": "string", "minLength": 1}
      }
    }
  }
}"#;
```

- [ ] Create `crates/cairn-core/src/pipeline/extract/agent/parse.rs`.

Keep the parser deterministic and pure:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEvidence {
    pub tool: String,
    pub record_id: Option<String>,
    pub claim: String,
}

#[derive(Debug, Clone)]
pub struct ParsedAgentResponse {
    pub drafts: Vec<MemoryDraft>,
    pub discards: Vec<DiscardCandidate>,
    pub evidence: Vec<AgentEvidence>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentParseError {
    #[error("agent output is not an object")]
    NotObject,
    #[error("agent output span {start}..{end} is outside source length {len}")]
    SpanOutOfBounds { start: usize, end: usize, len: usize },
    #[error("agent output field `{field}` is invalid: {reason}")]
    InvalidField { field: &'static str, reason: String },
}

pub fn parse_agent_response(
    event: &CaptureEvent,
    value: serde_json::Value,
) -> Result<ParsedAgentResponse, AgentParseError> {
    let source = event.payload.text_for_extraction();
    let len = source.len();
    let object = value.as_object().ok_or(AgentParseError::NotObject)?;
    let drafts = parse_drafts(object.get("drafts"), len)?;
    let discards = parse_discards(object.get("discards"), len)?;
    let evidence = parse_evidence_list(object.get("evidence"))?;
    Ok(ParsedAgentResponse { drafts, discards, evidence })
}
```

Use existing `MemoryDraft`, `DiscardCandidate`, `TextSpan`, `MemoryKind`, and confidence helpers from the LLM extractor parser instead of adding a separate draft model.

- [ ] Create `crates/cairn-core/src/pipeline/extract/agent/prompt.rs`.

```rust
pub fn render_agent_extract_prompt(event: &CaptureEvent, eligible_spans: &[TextSpan]) -> String {
    format!(
        "\
You are Cairn's read-only agent extractor.
Return JSON matching AGENT_EXTRACTOR_OUTPUT_SCHEMA.
Use only evidence from the capture text or read-only Cairn tools.
Never propose writes, deletes, promotions, or policy changes.

Capture event id: {event_id}
Eligible spans: {eligible_spans:?}
Capture text:
{text}
",
        event_id = event.id.as_str(),
        eligible_spans = eligible_spans,
        text = event.payload.text_for_extraction(),
    )
}
```

- [ ] Create `crates/cairn-core/src/pipeline/extract/agent/mod.rs` with module exports and keep implementation empty except parser exports until Task 3.

```rust
mod parse;
mod prompt;
mod schema;

pub use parse::{AgentEvidence, AgentParseError, ParsedAgentResponse, parse_agent_response};
pub use prompt::render_agent_extract_prompt;
pub use schema::AGENT_EXTRACTOR_OUTPUT_SCHEMA;
```

- [ ] Export the module in `crates/cairn-core/src/pipeline/extract/mod.rs`.

```rust
pub mod agent;
```

- [ ] Run parser tests.

```bash
cargo test -p cairn-core --test pipeline_extract_agent parser_
```

Expected pass: parser accepts valid JSON and rejects spans beyond source text.

## Task 3 - AgentExtractor Worker And Chain Builder

- [ ] Add failing worker tests in `crates/cairn-core/tests/pipeline_extract_agent.rs`.

Use the contract's scripted or test agent provider pattern:

```rust
#[tokio::test]
async fn agent_extractor_builds_read_only_request_and_returns_drafts() {
    let run = AgentRun {
        id: "run-agent-extract-1".to_owned(),
        status: AgentRunStatus::Completed,
        output: Some(AgentOutput::Json(serde_json::json!({
            "drafts": [{
                "kind": "insight",
                "body": "Refund routing uses shard alpha.",
                "confidence": 0.91,
                "span": {"start": 4, "end": 27}
            }],
            "discards": [],
            "evidence": [{"tool": "retrieve", "claim": "verified prior routing"}]
        }))),
        consumed: AgentBudgetConsumed { turns: 1, tool_calls: 1, cost_units: 1 },
        tool_attempts: vec![],
        abort_error: None,
    };
    let provider = Arc::new(RecordingAgentProvider::ok(run));
    let extractor = AgentExtractor::new(provider.clone()).with_budget(ExtractBudget {
        max_tokens: Some(2048),
        max_wall_ms: Some(30_000),
        max_turns: Some(4),
    });

    let result = extractor.extract(&cli_event("Use shard alpha for refunds.")).await.unwrap();

    assert_eq!(result.outputs.len(), 1);
    let request = provider.last_request().expect("request captured");
    assert_eq!(request.identity.as_str(), "agt:cairn-extractor:v1");
    assert!(request.scope.read_only);
    assert_eq!(request.cost_budget.max_turns, 4);
    assert!(request.tool_allowlist.iter().all(|call| !call.persist && !call.write_report));
}

#[tokio::test]
async fn augmenting_agent_failure_is_chain_failure_not_gate_failure() {
    let provider = Arc::new(RecordingAgentProvider::err(
        AgentProviderError::BudgetExceeded { limit: "turns" },
    ));
    let chain = ExtractChain::new(vec![
        Box::new(RegexExtractor::builtin()),
        Box::new(AgentExtractor::new(provider)),
    ])
    .expect("valid chain");

    let result = chain.run(&cli_event("Remember refund shard alpha.")).await.unwrap();
    assert!(!result.failures.is_empty());
}
```

- [ ] Run the red worker tests.

```bash
cargo test -p cairn-core --test pipeline_extract_agent agent_extractor_
cargo test -p cairn-core --test pipeline_extract_agent augmenting_agent_failure_
```

Expected failure: `AgentExtractor` does not exist.

- [ ] Implement `AgentExtractor` in `crates/cairn-core/src/pipeline/extract/agent/mod.rs`.

```rust
pub struct AgentExtractor {
    provider: Arc<dyn AgentProvider>,
    budget: ExtractBudget,
}

impl AgentExtractor {
    #[must_use]
    pub fn new(provider: Arc<dyn AgentProvider>) -> Self {
        Self {
            provider,
            budget: ExtractBudget::agent_default(),
        }
    }

    #[must_use]
    pub fn with_budget(mut self, budget: ExtractBudget) -> Self {
        self.budget = budget;
        self
    }

    fn spawn_request(&self, event: &CaptureEvent, spans: &[TextSpan]) -> AgentSpawnRequest {
        AgentSpawnRequest {
            identity: AgentIdentity::new("agt:cairn-extractor:v1").expect("static id"),
            scope: AgentScope::read_only(),
            tool_allowlist: AgentToolAllowlist::read_only_cairn(),
            cost_budget: AgentCostBudget {
                max_turns: self.budget.max_turns.unwrap_or(4),
                max_tool_calls: self.budget.max_tool_calls().unwrap_or(4),
                max_cost_units: self.budget.max_tokens.unwrap_or(2048),
            },
            wall_clock_budget: AgentWallClockBudget {
                max_millis: self.budget.max_wall_ms.unwrap_or(30_000),
            },
            output_schema: AgentOutputSchema::Json(AGENT_EXTRACTOR_OUTPUT_SCHEMA.to_owned()),
            prompt: render_agent_extract_prompt(event, spans),
        }
    }
}
```

If `pipeline::extract::ExtractBudget` has no `max_tool_calls()` helper, add a private mapper:

```rust
fn tool_budget_from_extract_budget(budget: &ExtractBudget) -> u32 {
    budget.max_turns.unwrap_or(4).max(1)
}
```

- [ ] Implement `ExtractorWorker` for `AgentExtractor`.

```rust
#[async_trait::async_trait]
impl ExtractorWorker for AgentExtractor {
    fn name(&self) -> &'static str { "agent" }
    fn role(&self) -> WorkerRole { WorkerRole::Augmenting }
    fn budget(&self) -> ExtractBudget { self.budget }

    async fn extract(&self, event: &CaptureEvent) -> Result<ExtractResult, ExtractError> {
        let spans = event.default_eligible_spans();
        let request = self.spawn_request(event, &spans);
        let run = self.provider.spawn(request).await.map_err(|source| {
            ExtractError::AgentProvider {
                worker: "agent",
                source,
            }
        })?;
        let Some(AgentOutput::Json(value)) = run.output else {
            return Err(ExtractError::Malformed {
                worker: "agent",
                reason: "agent run completed without JSON output".to_owned(),
            });
        };
        let parsed = parse_agent_response(event, value).map_err(|source| {
            ExtractError::Malformed {
                worker: "agent",
                reason: source.to_string(),
            }
        })?;
        Ok(ExtractResult {
            outputs: parsed.drafts,
            discards: parsed.discards,
            truncated: false,
            llm_eligible_spans: vec![],
        })
    }
}
```

Match the actual `ExtractResult` field names in `crates/cairn-core/src/pipeline/extract/mod.rs`; keep the mapping explicit.

- [ ] Extend `ExtractError` in `crates/cairn-core/src/pipeline/extract/mod.rs`.

```rust
#[error("{worker} agent provider failed: {source}")]
AgentProvider {
    worker: &'static str,
    #[source]
    source: crate::contract::agent_provider::AgentProviderError,
},
```

- [ ] Add `ExtractBudget::agent_default()` if the type has existing `regex_default()` and `llm_default()` constructors.

```rust
pub const fn agent_default() -> Self {
    Self {
        max_tokens: Some(4096),
        max_wall_ms: Some(30_000),
        max_turns: Some(4),
    }
}
```

- [ ] Add chain builder tests in `crates/cairn-core/src/pipeline/extract/build.rs`.

```rust
#[test]
fn build_chain_rejects_agent_entry_without_provider() {
    let mut config = ExtractConfig::default();
    config.chain.push(ExtractorEntry {
        worker: ExtractorWorkerKind::Agent,
        kinds: vec![],
        trigger: None,
        budget: crate::config::ExtractBudget::default(),
    });

    let err = build_extract_chain(&config, ExtractProviders::default())
        .expect_err("provider required");
    assert!(matches!(err, ExtractBuildError::MissingAgentProvider));
}
```

- [ ] Implement `crates/cairn-core/src/pipeline/extract/build.rs`.

```rust
#[derive(Default, Clone)]
pub struct ExtractProviders {
    pub llm: Option<Arc<dyn LLMProvider>>,
    pub agent: Option<Arc<dyn AgentProvider>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractBuildError {
    #[error("extract chain entry uses llm worker but no LLMProvider is wired")]
    MissingLlmProvider,
    #[error("extract chain entry uses agent worker but no AgentProvider is wired")]
    MissingAgentProvider,
    #[error(transparent)]
    InvalidChain(#[from] ChainBuildError),
}

pub fn build_extract_chain(
    config: &ExtractConfig,
    providers: ExtractProviders,
) -> Result<ExtractChain, ExtractBuildError> {
    let mut workers: Vec<Box<dyn ExtractorWorker>> = Vec::new();
    for entry in &config.chain {
        match entry.worker {
            ExtractorWorkerKind::Regex => workers.push(Box::new(RegexExtractor::builtin())),
            ExtractorWorkerKind::Llm => {
                let llm = providers.llm.clone().ok_or(ExtractBuildError::MissingLlmProvider)?;
                workers.push(Box::new(LLMExtractor::new(llm).with_budget(entry.budget.into())));
            }
            ExtractorWorkerKind::Agent => {
                let agent = providers.agent.clone().ok_or(ExtractBuildError::MissingAgentProvider)?;
                workers.push(Box::new(AgentExtractor::new(agent).with_budget(entry.budget.into())));
            }
            _ => {}
        }
    }
    ExtractChain::new(workers).map_err(ExtractBuildError::InvalidChain)
}
```

Adjust the conversion from config budget to pipeline budget according to existing type boundaries.

- [ ] Export builder from `crates/cairn-core/src/pipeline/extract/mod.rs`.

```rust
pub mod build;
pub use build::{ExtractBuildError, ExtractProviders, build_extract_chain};
pub use agent::AgentExtractor;
```

- [ ] Run task tests.

```bash
cargo test -p cairn-core --test pipeline_extract_agent
cargo test -p cairn-core pipeline::extract::build
cargo test -p cairn-core pipeline::extract::chain
```

Expected pass: agent worker is augmenting, returns `MemoryDraft` and `DiscardCandidate`, policy request is read-only, and augmenting failures remain chain failures with regex fallback preserved.

## Task 4 - Bundled `cairn-agent-core` Provider Runtime

- [ ] Add failing runtime tests in `crates/cairn-agent-core/tests/provider.rs`.

```rust
#[tokio::test]
async fn provider_rejects_unlisted_tool_before_executor_runs() {
    let llm = SequenceLlm::new(vec![serde_json::json!({
        "action": "tool",
        "tool": {
            "verb": "ingest",
            "write_report": false,
            "persist": false
        },
        "args": {}
    })]);
    let executor = RecordingToolExecutor::default();
    let provider = CairnAgentProvider::new(Arc::new(llm), Arc::new(executor.clone()));
    let request = test_request_with_allowlist(AgentToolAllowlist::read_only_cairn());

    let run = provider.spawn(request).await.expect("run is reported");

    assert!(matches!(run.status, AgentRunStatus::Aborted));
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::ToolNotAllowed { .. })
    ));
    assert_eq!(executor.calls(), 0);
}

#[tokio::test]
async fn provider_returns_final_json_and_consumed_budget() {
    let llm = SequenceLlm::new(vec![
        serde_json::json!({
            "action": "tool",
            "tool": {"verb": "search", "write_report": false, "persist": false},
            "args": {"q": "refund shard alpha"}
        }),
        serde_json::json!({
            "action": "final",
            "output": {
                "drafts": [],
                "discards": [],
                "evidence": [{"tool": "search", "claim": "found one record"}]
            }
        }),
    ]);
    let executor = RecordingToolExecutor::with_output(serde_json::json!({
        "results": [{"id": "mem_1", "snippet": "refunds use shard alpha"}]
    }));
    let provider = CairnAgentProvider::new(Arc::new(llm), Arc::new(executor));

    let run = provider.spawn(test_request()).await.expect("spawn");

    assert_eq!(run.status, AgentRunStatus::Completed);
    assert_eq!(run.consumed.turns, 2);
    assert_eq!(run.consumed.tool_calls, 1);
    assert!(matches!(run.output, Some(AgentOutput::Json(_))));
}
```

- [ ] Create `crates/cairn-agent-core/Cargo.toml`.

```toml
[package]
name = "cairn-agent-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "Bundled bounded Cairn AgentProvider runtime."

[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["rt", "macros", "time"] }

[dev-dependencies]
cairn-core = { workspace = true, features = ["test-helpers"] }
```

Add workspace dependency in root `Cargo.toml`:

```toml
cairn-agent-core = { path = "crates/cairn-agent-core", version = "0.0.1" }
```

- [ ] Create `src/action.rs` with strict action parsing.

```rust
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentAction {
    Tool {
        tool: AgentToolCall,
        #[serde(default)]
        args: serde_json::Value,
    },
    Final {
        output: serde_json::Value,
    },
}

pub fn parse_action(value: serde_json::Value) -> Result<AgentAction, AgentProviderError> {
    serde_json::from_value(value).map_err(|source| AgentProviderError::InvalidOutput {
        reason: source.to_string(),
    })
}
```

- [ ] Create `src/tool.rs`.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecution {
    pub output: serde_json::Value,
    pub cost_units: u64,
}

#[async_trait::async_trait]
pub trait AgentToolExecutor: Send + Sync {
    async fn execute(
        &self,
        call: &AgentToolCall,
        args: serde_json::Value,
    ) -> Result<ToolExecution, AgentProviderError>;
}

pub struct CairnCliToolExecutor {
    command: String,
}

impl CairnCliToolExecutor {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into() }
    }
}
```

Implement CLI execution with `std::process::Command` inside `tokio::task::spawn_blocking`. Only support:

- `CairnVerb::Search` -> `cairn search --json` with query fields from the tool request
- `CairnVerb::Retrieve` -> `cairn retrieve --json` with record/key fields from the tool request
- `CairnVerb::Lint` with `write_report == false` and `persist == false` -> `cairn lint --dry-run --json` with lint target fields from the tool request

Before executing, call `evaluate_tool_policy(&request.scope, &request.tool_allowlist, call)` in the provider. The executor must still reject any unsupported verb defensively.

- [ ] Create `src/provider.rs`.

```rust
pub struct CairnAgentProvider {
    llm: Arc<dyn LLMProvider>,
    tools: Arc<dyn AgentToolExecutor>,
    capabilities: AgentProviderCapabilities,
}

impl CairnAgentProvider {
    #[must_use]
    pub fn new(llm: Arc<dyn LLMProvider>, tools: Arc<dyn AgentToolExecutor>) -> Self {
        Self {
            llm,
            tools,
            capabilities: AgentProviderCapabilities {
                honors_cost_budget: true,
                scope_enforced: true,
                mcp_tools: false,
                cli_subprocess_tools: true,
            },
        }
    }
}
```

Runtime loop:

```rust
for turn in 0..request.cost_budget.max_turns {
    meter.record_turn()?;
    let completion = self.llm.complete(CompletionRequest {
        prompt: render_turn_prompt(&request, &tool_history),
        max_tokens: Some(request.cost_budget.max_cost_units.min(u64::from(u32::MAX)) as u32),
        temperature: Some(0.0),
        response_schema: None,
    }).await.map_err(map_llm_error)?;

    let value = serde_json::from_str::<serde_json::Value>(&completion.text)
        .map_err(|source| AgentProviderError::InvalidOutput { reason: source.to_string() })?;
    match parse_action(value)? {
        AgentAction::Final { output } => return complete_json_run(request, meter, output),
        AgentAction::Tool { tool, args } => {
            evaluate_tool_policy(&request.scope, &request.tool_allowlist, &tool)?;
            meter.record_tool_call()?;
            let result = self.tools.execute(&tool, args).await?;
            meter.record_cost_units(result.cost_units)?;
            tool_history.push(compact_tool_output(&tool, result.output));
        }
    }
}
```

When budget is exceeded, return an `AgentRun` with `status: AgentRunStatus::Aborted`, `output: None`, `consumed` copied from the meter, current `tool_attempts`, and `abort_error: Some(AgentProviderError::BudgetExceeded { limit })` rather than surfacing an outer `Err`; use outer `Err` only when the request itself is invalid before a run can be created.

- [ ] Implement `AgentProvider` for `CairnAgentProvider`.

```rust
#[async_trait::async_trait]
impl AgentProvider for CairnAgentProvider {
    const CONTRACT_VERSION_RANGE: ContractVersionRange =
        ContractVersionRange::new(CONTRACT_VERSION, CONTRACT_VERSION);

    fn capabilities(&self) -> &AgentProviderCapabilities {
        &self.capabilities
    }

    async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
        request.validate()?;
        self.run_loop(request).await
    }
}
```

- [ ] Add plugin registration with manifest in `crates/cairn-agent-core/src/lib.rs`.

If the registry requires a default constructible provider, register an unavailable default provider whose manifest is truthful and whose `spawn` returns `ProviderUnavailable`. Use `CairnAgentProvider::new` for configured runtime wiring in CLI.

```rust
pub use provider::CairnAgentProvider;
pub use tool::{AgentToolExecutor, CairnCliToolExecutor, ToolExecution};

const MANIFEST_TOML: &str = r#"
name = "cairn-agent-core"
contract = "AgentProvider"
version = "0.0.1"
"#;

cairn_core::register_plugin!(AgentProvider, UnconfiguredCairnAgentProvider, "cairn-agent-core", MANIFEST_TOML);
```

- [ ] Run runtime tests.

```bash
cargo test -p cairn-agent-core
cargo test -p cairn-core contract::conformance::agent_provider
```

Expected pass: read-only policy is enforced before tool execution, budgets are metered, final JSON is validated through the contract, and conformance still routes `AgentProvider`.

## Task 5 - CLI Provider Wiring And Plugin Discovery

- [ ] Add `cairn-agent-core` dependencies.

`crates/cairn-cli/Cargo.toml`:

```toml
cairn-agent-core = { workspace = true }
```

`crates/cairn-workflows/Cargo.toml`:

```toml
cairn-agent-core = { workspace = true }
```

Only keep the workflows dependency if workflow tests need direct test construction. Runtime construction can stay in CLI and pass `Arc<dyn AgentProvider>` into workflows.

- [ ] Add failing plugin host tests in `crates/cairn-cli/src/plugins/host.rs`.

Change `register_all_succeeds_and_populates_seven_plugins` to expect eight plugins and include `"cairn-agent-core"`.

Change sorted manifest assertion to:

```rust
vec![
    "cairn-agent-core",
    "cairn-frontend-logseq",
    "cairn-frontend-obsidian",
    "cairn-frontend-vscode",
    "cairn-mcp",
    "cairn-sensors-local",
    "cairn-store-sqlite",
    "cairn-workflows",
]
```

- [ ] Register the new plugin first in alphabetical order.

```rust
cairn_agent_core::register(&mut reg)?;
cairn_frontend_logseq::register(&mut reg)?;
```

- [ ] Update `crates/cairn-cli/src/plugins/list.rs` capability rendering for `AgentProvider`.

```rust
ContractKind::AgentProvider => registry.agent_provider(name).map_or_else(
    || serde_json::json!({}),
    |p| {
        let c = p.capabilities();
        serde_json::json!({
            "honors_cost_budget": c.honors_cost_budget,
            "scope_enforced": c.scope_enforced,
            "mcp_tools": c.mcp_tools,
            "cli_subprocess_tools": c.cli_subprocess_tools,
        })
    },
),
```

Leave `LLMProvider` empty unless a bundled LLM provider is added by another task.

- [ ] Add provider builder in `crates/cairn-cli/src/mcp.rs`.

```rust
use cairn_core::contract::{AgentProvider, LLMProvider};

fn workflow_agent_provider(
    config: &CairnConfig,
    llm: Option<Arc<dyn LLMProvider>>,
) -> Option<Arc<dyn AgentProvider>> {
    config.agent_provider.kind.as_ref()?;
    let llm = match llm {
        Some(provider) => provider,
        None => {
            tracing::warn!("workflow agent provider unavailable: no LLM provider");
            return None;
        }
    };
    let tools = Arc::new(cairn_agent_core::CairnCliToolExecutor::new(
        config.agent_provider.command.clone(),
    ));
    Some(Arc::new(cairn_agent_core::CairnAgentProvider::new(llm, tools)))
}
```

- [ ] Pass `agent_provider` into `DreamHandler`.

```rust
let llm_provider = workflow_llm_provider(config);
let agent_provider = workflow_agent_provider(config, llm_provider.clone());
let dream_handler =
    DreamHandler::new(store_dyn.clone(), config.dream, llm_provider.clone(), agent_provider.clone())
        .with_skillify_jobs(job_store.clone());
```

- [ ] Update MCP readiness.

```rust
let dream_ready = config.dream.enabled
    && match config.dream.requires_agent_provider() {
        true => agent_provider.is_some(),
        false => llm_provider.is_some(),
    };
```

Add `DreamConfig::requires_agent_provider()` in `crates/cairn-core/src/config/dream.rs`:

```rust
#[must_use]
pub fn requires_agent_provider(&self) -> bool {
    [self.light_sleep, self.rem_sleep, self.deep_dreaming]
        .iter()
        .any(|tier| matches!(tier.worker, DreamWorkerMode::Agent))
}
```

- [ ] Run plugin and MCP tests.

```bash
cargo test -p cairn-cli plugins::host
cargo test -p cairn-cli plugins::list
cargo test -p cairn-cli mcp
cargo run -p cairn-cli -- plugins list --json
cargo run -p cairn-cli -- plugins verify
```

Expected pass: plugin list includes `cairn-agent-core`, agent capabilities are visible, and verify does not pass a manifest with no conformance coverage.

## Task 6 - Dream Planning Seam And Agent Dream Worker

- [ ] Add failing dream tests in `crates/cairn-workflows/tests/dream.rs`.

```rust
#[tokio::test]
async fn agent_dream_outputs_evidence_and_budget_metadata() {
    let store = seeded_store_with_records(vec![
        record("mem_a", "Refund routing uses shard alpha."),
        record("mem_b", "Refund routing shard alpha was confirmed in incident review."),
    ]).await;
    let agent = Arc::new(RecordingAgentProvider::ok(AgentRun {
        id: "run-dream-1".to_owned(),
        status: AgentRunStatus::Completed,
        output: Some(AgentOutput::Json(serde_json::json!({
            "body": "Refund routing uses shard alpha and has repeated confirmation.",
            "evidence": [
                {"tool": "search", "record_id": "mem_a", "claim": "first source"},
                {"tool": "retrieve", "record_id": "mem_b", "claim": "confirmation source"}
            ]
        }))),
        consumed: AgentBudgetConsumed { turns: 2, tool_calls: 2, cost_units: 17 },
        tool_attempts: vec![],
        abort_error: None,
    }));
    let mut config = DreamConfig::default();
    config.enabled = true;
    config.deep_dreaming.worker = DreamWorkerMode::Agent;
    config.deep_dreaming.max_tool_calls = 4;

    let handler = DreamHandler::new(store.clone(), config, None, Some(agent));
    handler.run_once(test_payload(DreamTier::DeepDreaming)).await.unwrap();

    let record = latest_dream_record(&store).await;
    let dream = record.extra_frontmatter["dream"].as_object().expect("dream metadata");
    assert_eq!(dream["worker"], "agent");
    assert_eq!(dream["budget_consumed"]["tool_calls"], 2);
    assert!(dream["evidence"].as_array().expect("evidence").len() >= 2);
}

#[tokio::test]
async fn agent_dream_budget_abort_is_permanent_without_upsert() {
    let store = seeded_store_with_records(vec![record("mem_a", "Refund routing uses shard alpha.")]).await;
    let agent = Arc::new(RecordingAgentProvider::err(AgentProviderError::BudgetExceeded {
        limit: "turns",
    }));
    let mut config = DreamConfig::default();
    config.enabled = true;
    config.deep_dreaming.worker = DreamWorkerMode::Agent;
    config.deep_dreaming.max_tool_calls = 1;

    let handler = DreamHandler::new(store.clone(), config, None, Some(agent));
    let outcome = handler.handle(test_job(DreamTier::DeepDreaming)).await;

    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    assert_eq!(count_dream_records(&store).await, 0);
}
```

- [ ] Run the red dream tests.

```bash
cargo test -p cairn-workflows --test dream agent_dream_
```

Expected failure: `DreamWorkerMode::Agent` matching and `DreamHandler` agent constructor do not exist.

- [ ] Create `crates/cairn-workflows/src/dream/plan.rs`.

```rust
use cairn_core::domain::flush_plan::{FlushMode, FlushPlan, PlanReason, PlannedMutation};

pub struct DreamFlushPlanInput {
    pub operation_id: String,
    pub record: MemoryRecord,
}

pub fn dream_record_flush_plan(input: DreamFlushPlanInput) -> FlushPlan {
    FlushPlan {
        operation_id: input.operation_id,
        mode: FlushMode::AutoApply,
        reason: PlanReason::Workflow {
            workflow: "dream".to_owned(),
        },
        mutations: vec![PlannedMutation::Upsert {
            record: input.record,
            prior_version: None,
        }],
    }
}
```

If `FlushMode::AutoApply` or `PlanReason::Workflow` differs in the actual domain model, use the existing workflow planner forms from `crates/cairn-workflows/src/planners.rs`.

- [ ] Modify `DreamHandler`.

Constructor:

```rust
pub struct DreamHandler {
    store: Arc<dyn MemoryStore>,
    config: DreamConfig,
    llm: Option<Arc<dyn LLMProvider>>,
    agent: Option<Arc<dyn AgentProvider>>,
    skillify_jobs: Option<Arc<dyn JobStore>>,
}

pub fn new(
    store: Arc<dyn MemoryStore>,
    config: DreamConfig,
    llm: Option<Arc<dyn LLMProvider>>,
    agent: Option<Arc<dyn AgentProvider>>,
) -> Self {
    Self { store, config, llm, agent, skillify_jobs: None }
}
```

Keep existing tests compiling by updating all call sites. Where a test only exercises LLM/hybrid mode, pass `None`.

- [ ] Split worker execution from side effects.

Add:

```rust
struct DreamWorkerPlan {
    body: String,
    evidence: Vec<serde_json::Value>,
    budget_consumed: serde_json::Value,
    produced_by: &'static str,
}

async fn run_dream_worker(
    &self,
    payload: &DreamPayload,
    tier_config: &DreamTierConfig,
    filtered: &[MemoryRecord],
) -> Result<DreamWorkerPlan, Box<dyn std::error::Error + Send + Sync>> {
    match tier_config.worker {
        DreamWorkerMode::Llm | DreamWorkerMode::Hybrid => self.run_llm_dream_worker(payload, tier_config, filtered).await,
        DreamWorkerMode::Agent => self.run_agent_dream_worker(payload, tier_config, filtered).await,
    }
}
```

- [ ] Implement `run_agent_dream_worker`.

Request properties:

```rust
AgentSpawnRequest {
    identity: AgentIdentity::new("agt:cairn-librarian:v2").expect("static id"),
    scope: AgentScope::read_only(),
    tool_allowlist: AgentToolAllowlist::read_only_cairn(),
    cost_budget: AgentCostBudget {
        max_turns: tier_config.max_tool_calls.max(1),
        max_tool_calls: tier_config.max_tool_calls,
        max_cost_units: u64::from(tier_config.completion_token_budget),
    },
    wall_clock_budget: AgentWallClockBudget {
        max_millis: u64::from(tier_config.max_wall_ms),
    },
    output_schema: AgentOutputSchema::Json(DREAM_AGENT_OUTPUT_SCHEMA.to_owned()),
    prompt: render_agent_dream_prompt(payload, tier_config, filtered),
}
```

Output schema:

```json
{
  "type": "object",
  "required": ["body", "evidence"],
  "properties": {
    "body": {"type": "string", "minLength": 1},
    "evidence": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["tool", "claim"],
        "properties": {
          "tool": {"type": "string"},
          "record_id": {"type": ["string", "null"]},
          "claim": {"type": "string"}
        }
      }
    }
  }
}
```

If the run returns `AgentRunStatus::Aborted`, return an error string that includes the budget or policy reason. The scheduler should classify it as permanent for policy/budget/config failures and retryable only for provider transport failures.

- [ ] Lower all dream worker outputs to `FlushPlan` before applying.

After source liveness and idempotency checks build the synthetic record as now, then:

```rust
let plan = dream_record_flush_plan(DreamFlushPlanInput {
    operation_id: format!("dream:{}:{}", payload.tier.as_str(), target_key),
    record,
});
apply_dream_plan(&*self.store, plan).await?;
```

For the first implementation, `apply_dream_plan` may apply the single upsert through `MemoryStore::upsert` after validating there is exactly one `PlannedMutation::Upsert`. Keep the public seam returning `FlushPlan` so the drainer can replace this direct apply path without changing worker logic.

- [ ] Preserve existing LLM/hybrid behavior.

Existing metadata keys must remain:

- `tier`
- `worker`
- `source_record_ids`
- `window_size_records`
- `budget`
- `produced_by`

Add these for all modes:

- `evidence`
- `budget_consumed`

For LLM/hybrid, `evidence` can be derived from source IDs and `budget_consumed` can record completion tokens if the `LLMProvider` output exposes them, otherwise `{ "tool_calls": 0 }`.

- [ ] Update `render_dream_prompt` matching.

```rust
match tier_config.worker {
    DreamWorkerMode::Llm => "llm",
    DreamWorkerMode::Hybrid => "hybrid",
    DreamWorkerMode::Agent => "agent",
}
```

- [ ] Run dream tests.

```bash
cargo test -p cairn-workflows --test dream
cargo test -p cairn-workflows dream
```

Expected pass: existing LLM/hybrid tests still pass, agent mode writes one reasoning record with evidence and budget metadata, and provider budget/policy failures do not write partial records.

## Task 7 - Acceptance Fixtures, Policy Bypass, And Fallback Coverage

- [ ] Add fixture-style extraction tests in `crates/cairn-core/tests/pipeline_extract_agent.rs`.

Use one ambiguous high-stakes text where regex yields no durable draft and the agent yields one grounded draft:

```rust
#[tokio::test]
async fn agent_extraction_improves_ambiguous_high_stakes_fixture() {
    let event = cli_event(
        "For chargeback reviews, use shard alpha only after legal approval. \
         The earlier beta shard note was wrong."
    );
    let regex_only = ExtractChain::new(vec![Box::new(RegexExtractor::builtin())])
        .expect("regex chain")
        .run(&event)
        .await
        .expect("regex result");

    let agent = Arc::new(RecordingAgentProvider::ok(agent_run_with_json(serde_json::json!({
        "drafts": [{
            "kind": "policy",
            "body": "Chargeback reviews use shard alpha only after legal approval.",
            "confidence": 0.93,
            "span": {"start": 0, "end": 66}
        }],
        "discards": [{
            "reason": "source says the beta shard note was wrong",
            "span": {"start": 68, "end": 103}
        }],
        "evidence": [{"tool": "retrieve", "claim": "source text contains legal approval condition"}]
    }))));
    let agent_chain = ExtractChain::new(vec![
        Box::new(RegexExtractor::builtin()),
        Box::new(AgentExtractor::new(agent)),
    ]).expect("agent chain");

    let improved = agent_chain.run(&event).await.expect("agent result");
    assert!(regex_only.outputs.len() < improved.outputs.len());
    assert!(improved.outputs.iter().any(|draft| draft.body.contains("legal approval")));
}
```

- [ ] Add policy bypass tests in `crates/cairn-agent-core/tests/provider.rs`.

Cover each disallowed action:

- `ingest`
- `forget`
- `lint` with `write_report = true`
- `search` with `persist = true`

Each test must assert:

- run status is aborted,
- `abort_error` is `ToolNotAllowed` or `MutatingVerbNotScoped`,
- executor call count is zero.

- [ ] Add fallback tests.

Core extraction fallback:

```rust
#[tokio::test]
async fn agent_budget_failure_preserves_regex_outputs() {
    let event = cli_event("Remember refund shard alpha.");
    let agent = Arc::new(RecordingAgentProvider::err(AgentProviderError::BudgetExceeded {
        limit: "turns",
    }));
    let chain = ExtractChain::new(vec![
        Box::new(RegexExtractor::builtin()),
        Box::new(AgentExtractor::new(agent)),
    ]).expect("chain");

    let result = chain.run(&event).await.expect("chain result");
    assert!(!result.outputs.is_empty(), "regex result remains available");
    assert_eq!(result.failures.len(), 1);
}
```

Dream fallback:

```rust
#[tokio::test]
async fn llm_dream_remains_available_when_agent_mode_is_not_configured() {
    let mut config = DreamConfig::default();
    config.enabled = true;
    config.light_sleep.worker = DreamWorkerMode::Llm;
    let handler = DreamHandler::new(store, config, Some(fake_llm()), None);
    let outcome = handler.handle(test_job(DreamTier::LightSleep)).await;
    assert!(matches!(outcome, HandlerOutcome::Complete));
}
```

- [ ] Run acceptance tests.

```bash
cargo test -p cairn-core --test pipeline_extract_agent
cargo test -p cairn-agent-core
cargo test -p cairn-workflows --test dream
```

Expected pass: selected fixture improves with agent extraction, policy bypass attempts are blocked, regex and LLM fallbacks remain available, and budget failures are surfaced without partial writes.

## Task 8 - Full Verification And Cleanup

- [ ] Run formatting.

```bash
cargo fmt --all -- --check
```

If it fails, run:

```bash
cargo fmt --all
cargo fmt --all -- --check
```

- [ ] Run focused test suite.

```bash
cargo test -p cairn-core --test pipeline_extract_agent
cargo test -p cairn-core config::tests
cargo test -p cairn-core pipeline::extract
cargo test -p cairn-core contract::conformance::agent_provider
cargo test -p cairn-agent-core
cargo test -p cairn-workflows --test dream
cargo test -p cairn-cli plugins::host
cargo test -p cairn-cli plugins::list
cargo test -p cairn-cli mcp
```

- [ ] Run workspace checks if time is acceptable.

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] Run plugin commands.

```bash
cargo run -p cairn-cli -- plugins list --json
cargo run -p cairn-cli -- plugins verify
```

Expected output:

- `plugins list --json` contains `cairn-agent-core`.
- `plugins verify` reports the agent provider conformance cases as pass or documented pending according to the existing conformance runner, never as silently skipped.

- [ ] Run diff hygiene.

```bash
git diff --check
git status --short
```

- [ ] Manual review checklist.

Confirm:

- All agent tool calls use `AgentScope::read_only()`.
- All request allowlists are `AgentToolAllowlist::read_only_cairn()`.
- Agent extraction returns the same public `MemoryDraft` and `DiscardCandidate` types as existing extractors.
- Agent dream routes through a `FlushPlan` value before write application.
- Agent dream metadata includes `worker`, `source_record_ids`, `evidence`, `budget`, `budget_consumed`, `policy_trace` where available, and `produced_by`.
- `DreamWorkerMode::Llm` and `DreamWorkerMode::Hybrid` tests still pass.
- Config validation fails closed when agent mode is selected without `agent_provider.kind`.
- Budget failures do not bypass regex/LLM fallback behavior.

## Commit Plan

Commit after each logical task when tests for that task pass:

```bash
git add Cargo.toml crates/cairn-core crates/cairn-agent-core
git commit -m "feat: add agent extractor"

git add Cargo.toml crates/cairn-cli crates/cairn-agent-core
git commit -m "feat: add bundled agent provider runtime"

git add crates/cairn-workflows crates/cairn-cli crates/cairn-core
git commit -m "feat: add agent dream worker"
```

Adjust staged paths so each commit contains only the files changed by that task.
