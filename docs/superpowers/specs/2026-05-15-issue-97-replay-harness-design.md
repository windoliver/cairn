# Issue 97 Replay Harness Design

## Context

GitHub issue #97 asks for a P0 local replay engine and golden-query fixture harness. The design source is the Cairn brief, especially §15 Evaluation and §18.c User Story Coverage. Both listed dependencies are closed upstream: #62 wires search, lint, forget, and capability-aware errors; #91 wires minimum Dream, Expiration, and Evaluation workflows.

The branch for implementation starts from `origin/main`, because the initial detached worktree was behind the merged #91 workflow changes. The upstream code already includes `cairn_core::replay`, `cairn_workflows::evaluation`, search golden tests, SQLite store tests, and dev-only fixture helpers. This issue should compose those pieces into scenario-level replay fixtures and reports rather than reimplement search, forget, or workflow primitives.

## Recommendation

Build a CI/test-only Rust replay harness first. Do not add a user-facing `cairn eval replay` CLI in this issue.

This keeps the initial surface dev-only, satisfies #97's acceptance criteria, and avoids freezing a CLI contract while the scenario manifest format is still new. A CLI can later wrap the same fixture/report model without changing the harness internals.

## Scope

In scope:

- A versioned replay fixture directory under `fixtures/v0/replay/`.
- A dev-only `cairn_test_fixtures::replay` module that loads scenario manifests, creates temp vaults, seeds deterministic records, applies replay actions, runs golden query checks, and returns machine-readable reports.
- Scenario coverage for P0 stories from §18.c: US1, US2 active session reload, US3 user memory, US4 rolling summary presence, US5 tool-call records, US7 keyword/semantic/hybrid search, and US8 record-level forget.
- A degraded keyword-only scenario where local embeddings are disabled and semantic/hybrid expectations are reported as capability rejections.
- Integration tests that verify replay output identifies failing scenario, verb, query, expected value, and actual value.

Out of scope:

- Public benchmark dashboards.
- Long-horizon multi-session coherence corpora.
- A stable CLI command.
- New production dependencies.
- Any network, cloud credential, or real embedding model requirement.

## Architecture

The harness lives in `cairn-test-fixtures`, which is already the dev-only crate for workspace fixture helpers. It uses the existing `cairn_store_sqlite` temp-store helpers and `MockEmbedder` paths used by search golden tests. It does not become a dependency of `cairn-core`.

Scenario manifests are JSON files in `fixtures/v0/replay/`. Each manifest has:

- `id`, `description`, and `stories`.
- `records` seed data with deterministic bodies and metadata.
- `actions` such as `search`, `retrieve_session`, `retrieve_turn`, and `forget_record`.
- `config` with `local_embeddings` so tests can run full-search and keyword-only variants.

The harness executes actions against a temp vault and accumulates a `ReplayReport`. Reports are plain serializable Rust structs so CI can write JSON later if needed. Each failed check records:

- `scenario_id`
- `story`
- `verb`
- `query` when applicable
- `expected`
- `actual`
- `message`

## Data Flow

1. Load a scenario manifest from `fixtures/v0/replay/<name>.json`.
2. Create a temporary vault using existing fixture helpers.
3. Seed deterministic records and trace/tool-call data into SQLite.
4. Apply actions in manifest order.
5. Normalize actual results to deterministic IDs, modes, snippets, and statuses.
6. Compare actual results to expected outcomes.
7. Return a `ReplayReport` with per-check pass/fail detail.

The report format is intentionally independent from `EvaluationWorkflow` report records. `EvaluationWorkflow` can consume these results later, but #97 only needs a local CI/release gate harness.

## Error Handling

The harness fails closed. Invalid scenario manifests return typed load errors. Unsupported actions become failed checks with explicit `verb` and `scenario_id`. Search capability mismatches are expected outcomes only when the scenario says the runtime is keyword-only. All other store or search errors become failed checks rather than panics.

Tests may use `expect("reason")` in line with repo test conventions, but the library helpers return `Result` so callers can produce reports.

## Testing

Tests are written first under `crates/cairn-test-fixtures/tests/replay_harness.rs`.

Required test coverage:

- The P0 replay scenario passes end to end with local embeddings enabled.
- The keyword-only scenario reports keyword success and semantic/hybrid `CapabilityUnavailable` outcomes.
- A deliberately failing in-memory check reports scenario, verb, query, expected, and actual fields.
- Scenario manifests deserialize from `fixtures/v0/replay/`.

Verification commands:

- `cargo nextest run -p cairn-test-fixtures --test replay_harness`
- `cargo nextest run -p cairn-cli --test search_modes_golden`
- `cargo nextest run -p cairn-workflows --test evaluation`
- `scripts/check-core-boundary.sh`

## Future CLI Path

If the manifest and report structs stabilize, a later issue can add `cairn eval replay --scenario <id> --json` as a thin wrapper over this harness. That wrapper should not change fixture semantics.
