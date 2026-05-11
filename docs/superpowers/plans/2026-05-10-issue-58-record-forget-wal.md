# Issue 58 Record Forget WAL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `cairn forget --record <record_id>` as a WAL-backed, crash-safe two-phase delete that tombstones a target lineage, purges primary vault surfaces, scrubs body-bearing WAL retention, recovers after crash, and advertises only the record forget capability.

**Architecture:** Extend the existing `record_wal` subsystem with a third payload/body pair for `WalKind::ForgetRecord`; no new `MemoryStore` trait method is added. The public store helper resolves public `record_id` to internal `target_id`, persists a body-free `ForgetPayload`, acquires the existing record locks, runs `FORGET_RECORD_STEPS`, and finalizes `COMMITTED` only after Phase B completes. Boot recovery loads the durable forget payload through `RecordWalRegistry`, reacquires the same locks, resumes incomplete steps, and `cairn lint` surfaces exhausted Phase B work as a body-free `purge_pending` deferred check.

**Tech Stack:** Rust 2024, `tokio_rusqlite`, `rusqlite`, SQLite migrations, `cairn_core::wal` step graphs, existing record WAL runner/locks, existing consent journal helpers, generated CLI envelopes, integration tests under `crates/cairn-store-sqlite/tests` and `crates/cairn-cli/tests`.

---

## File Structure

- Create: `crates/cairn-store-sqlite/src/migrations/sql/0055_forget_record_payload_scrub.sql`
  - Widens `wal_payloads.kind` to include `forget_record` and `purged`.
  - Replaces the all-update ban with a trigger that permits only scrub-to-`purged` updates.
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
  - Registers migration 55 after existing migration 54.
- Modify: `crates/cairn-store-sqlite/src/verify.rs`
  - Replaces the expected `wal_payloads_immutable` trigger name with `wal_payloads_scrub_only`.
- Modify: `crates/cairn-store-sqlite/tests/wal_payloads_migration.rs`
  - Keeps normal immutability assertions and adds `forget_record` plus scrub-stub coverage.
- Modify: `crates/cairn-store-sqlite/src/record_wal/payload.rs`
  - Adds `ForgetPayload`, `PurgedPayload`, `RecordWalPayload::ForgetRecord`, and test helpers.
- Create: `crates/cairn-store-sqlite/src/record_wal/forget.rs`
  - Owns record-id resolution, body-free audit hash construction, WAL orchestration, and `ForgetOutcome`.
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
  - Adds `RecordStepPayload::ForgetRecord` and all seven `FORGET_RECORD_STEPS` bodies.
- Modify: `crates/cairn-store-sqlite/src/record_wal/recovery.rs`
  - Maps `WalKind::ForgetRecord` to `RecordStepBody::new_forget_record`.
- Modify: `crates/cairn-store-sqlite/src/record_wal/mod.rs`
  - Exports `forget` inside the crate and re-exports `apply_forget_record`.
- Create: `crates/cairn-store-sqlite/src/store/forget.rs`
  - Adds `SqliteMemoryStore::forget_record(&RecordId)` as the concrete public adapter API.
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
  - Registers the `forget` module.
- Modify: `crates/cairn-store-sqlite/src/lint.rs`
  - Adds purge-pending detection for exhausted or open forget Phase B operations.
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
  - Re-exports `lint_purge_pending` for integration tests and CLI-facing diagnostics.
- Create: `crates/cairn-store-sqlite/tests/forget_record.rs`
  - Store integration tests for tombstone, purge, WAL scrub, recovery, idempotency, sibling preservation, and leakage sentinels.
- Modify: `crates/cairn-cli/src/main.rs`
  - Routes `forget` through `resolve_vault_and_config`, matching wired verbs.
- Modify: `crates/cairn-cli/src/verbs/forget.rs`
  - Replaces the stub autonomous record branch with the real store call while preserving dry-run/human-review and session/scope rejection.
- Create: `crates/cairn-cli/tests/forget_record.rs`
  - CLI tests for JSON commit, search/retrieve invisibility, session/scope rejection, and status advertisement.
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`
  - Replaces the old record-mode capability-unavailable assertion with session/scope-only rejection.
- Modify: `crates/cairn-cli/tests/issue7_cli_e2e.rs`
  - Updates status capability expectations to include `cairn.mcp.v1.forget.record`.
- Modify: `crates/cairn-core/src/status/wiring.rs`
  - Flips `FORGET_RECORD_WIRED` to `true`.
- Modify: `crates/cairn-core/src/status/tests.rs`
  - Updates the record forget gate test and keeps session/scope pinned off.

---

### Task 1: Widen WAL Payload Storage For Forget And Scrub Stubs

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0055_forget_record_payload_scrub.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/verify.rs`
- Modify: `crates/cairn-store-sqlite/tests/wal_payloads_migration.rs`

- [ ] **Step 1: Write the failing migration test**

Append this test to `crates/cairn-store-sqlite/tests/wal_payloads_migration.rs`:

```rust
#[tokio::test]
async fn wal_payloads_accepts_forget_record_and_only_scrub_updates() {
    let store = open_in_memory().await.expect("open in-memory store");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));

    conn.call(|c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('forget-record-payload', 1, 'forget_record', 'ISSUED', '{}', \
                     'issuer', 'hash:00000000000000000000000000000000', 'user=hmn:tafeng', \
                     0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
             VALUES ('forget-record-payload', 'forget_record', \
                     '{\"type\":\"forget_record\",\"target_hash\":\"hash:00000000000000000000000000000000\"}', 1)",
            [],
        )?;

        let normal_update = c.execute(
            "UPDATE wal_payloads SET payload_json = '{}' \
             WHERE operation_id = 'forget-record-payload'",
            [],
        );
        assert!(
            normal_update.is_err(),
            "non-scrub payload updates must still be blocked"
        );

        c.execute(
            "UPDATE wal_payloads \
                SET kind = 'purged', \
                    payload_json = '{\"type\":\"purged\",\"target_hash\":\"hash:00000000000000000000000000000000\",\"purged_by\":\"forget-record-payload\",\"purged_at\":1}' \
              WHERE operation_id = 'forget-record-payload'",
            [],
        )?;
        let kind: String = c.query_row(
            "SELECT kind FROM wal_payloads WHERE operation_id = 'forget-record-payload'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(kind, "purged");

        let delete = c.execute(
            "DELETE FROM wal_payloads WHERE operation_id = 'forget-record-payload'",
            [],
        );
        assert!(delete.is_err(), "payload rows remain append-only");

        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("schema assertions");
}
```

- [ ] **Step 2: Run the migration test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test wal_payloads_migration wal_payloads_accepts_forget_record_and_only_scrub_updates -- --nocapture`

Expected: FAIL with a SQLite `CHECK constraint failed` or `wal_payloads rows are immutable` message because the existing schema accepts only `upsert` and `expire` and blocks every update.

- [ ] **Step 3: Add migration 0055**

Create `crates/cairn-store-sqlite/src/migrations/sql/0055_forget_record_payload_scrub.sql`:

```sql
-- Migration 0055: allow record-forget payloads and body-scrub stubs.
-- Issue #58: forget_record recovery needs a body-free payload, and Phase B
-- must scrub body-bearing upsert/expire WAL retention without deleting audit rows.

