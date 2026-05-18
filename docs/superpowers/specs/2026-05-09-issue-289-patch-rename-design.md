# Issue 289 Design: Real Flush Apply for Patch and Rename

## Summary

Issue `#289` should land as a full vertical slice, not as a shape-only enum
change. The current `cairn flush apply` path is explicitly metadata-only for
non-placeholder plans, so the branch must first wire a real apply path for
concrete mutations and then add `Patch` and `Rename` on top.

This design keeps placeholder plans on the existing metadata-only behavior for
stub-planner compatibility while making non-placeholder plans execute real store
mutations.

## Goals

- Add `PlannedMutation::Patch` and `PlannedMutation::Rename`.
- Support patching both record bodies and session metadata documents.
- Replace the current non-placeholder metadata-only apply path with real
  mutation execution.
- Reject patch failures and rename collisions atomically with typed errors.
- Preserve Cairn's versioned-record model rather than mutating rows in place.
- Update human-review rendering so the new mutations are legible in diffs.

## Non-Goals

- Rework the placeholder-plan flow. Placeholder plans remain metadata-only.
- Introduce a separate generic provenance subsystem beyond what the current
  versioned record history already provides.
- Generalize session storage onto the `MemoryStore` trait in this branch.
- Implement bulk multi-record patching beyond multiple mutations in one plan.

## Current Constraints

- `flush apply` currently warns that `MemoryStore` mutations are not wired and
  always records `apply_kind = metadata_only` for real plans.
- Session persistence currently lives on inherent `SqliteMemoryStore` methods,
  not the `MemoryStore` trait.
- `PlannedMutation` is consumed by serde round-trip tests, fixtures, and the
  markdown diff renderer, so adding variants requires coordinated updates.

## Proposed Data Model

### New mutation shapes

Add the following types in `crates/cairn-core/src/domain/flush_plan/mod.rs`:

```rust
pub enum PlannedMutation {
    // existing variants...
    Patch {
        target: PatchTarget,
        str_replace: Vec<StrReplace>,
    },
    Rename {
        record_id: TargetId,
        new_id: TargetId,
    },
}

pub enum PatchTarget {
    Record(TargetId),
    Session(SessionId),
}

pub struct StrReplace {
    pub old: String,
    pub new: String,
    pub occurrence: ReplaceOccurrence,
}

pub enum ReplaceOccurrence {
    First,
    All,
    Nth(usize),
}
```

### Target semantics

- `PatchTarget::Record(TargetId)` patches the active record for that target.
- `PatchTarget::Session(SessionId)` patches the session metadata document stored
  at the well-known target `session:{session_id}/meta`.
- `Rename { record_id, new_id }` renames a record target, not an entire session.

## Apply Design

### High-level flow

For non-placeholder plans, `cairn flush apply` should:

1. Load and validate the pending plan as today.
2. Perform pre-state drift checks against `target_hashes`.
3. Execute all mutations inside one real apply unit.
4. Publish `PlanStatus::Applied { apply_kind = full }` only after successful
   mutation execution.
5. Roll back on any mutation failure so partial writes are impossible.

Placeholder plans keep the existing metadata-only branch and warning text.

### Execution boundary

The CLI apply command should delegate real execution into a dedicated apply
helper instead of keeping mutation logic inline inside `flush.rs`. The helper
can depend on:

- `MemoryStore::get_active_by_target`
- `MemoryStore::upsert`
- `MemoryStore::tombstone`
- concrete `SqliteMemoryStore` transaction/session helpers where generic store
  support is insufficient

The branch should prefer the generic trait where possible and use SQLite-specific
helpers only for session metadata lookup and inbound graph-edge rewrites.

### Patch execution

For each `Patch` mutation:

1. Resolve the target record.
2. Read the active body.
3. Apply every `StrReplace` left-to-right against the current body string.
4. If any `old` match is missing for its requested occurrence, fail with a typed
   error and abort the entire plan.
5. Rebuild the `MemoryRecord` with the patched body.
6. Re-run record validation before persistence.
7. Persist as a new record version via normal upsert/versioning semantics.

Patch is therefore append-only in storage terms even though it is a logical
edit. The previous body remains recoverable through existing history.

### Session patch execution

Session patches follow the same body-edit flow as record patches, but target
resolution first maps `SessionId` to the session metadata target
`session:{id}/meta`.

Failure mode:

- if the session does not exist, fail with typed `SessionNotFound`
- if the session exists but the metadata record is absent, treat that as a
  missing target error rather than silently creating one

### Rename execution

For each `Rename` mutation:

1. Resolve the active source record by `record_id`.
2. Reject if an active record already exists for `new_id`.
3. Create a logically equivalent new active record under `new_id`.
4. Retire the old active target without erasing historical versions.
5. Rewrite all inbound entity-graph edges from `record_id` to `new_id` in the
   same transaction.
6. Preserve atomicity so readers never observe mixed graph state.

Rename is modeled as a target migration with history retention, not as an
in-place primary-key mutation.

## Error Model

Add typed apply/store errors for:

- patch target missing
- patch substring missing
- patch occurrence invalid or not found
- session not found
- rename target collision

The CLI should surface these as plan-apply failures and leave the plan pending
after claim rollback, matching existing failure handling.

## Human Review Rendering

Update `crates/cairn-core/src/domain/flush_plan/diff.rs`:

- `PatchTarget::Record` renders with target id and a unified-style before/after
  body diff or a deterministic textual replacement view if a true line diff is
  not yet available.
- `PatchTarget::Session` renders with `Session: <id>` to distinguish the target
  from record patches.
- `Rename` renders as `old_id -> new_id`.

The renderer must remain deterministic for snapshot-style tests.

## Testing Strategy

### TDD order

1. Add failing core serde/property tests for the new mutation shapes.
2. Add failing diff-render tests for patch and rename.
3. Add failing apply-path tests showing non-placeholder plans now require real
   execution instead of metadata-only completion.
4. Add failing SQLite integration tests for:
   - record patch success
   - record patch missing substring atomic failure
   - session patch success
   - session patch missing session failure
   - rename collision failure
   - rename inbound-edge rewrite success

### Verification expectations

- Placeholder-plan tests must remain green and still produce
  `apply_kind = metadata_only`.
- Real plan apply tests must assert `apply_kind = full`.
- Rename tests must verify both record visibility and inbound graph-edge
  rewrites under SQLite.
- Patch tests must verify the old version remains available through history.

## Implementation Plan Shape

The branch should be developed in two reviewable commits:

1. real apply infrastructure for non-placeholder plans
2. issue `#289` mutation shapes, rendering, and integration behavior

This keeps the unavoidable infra work separate from the feature-specific code
while still delivering the full vertical slice in one branch.

## Risks

- The largest technical risk is fitting rename semantics cleanly into the
  existing versioned-record and graph-edge model without inventing a parallel
  provenance system.
- Session patching crosses the generic store boundary, so SQLite-specific code
  must be kept narrow and explicit.
- If the current `MemoryStore` trait surface proves too small for real apply,
  the branch may need a minimal trait extension before mutation execution can be
  properly isolated.
