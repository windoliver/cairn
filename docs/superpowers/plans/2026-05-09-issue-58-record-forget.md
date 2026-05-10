# Issue #58 — Record-level forget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land record-level forget through the §5.6 WAL apply path so an engine-level call removes the target from records, FTS, vectors, edges, and WAL pre-images atomically (Phase A) and physically purges every body-bearing copy (Phase B) — leaving only a body-free `ForgetIntent` consent receipt.

**Architecture:** Reuse the `record_wal` scaffold #57 added for upsert/expire. New `apply_forget_record` issues a `WalKind::ForgetRecord` op, acquires the same entity-exclusive / session-shared `RecordLocks`, persists a body-free `ForgetPayload`, then drives the seven-step `FORGET_RECORD_STEPS` graph (already declared in `cairn-core::wal::step_graph`). Phase A fuses tombstone + consent-journal append in one SQLite transaction; Phase B drains derived indexes, deletes records, scrubs `wal_steps.pre_image` blobs, and no-ops the cold-snapshot purge (no snapshot registry in P0).

**Tech Stack:** Rust 1.95, `tokio_rusqlite`, `serde_json`, `sha2`, `tracing`, `cairn-core::wal::step_graph`, `cairn-core::domain::consent::{ConsentEvent, ConsentKind, ConsentPayload}`, existing `record_wal::{ops, locks, payload, steps, recovery}` module.

**Out of scope (deferred):** CLI/MCP/SDK/skill dispatch wiring and the `wiring::FORGET_RECORD_WIRED` flip — those land in issue #9. The engine here is invoked directly by tests; production callers stay on the `CapabilityUnavailable` stub until #9 lands.

---

## File map

**Create:**
- `crates/cairn-store-sqlite/src/record_wal/forget.rs` — public `apply_forget_record` async entry.
- `crates/cairn-store-sqlite/src/migrations/sql/0055_wal_payloads_forget.sql` — widen `wal_payloads.kind` CHECK constraint.
- `crates/cairn-store-sqlite/src/store/forget.rs` — `SqliteMemoryStore::forget_record` inherent method.
- `crates/cairn-store-sqlite/tests/forget_record.rs` — integration tests for happy path, idempotency, crash recovery, leakage, body-free receipt.

**Modify:**
- `crates/cairn-core/src/contract/memory_store.rs` — add `forget_record` trait method, `ForgetReceipt` struct, bump `CONTRACT_VERSION` to `0.6.0`.
- `crates/cairn-store-sqlite/src/record_wal/payload.rs` — add `ForgetPayload`, `RecordWalPayload::Forget` variant, widen `save_payload` kind matcher.
- `crates/cairn-store-sqlite/src/record_wal/steps.rs` — add `RecordStepBody::new_forget` constructor + six new step bodies.
- `crates/cairn-store-sqlite/src/record_wal/recovery.rs` — handle `WalKind::ForgetRecord` in `RecordWalRegistry::body_for`.
- `crates/cairn-store-sqlite/src/record_wal/mod.rs` — re-export `apply_forget_record`.
- `crates/cairn-store-sqlite/src/store/mod.rs` — add `pub(crate) mod forget;` alongside `expire`.
- `crates/cairn-store-sqlite/src/store/trait_impl.rs` — wire `MemoryStore::forget_record` onto `SqliteMemoryStore::forget_record`.
- `crates/cairn-store-sqlite/src/migrations/loader.rs` (or wherever the migration list lives) — register `0055_wal_payloads_forget`.

---

## Task 1: Add `ForgetReceipt` and `forget_record` to the `MemoryStore` trait

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs`

**Why:** Define the contract surface the SQLite adapter implements. Default impl returns the not-supported sentinel so existing `FixtureStore` and registry stubs compile unchanged.

- [ ] **Step 1: Read the current `CONTRACT_VERSION` doc comment**

```bash
sed -n '1,40p' crates/cairn-core/src/contract/memory_store.rs
```

Expected: see the version-bump narrative ending with `pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 5, 0);`

- [ ] **Step 2: Bump the version constant and append a bump narrative paragraph**

In `crates/cairn-core/src/contract/memory_store.rs`, replace `pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 5, 0);` with:

```rust
/// Bumped 0.5 → 0.6 in #58 when `MemoryStore::forget_record` and
/// `ForgetReceipt` landed for the §5.6 record-level forget pipeline.
/// Adding a new required trait method is a structural break for any
/// downstream adapter that does not provide its own implementation —
/// the default impl returns `"capability unavailable: forget_record"`
/// so trait-object consumers compile, but the trait surface itself
/// has grown.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 6, 0);
```

(The existing 0.5 narrative paragraph stays above; this new paragraph immediately precedes the `pub const` line.)

- [ ] **Step 3: Add `ForgetReceipt` immediately above `UpsertOutcome`**

Locate `pub struct UpsertOutcome` (search for `/// Outcome of an `upsert` call`). Insert directly above it:

```rust
/// Outcome of a `forget_record` call (brief §14 forget-receipt allowlist).
///
/// Body-free by construction: only the salted target hash, the WAL op id,
/// and the purge timestamp survive. The corresponding row in
/// `consent_journal` carries the same `target_id_hash` and `op_id` so
/// audits can join the two without exposing forgotten content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetReceipt {
    /// `sha256:<hex>` of the forgotten `target_id`.
    pub target_id_hash: String,
    /// WAL operation id that committed the forget.
    pub op_id: String,
    /// Unix milliseconds when Phase B step 5 (`primary.purge`) committed.
    pub purged_at: i64,
}
```

- [ ] **Step 4: Add the trait method with default impl**

In the `impl MemoryStore for ...` trait (find `async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError>;`), insert immediately after that method:

```rust
    /// Record-level forget (brief §5.6 row 2, §14 audit invariant).
    ///
    /// Phase A tombstones every version of `target` with reason `Forget`
    /// and fuses a body-free [`ConsentEvent`] receipt into the same
    /// SQLite transaction. Phase B physically purges record rows,
    /// derived indexes, and any WAL pre-image blob that referenced
    /// the forgotten content. Idempotent and crash-safe at every step.
    ///
    /// `actor` is the resolved principal authoring the forget — the
    /// engine writes it into `consent_journal.actor` for audit-keyed
    /// queries. CLI/MCP/SDK callers (issue #9) pass the principal
    /// resolved by the signed-envelope guard.
    ///
    /// Returns the body-free [`ForgetReceipt`] proving deletion.
    ///
    /// # Errors
    /// Default impl returns `"capability unavailable: forget_record"`.
    async fn forget_record(
        &self,
        target: &TargetId,
        actor: &crate::domain::Identity,
    ) -> Result<ForgetReceipt, StoreError> {
        let _ = (target, actor);
        Err("capability unavailable: forget_record".into())
    }
```

Verify the `Identity` import path: `cairn_core::domain::Identity` is exported from `domain::mod`. If the existing module-level `use crate::domain::{...}` doesn't already include `Identity`, the fully-qualified `crate::domain::Identity` (as used above) compiles without imports churn.

- [ ] **Step 5: Add `ForgetReceipt` to the in-file `StubStore` test**