DROP TRIGGER IF EXISTS wal_payloads_kind_matches_wal;
DROP TRIGGER IF EXISTS wal_payloads_immutable;
DROP TRIGGER IF EXISTS wal_payloads_no_delete;

ALTER TABLE wal_payloads RENAME TO wal_payloads_old;

CREATE TABLE wal_payloads (
  operation_id TEXT NOT NULL PRIMARY KEY
                    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL CHECK (kind IN ('upsert', 'expire', 'forget_record', 'purged')),
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);

INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at)
SELECT operation_id, kind, payload_json, created_at
  FROM wal_payloads_old;

DROP TABLE wal_payloads_old;

CREATE TRIGGER wal_payloads_kind_matches_wal
  BEFORE INSERT ON wal_payloads
  FOR EACH ROW
  WHEN NEW.kind <> 'purged'
   AND EXISTS (
    SELECT 1
      FROM wal_ops
     WHERE operation_id = NEW.operation_id
       AND kind IS NOT NEW.kind
  )
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads.kind must match wal_ops.kind');
END;

CREATE TRIGGER wal_payloads_scrub_only
  BEFORE UPDATE ON wal_payloads
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.created_at IS NOT OLD.created_at
    OR NEW.kind <> 'purged'
    OR json_extract(NEW.payload_json, '$.type') <> 'purged'
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads updates are limited to purged scrub stubs');
END;

CREATE TRIGGER wal_payloads_no_delete
  BEFORE DELETE ON wal_payloads
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (55, '0055_forget_record_payload_scrub', '', strftime('%s','now') * 1000);
```

- [ ] **Step 4: Register migration 55**

In `crates/cairn-store-sqlite/src/migrations/mod.rs`, add this const after `M0054_RECORDS_COW_STAGING`:

```rust
// Issue #58 - forget_record payloads and WAL scrub stubs.
const M0055_FORGET_RECORD_PAYLOAD_SCRUB: &str =
    include_str!("sql/0055_forget_record_payload_scrub.sql");
```

Add this tuple after migration 54 in `MIGRATION_SOURCES`:

```rust
    (
        55,
        "0055_forget_record_payload_scrub",
        M0055_FORGET_RECORD_PAYLOAD_SCRUB,
    ),
```

Add this migration after `M::up(M0054_RECORDS_COW_STAGING),`:

```rust
        M::up(M0055_FORGET_RECORD_PAYLOAD_SCRUB),
```

- [ ] **Step 5: Update schema verification trigger expectations**

In `crates/cairn-store-sqlite/src/verify.rs`, replace this expected object:

```rust
("trigger", "wal_payloads_immutable"),
```

with:

```rust
("trigger", "wal_payloads_scrub_only"),
```

- [ ] **Step 6: Run migration tests**

Run: `cargo test -p cairn-store-sqlite --test wal_payloads_migration -- --nocapture`

Expected: PASS. The existing immutability test still proves ordinary updates fail, and the new test proves the single scrub path is accepted.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0055_forget_record_payload_scrub.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs \
        crates/cairn-store-sqlite/src/verify.rs \
        crates/cairn-store-sqlite/tests/wal_payloads_migration.rs
git commit -m "feat: allow forget record wal scrub payloads (#58)"
```

---

### Task 2: Add Body-Free Forget Payloads

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/payload.rs`
- Create: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Write the failing payload round-trip test**

Create `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
//! Issue #58: record-level forget through the record WAL.

#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, ScopeTuple};
use cairn_core::wal::WalKind;
use cairn_store_sqlite::{StoreError, open, open_in_memory};
use rusqlite::params;

fn sample_record() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn forget_payload_round_trips_body_free() {
    let store = open_in_memory().await.expect("open");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record = sample_record();
    let payload = cairn_store_sqlite::record_wal::payload::ForgetPayload::new_for_test(
        record.id.clone(),
        record.target_id.clone(),
        record.scope.clone(),
        vec![record.id.clone()],
        "hash:00000000000000000000000000000000".to_owned(),
    );

    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('op-forget-payload', 1, ?1, 'ISSUED', '{}', 'issuer', ?2, ?3, 0, 'sig', 1, 1)",
            params![
                WalKind::ForgetRecord.as_str(),
                payload.target_hash,
                payload.scope.canonical_wire(),
            ],
        )?;
        cairn_store_sqlite::record_wal::payload::save_forget_payload_for_test(
            c,
            "op-forget-payload",
            &payload,
        )
        .expect("save forget payload");
        let loaded = cairn_store_sqlite::record_wal::payload::load_forget_payload_for_test(
            c,
            "op-forget-payload",
        )
        .expect("load forget payload");
        assert_eq!(loaded.requested_record_id, payload.requested_record_id);
        assert_eq!(loaded.target_id, payload.target_id);
        assert_eq!(loaded.record_ids, payload.record_ids);
        let json = serde_json::to_string(&loaded).expect("payload json");
        assert!(!json.contains(&record.body), "forget payload must not contain body text");
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("payload round trip");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_payload_round_trips_body_free -- --nocapture`

Expected: FAIL to compile with `could not find ForgetPayload in payload`.

- [ ] **Step 3: Add payload structs and enum variant**

In `crates/cairn-store-sqlite/src/record_wal/payload.rs`, add `ForgetPayload` and `PurgedPayload` after `ExpirePayload`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForgetPayload {
    pub requested_record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub record_ids: Vec<RecordId>,
    pub target_hash: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PurgedPayload {
    pub target_hash: String,
    pub purged_by: String,
    pub purged_at: i64,
}
```

Add the enum variant:

```rust
pub enum RecordWalPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    ForgetRecord(Box<ForgetPayload>),
    Purged(Box<PurgedPayload>),
}
```

Update `save_payload` so only production payloads are inserted through it:

```rust
    let kind = match payload {
        RecordWalPayload::Upsert(_) => WalKind::Upsert.as_str(),
        RecordWalPayload::Expire(_) => WalKind::Expire.as_str(),
        RecordWalPayload::ForgetRecord(_) => WalKind::ForgetRecord.as_str(),
        RecordWalPayload::Purged(_) => {
            return Err(StoreError::Invariant {
                what: "purged wal payloads are written by scrub updates, not inserted".to_owned(),
            });
        }
    };
```

Update `load_upsert_payload_for_test` to reject the new variants:

```rust
        RecordWalPayload::Expire(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found expire payload".to_owned(),
        }),
        RecordWalPayload::ForgetRecord(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found forget_record payload".to_owned(),
        }),
        RecordWalPayload::Purged(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found purged payload".to_owned(),
        }),
```

- [ ] **Step 4: Add forget payload test helpers**

Append this helper block to `crates/cairn-store-sqlite/src/record_wal/payload.rs`:

