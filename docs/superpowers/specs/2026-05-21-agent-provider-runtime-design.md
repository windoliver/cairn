# AgentProvider Runtime Design - Issue #124

**Date:** 2026-05-21
**Issue:** [#124 - Define AgentProvider contract and constrained agent runtime](https://github.com/windoliver/cairn/issues/124)
**Brief sections:** section 4 AgentProvider; section 5.2.a AgentExtractor; section 10.2 DreamWorker
**Status:** Approved

---

## 1. Scope

Implement issue #124 in one PR by replacing the current `AgentProvider` forward stub with a
real contract surface, pure policy enforcement, and conformance coverage for constrained
agent-mode workers.

This PR adds:

- `AgentProvider::spawn` with a request that carries identity, scope, tool allowlist, cost
  budget, wall-clock budget, and output schema.
- Typed agent run outputs and typed agent errors for allowlist, mutating-scope, budget,
  wall-clock, schema, and unavailable-provider failures.
- Pure policy helpers that validate `cairn` CLI tool calls before any runtime invokes them.
- A minimal deterministic runtime harness for conformance tests that can exercise tool
  policy and budget accounting without making real LLM calls.
- An `AgentProvider` conformance runner with tier-1 registration checks and tier-2 safety
  cases for allowlist, budget exhaustion, and WAL-routed writes.

Out of scope: external agent runtime adapters, network/model calls, a real `AgentExtractor`,
`AgentDreamWorker`, and any `LLMProvider` contract expansion. The current checkout still
exposes `LLMProvider` as a surface-only trait, so this PR does not invent a hidden
completion API. Real model execution can plug into the same request/result shape once
`LLMProvider` exposes completion.

---

## 2. Architecture

The implementation keeps `cairn-core` pure and limits runtime behavior to deterministic
contract tests.

| Layer | Location | Responsibility |
|---|---|---|
| Contract types | `cairn-core::contract::agent_provider` | Public request, scope, budget, tool, output, result, and error types. |
| Policy validation | `cairn-core::contract::agent_provider` | Pure checks for allowlisted tools, mutating verbs, and budget accounting. |
| Conformance runner | `cairn-core::contract::conformance::agent_provider` | Tier-1 registry checks and tier-2 safety cases. |
| Test runtime | Unit tests in `agent_provider.rs` and conformance tests | Scripted agent steps that simulate proposed CLI calls and outputs. |

`cairn-core` owns the contract and safety invariants only. It does not spawn subprocesses,
open vault files, call MCP, or call an LLM. Future runtime crates can implement the same
trait by shelling out to `cairn search`, `cairn retrieve`, and `cairn lint --dry` after
passing the pure policy checks.

---

## 3. Contract Shape

`AgentProvider` becomes the P2 spawn boundary:

```rust
#[async_trait::async_trait]
pub trait AgentProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &AgentProviderCapabilities;
    fn supported_contract_versions(&self) -> VersionRange;

    async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError>;
}
```

`AgentSpawnRequest` includes:

- `identity`: stable agent identity string, such as `agt:cairn-librarian:v2`.
- `scope`: agent-mode read/write scope, including whether mutating verbs are permitted.
- `tool_allowlist`: allowed `cairn` CLI tool calls.
- `cost_budget`: maximum turns, tool calls, and token-cost units.
- `wall_clock_budget`: maximum elapsed runtime.
- `output_schema`: expected output mode for the final result.
- `prompt`: opaque task text supplied by the caller.

`AgentRun` includes:

- `status`: completed or aborted.
- `output`: text, JSON value, or empty when aborted before output.
- `budget_consumed`: turns, tool calls, and token-cost units consumed.
- `tool_calls`: attempted tool calls with policy outcome.
- `policy_trace`: compact events explaining why calls were allowed or rejected.

The existing capability struct remains boolean and gains fields only when they describe
static provider behavior. Runtime limits belong in `AgentSpawnRequest`, not capabilities.

---

## 4. Tool Policy

Agent mode treats `cairn` CLI verbs as data before execution. The default read-only tool
set is:

- `search`
- `retrieve`
- `lint --dry`

Mutating verbs are:

- `ingest`
- `summarize` when `persist` or an equivalent write path is requested
- `capture_trace`
- `forget`

A tool call is allowed only when:

1. The verb is present in `tool_allowlist`.
2. Required fixed arguments match the allowlist entry, such as `lint --dry`.
3. If the verb mutates the vault, `scope.mutations` explicitly grants that verb.
4. The run still has enough remaining tool-call and cost budget.
5. The wall-clock budget has not expired before the call is admitted.

Denied calls produce typed errors and a policy trace entry. They do not reach subprocess,
MCP, store, or workflow layers.

---

## 5. Runtime Data Flow

`AgentProvider::spawn` follows this sequence:

1. Validate identity, scope, allowlist, budgets, output schema, and prompt.
2. Create a metered run context with zero consumed turns, tool calls, and cost units.
3. For each agent step, charge the turn or cost unit before admitting more work.
4. Validate each proposed tool call through the pure tool policy.
5. Reject unallowlisted tools and mutating verbs without explicit write scope.
6. Record every attempted call and policy outcome in the run trace.
7. Validate the final output against the requested output mode.
8. Return `AgentRun` with completion or the typed abort reason.

The minimal runtime used in this PR is deterministic: tests script the proposed steps and
expected output. This proves the contract safety rules without depending on an unimplemented
LLM completion method.

---

## 6. WAL Boundary

Agent-mode code cannot mutate the vault directly. It has only two write paths:

- Return a proposed plan or output that an extractor/dream worker later applies through the
  normal workflow and WAL path.
- Invoke a mutating `cairn` CLI verb only when that verb is both allowlisted and granted by
  scope, which sends the write through the same CLI/envelope/WAL path as external callers.

The contract result should make this visible. A mutating attempt records whether it was
denied, proposed as a WAL-routed action, or admitted because the scope explicitly granted
that verb. Conformance tests assert there is no "direct write" outcome.

---

## 7. Error Handling

Errors are typed in `AgentProviderError`:

- `InvalidRequest`: malformed identity, empty allowlist entry, impossible budget, or invalid
  output schema.
- `ToolNotAllowed`: attempted verb was not in the allowlist.
- `MutatingVerbNotScoped`: mutating verb was allowlisted but not explicitly granted write scope.
- `BudgetExceeded`: turn, tool-call, or token-cost budget was exceeded before completion.
- `WallClockExceeded`: elapsed time crossed the wall-clock budget.
- `InvalidOutput`: final runtime output did not match the requested output mode.
- `ProviderUnavailable`: provider cannot run in the current build or configuration.

Budget and policy errors abort cleanly and return the consumed budget and policy trace
captured before the abort. They do not panic and do not partially apply writes.

---

## 8. Conformance

`cairn-core::contract::conformance` gains an `agent_provider` module and routes
`ContractKind::AgentProvider` to it instead of returning `no_conformance_runner`.

Tier-1 cases mirror the existing contract runners:

- manifest matches host contract version
- registry returns a stable `Arc`
- capability getters and name/version checks are self-consistent
- manifest feature flags match runtime capability fields

Tier-2 cases cover the issue acceptance criteria:

- `allowlist_rejects_unlisted_tool`: a scripted `forget` call with only read tools
  allowlisted returns `ToolNotAllowed`.
- `mutating_verb_requires_scope`: an allowlisted `ingest` call without write scope returns
  `MutatingVerbNotScoped`.
- `budget_exhaustion_aborts_cleanly`: a scripted run that exceeds a small turn or tool budget
  returns `BudgetExceeded` with consumed budget and no direct writes.
- `writes_are_wal_routed`: a scoped mutating call is represented as a WAL-routed action, not
  as direct vault mutation.

These cases can run entirely inside `cairn-core` using a scripted provider implementation.

---

## 9. Testing

Tests are written with the implementation and stay focused on the new contract surface:

1. Unit tests for request validation and budget accounting.
2. Unit tests for read-only default allowlists.
3. Unit tests for mutating verb detection, including `summarize` write-mode detection.
4. Unit tests for output schema validation modes.
5. Registry tests for `AgentProvider` conformance routing.
6. Conformance tests using a scripted provider.
7. Root export tests updated for the new public types when they are part of the public
   contract surface.

Verification for the PR:

```sh
cargo test -p cairn-core agent_provider
cargo test -p cairn-core conformance
cargo test -p cairn-core contract_root_exports
scripts/check-core-boundary.sh
```

Run broader workspace verification before publishing the PR if unrelated failures do not
block it:

```sh
cargo nextest run --workspace
cargo test --doc --workspace
```

---

## 10. Non-Goals And Future Work

This PR does not create a production `cairn-agent-core` runtime crate. If implementation
requires subprocess execution or async runtime dependencies beyond what `cairn-core` already
uses, that belongs in a follow-up runtime crate. The first PR should keep the safety contract
complete and independently testable.

Follow-up work can add:

- A real runtime crate that uses `LLMProvider` once completion exists in the trait surface.
- Agent-mode `AgentExtractorWorker`.
- Agent-mode `AgentDreamWorker`.
- External adapters for in-harness or third-party agent loops.
- CLI or config wiring for selecting an active `AgentProvider`.
