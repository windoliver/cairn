# Issue 57 COW Upsert Expire WAL Apply Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement WAL-driven copy-on-write apply for record upsert and expire so partial writes never expose half-updated records, recovery can replay body-bearing operations, and expired records disappear from default reads and search without hard deletion.

**Architecture:** Add a `record_wal` subsystem inside `cairn-store-sqlite` that owns record-operation payloads, lock acquisition, step bodies, and operation finalization. Public upsert and the new concrete expire API issue a body-bearing `wal_ops` row, persist a durable JSON payload, run the static step graph through the existing runner, and finalize `COMMITTED` only after all steps are `DONE`. Boot recovery initializes the daemon incarnation first, registers record step bodies, reacquires per-operation locks, and resumes `PREPARED` upsert and expire operations from their persisted payloads.

**Tech Stack:** Rust 2024, `tokio_rusqlite`, `rusqlite`, SQLite WAL tables, existing `cairn_core::wal` step graphs, existing `locks` module, serde JSON payloads, integration tests under `crates/cairn-store-sqlite/tests`.

---

## File Structure

- Create: `crates/cairn-store-sqlite/src/migrations/sql/0053_wal_payloads.sql`
  - Adds durable JSON payload storage keyed by `wal_ops.operation_id`.
  - Makes payload rows immutable and append-only.
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
  - Registers migration 53 in the manifest and migration list.
- Create: `crates/cairn-store-sqlite/tests/wal_payloads_migration.rs`
  - Verifies the schema exists, enforces the foreign key, and blocks update/delete.
- Modify: `crates/cairn-store-sqlite/src/locks/handle.rs`
  - Adds synchronous `LockHandle::assert_live_in_tx` for step bodies already running in a `rusqlite::Transaction`.
- Modify: `crates/cairn-store-sqlite/src/locks/error.rs`
  - No enum shape change required; reuse `LockError::Db`, `LockError::Clock`, and `LockError::Fenced`.
- Modify: `crates/cairn-store-sqlite/tests/locks_stale_fence.rs`
  - Adds coverage for the new transaction-local fencing assertion.
- Modify: `crates/cairn-store-sqlite/src/wal/recovery.rs`
  - Changes `StepBodyRegistry` to async and operation-aware so recovery bodies can load payloads and reacquire locks before `runner::run_from`.
- Modify: `crates/cairn-store-sqlite/tests/wal_recovery.rs`
  - Updates the synthetic registry to the new `StepBodyRegistry` signature.
- Create: `crates/cairn-store-sqlite/src/record_wal/mod.rs`
  - Module root and public-in-crate API for apply and recovery.
- Create: `crates/cairn-store-sqlite/src/record_wal/payload.rs`
  - Serializable `UpsertPayload`, `ExpirePayload`, embedding outcome, and payload load/save helpers.
- Create: `crates/cairn-store-sqlite/src/record_wal/locks.rs`
  - Shared lock-resource derivation and async acquisition for public apply and recovery.
- Create: `crates/cairn-store-sqlite/src/record_wal/ops.rs`
  - WAL issue, prepare, finalize, and payload persistence helpers.
- Create: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
  - `StepBody` implementation and all step dispatch for upsert and expire.
- Create: `crates/cairn-store-sqlite/src/record_wal/recovery.rs`
  - `RecordWalRegistry` implementation for boot recovery.
- Create: `crates/cairn-store-sqlite/src/record_wal/upsert.rs`
  - Public upsert WAL apply orchestration.
- Create: `crates/cairn-store-sqlite/src/record_wal/expire.rs`
  - Public expire WAL apply orchestration.
- Modify: `crates/cairn-store-sqlite/src/store/upsert.rs`
  - Keeps validation and embedding precompute, then delegates to `record_wal::apply_upsert`.
  - Exposes sync COW primitives used by WAL step bodies and transaction tests.
- Create: `crates/cairn-store-sqlite/src/store/expire.rs`
  - Concrete `SqliteMemoryStore::expire(&TargetId)` API delegating to `record_wal::apply_expire`.
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
  - Adds the `expire` module.
- Modify: `crates/cairn-store-sqlite/src/open.rs`
  - Initializes daemon incarnation before boot recovery and registers `RecordWalRegistry`.
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
  - Adds `mod record_wal`.
- Modify: `crates/cairn-store-sqlite/src/error.rs`
  - Adds record-WAL lock and apply variants that preserve lock and runner error sources.
- Create: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`
  - End-to-end acceptance tests for WAL conformance, search exclusion, expiry, and retry-safe derived steps.

---

### Task 1: Add Durable WAL Payload Storage

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0053_wal_payloads.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
- Create: `crates/cairn-store-sqlite/tests/wal_payloads_migration.rs`

- [ ] **Step 1: Write the failing migration test**

Add `crates/cairn-store-sqlite/tests/wal_payloads_migration.rs`:

```rust
//! Migration 0053 stores body-bearing WAL payloads for record operations.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::wal::WalKind;
use cairn_store_sqlite::open_in_memory;
use rusqlite::params;

#[tokio::test]
async fn wal_payloads_table_is_present_and_immutable() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-payload', 1, ?1, 'ISSUED', '{}', 'issuer', 'target', '{}', 0, 'sig', 1, 1)",
            params![WalKind::Upsert.as_str()],
        )?;
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES ('op-payload', 'upsert', '{\"kind\":\"upsert\"}', 1)",
            [],
        )?;

        let payload: String = c.query_row(
            "SELECT payload_json FROM wal_payloads WHERE operation_id = 'op-payload'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(payload, "{\"kind\":\"upsert\"}");

        let update = c.execute(
            "UPDATE wal_payloads SET payload_json = '{}' WHERE operation_id = 'op-payload'",
            [],
        );
        assert!(update.is_err(), "payload rows must not be updated");

        let delete = c.execute("DELETE FROM wal_payloads WHERE operation_id = 'op-payload'", []);
        assert!(delete.is_err(), "payload rows must not be deleted");

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("schema assertions");
}

#[tokio::test]
async fn wal_payloads_requires_existing_operation() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        let err = c
            .execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES ('missing-op', 'upsert', '{}', 1)",
                [],
            )
            .expect_err("foreign key rejects missing wal_ops row");
        assert!(err.to_string().contains("FOREIGN KEY"));
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("foreign-key assertion");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test wal_payloads_migration -- --nocapture`

Expected: FAIL with a SQLite message containing `no such table: wal_payloads`.

- [ ] **Step 3: Add the migration SQL**

Create `crates/cairn-store-sqlite/src/migrations/sql/0053_wal_payloads.sql`:

```sql
-- Migration 0053: durable body-bearing WAL payloads for record operations.
-- Issue #57: upsert and expire recovery need operation inputs after restart.

CREATE TABLE wal_payloads (
  operation_id TEXT NOT NULL PRIMARY KEY
                    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL CHECK (kind IN ('upsert', 'expire')),
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);

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
  VALUES (53, '0053_wal_payloads', '', strftime('%s','now') * 1000);
```

- [ ] **Step 4: Register migration 53**

In `crates/cairn-store-sqlite/src/migrations/mod.rs`, add the const after `M0052_LOCK_ACQUISITION_ULID_UNIQUE`:

```rust
// Issue #57 — durable body-bearing WAL payloads for upsert/expire recovery.
const M0053_WAL_PAYLOADS: &str = include_str!("sql/0053_wal_payloads.sql");
```

Add this tuple after migration 52 in `MIGRATION_SOURCES`:

```rust
    (53, "0053_wal_payloads", M0053_WAL_PAYLOADS),
```

Add this migration after `M::up(M0052_LOCK_ACQUISITION_ULID_UNIQUE),`:

```rust
        M::up(M0053_WAL_PAYLOADS),
```

- [ ] **Step 5: Run the migration test to verify it passes**

Run: `cargo test -p cairn-store-sqlite --test wal_payloads_migration -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/mod.rs \
  crates/cairn-store-sqlite/src/migrations/sql/0053_wal_payloads.sql \
  crates/cairn-store-sqlite/tests/wal_payloads_migration.rs