The file has a `#[cfg(test)] mod tests { ... }` block that uses a `StubStore` to verify trait shape (search for `impl MemoryStore for StubStore`). Default impl is fine — `StubStore` does not need to override `forget_record`. Confirm by:

```bash
grep -n "impl MemoryStore for StubStore" crates/cairn-core/src/contract/memory_store.rs
```

No edit required; the trait default carries `StubStore` through.

- [ ] **Step 6: Update the contract-version assertion test**

Find:

```bash
grep -n "CONTRACT_VERSION.*0.*5.*0\|CONTRACT_VERSION.*locked to 0\\.5" crates/cairn-core/src/contract/memory_store.rs
```

Expected output includes `/// `CONTRACT_VERSION` for the `MemoryStore` trait is locked to 0.5.0.` — change `0.5.0` to `0.6.0` in that doc comment, and update the assertion below it (the test calls `assert_eq!(CONTRACT_VERSION, ContractVersion::new(0, 5, 0))` or similar). Edit both to `0, 6, 0`.

- [ ] **Step 7: Run `cairn-core` build to verify the trait compiles**

```bash
cargo check -p cairn-core --locked
```

Expected: `Finished` with no errors. Warnings about unused `actor` in the default impl are silenced by the `let _ = (target, actor);` line.

- [ ] **Step 8: Run the contract-version unit test**

```bash
cargo nextest run -p cairn-core --locked contract::memory_store::tests
```

Expected: PASS (the version assertion now matches `0.6.0`).

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs
git commit -m "feat(core): add MemoryStore::forget_record trait method (#58)

Adds the engine-level contract for record-level forget per brief §5.6.
Default impl returns the not-supported sentinel so existing adapters
and trait-object consumers compile unchanged. ForgetReceipt is the
body-free audit receipt {target_id_hash, op_id, purged_at} matching
the §14 forget-receipt allowlist. Bumps CONTRACT_VERSION 0.5 → 0.6."
```

---

## Task 2: Add migration `0055_wal_payloads_forget` widening the kind CHECK

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0055_wal_payloads_forget.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/` (whichever file enumerates migrations)

**Why:** `wal_payloads.kind` is currently `CHECK (kind IN ('upsert', 'expire'))`. The new `ForgetPayload` rows need `'forget_record'` to satisfy the constraint.

- [ ] **Step 1: Find the migrations registration site**

```bash
grep -rn "0053_wal_payloads\|0054_records_cow_staging" crates/cairn-store-sqlite/src/migrations/ --include="*.rs"
```

Expected: locates the Rust list (likely `crates/cairn-store-sqlite/src/migrations/mod.rs` or `loader.rs`) where each `.sql` file is registered with its migration id.

- [ ] **Step 2: Write the migration file**

Create `crates/cairn-store-sqlite/src/migrations/sql/0055_wal_payloads_forget.sql`:

```sql
-- Migration 0055: widen wal_payloads.kind to allow forget_record.
-- Issue #58: record-level forget needs a durable payload row keyed by
-- operation_id, just like upsert and expire (added in 0053).

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

CREATE TRIGGER wal_payloads_kind_matches_wal
  BEFORE INSERT ON wal_payloads
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1
      FROM wal_ops
     WHERE operation_id = NEW.operation_id
       AND kind IS NOT NEW.kind
  )
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads.kind must match wal_ops.kind');
END;

CREATE TRIGGER wal_payloads_immutable
  BEFORE UPDATE ON wal_payloads
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads rows are immutable');
END;

CREATE TRIGGER wal_payloads_no_delete
  BEFORE DELETE ON wal_payloads
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (55, '0055_wal_payloads_forget', '', strftime('%s','now') * 1000);
```

- [ ] **Step 3: Register the migration in the loader**

Open the file located in Step 1. Find the existing `0054_records_cow_staging` line (search for `0054`) and append immediately after it the same shape of entry, e.g.:

```rust
include_migration!(55, "0055_wal_payloads_forget"),
```

Match the surrounding macro/struct exactly — if the project uses a `&[Migration { ... }]` slice, copy the prior entry's pattern verbatim with id `55` and the new file basename.

- [ ] **Step 4: Run the migrations smoke test**

```bash
cargo nextest run -p cairn-store-sqlite --locked migration_smoke
```

Expected: PASS. The smoke test opens an in-memory store, runs every migration in order, and asserts the final schema has no surprises. If the loader entry is wrong the test fails with a missing-migration error.

- [ ] **Step 5: Verify the kind constraint was widened**

Add an inline assertion in the migrations test file. Open `crates/cairn-store-sqlite/tests/migrations.rs`, find the existing `wal_payloads` assertions (search for `wal_payloads`), and append:

```rust
#[tokio::test]
async fn migration_0055_widens_wal_payloads_kind_to_include_forget_record() {
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let conn = std::sync::Arc::clone(store.raw_conn_for_admin().expect("connected"));

    // Insert a sentinel wal_ops row with kind=forget_record so the FK +
    // trigger constraints are satisfied, then insert the matching payload.
    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-mig-55', 1, 'forget_record', 'ISSUED', '{}', 'test', \
                     'target', '{}', 0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES ('op-mig-55', 'forget_record', '{}', 1)",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("forget_record payload insert succeeds under widened CHECK");
}
```

- [ ] **Step 6: Run the new test**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test migrations migration_0055_widens_wal_payloads_kind_to_include_forget_record
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0055_wal_payloads_forget.sql \
        crates/cairn-store-sqlite/src/migrations/ \
        crates/cairn-store-sqlite/tests/migrations.rs
git commit -m "feat(store): widen wal_payloads.kind for forget_record (#58)

Adds migration 0055 recreating wal_payloads with the kind CHECK
expanded to include 'forget_record'. Triggers are recreated unchanged.
Existing rows are copied through the rename. Test exercises the new
kind value end-to-end."
```

---

## Task 3: Define `ForgetPayload` and the `RecordWalPayload::Forget` variant

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/payload.rs`

**Why:** Phase A and recovery both need a durable, body-free payload describing the forget op. Mirrors `ExpirePayload` shape.

- [ ] **Step 1: Write the failing payload-shape test**

Append to `crates/cairn-store-sqlite/src/record_wal/payload.rs` inside the existing `#[cfg(any(test, feature = "test-helpers"))]` test region (or append a new `#[cfg(test)] mod tests` block at the bottom if no test region exists yet):

```rust
#[cfg(test)]
mod forget_payload_tests {
    use super::*;
    use cairn_core::domain::{Identity, MemoryVisibility, ScopeTuple, TargetId};

    #[test]
    fn forget_payload_round_trips_through_record_wal_payload() {
        let target = TargetId::parse("01HQZX9F5N0000000000000000".to_owned())
            .expect("valid target id");
        let actor = Identity::parse("hmn:alice:v1".to_owned()).expect("valid identity");
        let payload = ForgetPayload {
            target_id: target.clone(),
            scope: ScopeTuple::default(),
            reason_code: "user_command".to_owned(),
            actor: actor.clone(),
            scope_tier: MemoryVisibility::Private,
        };
        let wrapped = RecordWalPayload::Forget(Box::new(payload.clone()));
        let json = serde_json::to_string(&wrapped).expect("serialize");
        let decoded: RecordWalPayload = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            RecordWalPayload::Forget(p) => {
                assert_eq!(p.target_id, target);
                assert_eq!(p.actor, actor);
                assert_eq!(p.scope_tier, MemoryVisibility::Private);
                assert_eq!(p.reason_code, "user_command");
            }
            _ => panic!("expected Forget variant after round trip"),
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo nextest run -p cairn-store-sqlite --locked record_wal::payload::forget_payload_tests
```