```rust
#[cfg(any(test, feature = "test-helpers"))]
impl ForgetPayload {
    #[must_use]
    pub fn new_for_test(
        requested_record_id: RecordId,
        target_id: TargetId,
        scope: ScopeTuple,
        record_ids: Vec<RecordId>,
        target_hash: String,
    ) -> Self {
        Self {
            requested_record_id,
            target_id,
            scope,
            record_ids,
            target_hash,
            reason_code: "user_command".to_owned(),
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn save_forget_payload_for_test(
    conn: &Connection,
    op_id: &str,
    payload: &ForgetPayload,
) -> Result<(), StoreError> {
    let op = OperationId::parse(op_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid test op id: {e}"),
    })?;
    save_payload(
        conn,
        &op,
        &RecordWalPayload::ForgetRecord(Box::new(payload.clone())),
    )
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn load_forget_payload_for_test(
    conn: &Connection,
    op_id: &str,
) -> Result<ForgetPayload, StoreError> {
    let op = OperationId::parse(op_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid test op id: {e}"),
    })?;
    match load_payload(conn, &op)? {
        RecordWalPayload::ForgetRecord(payload) => Ok(*payload),
        RecordWalPayload::Upsert(_) => Err(StoreError::Invariant {
            what: "expected forget_record payload, found upsert payload".to_owned(),
        }),
        RecordWalPayload::Expire(_) => Err(StoreError::Invariant {
            what: "expected forget_record payload, found expire payload".to_owned(),
        }),
        RecordWalPayload::Purged(_) => Err(StoreError::Invariant {
            what: "expected forget_record payload, found purged payload".to_owned(),
        }),
    }
}
```

- [ ] **Step 5: Run the payload test**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_payload_round_trips_body_free -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/payload.rs \
        crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "feat: add forget record wal payload (#58)"
```

---

### Task 3: Resolve Public Record ID To Target Lineage

**Files:**
- Create: `crates/cairn-store-sqlite/src/record_wal/forget.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/mod.rs`
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Write the failing resolution tests**

Append these tests to `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
#[tokio::test]
async fn forget_resolution_loads_full_target_lineage() {
    let store = open_in_memory().await.expect("open");
    let first = sample_record();
    let mut second = first.clone();
    second.id = RecordId::parse("01J00000000000000000000002").expect("record id");
    second.body = "replacement body for same target".to_owned();
    second.body_hash = cairn_core::domain::BodyHash::compute(&second.body);

    store.upsert(&first).await.expect("first upsert");
    store.upsert(&second).await.expect("second upsert");

    let target = cairn_store_sqlite::record_wal::forget::resolve_forget_target_for_test(
        &store,
        &first.id,
    )
    .await
    .expect("resolve");

    assert_eq!(target.requested_record_id, first.id);
    assert_eq!(target.target_id, first.target_id);
    assert_eq!(target.record_ids.len(), 2);
    assert!(target.record_ids.contains(&first.id));
    assert!(target.record_ids.contains(&second.id));
    assert_eq!(target.scope, first.scope);
    assert!(target.target_hash.starts_with("hash:"));
}

#[tokio::test]
async fn forget_resolution_reports_not_found_for_missing_record() {
    let store = open_in_memory().await.expect("open");
    let missing = RecordId::parse("01J00000000000000000000999").expect("record id");
    let err = cairn_store_sqlite::record_wal::forget::resolve_forget_target_for_test(
        &store,
        &missing,
    )
    .await
    .expect_err("missing record rejects");
    assert!(matches!(err, StoreError::NotFound { id } if id == missing.as_str()));
}
```

- [ ] **Step 2: Run the resolution tests to verify they fail**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_resolution_ -- --nocapture`

Expected: FAIL to compile with `could not find forget in record_wal`.

- [ ] **Step 3: Create `record_wal::forget` with target resolution**

Create `crates/cairn-store-sqlite/src/record_wal/forget.rs`:

```rust
//! Public record-level forget apply through record WAL.

use std::sync::Arc;

use cairn_core::domain::{RecordId, ScopeTuple, TargetId};
use rusqlite::{OptionalExtension, params};

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetTarget {
    pub requested_record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub record_ids: Vec<RecordId>,
    pub target_hash: String,
}

async fn resolve_forget_target(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetTarget, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_record")?);
    let requested = record_id.clone();
    conn.call(move |c| {
        let row: Option<(String, String)> = c
            .query_row(
                "SELECT target_id, scope \
                   FROM records \
                  WHERE record_id = ?1 \
                  ORDER BY version DESC \
                  LIMIT 1",
                params![requested.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((target_raw, scope_json)) = row else {
            return Err(tokio_rusqlite::Error::Other(Box::new(StoreError::NotFound {
                id: requested.as_str().to_owned(),
            })));
        };
        let target_id = TargetId::parse(target_raw.clone())
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(StoreError::InvalidRecord(e))))?;
        let scope: ScopeTuple = serde_json::from_str(&scope_json)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;

        let mut stmt = c.prepare(
            "SELECT record_id \
               FROM records \
              WHERE target_id = ?1 \
              ORDER BY version ASC, record_id ASC",
        )?;
        let ids = stmt
            .query_map(params![target_id.as_str()], |row| row.get::<_, String>(0))?
            .map(|row| {
                row.and_then(|raw| {
                    RecordId::parse(raw).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let target_hash = hash_target_id(target_id.as_str());
        Ok::<_, tokio_rusqlite::Error>(ForgetTarget {
            requested_record_id: requested,
            target_id,
            scope,
            record_ids: ids,
            target_hash,
        })
    })
    .await
    .map_err(unpack_worker_err)
}

fn hash_target_id(target_id: &str) -> String {
    format!(
        "hash:{}",
        cairn_core::domain::projection::body_hash(&format!("forget_record:{target_id}"))
    )
}

fn unpack_worker_err(err: tokio_rusqlite::Error) -> StoreError {
    match err {
        tokio_rusqlite::Error::Other(boxed) => match boxed.downcast::<StoreError>() {
            Ok(inner) => *inner,
            Err(other) => StoreError::Worker(tokio_rusqlite::Error::Other(other)),
        },
        other => StoreError::from(other),
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub async fn resolve_forget_target_for_test(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetTarget, StoreError> {
    resolve_forget_target(store, record_id).await
}
```

- [ ] **Step 4: Export the module**

In `crates/cairn-store-sqlite/src/record_wal/mod.rs`, add:

```rust
pub mod forget;
```

- [ ] **Step 5: Run the resolution tests**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_resolution_ -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/forget.rs \
        crates/cairn-store-sqlite/src/record_wal/mod.rs \
        crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "feat: resolve forget record target lineage (#58)"
```

---

### Task 4: Run Forget Phase A And Phase B Through The Record WAL

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/forget.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/mod.rs`
- Create: `crates/cairn-store-sqlite/src/store/forget.rs`
- Modify: `crates/cairn-store-sqlite/src/store/mod.rs`
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Write failing end-to-end store tests**

