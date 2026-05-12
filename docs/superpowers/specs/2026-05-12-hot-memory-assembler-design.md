# Hot Memory Assembler Design

## Context

Issue #80 implements the P0 hot-memory path from the design brief:

- §7: hot memory is the always-loaded prefix, capped at 25 KB by default.
- §7.1: the auto-built user profile contributes its summary near the top of the prefix.
- §5.0: `SessionStart` calls `assemble_hot` before the agent turn begins.
- §8.0: `assemble_hot` is one of the eight core read verbs and is surfaced through the CLI, MCP, SDK, and skill.

The issue is a full vertical slice in one PR. It includes pure assembly logic, store retrieval, SQLite cache and invalidation, CLI wiring, generated wire type updates, and tests. The issue comment also adds entity graph degree centrality as a budget-ranking input.

## Goals

- Assemble hot memory from purpose, user profile, pinned records, high-salience project memories, project state, rolling summaries, and selected playbooks.
- Preserve deterministic source ordering under identical inputs.
- Enforce byte budgets with stable truncation.
- Report source counts and truncation decisions in the response.
- Cache warm assembled prefixes and invalidate them on relevant writes.
- Add configurable god-node degree centrality to ranking, blended with existing evidence and salience signals.
- Keep `cairn-core` adapter-free and keep the CLI thin.

## Non-Goals

- Cold rehydration and richer frontend context panes remain out of scope, matching issue #80.
- Partial cache patching is out of scope. Invalidation deletes affected cache rows; the next read rebuilds them.
- P1 LLM-generated folder summaries are consumed when present, but generating them is not part of this issue.
- Rich narrative user-profile synthesis is not introduced here beyond the P0 profile payload the store can provide.

## Architecture

`cairn-core` owns the pure hot-memory model and assembly algorithm. It receives prepared source buckets, ranks items within buckets, applies configured byte budgets, and returns an assembled prefix plus metadata. It does not read files, query SQLite, or cache.

`MemoryStore` grows the minimum read surface needed by this verb: fetch hot-memory inputs, compute or return centrality data, read a hot cache entry, write a hot cache entry, and invalidate hot cache rows for write categories that can affect hot memory.

`cairn-store-sqlite` implements those methods with real SQLite tables and migrations. It queries records and graph edges, computes degree centrality over live entity edges, filters structural hubs, returns prepared inputs to core, and stores hot-cache rows keyed by session, agent, config fingerprint, and source revision fingerprint.

`cairn-cli` loads config, opens the SQLite store, calls the shared hot-memory path, and renders either JSON or compact human output. MCP and SDK continue to use the generated verb types after codegen updates.

## Source Ordering

The assembler uses this deterministic top-level order:

1. Purpose
2. AutoUserProfile summary
3. Pinned user and feedback records
4. Recent high-salience memories
5. Project state
6. Rolling summaries
7. Selected playbooks
8. Recent user signals

Within a section, records are sorted by the section-specific ranking key and then stable tiebreakers:

1. Descending blended rank
2. Descending salience
3. Descending evidence score
4. Descending updated timestamp
5. Ascending record id

The existing config recipe remains accepted. It maps legacy recipe steps to the richer sections above:

- `purpose` -> Purpose
- `index` -> Project state
- `pinned_feedback` -> Pinned user and feedback records
- `top_salience_project` -> Recent high-salience memories and project state
- `active_playbook` -> Selected playbooks
- `recent_user_signal` -> Recent user signals

The AutoUserProfile summary is always inserted after purpose when available because §7.1 makes it part of the P0 hot prefix.

## Budgeting And Truncation

The default byte budget is `vault.hot_memory.max_bytes` from config, currently 25,600 bytes. The CLI `--budget` flag can lower or raise the request budget up to the IDL hard cap of 4 MiB. The assembler operates on UTF-8 byte boundaries and never splits inside a scalar value.

Each section has a soft share based on §7's token table. Soft shares guide allocation but do not force waste: unused budget rolls forward to later sections. Purpose and profile are treated as high-priority sections and are truncated only after lower-priority record sections have been omitted.

When a section does not fit, the assembler includes whole records until the next record would exceed the remaining budget. If a single high-priority text block must be truncated, it is cut at the last valid UTF-8 boundary below the remaining byte count and receives an explicit truncation note. Omitted records and sections are reported in metadata with source type, attempted count, included count, omitted count, and reason.

## Ranking And God-Node Centrality

The base rank for hot-memory records blends the existing record signals:

- `EvidenceVector.score`
- `salience`
- recency derived from `updated_at` and `evidence.recency_half_life_days`

The issue comment adds god-node degree centrality. SQLite computes degree as incoming plus outgoing live edges where `invalid_at IS NULL`. Structural or synthetic hubs are filtered out when the entity name matches the source file stem or when all of its live edges are only `contains` or `method`.

The centrality score is normalized across the candidate set and blended into the base rank:

```text
rank = (base_rank * (1.0 - god_node_weight)) + (centrality_score * god_node_weight)
```

`god_node_weight` defaults to `0.3` and is configured at `vault.hot_memory.god_node_weight`. Values are clamped by config validation to `[0.0, 1.0]`. A weight of `0.0` disables the signal.

## Cache And Invalidation

SQLite stores hot cache rows with:

- session id
- agent id when known
- effective byte budget
- config fingerprint
- source revision fingerprint
- assembled prefix
- metadata JSON
- creation timestamp

The source revision fingerprint is derived from the latest update timestamps and ids of source categories used by hot memory, plus the graph-edge revision used for centrality. Cache hits are returned only when every key component matches.

Invalidation is conservative. Writes that can affect purpose, profile, pins, summaries, playbooks, high-salience records, user signals, or entity graph edges delete matching hot cache rows for the affected session, agent, project, or workspace. If the write scope cannot be narrowed reliably, the adapter deletes all hot cache rows for the vault.

## Wire Shape

`AssembleHotData` is extended from `{ prefix, bytes }` to include metadata:

- `sources`: array of per-source summaries with source kind, included count, omitted count, and bytes included.
- `truncation`: array of truncation or omission decisions.
- `cache`: `{ status: "hit" | "miss" | "refreshed", key: string }`.

The existing `prefix` and `bytes` fields remain required for compatibility with current callers. Codegen regenerates SDK and MCP generated files from the IDL schema.

## Error Handling

- Missing store capability returns `CapabilityUnavailable`.
- Invalid config returns the existing config error path and CLI exit code.
- Store query failures return typed store errors and become aborted response envelopes.
- Empty vaults are valid: the verb returns an empty prefix, `bytes = 0`, and metadata explaining zero source counts.

No raw record bodies are logged above `debug`.

## Testing

Core unit tests cover:

- deterministic top-level ordering
- stable within-section tiebreaking
- byte-budget enforcement on UTF-8 boundaries
- section and record omission metadata
- centrality blending and weight `0.0`

SQLite integration tests cover:

- retrieving all hot source categories
- degree centrality with structural hub filtering
- cache hit after identical inputs
- invalidation after relevant record/profile/pin/summary/playbook/edge writes

CLI tests cover:

- JSON response shape with `prefix`, `bytes`, `sources`, `truncation`, and `cache`
- human output summary
- budget override behavior

Fixture tests cover:

- default 25 KB budget
- deterministic output under identical inputs
- truncation fixture snapshots

## Review Notes

This is intentionally larger than the narrow pure-function version because the requested PR scope is full vertical implementation. The design keeps the blast radius controlled by making `cairn-core` pure and adapter-free, confining persistence and invalidation to `cairn-store-sqlite`, and preserving generated wire type ownership through the IDL.
