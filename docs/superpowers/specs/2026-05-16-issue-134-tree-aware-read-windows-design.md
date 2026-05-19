# Design — issue #134 tree-aware read windows

**Status:** Approved for planning (brainstorm 2026-05-16).
**Issue:** [#134](https://github.com/windoliver/cairn/issues/134).
**Parent epic:** [#30](https://github.com/windoliver/cairn/issues/30).
**Brief refs:** §5.7 Sessions are trees, §7 Hot Memory, §8.0 core verbs.

## 1. Goal

Implement the first issue #134 slice: a reusable, deterministic tree-aware
read-window planner for `retrieve`, `summarize`, and `assemble_hot`.

The planner consumes the session-tree substrate from #133 and makes a branch
session readable as a path through the tree, not as an isolated flat log. The
same planned window is reused by all three read surfaces so ancestry, branch
locality, sibling/merge explanations, and budget trimming are consistent.

## 2. Non-goals

- New `cairn.sessiontree.v1` public verbs. Those remain governed by #30 and
  capability discovery.
- Task-trace canvas persistence or workflows (`trace_step`, `trace_canvas`,
  canvas markdown projection). The issue addendum is valid, but it should land
  after tree-aware read windows are stable.
- New LLM summarization behavior. This slice keeps P0 deterministic summary
  generation and only changes which authorized records enter the summary.
- GUI session-tree visualization. That belongs to #135.
- Direct DB mutation outside the existing session-tree substrate. This is a
  read-path feature.

## 3. Current base

`origin/main` already contains #133:

- `cairn_core::domain::session_tree` with `SessionTree`, `SessionParent`,
  lineage, children, subtree preorder, and merge validation.
- SQLite migration `0062_session_tree.sql` with `session_tree_nodes` and
  `session_tree_merges`.
- `SqliteMemoryStore::get_session_tree`, `record_session_fork`,
  `record_session_clone`, `record_session_tool_spawn`, and
  `record_session_merge`.
- Compatibility behavior: legacy flat sessions hydrate as one-node trees.

The read surfaces are already real:

- `cairn retrieve --session` loads records by `scope.session_id`, groups by
  turn, applies include flags, and trims to `search.max_snippet_chars_per_page`.
- `cairn summarize` renders deterministic P0 rollups from authorized records.
- `cairn assemble_hot --session` uses the session id today for recent
  `user_signal` records and cache keys, with segment-aware hot-prefix output.

## 4. Approach

Add a pure planner in `cairn-core`:

```text
SessionTree + target session + per-session record groups + budget policy
  -> TreeReadWindow
       ancestry_path
       selected_segments
       sibling_summaries
       merge_summaries
       deterministic trim report
       explanation strings / metadata
```

Adapters remain responsible for storage and authorization. The CLI loads only
authorized records from `MemoryStore`, then passes those records plus hydrated
`SessionTree` metadata into core. Core never queries SQLite or reads the
filesystem.

This mirrors the existing design rule: `cairn-core` owns pure behavior;
`cairn-store-sqlite` persists; `cairn-cli` wires adapters into the verb layer.

## 5. Window Semantics

### 5.1 Target and lineage

For `target_session = S`, the planner resolves:

1. `ancestry_path`: `root -> ... -> S`, inclusive.
2. `branch_local`: records whose `scope.session_id == S`.
3. `ancestor_context`: records from ancestors before the child branch boundary
   when turn ids are available. If the branch boundary is `"latest"` or the
   source data lacks turn ids, ancestor records are included as bounded summary
   context rather than pretending to know an exact cutoff.
4. `sibling_context`: sibling branch metadata only by default, plus compact
   sibling summaries when they fit the budget and are justified by a merge or
   explicit sibling relation.
5. `merge_context`: merge events where either `source == S` or
   `destination == S`, represented as compact explanatory context. A
   `reasoning_summary` merge cites `summary_record_id`; a
   `controlled_splice` merge cites the spliced turn range.

Legacy one-node trees produce the same flat session behavior as today.

### 5.2 Segment kinds

The planner classifies selected material into stable segment kinds:

| Segment kind | Purpose | Default priority |
|---|---|---:|
| `branch_local` | Direct target-session turns/records | 0 |
| `ancestor_context` | Required lineage context up to branch point | 1 |
| `merge_summary` | Explicit merge explanations and cited summaries | 2 |
| `sibling_summary` | Compact sibling branch context | 3 |

Lower priority numbers survive trimming first. Ties are broken by
`session_id`, then `turn_id`, then `record_id`, all ascending. This makes
budget trimming deterministic across platforms and runs.

### 5.3 Budget trimming

The planner trims in two passes:

1. **Segment pass:** reserve room for `branch_local` first, then add ancestor,
   merge, and sibling segments while budget remains.
2. **Record pass:** within each segment, trim record bodies at UTF-8-safe byte
   boundaries using the existing style of budget handling. Record ordering is
   stable before trimming, so repeated runs with unchanged inputs emit the
   same window.

The output includes a `TreeBudgetReport` with:

- budget bytes/chars requested by the caller,
- selected and skipped segment counts,
- selected and skipped record counts,
- `skipped_for_budget` by segment kind,
- whether sibling or merge context was omitted.

## 6. Verb Integration

### 6.1 `retrieve --session`

`retrieve --session <id>` becomes tree-aware when
`MemoryStore::get_session_tree` succeeds for that session:

1. Load and authorize records for every session on the target lineage plus
   relevant sibling/merge summary record ids.
2. Build a `TreeReadWindow`.
3. Flatten the selected records back into the existing `DataSession.items`
   shape so old clients keep working.
4. Add branch/path choices to the existing response policy trace, using
   metadata-only details. Do not leak unauthorized sibling ids or record ids.

If the store returns capability unavailable for session trees, retrieve keeps
the existing flat behavior. If the store has tree metadata but it is invalid,
the verb aborts rather than silently downgrading.

### 6.2 `summarize`

When summary inputs contain session/turn records with a common session id, the
CLI should use the same tree planner before rendering deterministic summary
data. The summary digest and facts still cite the selected source record ids;
records skipped for tree budget are not cited.

This keeps P0 offline and deterministic: the only change is source selection.

### 6.3 `assemble_hot --session`

`assemble_hot --session <id>` should be able to include compact current-branch
context when configured source material exists. This slice does not add a new
`HotRecipeStep`, because `cairn.mcp.v1` freezes the enum. Instead:

- tree-aware session context is folded into existing session-scoped sources
  where they already exist, starting with `recent_user_signal`;
- debug/explain output gets metadata-only branch choice notes where the current
  wire shape can carry them safely;
- a future `cairn.mcp.v2` can add a dedicated `current_task_context` or
  `session_tree_context` recipe step.

The hot prefix must still respect `HotMemoryConfig.max_bytes`, explicit
`--budget`, and #288 segment validation.

## 7. Response Metadata

The current `cairn.mcp.v1` retrieve/session and assemble-hot response shapes
are tight. This slice should avoid a broad schema churn unless implementation
proves it necessary.

Preferred metadata path:

- use `policy_trace` for retrieve tree choices:
  `tree.lineage`, `tree.branch_local`, `tree.merge_context`,
  `tree.sibling_context`, `tree.budget`;
- use `assemble_hot.debug` for hot-prefix tree choices only when `--explain`
  is set;
- keep metadata body-free and authorization-safe.

If a typed wire addition becomes necessary, it must be minimal, generated from
IDL, and codegen must be rerun in the same PR.

## 8. Error Semantics

| Condition | Behavior |
|---|---|
| Legacy flat session | Existing flat output. |
| Store lacks session-tree capability | Existing flat output, no branch metadata. |
| Requested session missing | Existing authorized empty/not-found behavior. |
| Tree metadata malformed | `aborted`, because branch semantics cannot be trusted. |
| Unauthorized ancestor/sibling record | Excluded before planning; no id leakage. |
| Sibling/merge context over budget | Omitted with metadata-only explanation. |

This preserves fail-closed behavior where capability is explicitly advertised
or metadata exists, while keeping older adapters compatible.

## 9. Tests

Use TDD. The first implementation plan must start with failing tests for:

- pure `TreeReadWindow` lineage and branch-local ordering,
- deterministic budget trimming with sibling and merge context,
- legacy flat-session compatibility,
- malformed tree abort behavior at the CLI/store boundary,
- `retrieve --session` fixture showing ancestry plus branch-local context,
- `assemble_hot --session --explain` showing safe branch-choice metadata when
  available.

Targeted verification for the branch:

```bash
cargo nextest run -p cairn-core tree_read_window
cargo nextest run -p cairn-store-sqlite sessions
cargo nextest run -p cairn-cli retrieve assemble_hot
cargo check --workspace --all-targets --locked
./scripts/check-core-boundary.sh
```

Before PR, run the full checklist from `AGENTS.md` that is relevant to changed
IDL/docs/generated code.

## 10. Deferred Trace Canvas

The task-trace canvas addendum should become a follow-up design and PR after
this read-window slice lands. It needs new storage tables, workflow locking,
projection rebuild, lint checks, metrics, and exact retrieve keys. Mixing that
with tree-aware read-window selection would make it difficult to review and
hard to recover if one half needs redesign.