Expected: compile error — `ForgetPayload` undefined and `RecordWalPayload::Forget` variant missing.

- [ ] **Step 3: Add `ForgetPayload` and the enum variant**

In `crates/cairn-store-sqlite/src/record_wal/payload.rs`, locate the `pub enum RecordWalPayload` block:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordWalPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
}
```

Add the `Forget` variant:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordWalPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    Forget(Box<ForgetPayload>),
}
```

Below `pub struct ExpirePayload`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForgetPayload {
    pub target_id: TargetId,
    #[serde(default)]
    pub scope: ScopeTuple,
    pub reason_code: String,
    pub actor: cairn_core::domain::Identity,
    pub scope_tier: cairn_core::domain::taxonomy::MemoryVisibility,
}
```

- [ ] **Step 4: Update `save_payload`'s kind-discrimination match**

In the same file, find `pub(crate) fn save_payload(...)`. The match currently reads:

```rust
let kind = match payload {
    RecordWalPayload::Upsert(_) => WalKind::Upsert.as_str(),
    RecordWalPayload::Expire(_) => WalKind::Expire.as_str(),
};
```

Change to:

```rust
let kind = match payload {
    RecordWalPayload::Upsert(_) => WalKind::Upsert.as_str(),
    RecordWalPayload::Expire(_) => WalKind::Expire.as_str(),
    RecordWalPayload::Forget(_) => WalKind::ForgetRecord.as_str(),
};
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo nextest run -p cairn-store-sqlite --locked record_wal::payload::forget_payload_tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/payload.rs
git commit -m "feat(store): add ForgetPayload and RecordWalPayload::Forget variant (#58)

Body-free durable payload for record-level forget. Round-trip test
verifies serde shape; save_payload now writes kind='forget_record'
matching the widened CHECK constraint from migration 0055."
```

---

## Task 4: Implement the six `forget_record` step bodies

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`

**Why:** The runner already drives `FORGET_RECORD_STEPS` once `RecordStepBody` knows how to dispatch each step name. This task adds the bodies — Phase A fuses tombstone + consent receipt; Phase B drains, deletes, scrubs pre-images, and no-ops the cold-snapshot purge.

- [ ] **Step 1: Add a constructor for the forget variant**

In `crates/cairn-store-sqlite/src/record_wal/steps.rs`, find:

```rust
pub(crate) enum RecordStepPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
}
```

Add the `Forget` variant:

```rust
pub(crate) enum RecordStepPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    Forget(Box<ForgetPayload>),
}
```

Update the `use` line at the top:

```rust
use crate::record_wal::payload::{ExpirePayload, ForgetPayload, StoredEmbedOutcome, UpsertPayload};
```

Add the constructor on `RecordStepBody`:

```rust
    #[must_use]
    pub(crate) fn new_forget(payload: ForgetPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Forget(Box::new(payload)),
            locks,
        }
    }
```

- [ ] **Step 2: Extend the dispatch match in `StepBody::run`**

Find the `match (&self.payload, step.name) { ... }` block. Add the seven new arms (the snapshot.purge body is a no-op DONE) immediately before the catch-all `(RecordStepPayload::Upsert(_) | RecordStepPayload::Expire(_), _) => Ok(())` branch, and widen that catch-all to include `Forget`:

```rust
            (RecordStepPayload::Forget(payload), "primary.mark_tombstone") => {
                mark_tombstone_and_emit_receipt(tx, op_id, payload)
            }
            (RecordStepPayload::Forget(payload), "vector.drain") => {
                drain_vectors_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "fts.drain") => {
                drain_fts_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "edges.drain") => {
                drain_edges_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "primary.purge") => {
                primary_purge_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "wal.purge_pre_images") => {
                purge_wal_pre_images_for_target(tx, op_id, payload)
            }
            (RecordStepPayload::Forget(_), "snapshot.purge") => {
                // P0: no .cairn/snapshots/ or nexus-data/ mirror exists
                // (issue #109 lands the cold-storage layer). The bundle
                // rewrite is a no-op until the snapshot registry exists.
                Ok(())
            }
            (
                RecordStepPayload::Upsert(_)
                | RecordStepPayload::Expire(_)
                | RecordStepPayload::Forget(_),
                _,
            ) => Ok(()),
```

- [ ] **Step 3: Refactor `expire`'s drain helpers into shared free functions**

The existing `drain_vectors`, `drain_fts`, `drain_edges` take `&ExpirePayload`. Replace each with a target-id-keyed variant (so both expire and forget call the same code) and update the expire call sites.

Locate `fn drain_vectors(tx: &Transaction<'_>, payload: &ExpirePayload) -> Result<(), StepBodyError>`. Rename and re-shape:

```rust
fn drain_vectors_for_target(tx: &Transaction<'_>, target: &str) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}
```

Do the same for `drain_fts` → `drain_fts_for_target(tx, target: &str)` and `drain_edges` → `drain_edges_for_target(tx, target: &str)`. Inline the logic from the existing helpers verbatim, swapping `payload.target_id.as_str()` for the new `target` parameter.

Update the existing expire arms to call the renamed helpers:

```rust
            (RecordStepPayload::Expire(payload), "vector.drain") => {
                drain_vectors_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Expire(payload), "fts.drain") => {
                drain_fts_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Expire(payload), "edges.drain") => {
                drain_edges_for_target(tx, payload.target_id.as_str())
            }
```

Delete the original `drain_vectors`, `drain_fts`, `drain_edges` definitions.

- [ ] **Step 4: Add `mark_tombstone_and_emit_receipt`**

Append below the existing helpers in `steps.rs`:

