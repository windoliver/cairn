# Issue #134 Task-Trace Canvas Foundation

## Scope

This is the first implementation slice for the task-trace canvas addendum on
issue #134, after the tree-aware read-window work landed. It follows the design
brief sections that govern this area:

- §5.7 Sessions are trees: canvas context must remain session and branch aware.
- §7 Hot Memory: canvas summaries will later feed the current-task segment.
- §10 Continuous Learning: projection and consolidation should be workflow
  driven, not ad hoc in read paths.

The slice adds the durable `SQLite` substrate:

- `trace_steps`
- `trace_canvases`
- `trace_canvas_nodes`
- `trace_canvas_edges`

It also exposes adapter-local `SqliteMemoryStore` helpers for the next
workflow slice:

- idempotent trace-step upsert keyed by `(source_hash, tool_call_id)`
- exact result-reference lookup for trace steps
- canvas, node, and edge upserts
- active canvas context reads with deterministic node and edge ordering

The workflows crate now adds a narrow `TraceCanvasMaterializer` helper that
projects one trace step into the session's active canvas. It creates the active
canvas if needed, writes the step and node, advances the active node pointer,
and links the previous active node to the new node with a deterministic
`depends_on` edge.

The same module exposes `TraceCanvasPayload` and `TraceCanvasHandler` under
`trace_canvas.materialize_step` so the scheduler can dispatch materialization
jobs once capture enqueueing lands.

## Non-Goals

This slice does not implement workflow materialization, hot-memory selection,
lint checks, metric emission, exact retrieval keys, or search ranking changes.
Those build on the schema and adapter-local helpers once the storage invariants
are pinned.

## Invariants

- Trace-step replay is idempotent on `(source_hash, tool_call_id)`, with
  missing tool-call IDs normalized by the unique expression index.
- Canvas nodes must cite at least one source step.
- Canvas edge kinds are closed and validated by the database.
- Result references have a dedicated lookup index for later exact retrieve
  paths.