git commit -m "feat: add wal payload storage"
```

---

### Task 2: Make Recovery Registries Operation-Aware and Add Transaction Fencing

**Files:**
- Modify: `crates/cairn-store-sqlite/src/wal/recovery.rs`
- Modify: `crates/cairn-store-sqlite/tests/wal_recovery.rs`
- Modify: `crates/cairn-store-sqlite/src/locks/handle.rs`
- Modify: `crates/cairn-store-sqlite/tests/locks_stale_fence.rs`

- [ ] **Step 1: Write the failing lock assertion test**

Add this test to `crates/cairn-store-sqlite/tests/locks_stale_fence.rs`:

```rust
#[tokio::test]
async fn assert_live_in_tx_blocks_stale_holder_inside_existing_transaction() {
    let store = open_in_memory().await.unwrap();
    let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
    let inc = store.incarnation().cloned().unwrap();
    let resource = ResourceKey::entity("t1", "default", "rec-tx");

    let h_a = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "holder_a_tx",
        Duration::from_millis(80),
        &inc,
        "writer_a_tx",
    )
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;

    let _h_b = acquire(
        &conn,
        &resource,
        LockMode::Exclusive,
        "holder_b_tx",
        Duration::from_secs(5),
        &inc,
        "writer_b_tx",
    )
    .await
    .unwrap();

    let err = conn
        .call(move |c| {
            let tx = c.transaction()?;
            let result = h_a.assert_live_in_tx(&tx);
            drop(tx);
            Ok::<_, tokio_rusqlite::Error>(result)
        })
        .await
        .unwrap()
        .unwrap_err();

    assert!(matches!(err, LockError::Fenced { .. }));
}
```

- [ ] **Step 2: Run the lock test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test locks_stale_fence assert_live_in_tx_blocks_stale_holder_inside_existing_transaction -- --nocapture`

Expected: FAIL with a compiler error containing `no method named assert_live_in_tx`.

- [ ] **Step 3: Implement transaction-local fencing**

Add this method inside `impl LockHandle` in `crates/cairn-store-sqlite/src/locks/handle.rs`:

```rust
    /// Assert this lock is still live inside an existing transaction.
    ///
    /// Step bodies use this when the WAL runner already opened the
    /// transaction that will contain the side effect and `wal_steps` update.
    ///
    /// # Errors
    /// - `LockError::Fenced` if the acquisition row expired, was reclaimed,
    ///   or the resource epoch changed.
    /// - `LockError::Clock` if system time is before Unix epoch.
    /// - `LockError::Db` for SQLite failures.
    pub fn assert_live_in_tx(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), LockError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| LockError::Clock)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))?;

        let (group_epoch, holder_alive): (Option<i64>, i64) = tx
            .query_row(
                "SELECT \
                   (SELECT epoch FROM locks WHERE resource = ?1) AS group_epoch, \
                   EXISTS (SELECT 1 FROM lock_holders \
                            WHERE acquisition_ulid = ?2 AND expires_at > ?3) \
                        AS holder_alive",
                params![self.resource, self.acquisition_ulid, now_ms],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|e| LockError::Db(tokio_rusqlite::Error::Rusqlite(e)))?;

        let observed = group_epoch.unwrap_or(-1);
        if observed != self.acquired_epoch || holder_alive == 0 {
            return Err(LockError::Fenced {
                resource: self.resource.clone(),
                expected_epoch: self.acquired_epoch,
                observed_epoch: observed,
                retry: default_fenced_retry(),
            });
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the lock test to verify it passes**

Run: `cargo test -p cairn-store-sqlite --test locks_stale_fence assert_live_in_tx_blocks_stale_holder_inside_existing_transaction -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Write the failing recovery registry test update**

In `crates/cairn-store-sqlite/tests/wal_recovery.rs`, change `OneKindRegistry` to count which operation was requested:

```rust
struct OneKindRegistry {
    kind: WalKind,
    body: Arc<dyn StepBody>,
    requested_ops: parking_lot::Mutex<Vec<String>>,
}
```

Update `upsert_with_body`:

```rust
fn upsert_with_body(body: Arc<dyn StepBody>) -> RecoveryConfig {
    RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body,
            requested_ops: parking_lot::Mutex::new(Vec::new()),
        }),
    }
}
```

Replace the `StepBodyRegistry` impl:

```rust
#[async_trait::async_trait]
impl StepBodyRegistry for OneKindRegistry {
    async fn body_for(
        &self,
        _conn: &Arc<Connection>,
        kind: WalKind,
        op_id: &OperationId,
    ) -> Result<Option<Arc<dyn StepBody>>, cairn_store_sqlite::wal::RecoveryError> {
        self.requested_ops.lock().push(op_id.as_str().to_owned());
        if kind == self.kind {
            Ok(Some(Arc::clone(&self.body)))
        } else {
            Ok(None)
        }
    }
}
```

- [ ] **Step 6: Run WAL recovery tests to verify the signature fails**

Run: `cargo test -p cairn-store-sqlite --test wal_recovery -- --nocapture`

Expected: FAIL with trait signature mismatch errors for `StepBodyRegistry`.

- [ ] **Step 7: Change `StepBodyRegistry` to async operation-aware lookup**

In `crates/cairn-store-sqlite/src/wal/recovery.rs`, replace the trait and empty registry with:

```rust
#[async_trait::async_trait]
pub trait StepBodyRegistry: Send + Sync {
    /// Returns the body for `kind` and `op_id`.
    ///
    /// Recovery registries may load durable payloads and reacquire locks
    /// before returning a body that can run synchronously inside runner
    /// transactions.
    async fn body_for(
        &self,
        conn: &Arc<Connection>,
        kind: WalKind,
        op_id: &OperationId,
    ) -> Result<Option<Arc<dyn StepBody>>, RecoveryError>;
}

pub struct EmptyRegistry;

#[async_trait::async_trait]
impl StepBodyRegistry for EmptyRegistry {
    async fn body_for(
        &self,
        _conn: &Arc<Connection>,
        _kind: WalKind,
        _op_id: &OperationId,
    ) -> Result<Option<Arc<dyn StepBody>>, RecoveryError> {
        Ok(None)
    }
}
```

In `handle_resume`, replace:

```rust
    let Some(body) = config.bodies.body_for(snapshot.kind) else {
```

with:

```rust
    let Some(body) = config.bodies.body_for(conn, snapshot.kind, op_id).await? else {
```

- [ ] **Step 8: Run the recovery tests to verify they pass**

Run: `cargo test -p cairn-store-sqlite --test wal_recovery -- --nocapture`

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-store-sqlite/src/wal/recovery.rs \
  crates/cairn-store-sqlite/tests/wal_recovery.rs \
  crates/cairn-store-sqlite/src/locks/handle.rs \
  crates/cairn-store-sqlite/tests/locks_stale_fence.rs
git commit -m "feat: make wal recovery bodies operation aware"
```

---

### Task 3: Add Record WAL Payload and Operation Infrastructure

**Files:**
- Create: `crates/cairn-store-sqlite/src/record_wal/mod.rs`
- Create: `crates/cairn-store-sqlite/src/record_wal/payload.rs`
- Create: `crates/cairn-store-sqlite/src/record_wal/locks.rs`
- Create: `crates/cairn-store-sqlite/src/record_wal/ops.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Modify: `crates/cairn-store-sqlite/src/error.rs`
- Create: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`

- [ ] **Step 1: Write the failing payload round-trip test**

Create `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs` with this initial content:

```rust
//! Issue #57: COW upsert and expire through body-bearing WAL.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{
    KeywordSearchArgs, ListArgs, MemoryStore, TombstoneReason,
};
use cairn_core::domain::{MemoryRecord, TargetId};
use cairn_store_sqlite::open_in_memory;
use rusqlite::params;

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn payload_round_trip_smoke() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record = sample();
    let payload = cairn_store_sqlite::record_wal::payload::UpsertPayload::new_for_test(record);

    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-round-trip', 1, 'upsert', 'ISSUED', '{}', 'issuer', 'target', '{}', 0, 'sig', 1, 1)",
            [],
        )?;
        cairn_store_sqlite::record_wal::payload::save_upsert_payload_for_test(
            c,
            "op-round-trip",
            &payload,
        )?;
        let loaded = cairn_store_sqlite::record_wal::payload::load_upsert_payload_for_test(
            c,
            "op-round-trip",
        )?;
        assert_eq!(loaded.record.target_id, payload.record.target_id);
        assert_eq!(loaded.record.body, payload.record.body);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("payload round trip");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire payload_round_trip_smoke -- --nocapture`

Expected: FAIL with an import error for `cairn_store_sqlite::record_wal`.

- [ ] **Step 3: Expose the module inside the crate**

In `crates/cairn-store-sqlite/src/lib.rs`, add:

```rust
pub mod record_wal;
```

Use `pub mod` because the integration tests in this task call test-only helper functions gated inside the module. Keep production functions `pub(crate)` unless tests need them.

- [ ] **Step 4: Add record WAL module root**

Create `crates/cairn-store-sqlite/src/record_wal/mod.rs`:

```rust
//! Body-bearing WAL apply for record upsert and expire operations.

pub mod payload;

pub(crate) mod locks;
pub(crate) mod ops;
pub(crate) mod recovery;
pub(crate) mod steps;
pub(crate) mod upsert;
pub(crate) mod expire;

pub(crate) use expire::apply_expire;
pub(crate) use upsert::apply_upsert;
pub use recovery::RecordWalRegistry;
```

- [ ] **Step 5: Add payload types and load/save helpers**

Create `crates/cairn-store-sqlite/src/record_wal/payload.rs`:

```rust
//! Durable JSON payloads for record WAL operations.