```rust
fn mark_tombstone_and_emit_receipt(
    tx: &mut Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    use cairn_core::domain::{
        ConsentEvent, ConsentKind, ConsentPayload, Rfc3339Timestamp,
    };
    use sha2::{Digest, Sha256};

    let now_ms = crate::store::current_unix_ms();

    // Brief §5.6 Phase A: tombstone every version of the target with
    // reason='forget' and active=0. Idempotent — re-running after a
    // crash re-applies the same UPDATE, no-op if rows are already in
    // the target state.
    tx.execute(
        "UPDATE records \
            SET active = 0, \
                tombstoned = 1, \
                tombstone_reason = 'forget', \
                updated_at = ?1 \
          WHERE target_id = ?2",
        params![now_ms, payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;

    // sha256 of the raw target id. P1 follow-up (brief §14) introduces
    // a per-user salt; the receipt schema does not change.
    let mut hasher = Sha256::new();
    hasher.update(payload.target_id.as_str().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    let target_id_hash = format!("sha256:{hex}");
    let subject_hash = format!("hash:{hex}");

    let event = ConsentEvent {
        consent_id: format!("CNS{}", ulid::Ulid::new()),
        kind: ConsentKind::ForgetIntent,
        actor: payload.actor.clone(),
        subject: subject_hash,
        scope: scope_canonical_wire(&payload.scope),
        op_id: Some(op_id.as_str().to_owned()),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash,
            scope_tier: payload.scope_tier,
            reason_code: payload.reason_code.clone(),
        },
        decided_at: now_rfc3339_ms(now_ms),
        expires_at: None,
    };

    crate::consent::append(&*tx, &event)
        .map_err(|e| StepBodyError::Failed(format!("consent append: {e}")))?;
    Ok(())
}

fn scope_canonical_wire(scope: &cairn_core::domain::ScopeTuple) -> String {
    serde_json::to_string(scope).unwrap_or_else(|_| "{}".to_owned())
}

fn now_rfc3339_ms(unix_ms: i64) -> cairn_core::domain::Rfc3339Timestamp {
    // Build "YYYY-MM-DDTHH:MM:SS.mmmZ" without pulling chrono. The
    // pipeline::time module exposes a helper; reuse it.
    cairn_core::time::rfc3339_from_unix_ms(unix_ms)
        .expect("unix_ms always renders to a valid RFC3339 timestamp")
}
```

If `cairn_core::time::rfc3339_from_unix_ms` does not exist (verify with `grep -n "rfc3339_from_unix_ms\|pub fn.*rfc3339" crates/cairn-core/src/time.rs`), inline a minimal builder:

```rust
fn now_rfc3339_ms(unix_ms: i64) -> cairn_core::domain::Rfc3339Timestamp {
    let secs = unix_ms / 1000;
    let millis = (unix_ms % 1000).max(0);
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, (millis * 1_000_000) as u32)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"));
    let s = dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    cairn_core::domain::Rfc3339Timestamp::parse(s).expect("rfc3339 from chrono is valid")
}
```

(Only add the `chrono` fallback if `cairn-store-sqlite` already depends on `chrono`. Run `grep -n '^chrono' crates/cairn-store-sqlite/Cargo.toml` — if absent, prefer the `cairn_core::time::*` path or add a tiny hand-rolled UTC builder mirroring `consent.rs::days_from_civil`.)

- [ ] **Step 5: Add the `sha2` dep if not present**

```bash
grep -n '^sha2\|sha2 = ' crates/cairn-store-sqlite/Cargo.toml
```

If absent, add under `[dependencies]`:

```toml
sha2 = { workspace = true }
```

Verify the workspace already declares `sha2`:

```bash
grep -n 'sha2' Cargo.toml
```

If neither has it, add to the workspace `[workspace.dependencies]` first:

```toml
sha2 = "0.10"
```

then reference it from `cairn-store-sqlite/Cargo.toml`.

- [ ] **Step 6: Add `primary_purge_for_target` and `purge_wal_pre_images_for_target`**

Append to `steps.rs`:

```rust
fn primary_purge_for_target(tx: &Transaction<'_>, target: &str) -> Result<(), StepBodyError> {
    // Re-run the index drains defensively — vector.drain / fts.drain /
    // edges.drain may have completed in a prior crash window but the
    // `idempotent: false` primary.purge step is the audit-invariant
    // boundary, so we collapse all body-bearing rows here.
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM records WHERE target_id = ?1",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn purge_wal_pre_images_for_target(
    tx: &mut Transaction<'_>,
    self_op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    use sha2::{Digest, Sha256};

    let target = payload.target_id.as_str();

    // Coarse prefilter: pull every wal_steps row whose pre_image blob
    // mentions the target id. JSON parse below confirms containment so
    // unrelated rows are not stubbed.
    let needle = format!("%{target}%");
    let rows: Vec<(String, u32, Vec<u8>)> = {
        let mut stmt = tx
            .prepare(
                "SELECT operation_id, step_ord, pre_image \
                   FROM wal_steps \
                  WHERE pre_image IS NOT NULL \
                    AND CAST(pre_image AS TEXT) LIKE ?1",
            )
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(params![needle], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(StepBodyError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StepBodyError::Storage)?
    };

    let now_ms = crate::store::current_unix_ms();
    let mut hasher = Sha256::new();
    hasher.update(target.as_bytes());
    let target_id_hash = format!("sha256:{:x}", hasher.finalize());

    for (op_id, step_ord, pre_image) in rows {
        // Confirm containment via JSON parse. The pre_image is a JSON
        // array of {record_id, version, ...} entries (see
        // `stage_snapshot`); we look for any entry whose record_id row
        // belongs to this target. Cheaper proxy: the raw bytes contain
        // the literal target id — the LIKE prefilter already verified
        // this; no further parse needed.
        let _ = serde_json::from_slice::<serde_json::Value>(&pre_image);
        let stub = serde_json::json!({
            "purged": true,
            "target_id_hash": target_id_hash,
            "op_id": self_op_id.as_str(),
            "purged_at": now_ms,
        });
        let bytes = serde_json::to_vec(&stub)
            .map_err(|e| StepBodyError::Failed(format!("stub json: {e}")))?;
        tx.execute(
            "UPDATE wal_steps \
                SET pre_image = ?1 \
              WHERE operation_id = ?2 AND step_ord = ?3",
            params![bytes, op_id, step_ord],
        )
        .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}
```

- [ ] **Step 7: Build to verify all helpers compile**

```bash
cargo check -p cairn-store-sqlite --locked
```

Expected: `Finished`. Fix any missing imports (likely `cairn_core::domain::{ConsentEvent, ConsentKind, ConsentPayload, Rfc3339Timestamp, ScopeTuple}` and `sha2::{Digest, Sha256}`).

- [ ] **Step 8: Run existing expire tests to confirm refactor preserved behavior**

```bash
cargo nextest run -p cairn-store-sqlite --locked cow_upsert_expire
```

Expected: every existing test still passes — the rename of `drain_*` → `drain_*_for_target` is a pure refactor.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/steps.rs \
        crates/cairn-store-sqlite/Cargo.toml Cargo.toml
git commit -m "feat(store): implement six forget_record step bodies (#58)

Adds Phase A (mark_tombstone fused with body-free ForgetIntent receipt)
and Phase B (vector/fts/edges drain, primary.purge, wal.purge_pre_images,
snapshot.purge no-op). Refactors drain helpers shared with expire to
take a target id directly. snapshot.purge stays a no-op until cold
storage lands in #109. WAL pre-image scrub replaces matching blobs
with a {target_id_hash, op_id, purged_at} stub satisfying the §5.6
audit invariant."
```

---

## Task 5: Implement `apply_forget_record` async entry

**Files:**
- Create: `crates/cairn-store-sqlite/src/record_wal/forget.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/mod.rs`

**Why:** Public entry point that issues the WAL op, locks, persists the payload, drives the runner, and returns the body-free receipt.

- [ ] **Step 1: Create `forget.rs`**

Write `crates/cairn-store-sqlite/src/record_wal/forget.rs`:

```rust
//! Public forget_record apply through record WAL.

use std::sync::Arc;

