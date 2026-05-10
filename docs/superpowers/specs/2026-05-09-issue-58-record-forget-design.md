# Record-level forget — design

- **Issue:** [#58](https://github.com/windoliver/cairn/issues/58)
- **Parent epic:** [#8](https://github.com/windoliver/cairn/issues/8) WAL, locks, replay, record-level forget
- **Phase:** v0.1 P0
- **Brief sections:** §5.6 forget_record · §14 Privacy and Consent · §15 audit invariants · §18.c US8
- **Depends on:** #57 COW upsert + expire WAL apply (landed in commit 3f9c46f8)
- **Date:** 2026-05-09

## 1. Goal

Run record-level forget through the §5.6 WAL apply path so the engine removes the target from every reader surface (records, FTS, vectors, edges, snapshots) atomically at Phase A and physically purges all body-bearing copies in Phase B, leaving only a body-free audit receipt in `consent_journal`.

This issue lands the engine — the `MemoryStore::forget_record` trait method, its `SqliteMemoryStore` implementation, the WAL apply path, the recovery shim, and the migration. The four-surface dispatch wiring (CLI / MCP / SDK / skill calling into `forget_record`) and the `wiring::FORGET_RECORD_WIRED` constant flip are deferred to issue #9 (`Implement eight core verbs in CLI and SDK`), per the existing `// deferred to issue #9` comment in `crates/cairn-cli/src/verbs/forget.rs:9`. Keeping the wiring flag at `false` until #9 lands the CLI/MCP/SDK dispatch preserves the §15 fail-closed invariant ("a capability appears in `status.capabilities` only when the runtime can honor every call against it").

## 2. Non-goals

- `forget --session` (v0.2, issue #108) and `forget --scope` (v0.3) fan-out. Both keep returning `CapabilityUnavailable` with the existing remediation hint.
- Cold-storage snapshot rewrite. v0.1 has no `nexus-data/` mirror nor `.cairn/snapshots/` directory; the `snapshot.purge` step is a no-op until issue #109 lands the cold layer.
- Per-user salt for `target_id_hash`. v0.1 ships `sha256:<hex>` of the raw target id; the brief §14 per-user salt rotation is a P1 follow-up that swaps the hash function without changing the receipt schema.
- Markdown projector and consent-log materializer integration. The §5.6 markdown projection drains itself when `records` rows are deleted (the projector reads the active set); the consent.log materializer tails `consent_journal` rowids and picks up the new `ForgetIntent` row automatically.

## 3. Source-of-truth alignment

| Brief section | Requirement | Design choice |
|---|---|---|
| §5.6 forget_record Phase A | One SQLite txn flips `tombstoned = 1` on every version of `target_id`, appends `consent_journal` row | `primary.mark_tombstone` step body fuses both writes inside the same `Transaction` the runner opens |
| §5.6 forget_record Phase B | Physical purge across vectors, FTS, edges, primary rows, WAL pre-images, snapshots | Step bodies 2–7 in `FORGET_RECORD_STEPS` (already declared in `cairn-core::wal::step_graph`) |
| §5.6 audit invariant | "no original body, frontmatter, index entry, graph edge, pre-image, or bundle copy" survives step 7 | `wal.purge_pre_images` zeroes any `wal_steps.pre_image` blob whose JSON references this `target_id` and replaces it with a `{target_id_hash, op_id, purged_at}` stub |
| §5.6 retry policy | Idempotent steps backoff 100/400/1600 ms, non-idempotent run once | Reuses existing `StepRunner` + `MAX_STEP_ATTEMPTS`; `primary.purge` is the only `idempotent: false` step in the graph |
| §5.6 lock model | Record-level forget = exclusive on entity, shared on session | Reuses `record_wal::locks::acquire_for_record` (same shape as upsert/expire) |
| §14 body-free receipt | `forget_intent` payload allowlist `{target_id_hash, op_id, purged_at}` | Emits `ConsentEvent { kind: ForgetIntent, payload: IntentReceipt { target_id_hash: sha256(target_id), scope_tier, reason_code: "user_command" } }` — already validated by `ConsentEvent::validate` and the `consent_journal_forget_receipt_body_free` trigger |
| §15 fail-closed advertise | Cap appears in `status` only when wiring constant is `true` | Flip `FORGET_RECORD_WIRED` from `false` to `true` in `cairn-core/src/status/wiring.rs` |
| §18.c US8 | "forget what I said about Y" → record disappears from retrieve / search / markdown | New integration test asserts post-forget `get`, `list`, `search_keyword`, `search_semantic` (when vector wired), `versions`, and edge endpoints all miss |

## 4. Architecture

Extend the `record_wal` module that #57 added.

```text
MemoryStore::forget_record(target)
  -> record_wal::apply_forget_record(target)
     -> issue PREPARED wal_ops row + persist ForgetPayload to wal_payloads
     -> run FORGET_RECORD_STEPS through StepRunner
        step 0  primary.mark_tombstone   [idem, fused: tombstone + consent receipt]
        step 1  vector.drain             [idem]
        step 2  fts.drain                [idem]
        step 3  edges.drain              [idem]
        step 4  primary.purge            [non-idem]
        step 5  wal.purge_pre_images     [idem]
        step 6  snapshot.purge           [idem, P0 no-op]
     -> finalize COMMITTED, return ForgetReceipt { target_id_hash, op_id, purged_at }
```

Recovery picks up after the last `DONE` step. Phase A (step 0) is a single SQLite transaction so a crash before commit leaves no tombstone and no consent row; a crash after commit leaves both, and Phase B retries idempotently from step 1. Step 4 (`primary.purge`) is `idempotent: false` because it is a destructive `DELETE`; once it succeeds the recovery shim never re-runs it, but the same `target_id` can be re-purged across operations because the `wal_payloads.kind` row is keyed by `operation_id`.

## 5. Module changes

### 5.1 `cairn-core` (additive)

- `wal::step_graph` already declares `FORGET_RECORD_STEPS` and `WalKind::ForgetRecord`. No changes here.
- `contract::memory_store`:
  - Add `async fn forget_record(&self, target: &TargetId, actor: &Identity) -> Result<ForgetReceipt, StoreError>` with a default impl returning `"capability unavailable: forget_record"`. Adapters that ship Phase A+B opt in; v0.1 fixture stores keep the default. The `actor` parameter is the resolved principal that authored the forget intent (passed in by issue #9's CLI/MCP/SDK dispatch); the engine writes it into the `consent_journal.actor` column on the receipt row.
  - Add `pub struct ForgetReceipt { target_id_hash: String, op_id: String, purged_at: i64 }` (body-free; matches the brief §14 forget-receipt allowlist).
  - Bump `CONTRACT_VERSION` 0.5.0 → 0.6.0 with a doc comment explaining the structural addition.
- `status::wiring::FORGET_RECORD_WIRED` stays `false` — flipped to `true` by issue #9 once the CLI/MCP/SDK dispatch lands.
- `status::REMEDIATION` for `cairn.mcp.v1.forget.record` is unchanged.

### 5.2 `cairn-store-sqlite` (additive)

- New `record_wal/forget.rs` mirroring `record_wal/expire.rs`:
  - `apply_forget_record(store, target, actor) -> Result<ForgetReceipt, StoreError>` constructs the op id, acquires `RecordLocks`, persists the `ForgetPayload`, runs the step graph, finalizes COMMITTED.
- `record_wal/payload.rs` adds:
  - `ForgetPayload { target_id: TargetId, scope: ScopeTuple, reason_code: String, actor: Identity, scope_tier: MemoryVisibility }` and a new `RecordWalPayload::Forget` variant. (The `scope_tier` is captured at issue time inside Phase A's read so recovery uses the same value the original call would have.)
  - `wal_payloads.kind` CHECK widening from `('upsert', 'expire')` to `('upsert', 'expire', 'forget_record')` via migration `0055_wal_payloads_forget.sql`.
- `record_wal/steps.rs` adds the six new step bodies. Bodies reuse existing helpers where possible (`drain_vectors`, `drain_fts`, `drain_edges` from expire; `current_unix_ms` for timestamps).
- `record_wal/recovery.rs` (`RecordWalRegistry`) routes `WalKind::ForgetRecord` to a `RecordStepBody::new_forget(payload, locks)`.
- `record_wal/mod.rs` re-exports `apply_forget_record`.
- New `store/forget.rs` exposes `SqliteMemoryStore::forget_record(target)` (parallel to `expire`); `trait_impl.rs` wires it onto the trait.
- `consent.rs::append` is unchanged — the call site inside `primary.mark_tombstone` builds the `ConsentEvent` and invokes `append` against the same `Transaction`. (`append` already takes a `&Connection`; we pass the txn's underlying connection through the standard `&*tx` deref pattern that existing fused writes use.)

### 5.3 `cairn-cli` (no changes in this issue)

`verbs/forget.rs` keeps its existing stub: dry-run / human-review still flow through the placeholder planner, and the `record_id` / `session_id` / `scope` branches all return `CapabilityUnavailable`. Issue #9 will replace the stub with a real call into `store.forget_record(target).await` and flip `FORGET_RECORD_WIRED` to `true` once the matching MCP and SDK dispatchers land.

### 5.4 Migration `0055_wal_payloads_forget.sql`

```sql
-- Migration 0055: widen wal_payloads.kind to allow forget_record.
-- Issue #58: record-level forget needs a durable payload like upsert/expire.

-- SQLite cannot ALTER a CHECK constraint in place; recreate the table.
CREATE TABLE wal_payloads_new (
  operation_id TEXT NOT NULL PRIMARY KEY
                    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL CHECK (kind IN ('upsert', 'expire', 'forget_record')),
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);

INSERT INTO wal_payloads_new SELECT * FROM wal_payloads;

DROP TRIGGER IF EXISTS wal_payloads_kind_matches_wal;
DROP TRIGGER IF EXISTS wal_payloads_immutable;
DROP TRIGGER IF EXISTS wal_payloads_no_delete;
DROP TABLE wal_payloads;
ALTER TABLE wal_payloads_new RENAME TO wal_payloads;

-- Recreate the three triggers from 0053 unchanged.
CREATE TRIGGER wal_payloads_kind_matches_wal ...;
CREATE TRIGGER wal_payloads_immutable ...;
CREATE TRIGGER wal_payloads_no_delete ...;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (55, '0055_wal_payloads_forget', '', strftime('%s','now') * 1000);
```

## 6. Step body details

### 6.1 `primary.mark_tombstone` (Phase A — fused atomic)

```sql
UPDATE records
   SET active = 0,
       tombstoned = 1,
       tombstone_reason = 'forget',
       updated_at = ?ms
 WHERE target_id = ?target;
```

Same transaction inserts a `consent_journal` row via `consent::append` with:

```rust
ConsentEvent {
    consent_id: ulid(),
    kind: ConsentKind::ForgetIntent,
    actor: store_actor(),                // identity of the caller; from incarnation
    subject: format!("hash:{sha256_hex}"), // same hash as payload.target_id_hash
    scope: scope_canonical_wire(&scope),
    op_id: Some(op_id.to_string()),
    sensor_id: None,
    payload: ConsentPayload::IntentReceipt {
        target_id_hash: format!("sha256:{sha256_hex}"),
        scope_tier: visibility_for_target(&target)?,
        reason_code: "user_command".to_owned(),
    },
    decided_at: now_rfc3339(),
    expires_at: None,
}
```

`scope_tier` is read from the active record's visibility column inside the same SELECT before the UPDATE so the receipt records the tier the row carried at forget time.

### 6.2 `vector.drain` / `fts.drain` / `edges.drain`

Identical SQL to the expire bodies in `record_wal/steps.rs`. Refactor those into shared free functions (`drain_vectors_for_target`, etc.) so both kinds call the same code.

### 6.3 `primary.purge` (Phase B step 4 — destructive)

```sql
DELETE FROM record_vectors WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?);
DELETE FROM pending_embeddings WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?);
DELETE FROM records WHERE target_id = ?;
```

The vector / pending DELETE statements are repeated here even though `vector.drain` already ran them — `vector.drain` could have failed and been retried, and the audit invariant test treats this step as the final guarantee. Both deletes are idempotent.

### 6.4 `wal.purge_pre_images`

```sql
SELECT operation_id, step_ord, pre_image
  FROM wal_steps
 WHERE pre_image IS NOT NULL
   AND pre_image LIKE '%' || ?target || '%';
```

For each row, parse the `pre_image` JSON, replace it with `{"purged": true, "target_id_hash": "sha256:...", "op_id": "...", "purged_at": ms}`, and `UPDATE wal_steps SET pre_image = ?stub WHERE operation_id = ? AND step_ord = ?`. The `wal_steps_state_transition` trigger does not fire on `pre_image` updates; the `wal_steps_identity_immutable` trigger guards `operation_id`/`step_ord`/`step_kind` only — `pre_image` is intentionally mutable for exactly this reason.

`LIKE '%target%'` is a coarse prefilter; the JSON parse step verifies actual containment so unrelated rows are not stubbed. The stub blob is JSON so a future replay validator can recognize it.

### 6.5 `snapshot.purge`

P0 has no `.cairn/snapshots/` directory and no `nexus-data/` mirror. Body checks for any registered cold-snapshot path (currently always empty) and marks DONE. When issue #109 introduces the snapshot registry, this step body grows the bundle-rewrite logic; the FORGET_RECORD_STEPS graph does not change.

## 7. Wire and capability surface

The CLI / MCP / SDK / skill behavior is **unchanged** by this issue. `status.capabilities` does **not** list `cairn.mcp.v1.forget.record` yet — the `wiring::FORGET_RECORD_WIRED` constant stays `false`. All four surfaces continue to return `CapabilityUnavailable` for `forget --record`. Issue #9 lands the dispatch wiring and the wiring-flag flip in one PR so the §15 fail-closed invariant is preserved.

## 8. Tests

Add `crates/cairn-store-sqlite/tests/forget_record.rs` (mirroring `cow_upsert_expire.rs`):

1. **Happy path** — upsert + forget; assert `get`, `list`, `versions`, keyword search, edges all miss; assert `wal_ops` has one COMMITTED `forget_record` row with seven DONE steps; assert one `consent_journal` row with `kind = forget_intent`, body-free payload, matching `op_id`.
2. **Idempotent re-forget** — call `forget_record` twice on the same target; second call returns `Ok(receipt)` with the same `target_id_hash` and a fresh `op_id` (the `wal_payloads` row is per-op, not per-target) and the SQL state stays identical (no-op tombstones, no-op deletes).
3. **Crash during Phase A** — abort before the txn commits (drop the connection mid-step); reopen, run `record_wal::recovery`; assert no tombstone and no consent row exist (Phase A is all-or-nothing).
4. **Crash during Phase B** — leave the WAL in `PREPARED` after `primary.mark_tombstone` DONE; reopen, run recovery; assert all seven steps land DONE and the op transitions to COMMITTED.
5. **WAL pre-image scrub** — perform an upsert that stages a pre_image referencing `target`, then forget; assert no `wal_steps.pre_image` blob contains the original target body bytes after step 5.
6. **Receipt body-free** — load the `consent_journal` row; assert `ConsentEvent::validate()` passes, the JSON form has no field from `ConsentEvent::BANNED_FIELDS`, and `payload.target_id_hash` matches `sha256(target_id)`.
7. **Status capability still unwired** — `cairn-core/src/status/tests.rs` continues to assert that `cairn.mcp.v1.forget.record` does not appear when `FORGET_RECORD_WIRED` is `false`; no snapshot churn from this issue.
8. **CLI/MCP/SDK still return CapabilityUnavailable** — existing snapshot tests for `cairn forget --record`, the MCP forget tool, and the SDK forget call stay green. Issue #9 will rewrite those snapshots when it flips the wiring flag.

## 9. Migration safety

- `0055_wal_payloads_forget.sql` recreates the `wal_payloads` table inside one transaction; existing rows survive the copy. The migration smoke test exercises this on a populated DB to catch any FK or trigger drift.
- The new `MemoryStore::forget_record` default impl returns the not-supported sentinel, so dyn-only consumers compile without changes. Adapters that opt in implement the method; the contract version bump (0.5 → 0.6) ensures the handshake refuses adapters built against the old surface.
- `FORGET_RECORD_WIRED` is checked by `cairn-core::status::advertise()`; flipping it to `true` requires the dispatch path to honor every call. The CI `status::tests::wired_caps_appear` test already encodes this invariant.

## 10. Open follow-ups (out of scope)

- Per-user salt rotation for `target_id_hash` (P1, brief §14).
- Cold-snapshot bundle rewrite for `snapshot.purge` (issue #109).
- `forget_session` Phase A fence + per-child Phase B fan-out (issue #108).
- Markdown projector reaction to `forget` (currently passive — projector reads active set, so deleted rows vanish; but a `--fix-markdown` lint pass should explicitly exercise this after #58 lands).
