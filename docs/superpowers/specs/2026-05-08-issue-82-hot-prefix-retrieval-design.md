# Issue #82 — Profile, Pinned, and Playbook Retrieval for Hot Prefix

**Status:** Approved 2026-05-08
**Issue:** [windoliver/cairn#82](https://github.com/windoliver/cairn/issues/82)
**Parent epic:** #14
**Depends on:** #81 (DataProfile synthesizer — merged)
**Successor work:** #80 (full assembler + ordering), #83 (cache + lint)
**Brief sections:** §7 Hot Memory, §7.1 AutoUserProfile, §6 Taxonomy

## Problem

`crates/cairn-core/src/verbs/assemble_hot/assembler.rs` ships with a stub
`load_step_body` that returns `""` for every recipe step. The verb produces
a syntactically-valid `AssembleHotData` of zero bytes regardless of vault
contents, so:

- Hot prefix has no content.
- Pinned `user`/`feedback` memories never reach the agent.
- The active playbook never reaches the agent.
- The lint check `hot_memory_over_budget` (§6.6) defers four of six steps
  because there is no source-side selector at all.

Issue #82 closes the gap for everything except cache + invalidation
(#83) and full cross-step ordering (#80).

## Goals (acceptance criteria)

1. Source-specific retrieval functions for the six frozen v1 recipe
   steps: `Purpose`, `Index`, `PinnedFeedback`, `TopSalienceProject`,
   `ActivePlaybook`, `RecentUserSignal`. Profile retrieval is exposed
   via `retrieve --profile` (shipped in #81); see "Profile in the hot
   prefix" below.
2. Visibility, scope, confidence, staleness, and salience scoring applied
   uniformly with normal `search` / `retrieve`.
3. Pinned records win over ordinary ranking but still obey visibility +
   forget state.
4. Debug output (per-segment included + excluded record list with typed
   reasons) explains why a memory entered or did not enter hot context.

## Non-goals

- Cache + invalidation (owned by #83).
- Cross-step ordering and global truncation (owned by #80).
- Adding `Profile` to `HotRecipeStep` — the IDL pins the enum:
  `"Frozen for the lifetime of cairn.mcp.v1. Adding a recipe step
  requires bumping to cairn.mcp.v2."` (see
  `crates/cairn-idl/schema/verbs/assemble_hot.json`). Including the
  synthesized profile inside the assembled prefix waits on the v2 bump
  and a recipe-shape decision; this issue does not touch it.
- Explicit per-record `pinned` schema column (out of scope; tracked as a
  follow-up — see [Open questions](#open-questions)).
- Rolling profile narrative (`summary` / `historical_summary`) — P1,
  produced by `DreamWorkflow`.

### Profile in the hot prefix

Brief §7's token budget table lists `AutoUserProfile summary ~400
tokens`. With the IDL recipe enum frozen in v1, we cannot add a
`Profile` recipe step in #82. Two paths remain available without
breaking wire compat, but both are out of scope here:

- **Option A (deferred):** bump to cairn.mcp.v2 and add the recipe
  step. Owner: a new design issue.
- **Option B (deferred):** synthesize the profile body into
  `purpose.md` or `index.md` so the existing recipe step carries it.
  Owner: `cairn-cli` adapter wiring (#80).

Either way, this issue keeps the synthesizer path callable via
`retrieve --profile` (already plumbed by #81) so the data is reachable
even before hot-prefix inclusion lands.

## Design decisions (Q&A)

| Question | Decision | Rationale |
|---|---|---|
| Pinned semantics | `kind ∈ {user, feedback} ∧ is_static = 1` | No schema change; closest existing signal per brief §7.1. Documented as v0.1 narrowing. |
| Profile in recipe | **Deferred** — IDL freezes the enum at v1. Profile stays on `retrieve --profile`. | `assemble_hot.json` says `Frozen for the lifetime of cairn.mcp.v1`. Adding a step is a v2 wire bump. Out of scope for #82. |
| Debug surface | Optional `debug` field on `AssembleHotData` IDL | One verb, one wire shape. Gated by `--explain` / `include_debug = true`. New optional fields are wire-compatible additions. |
| Adapter coupling | Pure-projection `HotMemoryInputs` | Mirrors profile synthesizer. Adapter pre-filters by kind/visibility/scope; core does ranking + top-K + body assembly. Keeps `cairn-core` boundary clean. |

## Architecture

```
crates/cairn-core/src/verbs/assemble_hot/
  mod.rs                  re-exports
  assembler.rs            (refactored) takes HotMemoryInputs
  segments.rs             unchanged
  raw.rs                  unchanged
  admissibility.rs        NEW — uniform per-record predicate
  inclusion.rs            NEW — InclusionTrace, ExclusionTrace, ExclusionReason
  sources/
    mod.rs
    purpose.rs            pass-through markdown
    index.rs              pass-through markdown
    pinned.rs             PinnedFeedback: salience × recency, top 8
    project.rs            TopSalienceProject: salience desc, top 6
    playbook.rs           ActivePlaybook: most-recent updated_at, top 1
    user_signal.rs        RecentUserSignal: last 24h window
```

Each `sources/<step>.rs` exposes:

```rust
pub(super) fn select(
    inputs: &HotMemoryInputs<'_>,
) -> LoadedSegment;
```

The assembler walks the recipe in `HotMemoryConfig.recipe`, calls each
`select` once, builds segments via the existing `segments::build_segments`,
re-applies the `validate` trust-boundary check, and assembles the
`AssembleHotData { prefix, segments, debug? }`.

## Public types

```rust
pub struct HotMemoryInputs<'a> {
    pub purpose_md: &'a str,
    pub index_md: &'a str,
    pub pinned_candidates: &'a [&'a MemoryRecord],
    pub project_candidates: &'a [&'a MemoryRecord],
    pub playbook_candidates: &'a [&'a MemoryRecord],
    pub user_signal_candidates: &'a [&'a MemoryRecord],
    pub now: Rfc3339Timestamp,
    pub scope: ScopeTuple,
    pub authorized_visibility: &'a [MemoryVisibility],
    pub include_debug: bool,
}

pub struct LoadedSegment {
    pub body: String,
    pub included: Vec<InclusionTrace>,
    pub excluded: Vec<ExclusionTrace>,
}

pub struct InclusionTrace {
    pub record_id: RecordId,
    pub score: f64,
    pub note: &'static str,
}

pub struct ExclusionTrace {
    pub record_id: RecordId,
    pub reason: ExclusionReason,
}

#[non_exhaustive]
pub enum ExclusionReason {
    Tombstoned,
    ForgottenScope,
    BelowConfidenceFloor,
    OutOfScope,
    VisibilityDenied,
    OutsideRecencyWindow,
    BeyondTopK,
    NotPinned,
    EmptyBody,
}
```

The existing `assemble_hot_with_loader(FnMut)` test entry point is
removed; tests rebuild via `HotMemoryInputs` constructors fed by
`cairn-test-fixtures`.

## Selection rules per source

Common admissibility (`admissibility.rs`), applied first by every source:

- `tombstoned` → `Tombstoned`
- `confidence < 0.3` → `BelowConfidenceFloor` (matches profile synthesizer)
- `record.visibility ∉ inputs.authorized_visibility` → `VisibilityDenied`
- `record.scope` not subset of `inputs.scope` → `OutOfScope`
- `body.is_empty()` → `EmptyBody`

Per-source extras + ranking:

| Source | Filter | Sort key | Cap |
|---|---|---|---|
| `Purpose` | n/a (string pass-through) | n/a | n/a |
| `Index` | n/a | n/a | n/a |
| `PinnedFeedback` | `kind ∈ {user, feedback} ∧ is_static = 1` | `salience × exp(-(now-updated_at)/30d)` desc, then `record_id` desc | top 8 |
| `TopSalienceProject` | `kind = project` | `salience` desc, then `len(body)` desc, then `record_id` desc | top 6 |
| `ActivePlaybook` | `kind = playbook` | `updated_at` desc, then `record_id` desc | top 1 |
| `RecentUserSignal` | `kind = user_signal ∧ now - updated_at ≤ 86_400s` | `updated_at` desc, then `record_id` desc | bounded by segment budget |

Determinism: tiebreakers always end on `record_id`, so identical inputs
produce byte-identical output. Forget-propagation tests rely on this.

Body rendering:

- File-backed (Purpose/Index): pass-through.
- Record-backed (Pinned/Project/Playbook/UserSignal): each record →
  `## <kind>: <first body line>\n<body>\n`. Blank line between records.

## IDL + config changes

`crates/cairn-idl/schema/verbs/assemble_hot.json`:

- **No change to `HotRecipeStep`.** Frozen for cairn.mcp.v1 per the
  schema's own description; bumping is out of scope for #82.
- Add optional `debug` property on `Data`:

  ```json
  "debug": {
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "steps": {
        "type": "array",
        "items": { "$ref": "#/$defs/HotStepTrace" }
      }
    }
  }
  ```

  Plus a new `$defs/HotStepTrace`:
  `{ step: HotRecipeStep, included: [{record_id, score, note}], excluded: [{record_id, reason}] }`.
  `null`-skipped on serialize. Optional fields are wire-compatible
  additions for cairn.mcp.v1 — older consumers ignore unknown fields.

`crates/cairn-core/src/config/mod.rs`:

- **No change.** Recipe enum and default recipe are unchanged.

`crates/cairn-core/src/verbs/lint/checks/hot_memory.rs`:

- **No new arm.** Recipe is unchanged. The runtime `inclusion.rs`
  trace produced by this issue could later feed a new lint check, but
  that is a follow-up under #259, not a #82 deliverable.

`crates/cairn-core/src/generated/`: regenerated by
`cargo run -p cairn-idl --bin cairn-codegen` for the new optional
`debug` field, and committed.

## Snapshot drift

The default recipe is unchanged. Existing snapshot tests in
`crates/cairn-core/tests/assemble_hot_snapshots.rs` may need
regeneration only if the per-record body framing (`## kind: ...`)
changes the assembled bytes for fixtures the snapshots already
exercise. Tests SHOULD pin a fixed body text to keep snapshots
deterministic across runs.

## Tests

**Unit (per source module):**

- `pinned`:
  - ranks by `salience × recency`, top-8 cap;
  - ties on score break by `record_id` desc;
  - `is_static = 0` records excluded with `NotPinned`;
  - non-`{user, feedback}` kinds excluded with `NotPinned`;
  - `confidence < 0.3` excluded with `BelowConfidenceFloor`.
- `project`:
  - top-6 by salience;
  - byte-size tiebreaker on tied salience matches lint regression;
  - non-`project` kind excluded with `NotPinned`-equivalent.
- `playbook`:
  - most-recent `updated_at` wins;
  - non-`playbook` kind excluded;
  - empty input → empty body, no error.
- `user_signal`:
  - record at `now - 86_400s` included;
  - record at `now - 86_401s` excluded with `OutsideRecencyWindow`.
- `admissibility`:
  - one minimal fixture per `ExclusionReason` variant.

**Integration (`crates/cairn-core/tests/`):**

- `assemble_hot_inputs.rs`: full default-recipe assembly with mixed-kind
  fixtures, asserts inclusion order + `bytes ≤ max_bytes`.
- `assemble_hot_debug.rs`: with `include_debug = true`, every step emits
  a `HotStepTrace`; with `false`, `data.debug` is `None`.
- `assemble_hot_privacy.rs`: tombstoned, low-confidence, out-of-scope,
  visibility-blocked records excluded across every source. A pinned
  record carrying any of these states never reaches the body.

**Property (`proptest`):**

- Determinism: `assemble_hot(inputs)` byte-equals second call with same
  inputs.
- Budget: arbitrary recipes never produce body bytes > `max_bytes`, or
  fail with `AssembleHotError::BudgetExceeded`.

**Snapshot (`insta`):**

- `assemble_hot_snapshots.rs` regenerated for the new default recipe.

**CLI smoke (`crates/cairn-cli/tests/`):**

- Build `HotMemoryInputs` from a fixture vault, call `assemble_hot`,
  assert the segment count matches the recipe length and at least one
  segment is non-empty. The full SQLite + FS adapter wiring is owned by
  #80; this issue ships the smallest test that exercises the end-to-end
  pure path.

## Verification (CLAUDE.md §8)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

## Open questions

- **Explicit `pinned` schema column.** Brief mentions `pinned: true`
  exactly once (§3852, OpenCode mapping). v0.1 reuses `is_static = 1`
  for `user`/`feedback`. A follow-up issue should decide whether to add
  a dedicated `is_pinned` column to `MemoryRecord` + the canonical
  signature surface; that decision is not part of #82.

## Risk + mitigation

- **Insta snapshot churn.** Per-record body framing may shift
  byte-counts for snapshots that include record-backed content.
  Mitigation: pin fixture bodies and verify the `## kind:` framing in
  the snapshot delta is intentional.
- **IDL deserializer drift.** New `Profile` enum variant + optional
  `debug` field both flagged `non_exhaustive` / serde `default`-skipped
  so older clients deserialize successfully.
- **Test coupling to time.** `now` is a parameter, not a clock read;
  recency-window tests pin `now` to a fixed RFC3339 timestamp.
- **Pinned narrowing too aggressive.** v0.1 maps `pinned ↔ is_static`
  for `user`/`feedback`. If a vault already carries `is_static = 0` user
  preferences, they are excluded from `PinnedFeedback`. Acceptable for
  P0 because the static-promotion classifier (#81 territory) writes
  `is_static = 1` by default for those kinds.

## Sequencing

1. IDL: add optional `debug` field + `HotStepTrace` definition;
   regenerate codegen.
2. Core: `admissibility.rs`, `inclusion.rs`, `sources/*` modules.
3. Core: refactor `assembler.rs` to take `HotMemoryInputs`.
4. Tests: unit per source, integration, property, snapshot.
5. CLI smoke: minimal fixture-driven assembly.
6. Verification sweep.