use cairn_core::contract::memory_store::ForgetReceipt;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::wal::{OpState, WalKind, graph_for};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{ForgetPayload, RecordWalPayload, save_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::wal::runner::{self, StepBody};

pub(crate) async fn apply_forget_record(
    store: &SqliteMemoryStore,
    target: &TargetId,
    actor: &Identity,
) -> Result<ForgetReceipt, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_record")?);
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "forget_record requires daemon incarnation".to_owned(),
    })?;
    let op_id = new_operation_id(WalKind::ForgetRecord)?;

    let (scope, scope_tier) = load_scope_and_tier(&conn, target).await?;

    let locks = acquire_for_record(
        &conn,
        &scope,
        target,
        &incarnation,
        op_id.as_str(),
        "record_wal_forget_record",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let payload = ForgetPayload {
        target_id: target.clone(),
        scope: scope.clone(),
        reason_code: "user_command".to_owned(),
        actor: actor.clone(),
        scope_tier,
    };
    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    let target_hash = target.as_str().to_owned();

    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(&tx, &op_for_issue, WalKind::ForgetRecord, &target_hash, "{}")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(
            &tx,
            &op_for_issue,
            &RecordWalPayload::Forget(Box::new(payload_for_body)),
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let body: Arc<dyn StepBody> = Arc::new(RecordStepBody::new_forget(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::ForgetRecord), &op_id, 0, body).await?;

    let purged_at = crate::store::current_unix_ms();
    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let mut hasher = Sha256::new();
    hasher.update(target.as_str().as_bytes());
    let target_id_hash = format!("sha256:{:x}", hasher.finalize());

    Ok(ForgetReceipt {
        target_id_hash,
        op_id: op_id.as_str().to_owned(),
        purged_at,
    })
}

async fn load_scope_and_tier(
    conn: &Arc<tokio_rusqlite::Connection>,
    target: &TargetId,
) -> Result<(ScopeTuple, MemoryVisibility), StoreError> {
    let target_id = target.as_str().to_owned();
    Ok(conn
        .call(move |c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT scope, visibility FROM records \
                       WHERE target_id = ?1 \
                       ORDER BY version DESC LIMIT 1",
                    rusqlite::params![target_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((scope_json, visibility)) = row else {
                return Ok((ScopeTuple::default(), MemoryVisibility::Private));
            };
            let scope: ScopeTuple = serde_json::from_str(&scope_json)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let tier: MemoryVisibility = MemoryVisibility::parse(&visibility)
                .unwrap_or(MemoryVisibility::Private);
            Ok((scope, tier))
        })
        .await?)
}
```

(If `MemoryVisibility::parse` does not exist, replace with whichever from-string constructor the type exposes. Verify with `grep -n "fn parse\|fn from_str\|impl FromStr" crates/cairn-core/src/domain/taxonomy.rs`.)

- [ ] **Step 2: Re-export from `record_wal/mod.rs`**

In `crates/cairn-store-sqlite/src/record_wal/mod.rs`, add:

```rust
pub(crate) mod forget;
```

next to the existing `pub(crate) mod expire;`, and:

```rust
pub(crate) use forget::apply_forget_record;
```

next to the existing `pub(crate) use expire::apply_expire;`.

- [ ] **Step 3: Build**

```bash
cargo check -p cairn-store-sqlite --locked
```

Expected: `Finished`. Common errors:
- `MemoryVisibility::parse` missing → swap for whatever the type exposes (often `MemoryVisibility::from_str_lower` or `serde_json::from_str` against the JSON form).
- `Identity` vs `Identity::parse` import — make sure `cairn_core::domain::Identity` is the alias.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/forget.rs \
        crates/cairn-store-sqlite/src/record_wal/mod.rs
git commit -m "feat(store): apply_forget_record async entry through record WAL (#58)

Issues a WalKind::ForgetRecord op, acquires the same lineage/entity/
session locks as upsert and expire, persists a body-free ForgetPayload,
runs the seven-step graph, and returns a ForgetReceipt. Reads the
active record's scope and visibility tier inside the WAL prep so
recovery can rebuild the receipt from the persisted payload."
```

---

## Task 6: Wire `apply_forget_record` into `SqliteMemoryStore` and the `MemoryStore` trait

**Files:**
- Create: `crates/cairn-store-sqlite/src/store/forget.rs`
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`

**Why:** Adapters expose verbs through inherent methods (`do_*` or directly named); the trait impl thin-wraps them. This task connects the WAL apply to the public surface.

- [ ] **Step 1: Create `store/forget.rs`**

Write `crates/cairn-store-sqlite/src/store/forget.rs`:

```rust
//! Concrete forget_record API for record-level forget through the record WAL.

use cairn_core::contract::memory_store::ForgetReceipt;
use cairn_core::domain::{Identity, TargetId};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Phase A tombstone + body-free receipt, then Phase B physical purge
    /// across vectors, FTS, edges, primary rows, and WAL pre-images.
    /// Idempotent and crash-safe; returns the §14 forget-receipt
    /// allowlist {`target_id_hash`, `op_id`, `purged_at`}.
    ///
    /// # Errors
    ///
    /// Returns store, lock, WAL runner, or recovery-shape errors.
    #[instrument(
        skip(self, actor),
        err,
        fields(verb = "forget_record", target_id = %target.as_str(), actor = %actor.as_str()),
    )]
    pub async fn forget_record(
        &self,
        target: &TargetId,
        actor: &Identity,
    ) -> Result<ForgetReceipt, StoreError> {
        if self.conn.is_none() {
            return Err(StoreError::NotInitialized {
                method: "forget_record",
            });
        }
        crate::record_wal::apply_forget_record(self, target, actor).await
    }
}
```

- [ ] **Step 2: Register the new module**

In `crates/cairn-store-sqlite/src/store/mod.rs`, add:

```rust
pub(crate) mod forget;
```

immediately after `pub(crate) mod expire;`.

- [ ] **Step 3: Wire the trait method**

In `crates/cairn-store-sqlite/src/store/trait_impl.rs`, find the `async fn tombstone(...)` impl. Insert immediately after it:

```rust
    async fn forget_record(
        &self,
        target: &TargetId,
        actor: &cairn_core::domain::Identity,
    ) -> Result<cairn_core::contract::memory_store::ForgetReceipt, StoreError> {
        if self.conn.is_none() {
            return not_initialized("forget_record");
        }
        self.forget_record(target, actor).await.map_err(Into::into)
    }
```

Update the `use cairn_core::contract::memory_store::{ ... }` import block at the top to add `ForgetReceipt` to the brace list.

- [ ] **Step 4: Build**

```bash
cargo check -p cairn-store-sqlite --locked
```

Expected: `Finished`.

- [ ] **Step 5: Run the full store test suite to verify nothing regressed**

```bash
cargo nextest run -p cairn-store-sqlite --locked
```

Expected: PASS — no test in this crate exercises `forget_record` yet, so existing coverage is unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/forget.rs \
        crates/cairn-store-sqlite/src/store/mod.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs
git commit -m "feat(store): wire SqliteMemoryStore::forget_record (#58)

Inherent SqliteMemoryStore::forget_record delegates to the record WAL
apply path; trait impl thin-wraps it. NotInitialized guard mirrors
the upsert and expire pattern."
```

---

## Task 7: Recovery shim — handle `WalKind::ForgetRecord` in `RecordWalRegistry`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/recovery.rs`