Append these tests to `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
#[tokio::test]
async fn forget_record_purges_primary_indexes_and_vectors() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    let body = record.body.clone();
    store.upsert(&record).await.expect("upsert");

    let outcome = store
        .forget_record(&record.id)
        .await
        .expect("forget record");
    assert_eq!(outcome.deleted_count, 1);
    assert_eq!(outcome.tombstones, vec![record.id.clone()]);

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert!(listed.records.is_empty(), "forgotten record must be invisible");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let record_id = record.id.as_str().to_owned();
    let target_id = record.target_id.as_str().to_owned();
    conn.call(move |c| {
        let records: i64 = c.query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1",
            params![target_id],
            |row| row.get(0),
        )?;
        let vectors: i64 = c.query_row(
            "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?1",
            params![record_id],
            |row| row.get(0),
        )?;
        let pending: i64 = c.query_row(
            "SELECT COUNT(*) FROM pending_embeddings WHERE record_id = ?1",
            params![record_id],
            |row| row.get(0),
        )?;
        let fts_rows: i64 = c.query_row(
            "SELECT COUNT(*) FROM records_fts WHERE body MATCH ?1",
            params![body],
            |row| row.get(0),
        )?;
        assert_eq!(records, 0);
        assert_eq!(vectors, 0);
        assert_eq!(pending, 0);
        assert_eq!(fts_rows, 0);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("physical purge assertions");
}

#[tokio::test]
async fn forget_record_keeps_session_siblings_visible() {
    let store = open_in_memory().await.expect("open");
    let first = sample_record();
    let mut sibling = sample_record();
    sibling.id = RecordId::parse("01J00000000000000000000003").expect("record id");
    sibling.target_id = cairn_core::domain::TargetId::parse("sibling-target").expect("target");
    sibling.body = "sibling body remains".to_owned();
    sibling.body_hash = cairn_core::domain::BodyHash::compute(&sibling.body);
    sibling.scope = ScopeTuple {
        session_id: first.scope.session_id.clone(),
        user: first.scope.user.clone(),
        agent: first.scope.agent.clone(),
        ..ScopeTuple::default()
    };

    store.upsert(&first).await.expect("first upsert");
    store.upsert(&sibling).await.expect("sibling upsert");
    store.forget_record(&first.id).await.expect("forget first");

    let listed = store.list(&ListArgs::default()).await.expect("list");
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].id, sibling.id);
}
```

- [ ] **Step 2: Run the store tests to verify they fail**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_record_ -- --nocapture`

Expected: FAIL to compile with `no method named forget_record found for struct SqliteMemoryStore`.

- [ ] **Step 3: Add store-facing API**

Create `crates/cairn-store-sqlite/src/store/forget.rs`:

```rust
//! Store-facing record forget helper.

use cairn_core::domain::RecordId;

use crate::error::StoreError;
use crate::record_wal::forget::{ForgetOutcome, apply_forget_record};
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Forget one public record id by deleting the full target lineage.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the record id is not present, WAL setup fails,
    /// lock fencing fails, or a Phase B step exhausts retries.
    pub async fn forget_record(&self, record_id: &RecordId) -> Result<ForgetOutcome, StoreError> {
        apply_forget_record(self, record_id).await
    }
}
```

In `crates/cairn-store-sqlite/src/store/mod.rs`, add:

```rust
pub mod forget;
```

- [ ] **Step 4: Complete WAL orchestration in `record_wal::forget`**

Extend `crates/cairn-store-sqlite/src/record_wal/forget.rs` with:

```rust
use cairn_core::wal::{OpState, WalKind, graph_for};

use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{ForgetPayload, RecordWalPayload, save_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::wal::runner::{self, StepBody};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetOutcome {
    pub deleted_count: u64,
    pub tombstones: Vec<RecordId>,
    pub operation_id: cairn_core::wal::OperationId,
}

pub(crate) async fn apply_forget_record(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetOutcome, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_record")?);
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "forget_record requires daemon incarnation".to_owned(),
    })?;
    let target = resolve_forget_target(store, record_id).await?;
    let op_id = new_operation_id(WalKind::ForgetRecord)?;
    let locks = acquire_for_record(
        &conn,
        &target.scope,
        &target.target_id,
        &incarnation,
        op_id.as_str(),
        "record_wal_forget_record",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let payload = ForgetPayload {
        requested_record_id: target.requested_record_id.clone(),
        target_id: target.target_id.clone(),
        scope: target.scope.clone(),
        record_ids: target.record_ids.clone(),
        target_hash: target.target_hash.clone(),
        reason_code: "user_command".to_owned(),
    };

    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    let target_hash = target.target_hash.clone();
    let scope_wire = target.scope.canonical_wire();
    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(&tx, &op_for_issue, WalKind::ForgetRecord, &target_hash, &scope_wire)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(
            &tx,
            &op_for_issue,
            &RecordWalPayload::ForgetRecord(Box::new(payload_for_body)),
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .map_err(unpack_worker_err)?;

    let body: Arc<dyn StepBody> = Arc::new(RecordStepBody::new_forget_record(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::ForgetRecord), &op_id, 0, body).await?;

    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .map_err(unpack_worker_err)?;

    Ok(ForgetOutcome {
        deleted_count: u64::try_from(target.record_ids.len()).unwrap_or(u64::MAX),
        tombstones: target.record_ids,
        operation_id: op_id,
    })
}
```

In `crates/cairn-store-sqlite/src/record_wal/mod.rs`, add:

```rust
pub(crate) use forget::apply_forget_record;
```

- [ ] **Step 5: Add forget step payload dispatch**

In `crates/cairn-store-sqlite/src/record_wal/steps.rs`, update imports:

```rust
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use crate::record_wal::payload::{ExpirePayload, ForgetPayload, PurgedPayload, StoredEmbedOutcome, UpsertPayload};
```

Add the enum variant and constructor:

```rust
pub(crate) enum RecordStepPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    ForgetRecord(Box<ForgetPayload>),
}
```

```rust
    #[must_use]
    pub(crate) fn new_forget_record(payload: ForgetPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::ForgetRecord(Box::new(payload)),
            locks,
        }
    }
