# Forget Session And Backup Phase B Design

## Context

The current branch already implements the supportable v0.1 `forget.record`
slice over the live SQLite store plus immutable `sources/` artifacts, including
source-forget receipts and optional source redaction. The remaining brief work
is larger than one handler:

- §5.6 defines `forget_session` as a session-scoped fan-out operation with
  Phase A tombstoning and Phase B physical purge.
- §5.6 also requires `forget_record` and `forget_session` to purge pre-images
  and retention surfaces before the forget operation reaches its committed
  terminal state.
- §3.0 operational notes require backups to be registered, rewritten after
  forget, and prevented from resurrecting forgotten targets on restore.
- §8 and §8.0.a require capability advertisement to remain phase-pinned and
  fail closed when a surface is not actually wired.

The repo does not yet have a real snapshot/backup/restore substrate, so the
remaining work must introduce that admin surface rather than pretending the
privacy invariant already holds.

## Goal

Ship the next honest forget/privacy slice in one branch:

1. Implement `forget.session` end to end on the CLI/store path.
2. Add a minimal local snapshot/backup substrate with registry tracking.
3. Rewrite tracked backups after `forget.record` and `forget.session` so
   forgotten targets cannot be resurrected from a registered backup.
4. Make restore apply current forget tombstones before the restored vault is
   reader-visible.

Acceptance target: a backup taken before forget, then restored after forget,
must not surface the forgotten target through normal reads.

## Non-Goals

- Federated or Nexus-side retention purge.
- General-purpose folder/scope forget in this slice.
- Implementing every possible admin feature that a future `snapshot` command
  may want beyond backup, rewrite, and restore.
- Claiming full `wal.purge_pre_images` coverage if the underlying pre-image
  substrate is not yet present for a given surface.

## Approaches Considered

1. **Recommended: add the missing backup substrate and wire session forget to
   it.** Build a minimal local backup/restore flow, then make both
   `forget.record` and `forget.session` invoke the same retention rewrite path.
   This matches the brief's privacy invariant and avoids shipping another
   partially-implemented forget surface.

2. **Implement `forget.session` only.** This is lower risk short term, but it
   leaves the backup resurrection path open and still does not satisfy the
   brief's stated forget-me invariant.

3. **Fake backup completion through lint-only diagnostics.** Detect stale
   backups but do not rewrite them. This is useful operationally, but it is not
   an implementation of the brief requirement and would overstate support.

Option 1 is the target for this branch.

## Proposed Design

### 1. Introduce a minimal snapshot admin surface

Add the smallest real CLI/admin substrate that can support the brief's backup
story:

- `cairn snapshot --backup <path>` creates a consistent vault backup artifact.
- each successful backup also writes a registry entry under
  `.cairn/backups/<backup_id>.json`;
- each registry entry records at minimum:
  - `backup_id`
  - `created_at`
  - `artifact_path`
  - `target_ids_included`
- `.cairn/backups/shredded.log` records invalidated backup artifacts that were
  superseded by forget-driven rewrites.

This substrate is intentionally local and file-backed. It does not need to
solve every future admin concern; it only needs to make registered backups
enumerable and rewriteable by later forget operations.

### 2. Define the backup artifact shape around current authority

The backup artifact should preserve only the current authoritative and
user-visible local surfaces:

- `.cairn/cairn.db`
- `wiki/`
- `raw/`
- `sources/`
- enough metadata to restore into a fresh vault root

For this slice, the simplest correct artifact is a filesystem copy under the
user-supplied backup path plus the registry entry that points at it. The backup
code must use a consistent SQLite copy strategy rather than a raw blind file
copy of a live database.

This design intentionally avoids inventing a new bundle format unless the
existing codebase already has a strong precedent for one.

### 3. Add backup rewrite support for forgotten targets

After live-store Phase B succeeds for either `forget.record` or
`forget.session`, Cairn must scan the backup registry and rewrite any registered
backup that includes one or more forgotten `target_id`s.

Rewrite behavior:

- restore or open the backup artifact in an isolated temp workspace;
- replay the same target purge against the backup's local vault surfaces;
- write a replacement backup artifact at a new path or replacement-safe path;
- append the superseded artifact to `.cairn/backups/shredded.log`;
- update or replace the registry entry so future rewrites target only the live
  replacement artifact.

The rewrite step must be idempotent. Re-running forget against an already
rewritten backup must not duplicate targets or leave the registry in an
ambiguous state.

### 4. Implement `forget.session` as session fan-out over existing record purge

