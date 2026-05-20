# Issue 313 Salience Decay Design

## Goal

Implement issue #313 end to end: access-frequency salience strengthening, decay-curve salience erosion, guardrailed auto-eviction through the existing forget path, pin support, config knobs, telemetry, and lint visibility.

## Design Sources

- Design brief section 3: vault layout, record frontmatter, configurable retention and hot-memory recipe.
- Design brief section 5.1: read path and ranking inputs.
- Design brief section 5.6: every mutation goes through WAL-backed store authority.
- Design brief section 10: background workflows and workflow drainer.
- Design brief section 11: salience as a prioritization and evolution signal.
- Design brief section 14: consent journal and forget guardrails.

## Architecture

The implementation uses the existing `MemoryRecord.salience` field as the durable score and adds the missing lifecycle machinery around it. Salience math lives in `cairn-core` as pure functions so all callers share the same closed-form behavior. SQLite owns persistence through new store operations that update salience metadata under the same store authority as other mutations. Read paths call access tracking after successful hits, and decay runs as a workflow batch.

Auto-eviction never deletes directly. The decay planner produces eviction candidates only when all guardrails pass: below threshold, older than the configured minimum age, not pinned, and consent permits forgetting. The executor routes each candidate through the existing record-forget implementation so tombstones, audit behavior, backup registry handling, and source-forget protections stay centralized.

## Data Model

`MemoryRecord.salience` remains the score of record. New records continue to be initialized at `0.5`.

The SQLite `records` table gains:

- `last_accessed_at_ms INTEGER`: null for never-accessed records.
- `pinned INTEGER NOT NULL DEFAULT 0`: operator pin flag, independent of the existing hot-memory `pinned` tag.

The existing tag/frontmatter `pinned` behavior remains supported for hot-memory selection. For eviction guardrails, the durable `pinned` column is authoritative, with a compatibility fallback that treats existing `pinned` tags or `extra_frontmatter.pinned == true` as pinned.

## Core Pure Functions

`cairn-core::pipeline::salience` exposes:

- `apply_access(salience: f32) -> f32`
- `decay_salience(salience: f32, decay_rate: f32, days_since_last_access: u32) -> f32`
- `should_auto_evict(candidate: &EvictionCandidate, policy: &EvictionPolicy) -> bool`

The functions clamp outputs to `[0.0, 1.0]`, reject non-finite policy values through constructors, and have proptests for monotonicity and bounds.

## Store Contract

The `MemoryStore` contract gets narrow salience operations:

- `record_access(record_ids, accessed_at_ms, reason)` strengthens each active, non-tombstoned record and stamps `last_accessed_at_ms`.
- `decay_salience_batch(now_ms, policy, limit)` decays eligible rows and returns records that qualify for auto-eviction.
- `pin_record(record_id, pinned)` flips the durable pin flag.

SQLite implements these operations. Other stores get fail-closed defaults until they implement the contract.

## Read-Path Wiring

- `search`: access tracking runs for committed hit record IDs after ranking.
- `assemble_hot`: access tracking runs for included loaded records.
- `retrieve`: record-level retrieval tracks the target record once retrieve dispatch exists in the surface being called.

Access tracking is best-effort and off the response-critical path where an async runtime is available. If background spawning is unavailable in a short-lived CLI path, the update runs after response assembly but before process exit; failures are logged and do not turn successful reads into errors.

Telemetry span: `workflow.access_tracker` with `record_id`, `old_salience`, `new_salience`, and `reason`.

## Decay Workflow

`cairn-workflows` gains a concrete salience decay workflow that:

1. Leases or receives a scheduled daily job.
2. Calls the store batch decay method with config policy.
3. For each eviction candidate, re-checks guardrails and calls existing `forget_record`.
4. Emits `workflow.decay` telemetry with records processed, evicted, and retained.

The workflow is idempotent over a day-sized window because each row stores `last_accessed_at_ms` and decay uses elapsed days from that marker or `updated_at_ms`.

## Config

Config gains:

- `vault.salience.decay_rate`, default `0.05`
- `vault.salience.eviction_threshold`, default `0.10`
- `vault.salience.min_age_days`, default `30`
- `vault.salience.batch_limit`, default `500`

CLI/config parsing follows the existing `.cairn/config.yaml` precedence.

## Forget And Pin CLI

`cairn forget --pin <record_id>` becomes a non-deleting command that flips the durable pin flag and exits successfully. Pinned records still appear in reads and hot-memory selection; they do not decay and cannot auto-evict.

Auto-eviction calls the existing store-facing forget helper, not a new deletion implementation.

## Lint

`cairn lint` surfaces salience and pin state in human-readable record diagnostics and JSON output. It also warns when a record has invalid salience metadata, missing migration columns, or a pinned tag/frontmatter marker that has not yet been normalized into the durable pin column.

## Testing

Tests are TDD-first:

- Core proptests for access and decay monotonicity.
- Store migration and CRUD tests for salience metadata and pinning.
- Store integration tests for decay batch guardrails.
- CLI tests for `forget --pin`.
- Search and assemble-hot tests showing access tracking changes salience.
- Workflow tests showing decay emits candidates and routes eviction through forget.
- Lint tests showing salience and pin state appear in output.

## Scope Notes

The PR attempts the full issue. If an existing surface is still intentionally stubbed, the implementation lands the shared core/store capability and wires the concrete paths that are currently dispatched, with tests documenting any remaining stubbed surface.