use cairn_core::domain::{MemoryRecord, TargetId};
use cairn_core::wal::{OperationId, WalKind};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::store::current_unix_ms;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordWalPayload {
    Upsert(UpsertPayload),
    Expire(ExpirePayload),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpsertPayload {
    pub record: MemoryRecord,
    pub embed: StoredEmbedOutcome,
    pub planned: PlannedUpsert,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlannedUpsert {
    pub outcome_record_id: String,
    pub target_id: String,
    pub version: u32,
    pub content_changed: bool,
    pub prior_record_id: Option<String>,
    pub prior_hash: Option<String>,
    pub consent_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StoredEmbedOutcome {
    Succeeded { vector: Vec<u8>, model_label: String },
    Failed { error: String },
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpirePayload {
    pub target_id: TargetId,
    pub reason: String,
}

pub(crate) fn save_payload(
    conn: &Connection,
    op_id: &OperationId,
    payload: &RecordWalPayload,
) -> Result<(), StoreError> {
    let kind = match payload {
        RecordWalPayload::Upsert(_) => WalKind::Upsert.as_str(),
        RecordWalPayload::Expire(_) => WalKind::Expire.as_str(),
    };
    let json = serde_json::to_string(payload)?;
    conn.execute(
        "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![op_id.as_str(), kind, json, current_unix_ms()],
    )?;
    Ok(())
}

pub(crate) fn load_payload(
    conn: &Connection,
    op_id: &OperationId,
) -> Result<RecordWalPayload, StoreError> {
    let json: String = conn
        .query_row(
            "SELECT payload_json FROM wal_payloads WHERE operation_id = ?1",
            params![op_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Invariant {
            what: format!("missing wal_payloads row for operation {}", op_id.as_str()),
        })?;
    Ok(serde_json::from_str(&json)?)
}

#[cfg(any(test, feature = "test-helpers"))]
impl UpsertPayload {
    #[must_use]
    pub fn new_for_test(record: MemoryRecord) -> Self {
        Self {
            planned: PlannedUpsert {
                outcome_record_id: record.id.as_str().to_owned(),
                target_id: record.target_id.as_str().to_owned(),
                version: 1,
                content_changed: true,
                prior_record_id: None,
                prior_hash: None,
                consent_model: "legacy_event".to_owned(),
            },
            record,
            embed: StoredEmbedOutcome::Skipped,
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn save_upsert_payload_for_test(
    conn: &Connection,
    op_id: &str,
    payload: &UpsertPayload,
) -> Result<(), StoreError> {
    let op = OperationId::parse(op_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid test op id: {e}"),
    })?;
    save_payload(conn, &op, &RecordWalPayload::Upsert(payload.clone()))
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn load_upsert_payload_for_test(
    conn: &Connection,
    op_id: &str,
) -> Result<UpsertPayload, StoreError> {
    let op = OperationId::parse(op_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid test op id: {e}"),
    })?;
    match load_payload(conn, &op)? {
        RecordWalPayload::Upsert(payload) => Ok(payload),
        RecordWalPayload::Expire(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found expire payload".to_owned(),
        }),
    }
}
```

- [ ] **Step 6: Add operation and lock modules with compilable shells**

Create `crates/cairn-store-sqlite/src/record_wal/ops.rs`:

```rust
//! `wal_ops` issue, prepare, and finalize helpers for record operations.

use cairn_core::wal::{OpState, OperationId, WalKind};
use rusqlite::{Connection, params};

use crate::error::StoreError;
use crate::store::current_unix_ms;

#[must_use]
pub(crate) fn new_operation_id(kind: WalKind) -> OperationId {
    let raw = format!("{}-{}", kind.as_str(), ulid::Ulid::new());
    OperationId::parse(raw).expect("operation id built from non-empty kind and ULID")
}

pub(crate) fn issue_prepared(
    conn: &Connection,
    op_id: &OperationId,
    kind: WalKind,
    target_hash: &str,
    scope_json: &str,
) -> Result<(), StoreError> {
    let now = current_unix_ms();
    conn.execute(
        "INSERT INTO wal_ops \
           (operation_id, issued_seq, kind, state, envelope, issuer, principal, \
            target_hash, scope_json, plan_ref, expires_at, signature, issued_at, updated_at) \
         VALUES (?1, COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0) + 1, ?2, \
            'ISSUED', '{}', 'cairn-store-sqlite', NULL, ?3, ?4, NULL, 0, 'local', ?5, ?5)",
        params![op_id.as_str(), kind.as_str(), target_hash, scope_json, now],
    )?;
    conn.execute(
        "UPDATE wal_ops SET state = 'PREPARED', updated_at = ?1 WHERE operation_id = ?2",
        params![now, op_id.as_str()],
    )?;
    Ok(())
}

pub(crate) fn finalize(
    conn: &Connection,
    op_id: &OperationId,
    state: OpState,
    reason: &str,
) -> Result<(), StoreError> {
    let now = current_unix_ms();
    conn.execute(
        "UPDATE wal_ops SET state = ?1, reason = COALESCE(reason, ?2), updated_at = ?3 \
         WHERE operation_id = ?4",
        params![state.as_str(), reason, now, op_id.as_str()],
    )?;
    Ok(())
}
```

Create `crates/cairn-store-sqlite/src/record_wal/locks.rs`:

```rust
//! Lock acquisition for record WAL operations.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::domain::{ScopeTuple, TargetId};
use tokio_rusqlite::Connection;

use crate::locks::{LockHandle, LockMode, ResourceKey, acquire};

pub(crate) struct RecordLocks {
    handles: Vec<LockHandle>,
}

impl RecordLocks {
    #[must_use]
    pub(crate) fn new(handles: Vec<LockHandle>) -> Self {
        Self { handles }
    }

    pub(crate) fn assert_live_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<(), crate::locks::LockError> {
        for handle in &self.handles {
            handle.assert_live_in_tx(tx)?;
        }
        Ok(())
    }
}

pub(crate) async fn acquire_for_record(
    conn: &Arc<Connection>,
    scope: &ScopeTuple,
    target: &TargetId,
    incarnation: &Arc<str>,
    op_id: &str,
    operation: &'static str,
) -> Result<RecordLocks, crate::locks::LockError> {
    let (tenant, workspace) = scope_lock_parts(scope);
    let mut handles = Vec::with_capacity(2);
    handles.push(
        acquire(
            conn,
            &ResourceKey::entity(&tenant, &workspace, target.as_str()),
            LockMode::Exclusive,
            &format!("{op_id}:entity"),
            Duration::from_secs(30),
            incarnation,
            operation,
        )
        .await?,
    );
    if let Some(session_id) = scope.session_id.as_deref() {
        handles.push(
            acquire(
                conn,
                &ResourceKey::session(&tenant, &workspace, session_id),
                LockMode::Shared,
                &format!("{op_id}:session"),
                Duration::from_secs(30),
                incarnation,
                operation,
            )
            .await?,
        );
    }
    Ok(RecordLocks::new(handles))
}

fn scope_lock_parts(scope: &ScopeTuple) -> (String, String) {
    (
        scope.tenant.as_deref().unwrap_or("default").to_owned(),
        scope.workspace.as_deref().unwrap_or("default").to_owned(),
    )
}
```

- [ ] **Step 7: Add StoreError variants used by record WAL**

In `crates/cairn-store-sqlite/src/error.rs`, add these variants near `Recovery` and `LockInit`:

```rust
    /// Record-WAL step runner failed during public apply.
    #[error("record wal runner")]
    RecordWalRunner(#[from] crate::wal::RunnerError),

    /// Record-WAL apply could not acquire or assert locks.
    #[error("record wal lock")]
    RecordWalLock(#[source] Box<crate::locks::LockError>),
```

- [ ] **Step 8: Add empty modules named by `mod.rs`**

Create `crates/cairn-store-sqlite/src/record_wal/steps.rs`:

```rust
//! Record WAL step bodies.
```

Create `crates/cairn-store-sqlite/src/record_wal/recovery.rs`:

```rust
//! Record WAL recovery registry.
```

Create `crates/cairn-store-sqlite/src/record_wal/upsert.rs`:

```rust
//! Public upsert apply through record WAL.
```

Create `crates/cairn-store-sqlite/src/record_wal/expire.rs`:

```rust
//! Public expire apply through record WAL.
```

- [ ] **Step 9: Run the payload test to verify it passes**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire payload_round_trip_smoke -- --nocapture`

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/cairn-store-sqlite/src/lib.rs \
  crates/cairn-store-sqlite/src/error.rs \
  crates/cairn-store-sqlite/src/record_wal \
  crates/cairn-store-sqlite/tests/cow_upsert_expire.rs
git commit -m "feat: add record wal payload infrastructure"
```

---

### Task 4: Route Public Upsert Through WAL and Keep Existing Semantics

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/upsert.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/upsert.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
- Modify: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`
- Modify: existing upsert tests remain unchanged and must pass.

- [ ] **Step 1: Add the failing WAL conformance test**

Append to `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`:

```rust
#[tokio::test]
async fn upsert_commits_wal_operation_and_all_steps() {
    let store = open_in_memory().await.expect("open");
    let record = sample();

    let out = store.upsert(&record).await.expect("upsert through wal");
    assert_eq!(out.version, 1);
    assert!(out.content_changed);

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(|c| {
        let row: (String, i64) = c.query_row(
            "SELECT state, COUNT(*) FROM wal_ops WHERE kind = 'upsert' GROUP BY state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(row, ("COMMITTED".to_owned(), 1));

        let done_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert' AND ws.state = 'DONE'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(done_count, 6);

        let payload_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_payloads wp \
              JOIN wal_ops wo ON wo.operation_id = wp.operation_id \
             WHERE wo.kind = 'upsert'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(payload_count, 1);

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("wal rows");
}
```

- [ ] **Step 2: Run the conformance test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire upsert_commits_wal_operation_and_all_steps -- --nocapture`

Expected: FAIL because `MemoryStore::upsert` writes no `wal_ops` row.

- [ ] **Step 3: Make embedding outcome reusable by WAL payloads**

In `crates/cairn-store-sqlite/src/store/upsert.rs`, change `enum EmbedOutcome` to:

```rust
pub(crate) enum EmbedOutcome {
    Succeeded {
        vector: Vec<u8>,
        model_label: String,
    },
    Failed { error: String },
    Skipped,
}
```

Add this impl below the enum:

```rust
impl From<EmbedOutcome> for crate::record_wal::payload::StoredEmbedOutcome {
    fn from(value: EmbedOutcome) -> Self {
        match value {
            EmbedOutcome::Succeeded {
                vector,
                model_label,
            } => Self::Succeeded {
                vector,
                model_label,
            },
            EmbedOutcome::Failed { error } => Self::Failed { error },
            EmbedOutcome::Skipped => Self::Skipped,
        }
    }
}
```

- [ ] **Step 4: Replace `do_upsert` SQL mutation with WAL apply**

In `crates/cairn-store-sqlite/src/store/upsert.rs`, keep validation and embedding precompute, then replace the `conn.call` block that calls `upsert_in_tx` with:

```rust
        let payload_embed = crate::record_wal::payload::StoredEmbedOutcome::from(embed_outcome);
        crate::record_wal::apply_upsert(self, record, payload_embed).await
```

Remove no longer used imports from `do_upsert`, but keep `upsert_in_tx` and its helpers for current transaction callers until Task 5 splits them into COW primitives.

- [ ] **Step 5: Implement minimal WAL apply using current upsert primitive**

Add to `crates/cairn-store-sqlite/src/record_wal/upsert.rs`:

```rust
use std::sync::Arc;

use cairn_core::contract::memory_store::UpsertOutcome;
use cairn_core::domain::MemoryRecord;
use cairn_core::wal::{OpState, WalKind, graph_for};

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{
    PlannedUpsert, RecordWalPayload, StoredEmbedOutcome, UpsertPayload, save_payload,
};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::store::upsert::upsert_in_tx;
use crate::wal::runner;

pub(crate) async fn apply_upsert(
    store: &SqliteMemoryStore,
    record: &MemoryRecord,
    embed: StoredEmbedOutcome,
) -> Result<UpsertOutcome, StoreError> {
    let conn = store.require_conn("upsert")?.clone();
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "upsert requires daemon incarnation".to_owned(),
    })?;
    let op_id = new_operation_id(WalKind::Upsert);
    let locks = acquire_for_record(
        &conn,
        &record.scope,
        &record.target_id,
        &incarnation,
        op_id.as_str(),
        "record_wal_upsert",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let record_for_plan = record.clone();
    let record_for_payload = record.clone();
    let planned = conn
        .call(move |c| {
            let mut tx = c.transaction()?;
            let out = upsert_in_tx(&mut tx, &record_for_plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(PlannedUpsert {
                outcome_record_id: out.record_id.as_str().to_owned(),
                target_id: out.target_id.as_str().to_owned(),
                version: out.version,
                content_changed: out.content_changed,
                prior_record_id: None,
                prior_hash: out.prior_hash.map(|h| h.to_string()),
                consent_model: "legacy_event".to_owned(),
            })
        })
        .await?;

    let payload = UpsertPayload {
        record: record_for_payload,
        embed,
        planned,
    };
    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(
            &tx,
            &op_for_issue,
            WalKind::Upsert,
            &payload_for_body.planned.target_id,
            "{}",
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(&tx, &op_for_issue, &RecordWalPayload::Upsert(payload_for_body))
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let planned_for_outcome = payload.planned.clone();
    let body = Arc::new(RecordStepBody::new_upsert(payload.clone(), locks));
    runner::run_from(&conn, graph_for(WalKind::Upsert), &op_id, 0, body).await?;

    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    Ok(UpsertOutcome {
        record_id: cairn_core::domain::RecordId::parse(
            planned_for_outcome.outcome_record_id.clone(),
        )
            .map_err(|e| StoreError::Invariant {
                what: format!("planned record_id invalid: {e}"),
            })?,
        target_id: record.target_id.clone(),
        version: planned_for_outcome.version,
        content_changed: planned_for_outcome.content_changed,
        prior_hash: planned_for_outcome
            .prior_hash
            .map(cairn_core::domain::BodyHash::parse)
            .transpose()
            .map_err(|e| StoreError::Invariant {
                what: format!("planned prior_hash invalid: {e}"),
            })?,
    })
}
```

This is a bootstrap implementation. Task 5 replaces the rollback planning call with a true planning helper and replaces the step body with real COW steps.

- [ ] **Step 6: Change the runner body contract to mutable transactions**

In `crates/cairn-store-sqlite/src/wal/runner.rs`, change the `StepBody::run` signature from `&Transaction<'_>` to `&mut Transaction<'_>`:

```rust
pub trait StepBody: Send + Sync {
    fn run(
        &self,
        tx: &mut Transaction<'_>,
        op_id: &OperationId,
        step: &StepDef,
    ) -> Result<(), StepBodyError>;
}
```

In `try_one_attempt`, change:

```rust
                let tx = c.transaction()?;
```

to:

```rust
                let mut tx = c.transaction()?;
```

and change:

```rust
                let r = body.run(&tx, &op_id_for_body, &step_owned);
```

to:

```rust
                let r = body.run(&mut tx, &op_id_for_body, &step_owned);
```

Update all test `StepBody` implementations to accept `&mut Transaction<'_>`.

- [ ] **Step 7: Add a step body that delegates to current upsert once**

Add to `crates/cairn-store-sqlite/src/record_wal/steps.rs`:


```rust
use cairn_core::wal::{OperationId, StepDef};
use rusqlite::Transaction;

use crate::record_wal::locks::RecordLocks;
use crate::record_wal::payload::{ExpirePayload, UpsertPayload};
use crate::store::upsert::upsert_in_tx;
use crate::wal::runner::{StepBody, StepBodyError};

pub(crate) enum RecordStepPayload {
    Upsert(UpsertPayload),
    Expire(ExpirePayload),
}

pub(crate) struct RecordStepBody {
    payload: RecordStepPayload,
    locks: RecordLocks,
}

impl RecordStepBody {
    #[must_use]
    pub(crate) fn new_upsert(payload: UpsertPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Upsert(payload),
            locks,
        }
    }
}

impl StepBody for RecordStepBody {
    fn run(
        &self,
        tx: &mut Transaction<'_>,
        _op_id: &OperationId,
        step: &StepDef,
    ) -> Result<(), StepBodyError> {
        self.locks
            .assert_live_in_tx(tx)
            .map_err(|e| StepBodyError::Failed(format!("fenced: {e}")))?;

        match (&self.payload, step.name) {
            (RecordStepPayload::Upsert(payload), "primary.upsert_cow") => {
                upsert_in_tx(tx, &payload.record)
                    .map_err(|e| StepBodyError::Failed(e.to_string()))?;
                Ok(())
            }
            (RecordStepPayload::Upsert(_), _) => Ok(()),
            (RecordStepPayload::Expire(_), _) => Ok(()),
        }
    }
}
```

- [ ] **Step 8: Run upsert regression tests**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire upsert_commits_wal_operation_and_all_steps -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test upsert_idempotent --test versioning -- --nocapture`

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/upsert.rs \
  crates/cairn-store-sqlite/src/record_wal/upsert.rs \
  crates/cairn-store-sqlite/src/record_wal/steps.rs \
  crates/cairn-store-sqlite/src/wal/runner.rs \
  crates/cairn-store-sqlite/tests/cow_upsert_expire.rs \
  crates/cairn-store-sqlite/tests/wal_recovery.rs
git commit -m "feat: route upsert through wal apply"
```

---

### Task 5: Replace Direct Upsert with True Copy-On-Write Steps

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/upsert.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/payload.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/upsert.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
- Modify: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`

- [ ] **Step 1: Add the failing COW visibility test**

Append this test to `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`:

```rust
#[tokio::test]
async fn upsert_stages_inactive_row_before_activation() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("initial upsert");

    let mut r2 = r.clone();
    r2.body = "changed body for cow staging".to_owned();

    let out = store.upsert(&r2).await.expect("second upsert");
    assert_eq!(out.version, 2);

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let rows: Vec<(String, i64, i64)> = conn
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT record_id, active, tombstoned FROM records \
                 WHERE target_id = ?1 ORDER BY version",
            )?;
            let rows = stmt
                .query_map(params![r.target_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await
        .expect("records");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, 0, "prior version inactive after activation");
    assert_eq!(rows[1].1, 1, "new version active after activation");
    assert_eq!(rows[0].2, 0, "superseded row is not tombstoned");
    assert_eq!(rows[1].2, 0, "new row is visible");
}
```

- [ ] **Step 2: Run the test to verify the current bridge is insufficient**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire upsert_stages_inactive_row_before_activation -- --nocapture`

Expected: PASS may occur because direct upsert has the final state. Keep the test; the following unit tests pin the intermediate COW primitives.

- [ ] **Step 3: Add unit tests for planning and staging**

Add a `#[cfg(test)] mod cow_tests` to `crates/cairn-store-sqlite/src/store/upsert.rs`:

```rust
#[cfg(test)]
mod cow_tests {
    use cairn_core::domain::record::tests_export::sample_record;

    use crate::open_in_memory;
    use crate::store::upsert::{
        activate_upsert_in_tx, plan_upsert_in_tx, stage_upsert_cow_in_tx,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn cow_stage_inserts_inactive_row() {
        let store = open_in_memory().await.expect("open");
        let conn = store.require_conn("test").expect("connected").clone();
        let record = sample_record();

        conn.call(move |c| {
            let mut tx = c.transaction()?;
            let plan = plan_upsert_in_tx(&tx, &record)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            stage_upsert_cow_in_tx(&tx, &record, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let active: i64 = tx.query_row(
                "SELECT active FROM records WHERE record_id = ?1",
                rusqlite::params![plan.outcome_record_id.as_str()],
                |row| row.get(0),
            )?;
            assert_eq!(active, 0);
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("cow stage");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cow_activate_flips_active_pointer_in_one_transaction() {
        let store = open_in_memory().await.expect("open");
        let conn = store.require_conn("test").expect("connected").clone();
        let record = sample_record();

        conn.call(move |c| {
            let tx = c.transaction()?;
            let plan = plan_upsert_in_tx(&tx, &record)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            stage_upsert_cow_in_tx(&tx, &record, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            activate_upsert_in_tx(&tx, &plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let active: i64 = tx.query_row(
                "SELECT active FROM records WHERE record_id = ?1",
                rusqlite::params![plan.outcome_record_id.as_str()],
                |row| row.get(0),
            )?;
            assert_eq!(active, 1);
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("cow activate");
    }
}
```

- [ ] **Step 4: Run unit tests to verify missing helpers**

Run: `cargo test -p cairn-store-sqlite store::upsert::cow_tests -- --nocapture`

Expected: FAIL with unresolved imports for `plan_upsert_in_tx`, `stage_upsert_cow_in_tx`, and `activate_upsert_in_tx`.

- [ ] **Step 5: Add COW planning and step primitives**

In `crates/cairn-store-sqlite/src/store/upsert.rs`, make `PriorActive` public in crate and add these helpers near `upsert_in_tx`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpsertPlan {
    pub outcome_record_id: RecordId,
    pub target_id: cairn_core::domain::TargetId,
    pub version: u32,
    pub content_changed: bool,
    pub prior_record_id: Option<String>,
    pub prior_hash: Option<BodyHash>,
    pub consent_model: String,
}

pub(crate) fn plan_upsert_in_tx(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
) -> Result<UpsertPlan, StoreError> {
    record.validate()?;
    let body_hash = BodyHash::compute(&record.body);
    let prior = read_active(tx, record.target_id.as_str())?;
    if let Some((prior_id, prior_version, prior_hash_str, prior_consent_model)) = prior.as_ref() {
        let _: cairn_core::domain::consent_timeline::ConsentModel =
            parse_consent_model(prior_consent_model)?;
        let prior_hash = body_hash_from_str(prior_hash_str)?;
        if prior_hash == body_hash {
            let prior_record_id =
                RecordId::parse(prior_id.clone()).map_err(|e| StoreError::Invariant {
                    what: format!("invalid prior record_id `{prior_id}`: {e}"),
                })?;
            return Ok(UpsertPlan {
                outcome_record_id: prior_record_id,
                target_id: record.target_id.clone(),
                version: u32::try_from(*prior_version).map_err(|_| StoreError::Invariant {
                    what: format!("prior version overflows u32: {prior_version}"),
                })?,
                content_changed: false,
                prior_record_id: Some(prior_id.clone()),
                prior_hash: Some(prior_hash),
                consent_model: prior_consent_model.clone(),
            });
        }
        return Ok(UpsertPlan {
            outcome_record_id: mint_record_id()?,
            target_id: record.target_id.clone(),
            version: next_version(*prior_version)?,
            content_changed: true,
            prior_record_id: Some(prior_id.clone()),
            prior_hash: Some(prior_hash),
            consent_model: prior_consent_model.clone(),
        });
    }

    let max_version: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM records WHERE target_id = ?1",
        params![record.target_id.as_str()],
        |row| row.get(0),
    )?;
    let version = next_version(max_version)?;
    let outcome_record_id = if max_version == 0 {
        record.id.clone()
    } else {
        mint_record_id()?
    };
    Ok(UpsertPlan {
        outcome_record_id,
        target_id: record.target_id.clone(),
        version,
        content_changed: true,
        prior_record_id: None,
        prior_hash: None,
        consent_model: "legacy_event".to_owned(),
    })
}

pub(crate) fn stage_upsert_cow_in_tx(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    plan: &UpsertPlan,
) -> Result<(), StoreError> {
    let body_hash = BodyHash::compute(&record.body);
    let now_ms = current_unix_ms();
    if !plan.content_changed {
        if let Some(prior_id) = plan.prior_record_id.as_deref() {
            reproject_in_place(
                tx,
                record,
                prior_id,
                i64::from(plan.version),
                &body_hash,
                now_ms,
            )?;
        }
        return Ok(());
    }
    let mut row_record = record.clone();
    row_record.id = plan.outcome_record_id.clone();
    insert_row_with_active(
        tx,
        &row_record,
        plan.version,
        now_ms,
        &body_hash,
        &plan.consent_model,
        false,
    )
}

pub(crate) fn activate_upsert_in_tx(
    tx: &Transaction<'_>,
    plan: &UpsertPlan,
) -> Result<(), StoreError> {
    if !plan.content_changed {
        return Ok(());
    }
    let now_ms = current_unix_ms();
    tx.execute(
        "UPDATE records SET active = 0, updated_at = ?1 \
          WHERE target_id = ?2 AND active = 1 AND record_id != ?3",
        params![
            now_ms,
            plan.target_id.as_str(),
            plan.outcome_record_id.as_str()
        ],
    )?;
    tx.execute(
        "UPDATE records SET active = 1, tombstoned = 0, updated_at = ?1 \
          WHERE record_id = ?2",
        params![now_ms, plan.outcome_record_id.as_str()],
    )?;
    Ok(())
}
```

Rename `insert_row` to call a new internal function:

```rust
fn insert_row(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    version: u32,
    now_ms: i64,
    body_hash: &BodyHash,
    consent_model: &str,
) -> Result<(), StoreError> {
    insert_row_with_active(tx, record, version, now_ms, body_hash, consent_model, true)
}

fn insert_row_with_active(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    version: u32,
    now_ms: i64,
    body_hash: &BodyHash,
    consent_model: &str,
    active: bool,
) -> Result<(), StoreError> {
    let _: cairn_core::domain::consent_timeline::ConsentModel = parse_consent_model(consent_model)?;
    let mut row =
        ProjectedRow::from_record(record, version, now_ms, now_ms, body_hash, active, false)?;
    row.consent_model = match consent_model {
        "receipt_timeline" => "receipt_timeline",
        "legacy_event" => "legacy_event",
        other => {
            return Err(StoreError::Invariant {
                what: format!("consent_model `{other}` survived parse_consent_model gate"),
            });
        }
    };
    tx.execute(
        "INSERT INTO records ( \
            record_id, target_id, version, path, kind, class, visibility, \
            scope, actor_chain, body, body_hash, created_at, updated_at, \
            active, tombstoned, is_static, record_json, confidence, \
            salience, target_id_explicit, tags_json, consent_model, \
            schema_version_major, schema_version_minor \
         ) VALUES ( \
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24 \
         ) \
         ON CONFLICT(record_id) DO UPDATE SET \
            updated_at = excluded.updated_at",
        params![
            row.record_id,
            row.target_id,
            row.version,
            row.path,
            row.kind,
            row.class,
            row.visibility,
            row.scope,
            row.actor_chain,
            row.body,
            row.body_hash,
            row.created_at,
            row.updated_at,
            row.active,
            row.tombstoned,
            row.is_static,
            row.record_json,
            row.confidence,
            row.salience,
            row.target_id_explicit,
            row.tags_json,
            row.consent_model,
            row.schema_version_major,
            row.schema_version_minor,
        ],
    )?;
    Ok(())
}
```

- [ ] **Step 6: Persist the real upsert plan in payload**

In `crates/cairn-store-sqlite/src/record_wal/upsert.rs`, replace rollback planning with a read-only transaction:

```rust
let planned = conn
    .call({
        let record_for_plan = record.clone();
        move |c| {
            let tx = c.transaction()?;
            let plan = crate::store::upsert::plan_upsert_in_tx(&tx, &record_for_plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(PlannedUpsert {
                outcome_record_id: plan.outcome_record_id.as_str().to_owned(),
                target_id: plan.target_id.as_str().to_owned(),
                version: plan.version,
                content_changed: plan.content_changed,
                prior_record_id: plan.prior_record_id,
                prior_hash: plan.prior_hash.map(|h| h.to_string()),
                consent_model: plan.consent_model,
            })
        }
    })
    .await?;
```

- [ ] **Step 7: Dispatch real COW step bodies**

In `crates/cairn-store-sqlite/src/record_wal/steps.rs`, replace the `primary.upsert_cow` and `primary.activate` branches:

```rust
            (RecordStepPayload::Upsert(payload), "primary.upsert_cow") => {
                let plan = payload.planned.to_store_plan()?;
                crate::store::upsert::stage_upsert_cow_in_tx(tx, &payload.record, &plan)
                    .map_err(|e| StepBodyError::Failed(e.to_string()))
            }
            (RecordStepPayload::Upsert(payload), "primary.activate") => {
                let plan = payload.planned.to_store_plan()?;
                crate::store::upsert::activate_upsert_in_tx(tx, &plan)
                    .map_err(|e| StepBodyError::Failed(e.to_string()))
            }
```

Add this conversion in `payload.rs`:

```rust
impl PlannedUpsert {
    pub(crate) fn to_store_plan(&self) -> Result<crate::store::upsert::UpsertPlan, StoreError> {
        Ok(crate::store::upsert::UpsertPlan {
            outcome_record_id: cairn_core::domain::RecordId::parse(
                self.outcome_record_id.clone(),
            )
            .map_err(|e| StoreError::Invariant {
                what: format!("planned record id invalid: {e}"),
            })?,
            target_id: cairn_core::domain::TargetId::parse(self.target_id.clone()).map_err(
                |e| StoreError::Invariant {
                    what: format!("planned target id invalid: {e}"),
                },
            )?,
            version: self.version,
            content_changed: self.content_changed,
            prior_record_id: self.prior_record_id.clone(),
            prior_hash: self
                .prior_hash
                .as_ref()
                .map(|h| cairn_core::domain::BodyHash::parse(h))
                .transpose()
                .map_err(|e| StoreError::Invariant {
                    what: format!("planned prior hash invalid: {e}"),
                })?,
            consent_model: self.consent_model.clone(),
        })
    }
}
```

- [ ] **Step 8: Store snapshot pre-images in `snapshot.stage`**

Append this test to `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`:

```rust
#[tokio::test]
async fn upsert_snapshot_stage_records_pre_image_blob() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(|c| {
        let count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'upsert' \
               AND ws.step_kind = 'snapshot.stage' \
               AND ws.pre_image IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("snapshot stage");
}
```

In `RecordStepBody::run`, rename `_op_id` to `op_id`, then add the upsert branch before `primary.upsert_cow`:

```rust
            (RecordStepPayload::Upsert(payload), "snapshot.stage") => {
                stage_snapshot(tx, op_id, step, &payload.planned.target_id)
            }
```

Add this helper in `crates/cairn-store-sqlite/src/record_wal/steps.rs`:

```rust
fn stage_snapshot(
    tx: &rusqlite::Transaction<'_>,
    op_id: &OperationId,
    step: &StepDef,
    target_id: &str,
) -> Result<(), StepBodyError> {
    let rows = {
        let mut stmt = tx
            .prepare(
                "SELECT record_id, version, active, tombstoned, tombstone_reason, body_hash \
                 FROM records WHERE target_id = ?1 ORDER BY version",
            )
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(rusqlite::params![target_id], |row| {
            Ok(serde_json::json!({
                "record_id": row.get::<_, String>(0)?,
                "version": row.get::<_, i64>(1)?,
                "active": row.get::<_, i64>(2)?,
                "tombstoned": row.get::<_, i64>(3)?,
                "tombstone_reason": row.get::<_, Option<String>>(4)?,
                "body_hash": row.get::<_, String>(5)?,
            }))
        })
        .map_err(StepBodyError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StepBodyError::Storage)?
    };
    let bytes = serde_json::to_vec(&rows)
        .map_err(|e| StepBodyError::Failed(format!("snapshot json: {e}")))?;
    tx.execute(
        "UPDATE wal_steps SET pre_image = ?1 \
         WHERE operation_id = ?2 AND step_ord = ?3",
        rusqlite::params![bytes, op_id.as_str(), step.ord],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}
```

- [ ] **Step 9: Run COW and upsert tests**

Run: `cargo test -p cairn-store-sqlite store::upsert::cow_tests -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire upsert_stages_inactive_row_before_activation -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test upsert_idempotent --test versioning --test upsert_embed -- --nocapture`

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/upsert.rs \
  crates/cairn-store-sqlite/src/record_wal/payload.rs \
  crates/cairn-store-sqlite/src/record_wal/upsert.rs \
  crates/cairn-store-sqlite/src/record_wal/steps.rs \
  crates/cairn-store-sqlite/tests/cow_upsert_expire.rs
git commit -m "feat: apply upsert with copy on write steps"
```

---

### Task 6: Make Derived Upsert Steps Idempotent and Retry-Safe

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
- Modify: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`
- Existing tests: `crates/cairn-store-sqlite/tests/upsert_embed.rs`, `crates/cairn-store-sqlite/tests/search_keyword.rs`

- [ ] **Step 1: Add the failing derived retry test**

Append to `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`:

```rust
#[tokio::test]
async fn repeated_upsert_steps_do_not_duplicate_derived_rows() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");
    store.upsert(&r).await.expect("idempotent upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let fts_rows: i64 = c.query_row("SELECT COUNT(*) FROM records_fts", [], |row| {
            row.get(0)
        })?;
        let active_records: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(fts_rows, active_records);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("derived counts");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire repeated_upsert_steps_do_not_duplicate_derived_rows -- --nocapture`

Expected: FAIL if FTS rows duplicate or PASS if triggers masked the gap. Keep the test either way.

- [ ] **Step 3: Add explicit FTS/vector/edges upsert functions**

In `crates/cairn-store-sqlite/src/record_wal/steps.rs`, add:

```rust
fn upsert_fts(tx: &rusqlite::Transaction<'_>, record_id: &str) -> Result<(), StepBodyError> {
    let row: (i64, String, String, String, String) = tx
        .query_row(
            "SELECT rowid, kind, class, scope, body FROM records WHERE record_id = ?1",
            rusqlite::params![record_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(StepBodyError::Storage)?;
    tx.execute("DELETE FROM records_fts WHERE rowid = ?1", rusqlite::params![row.0])
        .map_err(StepBodyError::Storage)?;
    tx.execute(
        "INSERT INTO records_fts(rowid, kind, class, scope, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![row.0, row.1, row.2, row.3, row.4],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn upsert_vector(
    tx: &rusqlite::Transaction<'_>,
    record_id: &str,
    embed: &crate::record_wal::payload::StoredEmbedOutcome,
) -> Result<(), StepBodyError> {
    match embed {
        crate::record_wal::payload::StoredEmbedOutcome::Succeeded {
            vector,
            model_label,
        } => {
            tx.execute(
                "DELETE FROM record_vectors WHERE record_id = ?1",
                rusqlite::params![record_id],
            )
            .map_err(StepBodyError::Storage)?;
            tx.execute(
                "INSERT INTO record_vectors(record_id, embedding, model) VALUES (?1, ?2, ?3)",
                rusqlite::params![record_id, vector, model_label],
            )
            .map_err(StepBodyError::Storage)?;
            tx.execute(
                "DELETE FROM pending_embeddings WHERE record_id = ?1",
                rusqlite::params![record_id],
            )
            .map_err(StepBodyError::Storage)?;
            Ok(())
        }
        crate::record_wal::payload::StoredEmbedOutcome::Failed { error } => {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            tx.execute(
                "INSERT INTO pending_embeddings \
                    (record_id, reason, attempt_count, last_error, enqueued_at) \
                 VALUES (?1, 'embed_failed', 0, ?2, ?3) \
                 ON CONFLICT(record_id) DO UPDATE SET \
                    attempt_count = pending_embeddings.attempt_count + 1, \
                    last_error = excluded.last_error, \
                    last_attempt_at = ?3",
                rusqlite::params![record_id, error, now_secs],
            )
            .map_err(StepBodyError::Storage)?;
            Ok(())
        }
        crate::record_wal::payload::StoredEmbedOutcome::Skipped => Ok(()),
    }
}

fn upsert_edges(_tx: &rusqlite::Transaction<'_>, _record_id: &str) -> Result<(), StepBodyError> {
    Ok(())
}
```

- [ ] **Step 4: Wire the derived step branches**

In `RecordStepBody::run`, add branches before the catch-all:

```rust
            (RecordStepPayload::Upsert(payload), "vector.upsert") => {
                upsert_vector(tx, &payload.planned.outcome_record_id, &payload.embed)
            }
            (RecordStepPayload::Upsert(payload), "fts.upsert") => {
                upsert_fts(tx, &payload.planned.outcome_record_id)
            }
            (RecordStepPayload::Upsert(payload), "edges.upsert") => {
                upsert_edges(tx, &payload.planned.outcome_record_id)
            }
```

- [ ] **Step 5: Run derived index tests**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire repeated_upsert_steps_do_not_duplicate_derived_rows -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test upsert_embed --test search_keyword --test index_stats -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/steps.rs \
  crates/cairn-store-sqlite/tests/cow_upsert_expire.rs
git commit -m "feat: make record upsert derived steps idempotent"
```

---

### Task 7: Add Expire Apply With Soft Retirement

**Files:**
- Create: `crates/cairn-store-sqlite/src/store/expire.rs`
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/expire.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
- Modify: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`

- [ ] **Step 1: Add the failing expire acceptance test**

Append to `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`:

```rust
#[tokio::test]
async fn expire_retires_target_without_hard_delete() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    let out = store.upsert(&r).await.expect("upsert");

    store.expire(&r.target_id).await.expect("expire");

    assert!(store.get(&out.record_id).await.expect("get").is_none());
    assert!(store.list(&ListArgs::default()).await.expect("list").records.is_empty());

    let versions = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(versions.len(), 1);
    assert!(!versions[0].active);
    assert!(versions[0].tombstoned);
    assert_eq!(versions[0].tombstone_reason, Some(TombstoneReason::Expire));

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let row_count: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
                .map_err(Into::into)
        })
        .await
        .expect("record count");
    assert_eq!(row_count, 1, "expire must not hard-delete records");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire expire_retires_target_without_hard_delete -- --nocapture`

Expected: FAIL with `no method named expire`.

- [ ] **Step 3: Add store expire module**

Create `crates/cairn-store-sqlite/src/store/expire.rs`:

```rust
//! Concrete expire API for target-level soft retirement.

use cairn_core::domain::TargetId;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Soft-expire every version in a target lineage through WAL.
    ///
    /// Expire removes the target from default read and search results by
    /// setting `active = 0`, `tombstoned = 1`, and
    /// `tombstone_reason = 'expire'`. It does not physically delete rows.
    ///
    /// # Errors
    /// Returns store, lock, WAL runner, or recovery-shape errors.
    #[instrument(skip(self), err, fields(verb = "expire", target_id = %target.as_str()))]
    pub async fn expire(&self, target: &TargetId) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return Err(StoreError::NotInitialized { method: "expire" });
        }
        crate::record_wal::apply_expire(self, target).await
    }
}
```

In `crates/cairn-store-sqlite/src/store/mod.rs`, add:

```rust
pub(crate) mod expire;
```

- [ ] **Step 4: Implement expire apply**

Create `crates/cairn-store-sqlite/src/record_wal/expire.rs`:

```rust
use std::sync::Arc;

use cairn_core::domain::{ScopeTuple, TargetId};
use cairn_core::wal::{OpState, WalKind, graph_for};

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{ExpirePayload, RecordWalPayload, save_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::wal::runner;

pub(crate) async fn apply_expire(
    store: &SqliteMemoryStore,
    target: &TargetId,
) -> Result<(), StoreError> {
    let conn = store.require_conn("expire")?.clone();
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "expire requires daemon incarnation".to_owned(),
    })?;
    let op_id = new_operation_id(WalKind::Expire);
    let scope = load_scope_for_target(&conn, target).await?;
    let locks = acquire_for_record(
        &conn,
        &scope,
        target,
        &incarnation,
        op_id.as_str(),
        "record_wal_expire",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let payload = ExpirePayload {
        target_id: target.clone(),
        reason: "expire".to_owned(),
    };
    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    let target_hash = target.as_str().to_owned();
    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(&tx, &op_for_issue, WalKind::Expire, &target_hash, "{}")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(&tx, &op_for_issue, &RecordWalPayload::Expire(payload_for_body))
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let body = Arc::new(RecordStepBody::new_expire(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::Expire), &op_id, 0, body).await?;

    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

async fn load_scope_for_target(
    conn: &Arc<tokio_rusqlite::Connection>,
    target: &TargetId,
) -> Result<ScopeTuple, StoreError> {
    let target_id = target.as_str().to_owned();
    conn.call(move |c| {
        let scope_json: Option<String> = c
            .query_row(
                "SELECT scope FROM records WHERE target_id = ?1 ORDER BY version DESC LIMIT 1",
                rusqlite::params![target_id],
                |row| row.get(0),
            )
            .optional()?;
        let scope = match scope_json {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(StoreError::Codec(e))))?,
            None => ScopeTuple::default(),
        };
        Ok::<_, tokio_rusqlite::Error>(scope)
    })
    .await
    .map_err(StoreError::from)
}
```

Add `use rusqlite::OptionalExtension;` at the top of `expire.rs`.

- [ ] **Step 5: Add expire step bodies**

In `crates/cairn-store-sqlite/src/record_wal/steps.rs`, add constructor:

```rust
    #[must_use]
    pub(crate) fn new_expire(payload: ExpirePayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Expire(payload),
            locks,
        }
    }
```

Add branches:

```rust
            (RecordStepPayload::Expire(payload), "snapshot.stage") => {
                stage_snapshot(tx, op_id, step, payload.target_id.as_str())
            }
            (RecordStepPayload::Expire(payload), "primary.mark_expired") => {
                mark_expired(tx, payload)
            }
            (RecordStepPayload::Expire(payload), "vector.drain") => {
                drain_vectors(tx, payload)
            }
            (RecordStepPayload::Expire(payload), "fts.drain") => {
                drain_fts(tx, payload)
            }
            (RecordStepPayload::Expire(payload), "edges.drain") => {
                drain_edges(tx, payload)
            }
```

Add helpers:

```rust
fn mark_expired(
    tx: &rusqlite::Transaction<'_>,
    payload: &ExpirePayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "UPDATE records \
            SET active = 0, tombstoned = 1, tombstone_reason = 'expire', updated_at = ?1 \
          WHERE target_id = ?2",
        rusqlite::params![crate::store::current_unix_ms(), payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_vectors(
    tx: &rusqlite::Transaction<'_>,
    payload: &ExpirePayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        rusqlite::params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        rusqlite::params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_fts(
    tx: &rusqlite::Transaction<'_>,
    payload: &ExpirePayload,
) -> Result<(), StepBodyError> {
    let rowids = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM records WHERE target_id = ?1")
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(rusqlite::params![payload.target_id.as_str()], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(StepBodyError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StepBodyError::Storage)?
    };
    for rowid in rowids {
        tx.execute("DELETE FROM records_fts WHERE rowid = ?1", rusqlite::params![rowid])
            .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}

fn drain_edges(
    tx: &rusqlite::Transaction<'_>,
    payload: &ExpirePayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM edges \
          WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
             OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
        rusqlite::params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}
```

- [ ] **Step 6: Run expire acceptance test**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire expire_retires_target_without_hard_delete -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Add and run post-expire upsert version test**

Append:

```rust
#[tokio::test]
async fn upsert_after_expire_creates_next_visible_version() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");
    store.expire(&r.target_id).await.expect("expire");

    let mut replacement = r.clone();
    replacement.body = "replacement after expire".to_owned();
    let out = store.upsert(&replacement).await.expect("replacement upsert");

    assert_eq!(out.version, 2);
    assert!(store.get(&out.record_id).await.expect("get").is_some());

    let versions = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(versions.len(), 2);
    assert!(versions[0].tombstoned);
    assert!(!versions[1].tombstoned);
    assert!(versions[1].active);
}
```

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire upsert_after_expire_creates_next_visible_version -- --nocapture`

Expected: PASS because Task 5 changed upsert planning to use `MAX(version)` when no active row exists.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/mod.rs \
  crates/cairn-store-sqlite/src/store/expire.rs \
  crates/cairn-store-sqlite/src/record_wal/expire.rs \
  crates/cairn-store-sqlite/src/record_wal/steps.rs \
  crates/cairn-store-sqlite/tests/cow_upsert_expire.rs
git commit -m "feat: apply expire through record wal"
```

---

### Task 8: Register Record WAL Boot Recovery

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/recovery.rs`
- Modify: `crates/cairn-store-sqlite/src/open.rs`
- Modify: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`

- [ ] **Step 1: Add failing recovery test for a prepared upsert**

Append to `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`:

```rust
#[tokio::test]
async fn prepared_upsert_recovers_from_persisted_payload() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let inc = store.incarnation().cloned().expect("incarnation");
    let record = sample();
    let payload =
        cairn_store_sqlite::record_wal::payload::UpsertPayload::new_for_test(record.clone());

    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-recover-upsert', 1, 'upsert', 'ISSUED', '{}', 'issuer', 'target', '{}', 0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 2 WHERE operation_id = 'op-recover-upsert'",
            [],
        )?;
        cairn_store_sqlite::record_wal::payload::save_upsert_payload_for_test(
            c,
            "op-recover-upsert",
            &payload,
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed prepared op");

    let cfg = cairn_store_sqlite::wal::RecoveryConfig {
        enabled: true,
        bodies: Box::new(cairn_store_sqlite::record_wal::RecordWalRegistry::new(inc)),
    };
    let report = cairn_store_sqlite::recover_pending(&conn, &cfg)
        .await
        .expect("recover");
    assert_eq!(report.resumed_committed.len(), 1);
    assert!(store.get(&record.id).await.expect("get").is_some());
}
```

- [ ] **Step 2: Run the recovery test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire prepared_upsert_recovers_from_persisted_payload -- --nocapture`

Expected: FAIL with missing `RecordWalRegistry`.

- [ ] **Step 3: Implement `RecordWalRegistry`**

Replace `crates/cairn-store-sqlite/src/record_wal/recovery.rs` with:

```rust
//! Record WAL recovery registry.

use std::sync::Arc;

use cairn_core::wal::{OperationId, WalKind};
use tokio_rusqlite::Connection;

use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::payload::{RecordWalPayload, load_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::wal::{RecoveryError, StepBody, StepBodyRegistry};

pub struct RecordWalRegistry {
    incarnation: Arc<str>,
}

impl RecordWalRegistry {
    #[must_use]
    pub fn new(incarnation: Arc<str>) -> Self {
        Self { incarnation }
    }
}

#[async_trait::async_trait]
impl StepBodyRegistry for RecordWalRegistry {
    async fn body_for(
        &self,
        conn: &Arc<Connection>,
        kind: WalKind,
        op_id: &OperationId,
    ) -> Result<Option<Arc<dyn StepBody>>, RecoveryError> {
        match kind {
            WalKind::Upsert | WalKind::Expire => {}
            WalKind::ForgetRecord => return Ok(None),
        }

        let op_for_load = op_id.clone();
        let payload = conn
            .call(move |c| {
                load_payload(c, &op_for_load)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await
            .map_err(RecoveryError::Storage)?;

        match payload {
            RecordWalPayload::Upsert(payload) => {
                let locks = acquire_for_record(
                    conn,
                    &payload.record.scope,
                    &payload.record.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_upsert",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_upsert(payload, locks))))
            }
            RecordWalPayload::Expire(payload) => {
                let scope = cairn_core::domain::ScopeTuple::default();
                let locks = acquire_for_record(
                    conn,
                    &scope,
                    &payload.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_expire",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_expire(payload, locks))))
            }
        }
    }
}
```

- [ ] **Step 4: Run recovery test**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire prepared_upsert_recovers_from_persisted_payload -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Wire boot recovery to record registry**

In `crates/cairn-store-sqlite/src/open.rs`, change `run_boot_recovery` signature:

```rust
async fn run_boot_recovery(
    conn: &Arc<AsyncConn>,
    incarnation: Arc<str>,
) -> Result<(), StoreError> {
    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(crate::record_wal::RecordWalRegistry::new(incarnation)),
    };
```

In both async open paths, replace:

```rust
    run_boot_recovery(&conn).await?;
    let incarnation = crate::locks::init_incarnation(&conn)
```

with:

```rust
    let incarnation = crate::locks::init_incarnation(&conn)
        .await
        .map_err(|e| StoreError::LockInit(Box::new(e)))?;
    run_boot_recovery(&conn, Arc::clone(&incarnation)).await?;
```

Keep the existing `build_store` call unchanged.

- [ ] **Step 6: Run open-path and WAL tests**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire prepared_upsert_recovers_from_persisted_payload -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test wal_recovery -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/recovery.rs \
  crates/cairn-store-sqlite/src/open.rs \
  crates/cairn-store-sqlite/tests/cow_upsert_expire.rs
git commit -m "feat: recover record wal operations on boot"
```

---

### Task 9: Pin Expired Search Exclusion and WAL Step Conformance

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/cow_upsert_expire.rs`
- Existing search modules should already filter `active = 1 AND tombstoned = 0`.

- [ ] **Step 1: Add keyword search exclusion test**

Append:

```rust
#[tokio::test]
async fn expired_record_is_excluded_from_keyword_search() {
    let store = open_in_memory().await.expect("open");
    let mut r = sample();
    r.body = "needle unique expired body".to_owned();
    store.upsert(&r).await.expect("upsert");
    store.expire(&r.target_id).await.expect("expire");

    let page = store
        .search_keyword(&KeywordSearchArgs {
            query: "needle".into(),
            filter: None,
            auth_scope: r.scope.clone(),
            visibility_allowlist: vec![r.visibility],
            limit: 10,
            cursor: None,
            with_explain: false,
        })
        .await
        .expect("keyword search");
    assert!(page.candidates.is_empty());
}
```

- [ ] **Step 2: Add expire WAL conformance test**

Append:

```rust
#[tokio::test]
async fn expire_commits_wal_operation_and_all_steps() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("upsert");
    store.expire(&r.target_id).await.expect("expire");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(|c| {
        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE kind = 'expire'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "COMMITTED");
        let done_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps ws \
              JOIN wal_ops wo ON wo.operation_id = ws.operation_id \
             WHERE wo.kind = 'expire' AND ws.state = 'DONE'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(done_count, 5);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("expire wal rows");
}
```

- [ ] **Step 3: Run search and expire WAL tests**

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire expired_record_is_excluded_from_keyword_search expire_commits_wal_operation_and_all_steps -- --nocapture`

Expected: PASS.

- [ ] **Step 4: Run broader read/search regression tests**

Run: `cargo test -p cairn-store-sqlite --test search_keyword --test search_keyword_e2e --test records_latest --test tombstone_reasons -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/tests/cow_upsert_expire.rs
git commit -m "test: pin expire search exclusion and wal conformance"
```

---

### Task 10: Final Verification and Cleanup

**Files:**
- All files touched by Tasks 1 through 9.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

Expected: no output and exit code 0.

- [ ] **Step 2: Run targeted tests**

Run:

```bash
cargo test -p cairn-store-sqlite --test cow_upsert_expire -- --nocapture
cargo test -p cairn-store-sqlite --test wal_payloads_migration -- --nocapture
cargo test -p cairn-store-sqlite --test wal_recovery -- --nocapture
cargo test -p cairn-store-sqlite --test locks_stale_fence -- --nocapture
cargo test -p cairn-store-sqlite --test upsert_idempotent --test upsert_embed --test versioning -- --nocapture
cargo test -p cairn-store-sqlite --test search_keyword --test search_keyword_e2e --test index_stats -- --nocapture
```

Expected: all tests PASS.

- [ ] **Step 3: Run package check**

Run: `cargo check -p cairn-store-sqlite --all-targets`

Expected: PASS.

- [ ] **Step 4: Run diff hygiene**

Run: `git diff --check`

Expected: no output and exit code 0.

- [ ] **Step 5: Inspect WAL state manually**

Run:

```bash
cargo test -p cairn-store-sqlite --test cow_upsert_expire upsert_commits_wal_operation_and_all_steps expire_commits_wal_operation_and_all_steps -- --nocapture
```

Expected: PASS, with one committed upsert operation containing 6 done steps and one committed expire operation containing 5 done steps.

- [ ] **Step 6: Final commit**

If formatting changed files after the prior task commits:

```bash
git add crates/cairn-store-sqlite
git commit -m "chore: format record wal apply changes"
```

If `git status --short` is clean, skip this commit.

---

## Spec Coverage

- Upsert creates and updates records with complete version metadata: Tasks 4 and 5 route public upsert through WAL and preserve the existing `UpsertOutcome`, `versions`, schema-version, and consent-model behavior.
- Expire marks records retired and removes them from default search/retrieve: Tasks 7 and 9 add target-level expire, mark all lineage rows inactive and tombstoned with reason `expire`, and pin `get`, `list`, and keyword search exclusion.
- Derived FTS/vector/edge updates are retry-safe: Task 6 implements DELETE plus INSERT style idempotent FTS/vector steps and a no-duplicate retry test; expire drains derived rows in Task 7.
- WAL conformance: Tasks 4, 7, 8, and 9 assert committed operation rows, expected done step counts, durable payloads, and recovery from a prepared operation.
- No hard delete outside forget flows: Task 7 asserts the record row remains present after expire.