```

Add these match arms before the catch-all arm:

```rust
            (RecordStepPayload::ForgetRecord(payload), "primary.mark_tombstone") => {
                mark_forget_tombstone(tx, op_id, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "vector.drain") => {
                drain_forget_vectors(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "fts.drain") => {
                drain_forget_fts(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "edges.drain") => {
                drain_forget_edges(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "primary.purge") => {
                purge_forget_primary(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "wal.purge_pre_images") => {
                scrub_forget_wal(tx, op_id, payload)
            }
            (RecordStepPayload::ForgetRecord(_), "snapshot.purge") => Ok(()),
```

Update the catch-all to include `RecordStepPayload::ForgetRecord(_)`.

- [ ] **Step 6: Add step helper functions**

Append these helpers to `crates/cairn-store-sqlite/src/record_wal/steps.rs` before `now_secs()`:

```rust
fn mark_forget_tombstone(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "UPDATE records \
            SET active = 0, \
                tombstoned = 1, \
                tombstone_reason = 'forget', \
                updated_at = ?1 \
          WHERE target_id = ?2",
        params![crate::store::current_unix_ms(), payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;

    let decided_at = Rfc3339Timestamp::from_unix_secs(now_secs())
        .map_err(|e| StepBodyError::Failed(format!("forget consent timestamp: {e}")))?;
    let event = ConsentEvent {
        consent_id: ulid::Ulid::new().to_string(),
        kind: ConsentKind::ForgetIntent,
        actor: Identity::parse("hmn:cairn-cli")
            .map_err(|e| StepBodyError::Failed(format!("forget consent actor: {e}")))?,
        subject: payload.target_hash.clone(),
        scope: payload.scope.canonical_wire(),
        op_id: Some(op_id.as_str().to_owned()),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash: payload.target_hash.clone(),
            scope_tier: MemoryVisibility::Private,
            reason_code: payload.reason_code.clone(),
        },
        decided_at,
        expires_at: None,
    };
    crate::consent::append(tx, &event).map_err(|e| StepBodyError::Failed(e.to_string()))?;
    Ok(())
}

fn drain_forget_vectors(
    tx: &Transaction<'_>,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors WHERE record_id IN \
         (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings WHERE record_id IN \
         (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_forget_fts(
    tx: &Transaction<'_>,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    let rowids = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM records WHERE target_id = ?1")
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(params![payload.target_id.as_str()], |row| row.get::<_, i64>(0))
            .map_err(StepBodyError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StepBodyError::Storage)?
    };
    for rowid in rowids {
        tx.execute("DELETE FROM records_fts WHERE rowid = ?1", params![rowid])
            .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}

fn drain_forget_edges(
    tx: &Transaction<'_>,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM edges \
          WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
             OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM entity_episodes \
          WHERE episode_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn purge_forget_primary(
    tx: &Transaction<'_>,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM records WHERE target_id = ?1",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn scrub_forget_wal(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    let stub = serde_json::to_string(&crate::record_wal::payload::RecordWalPayload::Purged(
        Box::new(PurgedPayload {
            target_hash: payload.target_hash.clone(),
            purged_by: op_id.as_str().to_owned(),
            purged_at: crate::store::current_unix_ms(),
        }),
    ))
    .map_err(|e| StepBodyError::Failed(format!("purged payload json: {e}")))?;
    let stub_bytes = stub.as_bytes();
    let mut needles = Vec::with_capacity(payload.record_ids.len() + 1);
    needles.push(payload.target_id.as_str().to_owned());
    needles.extend(payload.record_ids.iter().map(|id| id.as_str().to_owned()));

    for needle in needles {
        let like = format!("%{needle}%");
        tx.execute(
            "UPDATE wal_steps \
                SET pre_image = ?1 \
              WHERE operation_id <> ?2 \
                AND pre_image IS NOT NULL \
                AND CAST(pre_image AS TEXT) LIKE ?3",
            params![stub_bytes, op_id.as_str(), like],
        )
        .map_err(StepBodyError::Storage)?;
        tx.execute(
            "UPDATE wal_payloads \
                SET kind = 'purged', payload_json = ?1 \
              WHERE operation_id <> ?2 \
                AND kind IN ('upsert', 'expire') \
                AND payload_json LIKE ?3",
            params![stub, op_id.as_str(), like],
        )
        .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}
```

- [ ] **Step 7: Run the end-to-end store tests**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_record_ -- --nocapture`

Expected: PASS for the payload, resolution, purge, and sibling tests.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/forget.rs \
        crates/cairn-store-sqlite/src/record_wal/steps.rs \
        crates/cairn-store-sqlite/src/record_wal/mod.rs \
        crates/cairn-store-sqlite/src/store/forget.rs \
        crates/cairn-store-sqlite/src/store/mod.rs \
        crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "feat: apply record forget through wal (#58)"
```

---

### Task 5: Wire Recovery And Purge-Pending Lint

**Files:**
- Modify: `crates/cairn-store-sqlite/src/record_wal/recovery.rs`
- Modify: `crates/cairn-store-sqlite/src/lint.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Modify: `crates/cairn-cli/src/verbs/lint.rs`
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`

- [ ] **Step 1: Write failing recovery and lint tests**

Append these tests to `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
#[tokio::test]
async fn prepared_forget_recovers_from_persisted_payload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");
    let op_id = "forget_record-01J00000000000000000001000";
    let record = sample_record();
    let target_hash = "hash:00000000000000000000000000000000".to_owned();

    {
        let store = open(&path).await.expect("open #1");
        store.upsert(&record).await.expect("seed record");
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let payload_json = serde_json::json!({
            "type": "forget_record",
            "requested_record_id": record.id.as_str(),
            "target_id": record.target_id.as_str(),
            "scope": record.scope.clone(),
            "record_ids": [record.id.as_str()],
            "target_hash": target_hash.clone(),
            "reason_code": "user_command"
        })
        .to_string();
        conn.call(move |c| {
            c.execute("DELETE FROM lock_holders", [])?;
            c.execute("DELETE FROM locks", [])?;
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?1, 100, 'forget_record', 'ISSUED', '{}', 'issuer', ?2, ?3, 0, 'sig', 1, 1)",
                params![op_id, target_hash, record.scope.canonical_wire()],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'PREPARED', updated_at = 2 \
                 WHERE operation_id = ?1",
                params![op_id],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES (?1, 'forget_record', ?2, 1)",
                params![op_id, payload_json],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed prepared forget");
    }

    let store = open(&path).await.expect("open #2 triggers recovery");
    assert!(store.list(&ListArgs::default()).await.expect("list").records.is_empty());
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op_id],
            |row| row.get(0),
        )?;
        let done: i64 = c.query_row(
            "SELECT COUNT(*) FROM wal_steps WHERE operation_id = ?1 AND state = 'DONE'",
            params![op_id],
            |row| row.get(0),
        )?;
        assert_eq!(state, "COMMITTED");
        assert_eq!(done, 7);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("recovery assertions");
}

#[tokio::test]
async fn prepared_forget_recovers_after_tombstone_linearization() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cairn.sqlite");
    let op_id = "forget_record-01J00000000000000000001002";
    let record = sample_record();
    let target_hash = "hash:22222222222222222222222222222222".to_owned();

    {
        let store = open(&path).await.expect("open #1");
        store.upsert(&record).await.expect("seed record");
        let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
        let payload_json = serde_json::json!({
            "type": "forget_record",
            "requested_record_id": record.id.as_str(),
            "target_id": record.target_id.as_str(),
            "scope": record.scope.clone(),
            "record_ids": [record.id.as_str()],
            "target_hash": target_hash.clone(),
            "reason_code": "user_command"
        })
        .to_string();
        conn.call(move |c| {
            c.execute("DELETE FROM lock_holders", [])?;
            c.execute("DELETE FROM locks", [])?;
            c.execute(
                "INSERT INTO wal_ops \
                   (operation_id, issued_seq, kind, state, envelope, issuer, \
                    target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
                 VALUES (?1, 101, 'forget_record', 'PREPARED', '{}', 'issuer', ?2, ?3, 0, 'sig', 1, 2)",
                params![op_id, target_hash, record.scope.canonical_wire()],
            )?;
            c.execute(
                "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
                 VALUES (?1, 'forget_record', ?2, 1)",
                params![op_id, payload_json],
            )?;
            c.execute(
                "UPDATE records \
                    SET active = 0, tombstoned = 1, tombstone_reason = 'forget' \
                  WHERE target_id = ?1",
                params![record.target_id.as_str()],
            )?;
            c.execute(
                "INSERT INTO wal_steps \
                   (operation_id, step_ord, step_kind, state, attempts, started_at, finished_at) \
                 VALUES (?1, 0, 'primary.mark_tombstone', 'DONE', 1, 1, 2)",
                params![op_id],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed linearized forget");
    }

    let store = open(&path).await.expect("open #2 triggers recovery");
    assert!(
        store
            .list(&ListArgs::default())
            .await
            .expect("list")
            .records
            .is_empty(),
        "linearized forget must stay invisible during recovery"
    );
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let state: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op_id],
            |row| row.get(0),
        )?;
        let remaining_records: i64 = c.query_row(
            "SELECT COUNT(*) FROM records",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "COMMITTED");
        assert_eq!(remaining_records, 0);
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("linearized recovery assertions");
}

#[tokio::test]
async fn purge_pending_lint_reports_exhausted_forget_phase_b_without_raw_ids() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");
    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let raw_record_id = record.id.as_str().to_owned();
    let raw_target_id = record.target_id.as_str().to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES ('forget_record-01J00000000000000000001001', 200, 'forget_record', \
                     'PREPARED', '{}', 'issuer', \
                     'hash:11111111111111111111111111111111', 'user=hmn:tafeng', 0, 'sig', 1, 1)",
            [],
        )?;
        c.execute(
            "INSERT INTO wal_steps \
               (operation_id, step_ord, step_kind, state, attempts, last_error, started_at, finished_at) \
             VALUES ('forget_record-01J00000000000000000001001', 5, 'wal.purge_pre_images', \
                     'FAILED', 3, 'boom', 1, 2)",
            [],
        )?;
        let findings = cairn_store_sqlite::lint_purge_pending(c)
            .expect("lint purge pending");
        assert_eq!(findings.len(), 1);
        let rendered = serde_json::to_string(&findings).expect("finding json");
        assert!(rendered.contains("purge_pending"));
        assert!(rendered.contains("hash:11111111111111111111111111111111"));
        assert!(!rendered.contains(&raw_record_id));
        assert!(!rendered.contains(&raw_target_id));
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("lint assertions");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p cairn-store-sqlite --test forget_record prepared_forget_recovers_from_persisted_payload prepared_forget_recovers_after_tombstone_linearization purge_pending_lint_reports_exhausted_forget_phase_b_without_raw_ids -- --nocapture`

Expected: FAIL because `RecordWalRegistry` returns no body for `forget_record` and `lint_purge_pending` does not exist.

- [ ] **Step 3: Wire forget recovery body**

In `crates/cairn-store-sqlite/src/record_wal/recovery.rs`, change the upfront match to:

```rust
        match kind {
            WalKind::Upsert | WalKind::Expire | WalKind::ForgetRecord => {}
        }
```

Add this match arm:

```rust
            (WalKind::ForgetRecord, RecordWalPayload::ForgetRecord(payload)) => {
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
                Ok(Some(Arc::new(RecordStepBody::new_forget_record(*payload, locks))))
            }
```

Add mismatch arms:

```rust
            (WalKind::ForgetRecord, RecordWalPayload::Upsert(_)) => Err(RecoveryError::Invariant(
                "record wal payload variant upsert does not match wal kind forget_record".to_owned(),
            )),
            (WalKind::ForgetRecord, RecordWalPayload::Expire(_)) => Err(RecoveryError::Invariant(
                "record wal payload variant expire does not match wal kind forget_record".to_owned(),
            )),
            (_, RecordWalPayload::Purged(_)) => Err(RecoveryError::Invariant(
                "purged record wal payload cannot be recovered as an active operation".to_owned(),
            )),
```

- [ ] **Step 4: Add purge-pending lint helper**

In `crates/cairn-store-sqlite/src/lint.rs`, add this public function near `lint_edges`:

```rust
/// Find record-forget operations whose Phase B purge work is still incomplete.
///
/// # Errors
/// Returns [`StoreError`] when WAL tables are missing or SQLite rejects the query.
pub fn lint_purge_pending(conn: &Connection) -> Result<Vec<Finding>, StoreError> {
    ensure_table(conn, WAL_OPS_TABLE)?;
    ensure_table(conn, WAL_STEPS_TABLE)?;

    let mut stmt = conn.prepare(
        "SELECT wo.operation_id, wo.target_hash, ws.step_ord, ws.step_kind, ws.state, ws.attempts \
           FROM wal_ops wo \
           JOIN wal_steps ws ON ws.operation_id = wo.operation_id \
          WHERE wo.kind = 'forget_record' \
            AND wo.state = 'PREPARED' \
            AND ws.step_ord >= 1 \
            AND ws.state <> 'DONE' \
          ORDER BY wo.issued_seq, ws.step_ord",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;

    let mut findings = Vec::new();
    for row in rows {
        let (operation_id, target_hash, step_ord, step_kind, state, attempts) = row?;
        findings.push(Finding {
            kind: Kind::DeferredCheck,
            severity: Severity::Warning,
            message: format!(
                "purge_pending: forget_record operation {operation_id} target {target_hash} stalled at step {step_ord} ({step_kind}) state={state} attempts={attempts}"
            ),
            entities: None,
            suggested_fix: Some("restart Cairn to resume WAL recovery; if it repeats, inspect the WAL step error".to_owned()),
            target: Some(cairn_core::generated::verbs::lint::Target {
                operation_id: ulid_from_operation_id(&operation_id),
                path: None,
                record_id: None,
            }),
            tracking_issue: Some(58),
        });
    }
    Ok(findings)
}

fn ulid_from_operation_id(raw: &str) -> Option<cairn_core::generated::common::Ulid> {
    raw.rsplit_once('-')
        .map(|(_, tail)| tail)
        .filter(|tail| tail.len() == 26)
        .map(|tail| cairn_core::generated::common::Ulid(tail.to_owned()))
}
```

Update `lint_edges` so purge-pending findings are included with existing edge findings:

```rust
    let mut findings = contradiction_findings(conn)?;
    let ambiguous_findings = ambiguous_findings(conn)?;
    let purge_findings = lint_purge_pending(conn)?;
    let contradictions = usize_to_u64(findings.len());
    let ambiguous_edges = usize_to_u64(ambiguous_findings.len());
    findings.extend(ambiguous_findings);
    findings.extend(purge_findings);
```

In `crates/cairn-store-sqlite/src/lib.rs`, update the re-export:

```rust
pub use lint::{EdgeLintReport, lint_edges, lint_purge_pending, resolve_edge_contradictions};
```

- [ ] **Step 5: Update CLI human summary to show purge-pending counts**

In `crates/cairn-cli/src/verbs/lint.rs`, update the human summary format string and arguments:

```rust
    println!(
        "summary: total={} contradictions={} ambiguous_edges={} purge_pending={} auto_resolved={}",
        data.summary.total,
        summary_count(data, "contradictory_edge"),
        summary_count(data, "ambiguous_edge"),
        data.findings
            .iter()
            .filter(|finding| {
                finding.kind == Kind::DeferredCheck
                    && finding.message.starts_with("purge_pending:")
            })
            .count(),
        data.summary.auto_resolved.unwrap_or(0),
    );
```

- [ ] **Step 6: Run recovery and lint tests**

Run: `cargo test -p cairn-store-sqlite --test forget_record prepared_forget_recovers_from_persisted_payload prepared_forget_recovers_after_tombstone_linearization purge_pending_lint_reports_exhausted_forget_phase_b_without_raw_ids -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Run lint-focused CLI/unit tests**

Run: `cargo test -p cairn-cli --test lint_crash_recovery -- --nocapture`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/recovery.rs \
        crates/cairn-store-sqlite/src/lint.rs \
        crates/cairn-store-sqlite/src/lib.rs \
        crates/cairn-cli/src/verbs/lint.rs \
        crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "feat: recover and lint pending record forgets (#58)"
```

---

### Task 6: Add WAL Scrub And Leakage Regression Coverage

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/forget_record.rs`
- Modify: `crates/cairn-store-sqlite/src/record_wal/steps.rs`

- [ ] **Step 1: Write failing WAL scrub tests**

Append these tests to `crates/cairn-store-sqlite/tests/forget_record.rs`:

```rust
#[tokio::test]
async fn forget_record_scrubs_body_bearing_wal_payloads_and_pre_images() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    let leaked_body = record.body.clone();
    let leaked_record_id = record.id.as_str().to_owned();
    store.upsert(&record).await.expect("upsert");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    let body_for_seed = leaked_body.clone();
    let record_for_seed = leaked_record_id.clone();
    conn.call(move |c| {
        c.execute(
            "UPDATE wal_steps \
                SET pre_image = ?1 \
              WHERE operation_id IN (SELECT operation_id FROM wal_ops WHERE kind = 'upsert') \
                AND step_ord = 0",
            params![format!("{{\"record_id\":\"{record_for_seed}\",\"body\":\"{body_for_seed}\"}}").as_bytes()],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed pre_image leak");

    store.forget_record(&record.id).await.expect("forget");

    let conn = Arc::clone(store.raw_conn_for_admin().expect("connected"));
    conn.call(move |c| {
        let payload_text: String = c.query_row(
            "SELECT group_concat(payload_json, '\n') FROM wal_payloads",
            [],
            |row| row.get::<_, Option<String>>(0),
        )?.unwrap_or_default();
        let pre_image_text: String = c.query_row(
            "SELECT group_concat(CAST(pre_image AS TEXT), '\n') FROM wal_steps",
            [],
            |row| row.get::<_, Option<String>>(0),
        )?.unwrap_or_default();
        let combined = format!("{payload_text}\n{pre_image_text}");
        assert!(!combined.contains(&leaked_body), "body text must be scrubbed");
        assert!(!combined.contains(&leaked_record_id), "raw record id must be scrubbed");
        assert!(combined.contains("\"type\":\"purged\""), "audit scrub stub remains");
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("scrub assertions");
}

#[tokio::test]
async fn forget_record_replay_is_idempotent_after_primary_purge() {
    let store = open_in_memory().await.expect("open");
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");
    let first = store.forget_record(&record.id).await.expect("forget");
    assert_eq!(first.deleted_count, 1);
    let second = store.forget_record(&record.id).await.expect_err("record no longer resolvable");
    assert!(matches!(second, StoreError::NotFound { .. }));
}
```

- [ ] **Step 2: Run scrub tests to verify they fail if scrub is incomplete**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_record_scrubs_body_bearing_wal_payloads_and_pre_images forget_record_replay_is_idempotent_after_primary_purge -- --nocapture`

Expected: PASS if Task 4 scrub code already covers both surfaces. If it fails, the failure identifies the remaining surface containing body text or raw ids.

- [ ] **Step 3: Tighten scrub code if the test fails**

If the failure shows body text or raw ids still present, update `scrub_forget_wal` in `crates/cairn-store-sqlite/src/record_wal/steps.rs` so it searches every payload needle from:

```rust
let mut needles = Vec::with_capacity(payload.record_ids.len() + 1);
needles.push(payload.target_id.as_str().to_owned());
needles.extend(payload.record_ids.iter().map(|id| id.as_str().to_owned()));
```

and updates both retention tables:

```rust
tx.execute(
    "UPDATE wal_steps \
        SET pre_image = ?1 \
      WHERE operation_id <> ?2 \
        AND pre_image IS NOT NULL \
        AND CAST(pre_image AS TEXT) LIKE ?3",
    params![stub_bytes, op_id.as_str(), like],
)
.map_err(StepBodyError::Storage)?;
tx.execute(
    "UPDATE wal_payloads \
        SET kind = 'purged', payload_json = ?1 \
      WHERE operation_id <> ?2 \
        AND kind IN ('upsert', 'expire') \
        AND payload_json LIKE ?3",
    params![stub, op_id.as_str(), like],
)
.map_err(StepBodyError::Storage)?;
```

- [ ] **Step 4: Run scrub tests again**

Run: `cargo test -p cairn-store-sqlite --test forget_record forget_record_scrubs_body_bearing_wal_payloads_and_pre_images forget_record_replay_is_idempotent_after_primary_purge -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/record_wal/steps.rs \
        crates/cairn-store-sqlite/tests/forget_record.rs
git commit -m "test: cover record forget wal scrubbing (#58)"
```

---

### Task 7: Wire CLI Record Forget And Status Advertisement

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`
- Modify: `crates/cairn-cli/src/verbs/forget.rs`
- Create: `crates/cairn-cli/tests/forget_record.rs`
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`
- Modify: `crates/cairn-cli/tests/issue7_cli_e2e.rs`
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-core/src/status/tests.rs`

- [ ] **Step 1: Write failing CLI record forget tests**

Create `crates/cairn-cli/tests/forget_record.rs`:

```rust
//! Issue #58: CLI record forget wiring.

#![allow(missing_docs)]

use serde_json::Value;
use std::{path::Path, process::Command};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn json_stdout(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout was not valid JSON: {e}\nstdout: {stdout:?}");
    })
}

#[test]
fn forget_record_json_commits_in_bound_vault() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let vault = tmp.path();
    bootstrap_vault(vault);

    let ingest = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "forget me through cli",
            "--json",
        ])
        .output()
        .expect("ingest");
    assert!(ingest.status.success(), "stderr: {}", String::from_utf8_lossy(&ingest.stderr));
    let ingest_json = json_stdout(&ingest);
    let record_id = ingest_json["data"]["record_id"]
        .as_str()
        .expect("record id")
        .to_owned();

    let forget = cli()
        .current_dir(vault)
        .args([
            "forget",
            "--record",
            &record_id,
            "--json",
        ])
        .output()
        .expect("forget");
    assert!(forget.status.success(), "stderr: {}", String::from_utf8_lossy(&forget.stderr));
    let forget_json = json_stdout(&forget);
    assert_eq!(forget_json["verb"], "forget");
    assert_eq!(forget_json["status"], "committed");
    assert_eq!(forget_json["data"]["deleted_count"], 1);
    assert_eq!(forget_json["data"]["tombstones"][0], record_id);

    let search = cli()
        .current_dir(vault)
        .args([
            "search",
            "forget me through cli",
            "--mode",
            "keyword",
            "--json",
        ])
        .output()
        .expect("search after forget");
    assert!(search.status.success(), "stderr: {}", String::from_utf8_lossy(&search.stderr));
    let search_json = json_stdout(&search);
    assert_eq!(
        search_json["data"]["hits"].as_array().expect("hits").len(),
        0
    );
}

#[test]
fn forget_session_and_scope_remain_capability_unavailable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(tmp.path());
    for args in [
        vec!["forget", "--session", "session-1", "--json"],
        vec!["forget", "--scope", r#"{"user":"hmn:tafeng"}"#, "--json"],
    ] {
        let out = cli()
            .current_dir(tmp.path())
            .args(args)
            .output()
            .expect("forget rejection");
        assert_eq!(out.status.code(), Some(69));
        let json: Value = serde_json::from_slice(&out.stdout).expect("json");
        assert_eq!(json["error"]["code"], "CapabilityUnavailable");
    }
}
```

- [ ] **Step 2: Run CLI tests to verify they fail**

Run: `cargo test -p cairn-cli --test forget_record -- --nocapture`

Expected: FAIL because record mode still returns `CapabilityUnavailable`.

- [ ] **Step 3: Route `forget` through vault/config resolution**

In `crates/cairn-cli/src/main.rs`, replace:

```rust
        Some(("forget", sub)) => verbs::forget::run(sub),
```

with:

```rust
        Some(("forget", sub)) => match resolve_vault_and_config(explicit_vault.as_deref()) {
            Ok((vault_root, _source, config)) => verbs::forget::run(sub, vault_root, config),
            Err(code) => code,
        },
```

- [ ] **Step 4: Implement CLI record branch**

Change `crates/cairn-cli/src/verbs/forget.rs` imports to include:

```rust
use std::path::PathBuf;

use cairn_core::config::CairnConfig;
use cairn_core::domain::RecordId;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus};
use cairn_core::generated::verbs::forget::ForgetData;
use cairn_store_sqlite::StoreError;
```

Change the signature:

```rust
pub fn run(sub: &ArgMatches, vault_root: PathBuf, config: CairnConfig) -> ExitCode {
```

After the dry-run/human-review block and before capability fallback, add:

```rust
    if let Some(record_id_raw) = sub.get_one::<String>("record_id") {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let resp = super::signed::aborted(ResponseVerb::Forget, format!("runtime build: {e}"));
                emit_forget_response(json, &resp);
                return response_exit_code(&resp);
            }
        };
        let resp = rt.block_on(run_record_forget(
            record_id_raw.clone(),
            vault_root,
            config,
        ));
        emit_forget_response(json, &resp);
        return response_exit_code(&resp);
    }
```

Append helper functions:

```rust
async fn run_record_forget(record_id_raw: String, vault_root: PathBuf, config: CairnConfig) -> Response {
    let ctx = match super::signed::open_context(ResponseVerb::Forget, &vault_root, config).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let record_id = match RecordId::parse(record_id_raw.clone()) {
        Ok(record_id) => record_id,
        Err(e) => return super::signed::rejected_from_domain(ResponseVerb::Forget, e),
    };
    match ctx.store.forget_record(&record_id).await {
        Ok(outcome) => super::signed::committed(
            ResponseVerb::Forget,
            super::envelope::new_operation_id(),
            ResponseData::Forget(ForgetData {
                deleted_count: outcome.deleted_count,
                plan_ref: None,
                tombstones: Some(
                    outcome
                        .tombstones
                        .into_iter()
                        .map(|id| Ulid(id.as_str().to_owned()))
                        .collect(),
                ),
            }),
            Vec::new(),
        ),
        Err(StoreError::NotFound { id }) => super::envelope::not_found_response(
            ResponseVerb::Forget,
            "record",
            &format!("record {id} was not found"),
        ),
        Err(e) => super::signed::aborted(ResponseVerb::Forget, e.to_string()),
    }
}

fn emit_forget_response(json: bool, resp: &Response) {
    if json {
        emit_json(resp);
    } else if resp.status == ResponseStatus::Committed {
        println!("cairn forget: committed (operation_id: {})", resp.operation_id.0);
    } else {
        let code = resp
            .error
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Internal");
        let message = resp
            .error
            .as_ref()
            .and_then(|e| e.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("forget failed");
        human_error("forget", code, message, &resp.operation_id);
    }
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        ResponseStatus::Committed => ExitCode::SUCCESS,
        ResponseStatus::Rejected => {
            if super::signed::response_error_code(resp) == Some("CapabilityUnavailable") {
                ExitCode::from(EX_UNAVAILABLE)
            } else {
                ExitCode::from(64)
            }
        }
        ResponseStatus::Aborted => ExitCode::from(78),
        _ => ExitCode::FAILURE,
    }
}
```

- [ ] **Step 5: Flip status wiring and update status tests**

In `crates/cairn-core/src/status/wiring.rs`, change:

```rust
pub const FORGET_RECORD_WIRED: bool = false;
```

to:

```rust
pub const FORGET_RECORD_WIRED: bool = true;
```

In `crates/cairn-core/src/status/tests.rs`, replace `forget_record_held_back_until_wiring_flips` with:

```rust
#[test]
fn forget_record_advertises_when_runtime_is_wired() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(
        caps.contains(&Capabilities::CairnMcpV1ForgetRecord),
        "forget.record must advertise once runtime dispatch is wired"
    );
}
```

Update `crates/cairn-cli/tests/issue7_cli_e2e.rs` by replacing the assertion that `caps_one` does not contain `cairn.mcp.v1.forget.record` with:

```rust
    assert!(
        caps_one.contains(&"cairn.mcp.v1.forget.record".to_owned()),
        "forget.record is wired by issue #58 and must be advertised: {caps_one:?}"
    );
```

In `crates/cairn-cli/tests/envelope_tests.rs`, remove `forget_record_returns_capability_unavailable` and keep coverage for session/scope `CapabilityUnavailable`.

- [ ] **Step 6: Run CLI and status tests**

Run: `cargo test -p cairn-core status::tests::forget_record_advertises_when_runtime_is_wired -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-cli --test forget_record -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-cli --test envelope_tests forget_ -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/main.rs \
        crates/cairn-cli/src/verbs/forget.rs \
        crates/cairn-cli/tests/forget_record.rs \
        crates/cairn-cli/tests/envelope_tests.rs \
        crates/cairn-cli/tests/issue7_cli_e2e.rs \
        crates/cairn-core/src/status/wiring.rs \
        crates/cairn-core/src/status/tests.rs
git commit -m "feat: wire cli record forget capability (#58)"
```

---

### Task 8: Final Verification

**Files:**
- No new files.
- Verify all files changed in Tasks 1 through 7.

- [ ] **Step 1: Run focused store tests**

Run: `cargo test -p cairn-store-sqlite --test forget_record -- --nocapture`

Expected: PASS.

- [ ] **Step 2: Run migration and WAL regression tests**

Run: `cargo test -p cairn-store-sqlite --test wal_payloads_migration -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test cow_upsert_expire -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-store-sqlite --test wal_recovery -- --nocapture`

Expected: PASS.

- [ ] **Step 3: Run CLI tests affected by record forget**

Run: `cargo test -p cairn-cli --test forget_record -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-cli --test envelope_tests -- --nocapture`

Expected: PASS.

Run: `cargo test -p cairn-cli --test issue7_cli_e2e -- --nocapture`

Expected: PASS.

- [ ] **Step 4: Run core status tests**

Run: `cargo test -p cairn-core status::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Run formatting and lint checks**

Run: `cargo fmt --all -- --check`

Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: PASS.

- [ ] **Step 6: Run full workspace tests**

Run: `cargo test --workspace --all-targets`

Expected: PASS.

- [ ] **Step 7: Inspect final diff**

Run: `git status --short`

Expected: only intentional uncommitted changes, or no output if every task commit was made.

Run: `git log --oneline origin/main..HEAD`

Expected: includes the design commit, the plan commit if committed, and the Task 1 through Task 7 commits.

- [ ] **Step 8: Commit verification-only fixes if needed**

If formatting or verification required mechanical fixes, commit them:

```bash
git add crates/cairn-store-sqlite crates/cairn-cli crates/cairn-core
git commit -m "chore: verify record forget wal flow (#58)"
```