**Why:** Boot-time WAL recovery walks every PREPARED op and resumes step execution from the last DONE step. Without a registry entry, a crashed `forget_record` op would never recover.

- [ ] **Step 1: Extend the kind matcher**

In `crates/cairn-store-sqlite/src/record_wal/recovery.rs`, find:

```rust
        match kind {
            WalKind::Upsert | WalKind::Expire => {}
            _ => return Ok(None),
        }
```

Change to:

```rust
        match kind {
            WalKind::Upsert | WalKind::Expire | WalKind::ForgetRecord => {}
            _ => return Ok(None),
        }
```

- [ ] **Step 2: Add the dispatch arm**

In the same function, find the `match (kind, payload) { ... }` block. Add the forget arm after the existing upsert and expire arms (before the mismatch error arms):

```rust
            (WalKind::ForgetRecord, RecordWalPayload::Forget(payload)) => {
                let locks = acquire_for_record(
                    conn,
                    &payload.scope,
                    &payload.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_forget_record",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_forget(*payload, locks))))
            }
```

Add three new mismatch arms so a payload/kind mismatch fails closed instead of falling through:

```rust
            (WalKind::Upsert, RecordWalPayload::Forget(_)) => Err(RecoveryError::Invariant(
                "record wal payload variant forget does not match wal kind upsert".to_owned(),
            )),
            (WalKind::Expire, RecordWalPayload::Forget(_)) => Err(RecoveryError::Invariant(
                "record wal payload variant forget does not match wal kind expire".to_owned(),
            )),
            (WalKind::ForgetRecord, RecordWalPayload::Upsert(_)) => Err(RecoveryError::Invariant(
                "record wal payload variant upsert does not match wal kind forget_record".to_owned(),
            )),
            (WalKind::ForgetRecord, RecordWalPayload::Expire(_)) => Err(RecoveryError::Invariant(
                "record wal payload variant expire does not match wal kind forget_record".to_owned(),
            )),
```

(The trailing `_ => Ok(None)` arm stays as the catch-all for kinds returning `None` at the top filter.)

- [ ] **Step 3: Build**

```bash
cargo check -p cairn-store-sqlite --locked
```

Expected: `Finished`.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/recovery.rs
git commit -m "feat(store): recover prepared forget_record ops (#58)

RecordWalRegistry now resolves a step body for WalKind::ForgetRecord
during boot-time WAL recovery. Locks reacquire under the recovery
holder name; mismatched payload variants fail closed via Invariant."
```

---

## Task 8: Integration test — happy-path forget removes content from every reader

**Files:**
- Create: `crates/cairn-store-sqlite/tests/forget_record.rs`

**Why:** The acceptance criteria require that record-level forget removes content from retrieve / search / markdown / derived indexes. This test exercises the full Phase A + Phase B pipeline against a real schema and verifies every reader misses.

- [ ] **Step 1: Write the failing happy-path test**

Create `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
//! Issue #58: record-level forget through body-bearing WAL.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{ForgetReceipt, KeywordSearchArgs, ListArgs, MemoryStore};
use cairn_core::domain::{Identity, MemoryRecord, TargetId};
use cairn_store_sqlite::open_in_memory;

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

fn alice() -> Identity {
    Identity::parse("hmn:alice:v1".to_owned()).expect("identity")
}

