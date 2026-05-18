# Lint bitemporal edge contradictions - design

| Field | Value |
|---|---|
| Issue | [#192](https://github.com/windoliver/cairn/issues/192) - feat(lint): contradiction detection - bitemporal edge conflict surfacing and auto-resolution |
| Related | [#96](https://github.com/windoliver/cairn/issues/96) - lint checks; [#46](https://github.com/windoliver/cairn/issues/46) - MemoryStore graph edge operations |
| Brief sections | section 4 MemoryStore, section 5.6 WAL, section 8.0 lint verb |
| Date | 2026-05-05 |

## 1. Problem

`cairn lint` is currently a generated wire surface plus a CLI stub. Issue #192
adds a concrete lint check: detect live bitemporal entity edges that disagree
about the same `(source_id, target_id, relation)` triple, surface them as
structured findings, and optionally auto-resolve by invalidating the lower
confidence edge through the WAL path.

The current checkout does not yet contain the issue's assumed
`entity_edges(valid_at, invalid_at, expired_at, confidence_score, confidence)`
substrate or a persisted WAL implementation. This design therefore takes a
vertical minimal slice: add the bitemporal edge table and the WAL/ledger
operations needed by this lint check, without trying to finish the full
MemoryStore CRUD and search implementation.

## 2. Goals And Non-Goals

### Goals

1. Add first-class lint finding kinds for contradictory and ambiguous live
   entity edges across CLI, MCP, and SDK generated types.
2. Detect contradictions with a single SQL statement, not an N+1 scan.
3. Keep read-only `cairn lint` side-effect free.
4. Implement `cairn lint --fix` as an explicit mutating mode that records a
   WAL operation and invalidates losing edges in one SQLite write transaction.
5. Surface all live `AMBIGUOUS` edges as informational findings and never
   auto-resolve them.
6. Add focused tests for winner selection, SQLite detection, fix behavior,
   WAL reason recording, and CLI JSON shape.

### Non-Goals

- Full MemoryStore CRUD, FTS, vector search, record versioning, or markdown
  projection.
- Full generic WAL state machines for every operation kind.
- P1+ durable messaging or compensation across Nexus side effects.
- Backfilling existing vaults that already have a different edge schema. This
  repository has no committed SQLite migrations yet, so this issue defines the
  first narrow bitemporal edge migration.
- Automatically resolving `AMBIGUOUS` findings.

## 3. Chosen Approach

Use a vertical minimal slice.

The implementation adds the bitemporal edge substrate, lint queries, and
lint-specific WAL mutation together. This is broader than a schema-only change,
but narrower than implementing the whole MemoryStore backlog. It lets #192 meet
its acceptance criteria while keeping unrelated record-store work out of scope.

Two alternatives were considered:

1. Schema-first only: add IDL/core variants and migrations, but leave runtime
   behavior pending. This is lower risk but does not satisfy #192.
2. Full MemoryStore first: implement CRUD, versioning, graph operations, WAL,
   then lint. This is architecturally clean but too large for #192 and overlaps
   with separate store issues.

## 4. Architecture

### 4.1 `cairn-idl`

Extend `crates/cairn-idl/schema/verbs/lint.json` and regenerate outputs.

`LintArgs` gains:

- `fix: boolean` with write capability auth.

`LintDataSummary` gains:

- `ambiguous_edges: integer`
- `auto_resolved: integer`

`LintDataFindingsKind` gains:

- `contradictory_edge`
- `ambiguous_edge`

`LintDataFindings` gains optional structured fields:

- `severity: "info" | "warning" | "error"`
- `entities: string[]` for affected edge ids
- `suggestion: string`

The existing `contradiction` kind remains for wire compatibility with prior
generated output, but the new edge-specific check uses `contradictory_edge`.
Generated files are not edited by hand.

### 4.2 `cairn-core`

Add a small non-generated lint domain module. It contains pure logic only:

- `LintKind` with `ContradictoryEdge` and `AmbiguousEdge`.
- `Severity`.
- `LintFinding`.
- `EdgeConfidence` with parse/order values `Extracted > Inferred > Ambiguous`.
- `choose_edge_keeper(edges)` that selects the winner by:
  1. higher `confidence_score`,
  2. stronger `EdgeConfidence`,
  3. lexicographically smaller edge id.

The store maps SQLite rows into these core types, calls keeper selection, then
maps findings into generated response types at the verb boundary.

### 4.3 `cairn-store-sqlite`

Add migration-backed storage for this feature:

```sql
CREATE TABLE entity_edges (
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  target_id TEXT NOT NULL,
  relation TEXT NOT NULL,
  valid_at INTEGER NOT NULL,
  invalid_at INTEGER,
  expired_at INTEGER,
  confidence TEXT NOT NULL
    CHECK (confidence IN ('EXTRACTED', 'INFERRED', 'AMBIGUOUS')),
  confidence_score REAL NOT NULL
    CHECK (confidence_score >= 0.0 AND confidence_score <= 1.0),
  created_at INTEGER NOT NULL
);

CREATE INDEX entity_edges_live_triple_idx
  ON entity_edges(source_id, target_id, relation)
  WHERE invalid_at IS NULL AND expired_at IS NULL;

CREATE TABLE IF NOT EXISTS wal_ops (
  operation_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  kind TEXT NOT NULL,
  reason TEXT NOT NULL,
  envelope TEXT NOT NULL,
  issued_at INTEGER NOT NULL,
  committed_at INTEGER
);

CREATE TABLE IF NOT EXISTS replay_ledger (
  operation_id TEXT PRIMARY KEY,
  reason TEXT NOT NULL,
  committed_at INTEGER NOT NULL
);
```

The lint slice needs only committed local WAL audit rows. The broader WAL step
graph remains future work.

Store APIs:

- `lint_edges(&Connection) -> Result<EdgeLintReport, StoreError>`
- `resolve_edge_contradictions(&Connection, now, operation_id) -> Result<EdgeLintReport, StoreError>`

`lint_edges` is read-only and runs no write transaction. `resolve_edge_contradictions`
runs inside `BEGIN IMMEDIATE` and commits the WAL row, replay ledger row, and
edge invalidations atomically.

### 4.4 `cairn-cli`

The generated clap builder exposes `--fix`. The hand-written CLI handler stays
thin:

1. Parse generated `LintArgs`.
2. Load/open the vault database.
3. Dispatch read-only or fix mode.
4. Emit generated JSON envelope for `--json`, otherwise concise human output.

If the database does not contain `entity_edges`, return a typed schema/capability
error instead of reporting a clean lint run.

## 5. Detection And Fix Data Flow

### 5.1 Read-Only Lint

1. Open the SQLite database read-only when possible.
2. Run one contradiction query:

```sql
SELECT a.id AS edge_a,
       b.id AS edge_b,
       a.source_id,
       a.target_id,
       a.relation,
       a.confidence AS confidence_a,
       b.confidence AS confidence_b,
       a.confidence_score AS score_a,
       b.confidence_score AS score_b
  FROM entity_edges a
  JOIN entity_edges b
    ON a.source_id = b.source_id
   AND a.target_id = b.target_id
   AND a.relation = b.relation
   AND a.id < b.id
 WHERE a.invalid_at IS NULL
   AND b.invalid_at IS NULL
   AND a.expired_at IS NULL
   AND b.expired_at IS NULL;
```

3. Run one ambiguous-edge query:

```sql
SELECT id, source_id, target_id, relation, confidence_score
  FROM entity_edges
 WHERE invalid_at IS NULL
   AND expired_at IS NULL
   AND confidence = 'AMBIGUOUS';
```

4. Return findings and summary counts. No writes occur.

Read-only reporting emits one contradictory finding per conflicting pair. That
is the most direct representation of the SQL result and matches the issue's
two-edge finding shape.

### 5.2 Fix Mode

`cairn lint --fix` groups live edges by `(source_id, target_id, relation)` and
selects one keeper per group. Every non-keeper in a conflicting group receives
`invalid_at = now`.

This group-based write plan avoids pairwise oscillation. For three live edges
in one group, read-only lint reports three conflicting pairs; fix mode keeps
one edge and invalidates the other two in one transaction.

Transaction shape:

1. `BEGIN IMMEDIATE`.
2. Detect live conflicting groups.
3. Choose keepers via `cairn-core`.
4. Insert `wal_ops(operation_id, state='COMMITTED', kind='lint_fix',
   reason='lint:contradiction_resolution', ...)`.
5. Insert `replay_ledger(operation_id, reason, committed_at)`.
6. Update losing edges with `invalid_at = now`.
7. Commit.

If no contradictions exist, the operation returns `auto_resolved = 0` and does
not insert a WAL row.

## 6. Error Handling

- Missing `entity_edges`: typed schema/capability error.
- Missing WAL tables during `--fix`: run the migration before dispatch or
  return schema error; never partially mutate.
- Write transaction busy: fail closed with no invalidations.
- Invalid confidence string in existing data: surface a schema error tied to
  the edge id.
- Equal score and confidence: deterministic tie break by lexicographically
  smaller id.
- Ambiguous edges: `Severity::Info`, no auto-resolution.

## 7. Testing

### Core Unit Tests

- `choose_edge_keeper` prefers higher `confidence_score`.
- Equal score uses confidence ordering `EXTRACTED > INFERRED > AMBIGUOUS`.
- Equal score and confidence keeps lexicographically smaller edge id.
- Ambiguous edge findings are informational.

### SQLite Integration Tests

- Two live edges for the same triple produce exactly one contradiction finding.
- Expired or invalidated edges are ignored.
- Live `AMBIGUOUS` edges produce `AmbiguousEdge`.
- `resolve_edge_contradictions` leaves one live edge per group.
- `resolve_edge_contradictions` records `lint:contradiction_resolution` in WAL
  and replay ledger.
- Read-only lint does not change row counts or `invalid_at` values.

### CLI And Codegen Tests

- Generated CLI exposes `cairn lint --fix`.
- `cairn lint --json` returns findings and summary without writes.
- `cairn lint --fix --json` includes `auto_resolved`.
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` passes after
  generated files are committed.

## 8. Implementation Order

1. Update IDL schema and add failing generated-surface tests for `fix` and new
   finding fields.
2. Regenerate code and make codegen tests pass.
3. Add core lint domain types and failing keeper-selection tests.
4. Implement keeper-selection logic.
5. Add SQLite migration and failing store integration tests for detection.
6. Implement read-only detection queries.
7. Add failing fix-mode integration tests.
8. Implement WAL-backed fix transaction.
9. Wire the CLI handler and add CLI tests.
10. Run the focused verification set, then the workspace gates required by
    the touched crates.

## 9. Open Risks

- The current design brief's simple `edges` table and issue #192's
  bitemporal `entity_edges` table differ. This issue intentionally introduces
  `entity_edges` for bitemporal entity facts instead of repurposing the generic
  `edges` table.
- The WAL implementation here is intentionally narrow. It records committed
  local audit rows for this lint mutation but does not implement the full
  operation-step recovery engine.
- The CLI currently has no real vault-open path for lint. Implementation may
  need a small shared database-opening helper, but domain logic must stay out
  of `cairn-cli`.

## 10. Acceptance Mapping

| Acceptance criterion | Design coverage |
|---|---|
| `LintKind::ContradictoryEdge` and `LintKind::AmbiguousEdge` variants added | `cairn-core` lint module plus generated wire kind additions |
| Detection query runs in a single SQL statement | Section 5.1 contradiction query |
| `cairn lint` reports contradictions without writing | Sections 5.1 and 7 |
| `cairn lint --fix` resolves via WAL | Sections 5.2 and 7 |
| Test conflicting extractions surface exactly one contradiction | SQLite integration tests |
| Test fix leaves one live edge and WAL replay ledger has entry | SQLite integration tests |