`forget.session` should become the v0.2 live forget mode. The CLI handler stops
returning `CapabilityUnavailable` for `--session` and instead:

1. resolves all `target_id`s belonging to the requested `session_id`;
2. creates one outer forget operation id and deterministic child purge order;
3. applies the same per-target Phase A/Phase B pipeline used by
   `forget.record`;
4. emits one committed envelope summarizing deleted versions and tombstones.

For this branch, the implementation may reuse the existing record-forget helper
in a loop if that keeps semantics honest and testable. If the repo already has
real `reader_fence` substrate, the session path should use it; otherwise the
specifically supported invariant is "session-wide fan-out over the current local
store surfaces with no silent capability advertisement until the wiring is
real."

### 5. Restore must replay current tombstones before reads

Any restore path shipped in this slice must treat current forget receipts as a
post-restore reconciliation step, not as optional operator cleanup.

Restore flow:

1. materialize the backup artifact into a target vault root;
2. read the current consent/forget journal from the source of truth available
   to the restore operation;
3. replay all relevant forget tombstones against the restored vault before it
   becomes reader-visible;
4. only then mark restore success.

This is the mechanism that closes the "backup predates forget" resurrection
race described in the brief's operational notes.

### 6. Capability and phase behavior

Capability advertisement remains pinned to the brief:

- `forget.record` stays v0.1
- `forget.session` becomes wired only at `Phase::V0_2`
- `forget.scope` remains fail-closed and unadvertised until a later slice

If the branch introduces a new snapshot/admin CLI surface, it should obey the
same fail-closed principle: absent substrate means explicit operator-facing
error, never silent partial restore or best-effort backup semantics.

### 7. Honest retention-surface boundary

This branch should only claim retention coverage for surfaces it truly rewrites:

- live SQLite record store
- live `sources/` artifacts
- registered local backup artifacts created by the new snapshot path

If `wal.purge_pre_images` or other retention surfaces remain unimplemented in
the repo, the implementation should either add the minimal real support here or
document the remaining boundary in code comments, tests, and user-facing change
notes. The branch must not imply that an absent substrate was purged.

## Data Flow

### Backup

1. CLI snapshot command opens the bound vault.
2. It writes a consistent backup artifact to the requested path.
3. It records a registry entry under `.cairn/backups/`.

### Forget record/session

1. CLI forget resolves one target or a session's targets.
2. It purges the live store and source-artifact surfaces.
3. It scans registered backups for matching `target_id`s.
4. It rewrites affected backups and records superseded artifacts in
   `shredded.log`.
5. It returns one committed forget envelope.

### Restore

1. CLI restore materializes the requested backup into a target vault.
2. It replays current forget tombstones before opening that vault for normal
   reads.
3. It returns success only after the replay step is complete.

## Testing Strategy

The implementation should remain test-first and layered.

### Core and schema tests

- backup registry entry round-trips and validates required fields;
- rewrite planner is deterministic for one target and many targets;
- session-forget summary structures serialize consistently.

### CLI integration tests

- `forget --session` purges every record version in the session;
- `forget --session` records source-forget receipts and optional redactions the
  same way `forget --record` does;
- `status` advertises `forget.session` only when the contract phase and wiring
  both permit it;
- `forget.scope` remains unadvertised and unavailable.

### Backup/restore regressions

- take backup B, ingest content C, forget C, restore B, assert C is not
  retrievable;
- take backup B containing two targets, forget one, rewrite B, restore the
  rewritten backup, assert only the forgotten target is absent;
- re-run the same forget operation against an already rewritten registry entry
  and assert no duplicate shred-log ambiguity.

### Boundary tests

- when no backup registry entries exist, forget still succeeds and reports no
  rewrite work;
- malformed registry entries fail closed with actionable operator errors;
- restore refuses to expose a vault if tombstone replay fails.

## Risks and Mitigations

- **Scope expansion risk.** Snapshot/restore can balloon into a general backup
  system. Mitigation: keep the artifact model minimal and aligned only to the
  brief's forget/privacy path.
- **Rewrite correctness risk.** Backup rewrites can drift from live forget
  semantics. Mitigation: reuse the same purge helpers used for the live vault
  wherever possible.
- **Capability over-advertisement risk.** Session forget must not appear early.
  Mitigation: keep phase-pinned status tests and wire advertisement only after
  end-to-end CLI support exists.
- **Restore resurrection risk.** Backup restore can leak old content if replay
  is optional. Mitigation: make tombstone replay part of restore success, not a
  background best effort.