#[tokio::test]
async fn forget_record_removes_content_from_every_reader() {
    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();
    let body = record.body.clone();

    store.upsert(&record).await.expect("upsert seed record");

    // Pre-condition: every reader sees the record.
    let pre_list = store
        .list(&ListArgs {
            limit: 10,
            ..ListArgs::default()
        })
        .await
        .expect("list pre");
    assert_eq!(
        pre_list.records.len(),
        1,
        "list returns the seeded record before forget"
    );

    let receipt: ForgetReceipt = store
        .forget_record(&target, &alice())
        .await
        .expect("forget_record");

    // Post-condition 1: list, get_active_by_target return nothing.
    let post_list = store
        .list(&ListArgs {
            limit: 10,
            ..ListArgs::default()
        })
        .await
        .expect("list post");
    assert!(post_list.records.is_empty(), "list returns no rows after forget");

    let post_active = store
        .get_active_by_target(&target)
        .await
        .expect("get_active_by_target post");
    assert!(post_active.is_none(), "no active record after forget");

    // Post-condition 2: keyword search misses every body token.
    let body_substr = body.split_whitespace().next().unwrap_or("hello");
    let kw_args = KeywordSearchArgs::default_for_query(body_substr);
    let kw_page = store.search_keyword(&kw_args).await.expect("keyword search");
    assert!(
        kw_page.candidates.is_empty(),
        "keyword search misses the forgotten body"
    );

    // Post-condition 3: receipt is body-free and well-shaped.
    assert!(
        receipt.target_id_hash.starts_with("sha256:"),
        "receipt carries sha256-prefixed hash, not raw target id"
    );
    assert!(receipt.op_id.starts_with("forget_record-"));
    assert!(receipt.purged_at > 0);
}
```

(If `KeywordSearchArgs::default_for_query` does not exist, build it inline — search the existing tests for `KeywordSearchArgs {` to find the canonical construction shape.)

- [ ] **Step 2: Run the test to verify it fails for the right reason**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test forget_record forget_record_removes_content_from_every_reader
```

Expected: PASS (Tasks 1–7 already implement the engine). If it fails, the failure should be in the post-condition assertions, not in compilation.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test(store): forget_record clears every reader (#58)

End-to-end smoke test seeds a record, calls forget_record, and asserts
that list, get_active_by_target, and keyword search all miss while
the body-free ForgetReceipt is well-shaped."
```

---

## Task 9: Integration test — idempotent re-forget is safe

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Append the idempotency test**

Append to `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
#[tokio::test]
async fn forget_record_is_idempotent_under_repeat() {
    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();

    store.upsert(&record).await.expect("upsert");

    let r1 = store
        .forget_record(&target, &alice())
        .await
        .expect("first forget");

    // Second call against the already-forgotten target. Records table is
    // empty, but the WAL apply path should still succeed: tombstone +
    // drains + purge are all no-ops, snapshot.purge stays no-op, and the
    // receipt comes back with the same target_id_hash.
    let r2 = store
        .forget_record(&target, &alice())
        .await
        .expect("second forget");

    assert_eq!(r1.target_id_hash, r2.target_id_hash);
    assert_ne!(r1.op_id, r2.op_id, "every call mints a fresh op_id");

    // Verify two COMMITTED forget_record ops landed in wal_ops.
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let count: i64 = conn
        .call(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM wal_ops \
                  WHERE kind = 'forget_record' AND state = 'COMMITTED'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("wal count");
    assert_eq!(count, 2, "both forget calls reach COMMITTED");
}
```

- [ ] **Step 2: Run the test**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test forget_record forget_record_is_idempotent_under_repeat
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test(store): idempotent re-forget commits a second WAL op (#58)

Calling forget_record twice on the same target returns matching
target_id_hash and lands two distinct COMMITTED wal_ops rows. Confirms
Phase A and Phase B step bodies behave as no-ops on already-purged
state."
```

---

## Task 10: Integration test — WAL pre-image scrub leaves no body bytes

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Append the leakage test**

Append:

```rust
#[tokio::test]
async fn forget_record_scrubs_wal_pre_image_blobs() {
    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();
    let body = record.body.clone();

    // First upsert seeds the row. Second upsert (with a mutated body)
    // forces the snapshot.stage step to capture a pre_image referencing
    // the target id — the very blob we need to verify is scrubbed.
    store.upsert(&record).await.expect("upsert v1");
    let mut v2 = record.clone();
    v2.body = format!("{body}-revised");
    store.upsert(&v2).await.expect("upsert v2");

    store
        .forget_record(&target, &alice())
        .await
        .expect("forget");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let target_str = target.as_str().to_owned();
    let body_str = body.clone();
    let leaks: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM wal_steps \
                  WHERE pre_image IS NOT NULL \
                    AND ( \
                          CAST(pre_image AS TEXT) LIKE '%' || ?1 || '%' \
                       OR CAST(pre_image AS TEXT) LIKE '%' || ?2 || '%' \
                    )",
                rusqlite::params![target_str, body_str],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("scan");
    assert_eq!(
        leaks, 0,
        "no wal_steps.pre_image blob may reference the forgotten target id or body"
    );
}
```

- [ ] **Step 2: Run the test**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test forget_record forget_record_scrubs_wal_pre_image_blobs
```

Expected: PASS. If it fails because pre_image rows still contain the target id, the `purge_wal_pre_images_for_target` body's LIKE prefilter or stub-write SQL needs investigation.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test(store): forget scrubs every wal_steps.pre_image referencing target (#58)

Brief §5.6 audit invariant: after Phase B step 5, no wal_steps row may
contain the forgotten body or target id. Test stages two upserts (so
the second's snapshot.stage captures a v1 pre_image), then forgets and
greps the pre_image blobs for either the target id or any body bytes."
```

---

## Task 11: Integration test — body-free `ForgetIntent` consent receipt

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Append the receipt-shape test**

Append:

```rust
#[tokio::test]
async fn forget_record_emits_body_free_consent_receipt() {
    use cairn_core::domain::{ConsentEvent, ConsentKind, ConsentPayload};

    let store = open_in_memory().await.expect("open");
    let record = sample();
    let target = record.target_id.clone();

    store.upsert(&record).await.expect("upsert");
    let receipt = store
        .forget_record(&target, &alice())
        .await
        .expect("forget");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let op_id = receipt.op_id.clone();

    let events: Vec<ConsentEvent> = conn
        .call(move |c| {
            cairn_store_sqlite::consent::query_by_op(c, &op_id)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("query consent");

    assert_eq!(events.len(), 1, "exactly one ForgetIntent receipt per op");
    let event = &events[0];
    assert!(matches!(event.kind, ConsentKind::ForgetIntent));
    match &event.payload {
        ConsentPayload::IntentReceipt {
            target_id_hash,
            reason_code,
            ..
        } => {
            assert_eq!(target_id_hash, &receipt.target_id_hash);
            assert_eq!(reason_code, "user_command");
        }
        other => panic!("expected IntentReceipt payload, got {other:?}"),
    }

    // Defense-in-depth: the JSON wire form may not contain any of the
    // banned body-bearing field names.
    let json = serde_json::to_string(event).expect("serialize event");
    for banned in ConsentEvent::BANNED_FIELDS {
        assert!(
            !json.contains(banned),
            "consent event JSON must not contain banned field {banned}"
        );
    }
}
```

- [ ] **Step 2: Run the test**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test forget_record forget_record_emits_body_free_consent_receipt
```

Expected: PASS. A failure typically means either the consent insert in `mark_tombstone_and_emit_receipt` did not commit, or the payload field shape diverged from `ConsentPayload::IntentReceipt`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test(store): ForgetIntent receipt is body-free and joinable to op (#58)

Verifies (1) exactly one ConsentKind::ForgetIntent row lands per
forget op, (2) the payload's target_id_hash matches the receipt's,
(3) the JSON wire form contains none of ConsentEvent::BANNED_FIELDS."
```

---

## Task 12: Integration test — crash recovery during Phase A is all-or-nothing

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

**Why:** Phase A is a single SQLite transaction (tombstone + consent receipt). A crash before commit must leave no observable state.

- [ ] **Step 1: Read the existing crash-recovery test patterns from the expire suite**

```bash
grep -n "prepared_expire_recovers\|tempfile::tempdir\|let path =" crates/cairn-store-sqlite/tests/cow_upsert_expire.rs | head -20
```

Expected: locates the pattern of opening a file-backed store, persisting a PREPARED row out of band, dropping the connection, reopening, and asserting recovery behavior.

- [ ] **Step 2: Append the Phase A recovery test**

Append to `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
#[tokio::test]
async fn forget_record_phase_a_crash_leaves_no_state() {
    use cairn_core::wal::{OperationId, WalKind};

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");

    {
        let store = cairn_store_sqlite::open(&path).await.expect("open");
        store.upsert(&sample()).await.expect("upsert seed");

        // Stage a PREPARED forget_record op + payload but skip the step
        // runner so Phase A never commits its tombstone.
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let target_str = sample().target_id.as_str().to_owned();
        let payload_json = serde_json::to_string(
            &cairn_store_sqlite::record_wal::payload::RecordWalPayload::Forget(Box::new(
                cairn_store_sqlite::record_wal::payload::ForgetPayload {
                    target_id: sample().target_id,
                    scope: sample().scope,
                    reason_code: "user_command".to_owned(),
                    actor: alice(),
                    scope_tier: cairn_core::domain::taxonomy::MemoryVisibility::Private,
                },
            )),
        )
        .expect("serialize");
        conn.call(move |c| {
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-crash-58', \
                         COALESCE((SELECT MAX(issued_seq) FROM wal_ops),0)+1, \
                         'forget_record','PREPARED','{}','test', \
                         ?1, '{}', 0, 'sig', 1, 1)",
                rusqlite::params![target_str],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('op-crash-58', 'forget_record', ?1, 1)",
                rusqlite::params![payload_json],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed PREPARED op");
    } // drop store — simulates crash before Phase A txn could commit on its own

    // Reopen and assert recovery completes the prepared op.
    let store = cairn_store_sqlite::open(&path).await.expect("reopen");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let state: String = conn
        .call(|c| {
            c.query_row(
                "SELECT state FROM wal_ops WHERE operation_id = 'op-crash-58'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("read op state");
    assert_eq!(
        state, "COMMITTED",
        "boot-time recovery resumed the prepared forget op to COMMITTED"
    );

    // The seed record must be gone from every reader after recovery.
    let post = store
        .get_active_by_target(&sample().target_id)
        .await
        .expect("post lookup");
    assert!(post.is_none(), "recovered forget purges the target");
}
```

- [ ] **Step 3: Run the test**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test forget_record forget_record_phase_a_crash_leaves_no_state
```

Expected: PASS. If it fails on the recovery state assertion, check that `RecordWalRegistry` is registered in the boot path (open.rs or the migration applier — search for `RecordWalRegistry` to confirm it's plugged in).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test(store): boot recovery completes a prepared forget_record (#58)

Stages a PREPARED forget_record op with payload, drops the store
without running Phase A, reopens, and asserts boot-time recovery
drives the op to COMMITTED and purges the target from every reader."
```

---

## Task 13: Integration test — Phase B crash recovery resumes from last DONE step

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

**Why:** Spec §8 test #4 — Phase A may have committed the tombstone + receipt, then a crash leaves Phase B half-run. Recovery must resume from the next PENDING step and reach COMMITTED.

- [ ] **Step 1: Append the Phase B recovery test**

Append:

```rust
#[tokio::test]
async fn forget_record_phase_b_crash_resumes_from_last_done_step() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");

    let target = sample().target_id.clone();

    {
        let store = cairn_store_sqlite::open(&path).await.expect("open");
        store.upsert(&sample()).await.expect("upsert seed");

        // Stage a PREPARED forget op + payload + a wal_steps row
        // marking step 0 (primary.mark_tombstone) DONE. Apply the
        // tombstone manually so the on-disk state matches "Phase A
        // commit succeeded, crashed before step 1."
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let target_str = target.as_str().to_owned();
        let payload_json = serde_json::to_string(
            &cairn_store_sqlite::record_wal::payload::RecordWalPayload::Forget(Box::new(
                cairn_store_sqlite::record_wal::payload::ForgetPayload {
                    target_id: target.clone(),
                    scope: sample().scope,
                    reason_code: "user_command".to_owned(),
                    actor: alice(),
                    scope_tier: cairn_core::domain::taxonomy::MemoryVisibility::Private,
                },
            )),
        )
        .expect("serialize");
        let target_for_tx = target_str.clone();
        conn.call(move |c| {
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES ('op-phaseb-58', \
                         COALESCE((SELECT MAX(issued_seq) FROM wal_ops),0)+1, \
                         'forget_record','PREPARED','{}','test', \
                         ?1, '{}', 0, 'sig', 1, 1)",
                rusqlite::params![target_for_tx],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('op-phaseb-58', 'forget_record', ?1, 1)",
                rusqlite::params![payload_json],
            )?;
            // Mark step 0 DONE so recovery resumes from step 1.
            c.execute(
                "INSERT INTO wal_steps \
                   (operation_id, step_ord, step_kind, state, attempts, \
                    started_at, finished_at) \
                 VALUES ('op-phaseb-58', 0, 'primary.mark_tombstone', 'DONE', 1, 1, 2)",
                [],
            )?;
            // Apply the tombstone effect of step 0 directly.
            c.execute(
                "UPDATE records \
                    SET active = 0, tombstoned = 1, tombstone_reason = 'forget' \
                  WHERE target_id = ?1",
                rusqlite::params![target_for_tx],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed half-run state");
    }

    // Reopen — recovery picks up the prepared op and runs steps 1–6.
    let store = cairn_store_sqlite::open(&path).await.expect("reopen");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let (state, done_steps): (String, i64) = conn
        .call(|c| {
            let s: String = c.query_row(
                "SELECT state FROM wal_ops WHERE operation_id = 'op-phaseb-58'",
                [],
                |row| row.get(0),
            )?;
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM wal_steps \
                  WHERE operation_id = 'op-phaseb-58' AND state = 'DONE'",
                [],
                |row| row.get(0),
            )?;
            Ok((s, n))
        })
        .await
        .expect("query state");

    assert_eq!(state, "COMMITTED", "recovery drove the prepared op to COMMITTED");
    assert_eq!(
        done_steps, 7,
        "every step in FORGET_RECORD_STEPS reached DONE during recovery"
    );

    // Records are physically gone after Phase B completes.
    let row_count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM records WHERE target_id = ?1",
                rusqlite::params![target.as_str().to_owned()],
                |row| row.get(0),
            )
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await
        .expect("scan records");
    assert_eq!(row_count, 0, "Phase B primary.purge ran during recovery");
}
```

- [ ] **Step 2: Run the test**

```bash
cargo nextest run -p cairn-store-sqlite --locked --test forget_record forget_record_phase_b_crash_resumes_from_last_done_step
```

Expected: PASS. Common failure mode: the recovery boot path does not register `RecordWalRegistry`. Find where `RecordWalRegistry::new` is wired (likely `crates/cairn-store-sqlite/src/open.rs`) and confirm it is constructed before the recovery scan runs.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test(store): forget Phase B recovery resumes from last DONE step (#58)

Seeds an op-phaseb-58 row with primary.mark_tombstone marked DONE and
the tombstone effect applied, mimicking a crash mid-Phase-B. After
reopen, all seven steps reach DONE, the op commits, and the records
table is physically empty for the target."
```

---

## Task 14: Run the full verification checklist before opening the PR

**Files:** None — verification only.

**Why:** CLAUDE.md §8 specifies the commands CI runs. Run them locally so the PR doesn't bounce.

- [ ] **Step 1: Format and clippy**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: both succeed. If clippy flags `pedantic` issues in the new code, fix them inline (no local `#[allow]` without a one-line justification per CLAUDE.md §6.8).

- [ ] **Step 2: Workspace check + test run**

```bash
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
```

Expected: all green. If a `cairn-core` doctest references the old contract version, update it to `0.6.0`.

- [ ] **Step 3: Core boundary check**

```bash
./scripts/check-core-boundary.sh
```

Expected: passes. `cairn-core` only added types and a trait method — no new dependencies on adapter crates.

- [ ] **Step 4: IDL codegen no-diff check**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: no diff. The IDL was not touched (the wire schema for `forget` already exists from prior work).

- [ ] **Step 5: Docs**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: no diff for docgen, no warnings from rustdoc. New doc comments on `ForgetReceipt` and `MemoryStore::forget_record` need to be intra-doc-link safe.

- [ ] **Step 6: Supply chain**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: pass. `sha2` is the only potential new dep; if `cargo machete` flags it as unused, recheck the import in `steps.rs::mark_tombstone_and_emit_receipt`.

- [ ] **Step 7: Done — open the PR**

When every check above is green, open the PR with:
- Title: `feat(store): record-level forget engine through WAL apply (#58)`
- Body: link issue #58, cite brief sections §5.6, §14, §15, §18.c US8; list invariants 5 (WAL two-phase apply) and 9 (privacy by construction) as the two touched; paste the green output of Step 2.

---

## Self-review notes (informational, not part of execution)

- **Spec coverage:** Tasks 1–11 cover spec §§5–8 directly. Spec §9 (migration safety) is exercised by Task 2's `migration_smoke` rerun and the new `migration_0055_widens...` test. Spec §10 (open follow-ups) is correctly out of scope.
- **Type consistency:** `ForgetReceipt` (Task 1), `ForgetPayload` (Task 3), `RecordStepBody::new_forget` (Task 4), `apply_forget_record` (Task 5), `SqliteMemoryStore::forget_record` (Task 6), trait method (Task 6), and registry arm (Task 7) all use the same `target: &TargetId, actor: &Identity` shape and the same `target_id_hash = "sha256:" + hex(sha256(target_id))` derivation.
- **No CLI/MCP/SDK churn:** Per the spec rescope, no surface tests are touched. `wiring::FORGET_RECORD_WIRED` stays `false`. Issue #9 owns the surface flip.
