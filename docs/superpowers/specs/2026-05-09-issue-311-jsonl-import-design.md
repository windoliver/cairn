# Issue 311 JSONL Import Design

**Date:** 2026-05-09

**Related issues:** `#311`, `#290`, `#188`

## Goal

Add full-scope harness transcript backfill via `cairn ingest --jsonl <path>` and include the trace-block foundation needed to preserve reasoning, tool calls, tool results, and text blocks structurally.

## Scope

This design intentionally covers two tightly-coupled layers in one branch:

1. The trace-block and reasoning-preservation model needed so imported transcripts have a correct target representation.
2. The JSONL import pipeline and CLI surface that parse historical harness transcripts and persist them idempotently.

The branch should deliver both layers together because `#311` depends on `#290` semantics for a complete import story.

## Non-Goals

- Live tailing of active transcript files.
- Cross-harness deduplication of semantically identical conversations stored in multiple formats.
- Provider-side verification of reasoning signatures.
- Full parsers for every harness. `claude-code` lands first and `generic` lands as the fallback path.

## Current State

The repository already has:

- A synchronous `cairn ingest` CLI path for body, file, folder, and URL ingest.
- A `capture_trace` JSONL reader and persistence pipeline for Cairn-native `CaptureEvent` records.
- Existing folder ingest and `FlushPlan` groundwork from `#188`.

The repository does not yet have:

- An `ingest --jsonl` CLI/schema surface.
- A transcript parser registry for external harness transcript formats.
- A `TraceBlock` representation matching `#290`.
- Default query behavior that excludes reasoning blocks while allowing opt-in retrieval.

## Architecture

### Layer A: Trace Block Foundation

Introduce a structured trace-block model in core domain code with four variants:

- `Reasoning { text, signature }`
- `Text { text }`
- `ToolUse { tool, input, id }`
- `ToolResult { tool_use_id, content, is_error }`

The branch should persist these blocks in a way that preserves order and provenance. Reasoning signatures are opaque bytes to Cairn and must round-trip byte-for-byte.

Default user-facing query paths should not surface reasoning content unless an explicit opt-in flag is passed. This keeps imported traces safe by default while preserving full fidelity in storage.

### Layer B: Transcript Import

Add `cairn ingest --jsonl <path>` as a new ingest source with optional flags:

- `--recursive`
- `--harness <name>`
- `--session-id-from <key>`
- `--limit <n>`
- existing `--dry-run`
- existing `--json`

If `--harness` is not provided, the importer auto-detects from the first non-empty line. The first supported concrete parser is `claude-code`. The generic parser is a safe fallback for lines that do not map to structured blocks.

Imported source files should be copied or referenced under `sources/transcripts/` so provenance remains inside the managed vault layout.

## Module Boundaries

### CLI Surface

Update the ingest IDL and generated bindings so `IngestArgs` can express transcript import inputs. The CLI dispatch should continue to route body/file/folder/url as it does today, but branch early into a dedicated JSONL import path when `--jsonl` is present.

### Parser Registry

Add a replay/transcript parsing module with:

- parser trait
- parser registry
- auto-detection helper
- parser-specific error type that records parser name and line number

The parser layer should output a normalized intermediate structure that the persistence layer can consume without knowing about raw harness JSON.

### Persistence Layer

Refactor the existing trace persistence flow just enough to share batching and per-session application logic between `capture_trace` and `ingest --jsonl`.

The importer should:

1. enumerate input files
2. detect parser
3. parse lines into normalized turn/session structures
4. bucket by imported session id
5. write one logical batch per imported session
6. emit summary counts

Avoid a large rewrite of `capture_trace`. Prefer extracting reusable helpers while keeping the current verb behavior stable.

## Data Model Decisions

### Session Bucketing

Historical imports are grouped by session id derived from harness data. Every imported session becomes one logical batch boundary so replay is stable and easier to reason about.

### Idempotency

The importer computes a deterministic import key from file path plus session id. Re-importing the same file/session pair should detect prior import and produce zero new records.

### Provenance

Imported records should carry provenance that makes the source explicit, including:

- `provenance.source = "jsonl_import"`
- harness name
- original transcript path or stable vault-relative copy
- reconstructed actor chain where the harness supplies enough information

### Reasoning Visibility

Reasoning blocks are stored faithfully but excluded from default search/retrieve flows. Opt-in behavior should be explicit and test-covered.

## Parser Behavior

### Claude Code

Support transcript lines shaped like Claude Code session JSONL entries with `message.role`, `message.content`, and block arrays. Map block kinds as follows:

- text block -> `TraceBlock::Text`
- tool use block -> `TraceBlock::ToolUse`
- tool result block -> `TraceBlock::ToolResult`
- reasoning or thinking block -> `TraceBlock::Reasoning`

If a line omits blocks but still represents a usable turn, fail narrowly with parser-specific diagnostics rather than silently downgrading to generic behavior in forced `claude-code` mode.

### Generic

Treat each JSONL line as a coarse private trace payload when structured block mapping is not available. This fallback is intentionally lower fidelity but still preserves migration value.

## Error Handling

Malformed JSONL must surface:

- file path
- line number
- parser name
- parse context

`--dry-run` should surface parse counts and any file/session grouping summary without writing records.

Parser auto-detection failures should be explicit rather than guessing wrong silently.

## Testing Strategy

Use TDD in this order:

1. Core trace-block round-trip tests, including reasoning signature preservation.
2. Parser tests for Claude Code and generic fallback.
3. CLI/schema tests for the new ingest args and exclusivity rules.
4. End-to-end import tests that assert:
   - mixed-block fixture import works
   - one imported session becomes one logical batch
   - second import is idempotent
   - `--dry-run` makes zero writes
   - malformed rows report parser name and line number
5. Query behavior tests showing reasoning is hidden by default and available only with explicit opt-in.

Fixture coverage should include a ten-turn transcript with text, tool use, tool result, and reasoning blocks.

## Implementation Strategy

Build in two internal stages on the same branch:

1. Land the trace-block and reasoning foundation needed by `#290`.
2. Land the JSONL import path for `#311` on top of that foundation.

This keeps the final branch aligned with the full-scope requirement while preserving a clean internal dependency order.

## Risks and Mitigations

### Risk: `capture_trace` refactor becomes too large

Mitigation: extract only narrowly-scoped helpers for shared batch application and keep the rest of `capture_trace` behavior intact.

### Risk: generated schema changes ripple into multiple crates

Mitigation: start with schema and generated wire tests first so later CLI work builds on a stable contract.

### Risk: reasoning visibility changes break existing retrieve/search expectations

Mitigation: add explicit regression tests around default exclusion before wiring import behavior.

## Success Criteria

The branch is complete when:

- `cairn ingest --jsonl <path>` exists and works for `claude-code`
- `generic` fallback exists
- reasoning blocks and signatures round-trip structurally
- imported sessions are idempotent on re-run
- dry-run mode produces counts with zero writes
- default query paths do not surface reasoning unless explicitly requested
- transcript fixture and malformed-line tests pass
