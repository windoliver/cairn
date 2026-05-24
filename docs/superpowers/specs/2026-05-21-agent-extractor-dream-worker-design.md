# Agent Extractor And Dream Worker Design - Issue #125

**Date:** 2026-05-21
**Issue:** [#125 - Implement AgentExtractor and agent-mode DreamWorker](https://github.com/windoliver/cairn/issues/125)
**Dependency:** [#124 - Define AgentProvider contract and constrained agent runtime](https://github.com/windoliver/cairn/issues/124), merged in PR #403
**Brief sections:** section 4 AgentProvider; section 5.2.a AgentExtractor; section 10.2 DreamWorker
**Status:** Approved

---

## 1. Scope

Implement the full issue #125 behavior on top of current `origin/main`. The
local worktree must start from a branch based on `origin/main` because the
merged #124 contract is required for this work.

This PR adds:

- `AgentExtractor`, an opt-in augmenting extractor for high-stakes or
  low-confidence capture events.
- Agent-mode `DreamWorker` execution for configured dream tiers.
- A bundled minimal Cairn agent provider runtime that uses `LLMProvider` for
  model turns and read-only `cairn` CLI subprocess tools for search,
  retrieve, and lint dry-runs.
- Config and capability validation that fails closed when agent modes are
  selected without a usable `AgentProvider`.
- Tests proving agent extraction improves selected fixture cases, policy
  bypasses are rejected, budgets abort cleanly, and regex/LLM/hybrid fallbacks
  remain available.

Out of scope: external agent runtimes, mutating agent tools by default,
autonomous self-evolution, and broad dream workflow rewrites unrelated to
agent mode.

---

## 2. Architecture

The implementation keeps `cairn-core` pure and places I/O in adapter crates.

| Layer | Location | Responsibility |
|---|---|---|
| Agent contract | `crates/cairn-core/src/contract/agent_provider.rs` | Already merged #124 request, output, budget, and tool policy surface. |
| Agent extraction data | `crates/cairn-core/src/pipeline/extract/agent/` | Agent output schema, prompt rendering, JSON parsing, validation, fallback-facing errors. |
| Extract chain wiring | `crates/cairn-core/src/pipeline/extract/chain.rs` and CLI construction code | Treat `AgentExtractor` as an augmenting worker after regex/LLM. |
| Agent runtime | new `crates/cairn-agent-core/` | Default minimal `AgentProvider` implementation over `LLMProvider` plus read-only CLI subprocess tools. |
| Dream mode | `crates/cairn-core/src/config/dream.rs`, `crates/cairn-workflows/src/dream/handler.rs` | Add `DreamWorkerMode::Agent` and route configured tiers through `AgentProvider`. |
| CLI/runtime host | `crates/cairn-cli/src/plugins/host.rs`, `crates/cairn-cli/src/mcp.rs`, ingest wiring | Construct the active provider from config and pass it to ingest and workflow handlers. |

`cairn-core` owns only pure data and validation. The new runtime crate owns
subprocess execution, turn loops, and LLM calls. Workflow and CLI code pass
dependency objects explicitly instead of introducing hidden global state.

---

## 3. Config And Capabilities

Agent extraction remains opt-in through `pipeline.extract.chain`:

```yaml
pipeline:
  extract:
    chain:
      - worker: regex
      - worker: llm
        trigger: confidence_below
      - worker: agent
        trigger: confidence_below
        budget:
          max_tokens: 8000
          max_wall_ms: 15000
          max_turns: 4
```

Agent dreaming remains opt-in per tier:

```yaml
dream:
  enabled: true
  deep_dreaming:
    worker: agent
    max_tool_calls: 20
    max_wall_ms: 900000
    completion_token_budget: 800000
```

The config model adds an agent provider block:

```yaml
agent_provider:
  kind: cairn-core
  command: cairn
```

Validation rules:

- `pipeline.extract.chain[].worker = agent` requires `agent_provider.kind`.
- `dream.*.worker = agent` requires `agent_provider.kind`.
- Agent modes require an `LLMProvider` because the bundled runtime uses
  `LLMProvider` for model turns.
- Agent extraction budgets must map to nonzero agent turn, tool-call,
  wall-clock, and cost budgets.
- Agent dream tiers must set nonzero `max_tool_calls`.
- Missing requirements produce typed config/capability errors. They do not
  silently downgrade to regex, LLM, or hybrid modes.

`status` advertises agent extraction and agent dream capability only when the
active config and provider capabilities make the selected mode runnable.

---

## 4. AgentExtractor Data Flow

`AgentExtractor` is an augmenting extractor. Regex remains the gating worker.
LLM remains the normal augmenting worker when configured. Agent extraction runs
only when the chain contains an agent entry and that entry's trigger matches.

For each eligible event, the extractor builds an `AgentSpawnRequest`:

- `identity`: `agt:cairn-extractor:v1`
- `scope`: `AgentScope::read_only()`
- `tool_allowlist`: `AgentToolAllowlist::read_only_cairn()`
- `cost_budget`: derived from `ExtractBudget`
- `wall_clock_budget`: derived from `ExtractBudget.max_wall_ms`
- `output_schema`: JSON
- `prompt`: deterministic prompt containing the event metadata, eligible body
  spans, downstream output schema, and instructions to cite evidence

The agent output is strict JSON:

```json
{
  "drafts": [
    {
      "kind_hint": "rule",
      "body": "Use the release checklist before publishing.",
      "confidence": 0.91,
      "source_span": { "start": 10, "end": 58 },
      "trigger_id": "agent.high_stakes"
    }
  ],
  "discards": [],
  "evidence": [
    {
      "tool": "search",
      "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
      "claim": "Existing release memory refers to the same checklist."
    }
  ]
}
```

Parsing rules:

- Drafts lower to existing `MemoryDraft`.
- Discards lower to existing `DiscardCandidate`.
- Confidence must be finite and in `[0.0, 1.0]`.
- Every text-derived draft or discard must carry a source span.
- Source spans must remain inside the current extract-chain eligibility set.
- Evidence metadata is body-free and may be attached to policy trace or
  extractor diagnostics, but it does not alter the downstream draft schema.

If parsing, policy, budget, or provider execution fails, `AgentExtractor`
records an augmenting `WorkerFailure` and returns no agent outputs. Earlier
regex/LLM outputs remain intact.

---

## 5. Agent Dream Data Flow

`DreamWorkerMode` gains `Agent`. `DreamHandler` keeps the existing workflow
shape:

1. Decode `DreamPayload`.
2. Check dream config and provider availability.
3. Collect the tier's bounded source window.
4. Apply hybrid-style duplicate pruning when useful.
5. Build a deterministic target key from tier, scope, key, and source hash.
6. Check idempotency and source liveness before invoking the worker.
7. Run the selected worker.
8. Recheck idempotency and source liveness.
9. Emit the same durable mutation shape as non-agent dream mode.

Agent mode replaces only step 7. It builds an `AgentSpawnRequest`:

- `identity`: `agt:cairn-librarian:v2`
- `scope`: `AgentScope::read_only()`
- `tool_allowlist`: `AgentToolAllowlist::read_only_cairn()`
- `cost_budget`: derived from tier token, turn, and tool-call budgets
- `wall_clock_budget`: derived from tier `max_wall_ms`
- `output_schema`: JSON
- `prompt`: deterministic prompt containing tier, key, source ids, compact
  source excerpts, expected plan schema, and evidence requirements

The agent output is strict JSON:

```json
{
  "body": "Three to five sentence Markdown distillation.",
  "evidence": [
    {
      "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
      "claim": "Primary source for the release checklist pattern."
    }
  ]
}
```

The resulting dream metadata includes:

- `worker: "agent"`
- `source_record_ids`
- `evidence`
- `budget` from config
- `budget_consumed` from `AgentRun`
- `policy_trace` from `AgentRun`, with no raw record bodies
- `produced_by: "cairn-workflows::DreamHandler"`

This issue introduces a dream planning seam used by LLM, hybrid, and agent
workers. The seam returns a `FlushPlan` containing the same durable mutation
shape the current handler applies today, usually a `PlannedMutation::Upsert`
for the deterministic reasoning record. Autonomous workflow execution may
apply that plan immediately, but every dream worker must produce the plan
before side effects so agent mode cannot introduce a direct-write bypass.

---

## 6. Default Agent Runtime

The bundled runtime crate implements a small bounded loop:

1. Validate `AgentSpawnRequest`.
2. Render a system prompt containing tool policy, budget, and output schema.
3. Ask `LLMProvider` for the next action.
4. Parse the action as either a tool call or final output.
5. Validate the tool call with `evaluate_tool_policy`.
6. Execute allowlisted read-only CLI calls by spawning the configured `cairn`
   binary.
7. Feed compact tool output back into the next model turn.
8. Abort cleanly on budget, wall-clock, policy, malformed action, provider, or
   schema errors.
9. Return `AgentRun` with consumed budget, tool attempts, and policy trace.

The default read-only CLI tools are:

- `cairn search --json ...`
- `cairn retrieve --json ...`
- `cairn lint --json` without report writing

The runtime never executes mutating verbs under the default read-only scope.
If a future caller explicitly grants mutations, the tool policy outcome must be
`AllowedWalRoutedMutation`; no runtime path may write vault files directly.

---

## 7. Policy And Privacy

Agent modes inherit the existing fail-closed pipeline rules:

- Agent output cannot widen extract-chain eligibility.
- Agent output cannot bypass PII redaction, filter, classification, scope, or
  plan/apply stages.
- Tool calls are policy-checked before subprocess execution.
- Mutating calls require both allowlist and explicit write scope.
- Policy traces and evidence metadata must not include raw record bodies.
- Dream source liveness is checked before and after agent execution.
- Budget exhaustion aborts without partial persistence.

The important invariant is that an agent can only propose drafts or plans. It
cannot make its own durable memory writes outside the normal Cairn surfaces.

---

## 8. Error Handling And Fallback

Agent extraction errors become augmenting worker failures:

- `ToolNotAllowed`
- `MutatingVerbNotScoped`
- `BudgetExceeded`
- `WallClockExceeded`
- malformed JSON output
- invalid draft/discard schema
- provider unavailable

The chain keeps earlier outputs and continues the normal fallback behavior.
Regex is still always available. LLM remains selectable when configured.

Agent dream errors do not produce a dream record or partial plan. Classification:

- Config and missing-provider errors are permanent validation failures.
- Policy bypass attempts are permanent validation failures.
- Budget exhaustion is a clean abort with no write.
- Provider unreachable may retry according to existing scheduler rules.
- Invalid agent output is permanent for that run because retrying the same
  malformed schema under the same prompt is not a safe mutation path.

---

## 9. Testing

Tests are written before implementation.

Agent extraction tests:

- A fixture case with an ambiguous high-stakes instruction produces an
  additional high-confidence `MemoryDraft` in agent mode compared with regex
  alone.
- Agent JSON output lowers to the same `MemoryDraft` shape used by regex and
  LLM extraction.
- A mutating tool attempt such as `forget` is rejected before execution.
- An out-of-span draft is dropped and recorded as an augmenting failure.
- Budget exhaustion records a worker failure and preserves regex/LLM outputs.
- Config without an active provider rejects agent extractor selection.

Agent dream tests:

- `DreamWorkerMode::Agent` emits a dream record or plan with `worker: "agent"`,
  evidence, budget, and `budget_consumed` metadata.
- Mutating tool attempts produce no dream record.
- Budget exhaustion produces no dream record.
- Missing provider is a permanent validation failure.
- LLM and hybrid dream modes continue to pass existing tests.
- Source liveness and idempotency checks still run around agent execution.

Runtime/provider tests:

- The default provider admits only `search`, `retrieve`, and `lint --dry`.
- CLI executor arguments are built without shell string interpolation.
- Tool output is compacted before re-entering the LLM prompt.
- `AgentRun::validate` rejects schema mismatches.
- `plugins verify` keeps AgentProvider capability claims truthful.

Verification commands for the implementation PR:

```sh
cargo test -p cairn-core pipeline::extract
cargo test -p cairn-core agent_provider
cargo test -p cairn-agent-core
cargo test -p cairn-workflows dream
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

---

## 10. Acceptance Mapping

| Issue requirement | Design coverage |
|---|---|
| Add AgentExtractor for high-stakes or low-confidence captures with read-only tools. | Sections 3, 4, 6, and 7. |
| Add agent-mode DreamWorker for deeper synthesis when configured. | Sections 3, 5, 6, and 7. |
| Same MemoryDraft/FlushPlan outputs as non-agent modes. | Sections 4 and 5 require lowering through existing draft and durable mutation paths. |
| Agent extraction improves selected fixture cases without bypassing policy. | Sections 4, 7, and 9. |
| Agent dream outputs include evidence and budget metadata. | Section 5. |
| Fallback to LLM/regex remains available. | Section 8. |
| Run agent extraction fixture tests. | Section 9. |
| Run policy bypass tests. | Section 9. |
| Run fallback and budget tests. | Section 9. |

---

## 11. Implementation Notes

The implementation should be split into small TDD tasks:

1. Config support and validation for `agent_provider` and `DreamWorkerMode::Agent`.
2. Pure agent extraction schema and parser.
3. `AgentExtractor` worker and extract-chain tests.
4. `cairn-agent-core` runtime and read-only CLI executor.
5. Agent dream mode in `DreamHandler`.
6. End-to-end fixture tests and conformance/snapshot updates.

No task should add production code before a failing test exists.
