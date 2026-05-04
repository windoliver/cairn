# Issue 267 Consent Journal Repair Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deletion-only `cairn repair consent-journal` recovery path for legacy rows that block migration 0021.

**Architecture:** The CLI is the ground-truth maintenance surface and calls a focused `cairn_store_sqlite::repair::consent_journal` sync API over `rusqlite::Connection`. The store API classifies blockers, performs one controlled trigger bypass inside `BEGIN IMMEDIATE`, writes an append-only audit row, deletes only eligible rows, and inserts a `consent_mirror_resets` marker. The reusable store API is the SDK-consumable path for this maintenance command; a generated MCP repair tool is deferred until the IDL grows a repair namespace.

**Tech Stack:** Rust 2024, clap 4, rusqlite, rusqlite_migration, serde/serde_json, ULID, cargo nextest.

---

## File Structure

- Create `crates/cairn-store-sqlite/src/repair/mod.rs`: store repair module namespace.
- Create `crates/cairn-store-sqlite/src/repair/consent_journal.rs`: blocker classifier, audit schema bootstrap, deletion transaction, receipt types.
- Create `crates/cairn-store-sqlite/src/migrations/sql/0046_consent_journal_repair_audit.sql`: append-only audit table for healthy vaults.
- Modify `crates/cairn-store-sqlite/src/migrations/mod.rs`: register migration 0046.
- Modify `crates/cairn-store-sqlite/src/verify.rs`: add audit table and triggers to expected schema fingerprint.
- Modify `crates/cairn-store-sqlite/src/error.rs`: add a repair-not-eligible error.
- Modify `crates/cairn-store-sqlite/src/lib.rs`: export `repair`.
- Modify `crates/cairn-store-sqlite/Cargo.toml`: add `serde` runtime dependency for receipt serialization.
- Create `crates/cairn-store-sqlite/tests/consent_journal_repair.rs`: store integration tests.
- Create `crates/cairn-cli/src/repair.rs`: CLI rendering and dispatch for `repair consent-journal`.
- Modify `crates/cairn-cli/src/command.rs`: add the `repair` command tree.
- Modify `crates/cairn-cli/src/main.rs`: route `repair`, exclude it from the ordinary vault guard, and pass the top-level vault selector.
- Modify `crates/cairn-cli/src/lib.rs`: export `repair`.
- Modify `crates/cairn-cli/tests/cli.rs`: CLI parsing and behavior tests.

### Task 1: Audit Schema Migration

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0046_consent_journal_repair_audit.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/verify.rs`
- Test: `crates/cairn-store-sqlite/tests/migrations.rs`

- [ ] **Step 1: Write the failing migration-head test**

Change the two head assertions in `crates/cairn-store-sqlite/tests/migrations.rs`:

```rust
assert_eq!(head, 46);
```

Add this test near the other schema drift tests:

```rust
#[test]
fn consent_journal_repair_audit_is_append_only() {
    let conn = open_in_memory().expect("open");
    conn.execute(
        "INSERT INTO consent_journal_repair_audit \
          (repair_id, action, target_rowid, blocker_codes, operator, reason, row_snapshot, repaired_at) \
         VALUES ('repair-1', 'delete', 0, '[\"non_positive_rowid\"]', 'hmn:operator', \
                 'manual recovery', '{\"rowid\":0}', 0)",
        [],
    )
    .expect("insert audit row");

    let update = conn
        .execute(
            "UPDATE consent_journal_repair_audit SET reason = 'changed' WHERE repair_id = 'repair-1'",
            [],
        )
        .unwrap_err();
    assert!(format!("{update}").contains("append-only"));

    let delete = conn
        .execute(
            "DELETE FROM consent_journal_repair_audit WHERE repair_id = 'repair-1'",
            [],
        )
        .unwrap_err();
    assert!(format!("{delete}").contains("append-only"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p cairn-store-sqlite --test migrations consent_journal_repair_audit_is_append_only --locked
```

Expected: FAIL because `consent_journal_repair_audit` does not exist.

- [ ] **Step 3: Add migration and verification entries**

Create `0046_consent_journal_repair_audit.sql`:

```sql
-- Migration 0046: append-only audit table for consent_journal repair tool.
-- Brief §3 / §5.6 / §14. Issue #267.

CREATE TABLE IF NOT EXISTS consent_journal_repair_audit (
  repair_id        TEXT NOT NULL PRIMARY KEY,
  action           TEXT NOT NULL CHECK (action IN ('delete')),
  target_rowid     INTEGER NOT NULL,
  blocker_codes    TEXT NOT NULL CHECK (json_valid(blocker_codes) = 1),
  operator         TEXT NOT NULL,
  reason           TEXT NOT NULL,
  row_snapshot     TEXT NOT NULL CHECK (json_valid(row_snapshot) = 1),
  repaired_at      INTEGER NOT NULL
);

CREATE TRIGGER IF NOT EXISTS consent_journal_repair_audit_immutable
  BEFORE UPDATE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;

CREATE TRIGGER IF NOT EXISTS consent_journal_repair_audit_no_delete
  BEFORE DELETE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (46, '0046_consent_journal_repair_audit', '', strftime('%s','now') * 1000);
```

Register `M0046_CONSENT_JOURNAL_REPAIR_AUDIT` in `migrations/mod.rs`, append `(46, "0046_consent_journal_repair_audit", M0046_CONSENT_JOURNAL_REPAIR_AUDIT)` to `MIGRATION_SOURCES`, and append `M::up(M0046_CONSENT_JOURNAL_REPAIR_AUDIT)` to `migrations()`.

Add to `EXPECTED_OBJECTS` in `verify.rs`:

```rust
("table", "consent_journal_repair_audit"),
("trigger", "consent_journal_repair_audit_immutable"),
("trigger", "consent_journal_repair_audit_no_delete"),
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p cairn-store-sqlite --test migrations consent_journal_repair_audit_is_append_only --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0046_consent_journal_repair_audit.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs \
        crates/cairn-store-sqlite/src/verify.rs \
        crates/cairn-store-sqlite/tests/migrations.rs
git commit -m "feat(store): add consent journal repair audit schema (#267)"
```

### Task 2: Store Blocker Classifier

**Files:**
- Create: `crates/cairn-store-sqlite/src/repair/mod.rs`
- Create: `crates/cairn-store-sqlite/src/repair/consent_journal.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Modify: `crates/cairn-store-sqlite/Cargo.toml`
- Test: `crates/cairn-store-sqlite/tests/consent_journal_repair.rs`

- [ ] **Step 1: Write failing classifier tests**

Create `crates/cairn-store-sqlite/tests/consent_journal_repair.rs`:

```rust
use cairn_store_sqlite::migrations::migrations;
use cairn_store_sqlite::repair::consent_journal::{BlockerCode, list_blockers};

fn open_at_version(version: usize) -> rusqlite::Connection {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory");
    migrations()
        .to_version(&mut conn, version)
        .expect("apply migrations to version");
    conn
}

#[test]
fn list_blockers_finds_legacy_non_positive_rowid() {
    let conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'rowid-zero', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed blocker");

    let blockers = list_blockers(&conn).expect("list blockers");
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].rowid, 0);
    assert!(blockers[0].blocker_codes.contains(&BlockerCode::NonPositiveRowid));
}

#[test]
fn list_blockers_finds_unrenderable_legacy_decided_at() {
    let conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('bad-time', 'sub', 'private', 'GRANT', 'hmn:t', 253402300800000000)",
        [],
    )
    .expect("seed blocker");

    let blockers = list_blockers(&conn).expect("list blockers");
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0]
        .blocker_codes
        .contains(&BlockerCode::UnrenderableDecidedAt));
}

#[test]
fn list_blockers_finds_kind_null_event_field_drift() {
    let conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at, actor) \
         VALUES ('drift', 'sub', 'private', 'GRANT', 'hmn:t', 0, 'hmn:real')",
        [],
    )
    .expect("seed blocker");

    let blockers = list_blockers(&conn).expect("list blockers");
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0]
        .blocker_codes
        .contains(&BlockerCode::KindNullEventFieldDrift));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-store-sqlite --test consent_journal_repair --locked
```

Expected: FAIL because `repair::consent_journal` does not exist.

- [ ] **Step 3: Implement classifier types and query**

Add `serde = { workspace = true }` to `crates/cairn-store-sqlite/Cargo.toml`.

Create `repair/mod.rs`:

```rust
//! Operator-driven store repair helpers.

pub mod consent_journal;
```

Export it in `lib.rs`:

```rust
pub mod repair;
```

Implement `consent_journal.rs` with these public types:

```rust
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    NonPositiveRowid,
    UnrenderableDecidedAt,
    UnrenderableExpiresAt,
    KindNullEventFieldDrift,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsentJournalRepairRow {
    pub rowid: i64,
    pub consent_id: String,
    pub subject: String,
    pub scope: String,
    pub decision: String,
    pub granted_by: String,
    pub decided_at: i64,
    pub expires_at: Option<i64>,
    pub op_id: Option<String>,
    pub kind: Option<String>,
    pub sensor_id: Option<String>,
    pub actor: Option<String>,
    pub payload_json: Option<String>,
    pub decided_at_iso: Option<String>,
    pub expires_at_iso: Option<String>,
    pub blocker_codes: Vec<BlockerCode>,
}
```

Use SQL to select `kind IS NULL` rows and derive `blocker_codes` in Rust. The classifier must match migration 0021's preflights for the v1 repair cases:

```rust
pub fn list_blockers(conn: &Connection) -> Result<Vec<ConsentJournalRepairRow>, StoreError> {
    apply_repair_pragmas(conn)?;
    let mut stmt = conn.prepare(
        "SELECT rowid, consent_id, subject, scope, decision, granted_by, decided_at, expires_at, \
                op_id, kind, sensor_id, actor, payload_json, decided_at_iso, expires_at_iso, \
                strftime('%Y-%m-%dT%H:%M:%SZ', decided_at / 1000, 'unixepoch') IS NULL AS bad_decided, \
                (expires_at IS NOT NULL AND strftime('%Y-%m-%dT%H:%M:%SZ', expires_at / 1000, 'unixepoch') IS NULL) AS bad_expires \
         FROM consent_journal \
         WHERE kind IS NULL \
         ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let mut item = ConsentJournalRepairRow {
            rowid: row.get("rowid")?,
            consent_id: row.get("consent_id")?,
            subject: row.get("subject")?,
            scope: row.get("scope")?,
            decision: row.get("decision")?,
            granted_by: row.get("granted_by")?,
            decided_at: row.get("decided_at")?,
            expires_at: row.get("expires_at")?,
            op_id: row.get("op_id")?,
            kind: row.get("kind")?,
            sensor_id: row.get("sensor_id")?,
            actor: row.get("actor")?,
            payload_json: row.get("payload_json")?,
            decided_at_iso: row.get("decided_at_iso")?,
            expires_at_iso: row.get("expires_at_iso")?,
            blocker_codes: Vec::new(),
        };
        if item.rowid <= 0 {
            item.blocker_codes.push(BlockerCode::NonPositiveRowid);
        }
        let bad_decided: i64 = row.get("bad_decided")?;
        if bad_decided != 0 {
            item.blocker_codes.push(BlockerCode::UnrenderableDecidedAt);
        }
        let bad_expires: i64 = row.get("bad_expires")?;
        if bad_expires != 0 {
            item.blocker_codes.push(BlockerCode::UnrenderableExpiresAt);
        }
        if item.actor.is_some()
            || item.payload_json.is_some()
            || item.decided_at_iso.is_some()
            || item.expires_at_iso.is_some()
            || item.op_id.is_some()
            || item.sensor_id.is_some()
        {
            item.blocker_codes.push(BlockerCode::KindNullEventFieldDrift);
        }
        Ok(item)
    })?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if !row.blocker_codes.is_empty() {
            out.push(row);
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p cairn-store-sqlite --test consent_journal_repair --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/Cargo.toml \
        crates/cairn-store-sqlite/src/lib.rs \
        crates/cairn-store-sqlite/src/repair \
        crates/cairn-store-sqlite/tests/consent_journal_repair.rs
git commit -m "feat(store): classify consent journal repair blockers (#267)"
```

### Task 3: Store Delete Repair Path

**Files:**
- Modify: `crates/cairn-store-sqlite/src/repair/consent_journal.rs`
- Modify: `crates/cairn-store-sqlite/src/error.rs`
- Test: `crates/cairn-store-sqlite/tests/consent_journal_repair.rs`

- [ ] **Step 1: Write failing delete tests**

Append tests to `consent_journal_repair.rs`:

```rust
use cairn_store_sqlite::repair::consent_journal::delete_blocker;

#[test]
fn delete_blocker_removes_row_audits_and_resets_mirror() {
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'rowid-zero', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed blocker");

    let receipt = delete_blocker(&mut conn, 0, "operator chose to drop corrupt legacy row", "hmn:operator")
        .expect("delete blocker");
    assert_eq!(receipt.target_rowid, 0);

    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE consent_id = 'rowid-zero'",
            [],
            |r| r.get(0),
        )
        .expect("count row");
    assert_eq!(remaining, 0);

    let audit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM consent_journal_repair_audit", [], |r| r.get(0))
        .expect("count audit");
    assert_eq!(audit_count, 1);

    let reset_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM consent_mirror_resets WHERE migration_id = 21", [], |r| r.get(0))
        .expect("count reset");
    assert_eq!(reset_count, 1);

    migrations()
        .to_version(&mut conn, 46)
        .expect("repaired DB migrates through 0046");
}

#[test]
fn delete_blocker_refuses_non_blocker() {
    let mut conn = open_at_version(20);
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('ok', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed non-blocker");
    let rowid = conn.last_insert_rowid();

    let err = delete_blocker(&mut conn, rowid, "should fail", "hmn:operator")
        .expect_err("non-blocker must be refused");
    assert!(format!("{err}").contains("not repair-eligible"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-store-sqlite --test consent_journal_repair delete_blocker --locked
```

Expected: FAIL because `delete_blocker` does not exist.

- [ ] **Step 3: Implement `delete_blocker`**

Add `StoreError::RepairNotEligible { rowid: i64 }`:

```rust
#[error("consent_journal rowid {rowid} is not repair-eligible")]
RepairNotEligible { rowid: i64 },
```

Add receipt type:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ConsentJournalRepairReceipt {
    pub repair_id: String,
    pub target_rowid: i64,
    pub blocker_codes: Vec<BlockerCode>,
    pub operator: String,
    pub reason: String,
    pub repaired_at: i64,
}
```

Implement:

```rust
pub fn delete_blocker(
    conn: &mut Connection,
    rowid: i64,
    reason: &str,
    operator: &str,
) -> Result<ConsentJournalRepairReceipt, StoreError> {
    apply_repair_pragmas(conn)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    ensure_audit_schema(&tx)?;
    ensure_mirror_resets_schema(&tx)?;
    let blocker = find_blocker_by_rowid(&tx, rowid)?.ok_or(StoreError::RepairNotEligible { rowid })?;
    let repair_id = ulid::Ulid::new().to_string();
    let repaired_at = now_millis();
    let blocker_codes_json = serde_json::to_string(&blocker.blocker_codes)?;
    let row_snapshot = serde_json::to_string(&serde_json::json!({
        "rowid": blocker.rowid,
        "consent_id": blocker.consent_id,
        "subject": blocker.subject,
        "scope": blocker.scope,
        "decision": blocker.decision,
        "granted_by": blocker.granted_by,
        "decided_at": blocker.decided_at,
        "expires_at": blocker.expires_at,
        "op_id": blocker.op_id,
        "kind": blocker.kind,
        "sensor_id": blocker.sensor_id,
        "actor": blocker.actor,
        "payload_json": blocker.payload_json,
        "decided_at_iso": blocker.decided_at_iso,
        "expires_at_iso": blocker.expires_at_iso,
        "blocker_codes": blocker.blocker_codes,
    }))?;
    tx.execute(
        "INSERT INTO consent_journal_repair_audit \
          (repair_id, action, target_rowid, blocker_codes, operator, reason, row_snapshot, repaired_at) \
         VALUES (?1, 'delete', ?2, ?3, ?4, ?5, ?6, ?7)",
        params![repair_id, rowid, blocker_codes_json, operator, reason, row_snapshot, repaired_at],
    )?;
    tx.execute_batch(
        "DROP TRIGGER IF EXISTS consent_journal_immutable;
         DROP TRIGGER IF EXISTS consent_journal_no_delete;",
    )?;
    tx.execute("DELETE FROM consent_journal WHERE rowid = ?1", params![rowid])?;
    tx.execute_batch(CONSENT_JOURNAL_APPEND_ONLY_TRIGGERS)?;
    tx.execute(
        "INSERT OR REPLACE INTO consent_mirror_resets (migration_id, applied_at, consumed, db_nonce) \
         VALUES (21, ?1, 0, lower(hex(randomblob(16))))",
        params![repaired_at],
    )?;
    tx.commit()?;
    Ok(ConsentJournalRepairReceipt {
        repair_id,
        target_rowid: rowid,
        blocker_codes: blocker.blocker_codes,
        operator: operator.to_owned(),
        reason: reason.to_owned(),
        repaired_at,
    })
}
```

Use `CREATE TABLE IF NOT EXISTS consent_mirror_resets (...)` in `ensure_mirror_resets_schema`, because v20 databases do not have the table yet.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p cairn-store-sqlite --test consent_journal_repair --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/error.rs \
        crates/cairn-store-sqlite/src/repair/consent_journal.rs \
        crates/cairn-store-sqlite/tests/consent_journal_repair.rs
git commit -m "feat(store): delete eligible consent journal blockers (#267)"
```

### Task 4: CLI Repair Command

**Files:**
- Create: `crates/cairn-cli/src/repair.rs`
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Modify: `crates/cairn-cli/src/lib.rs`
- Test: `crates/cairn-cli/tests/cli.rs`

- [ ] **Step 1: Write failing CLI tests**

Add tests to `crates/cairn-cli/tests/cli.rs`:

```rust
#[test]
fn repair_consent_journal_help_exits_zero() {
    let out = cli()
        .args(["repair", "consent-journal", "--help"])
        .output()
        .expect("cairn repair consent-journal --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("--delete-rowid"));
    assert!(stdout.contains("--reason"));
    assert!(stdout.contains("--yes"));
}

#[test]
fn repair_consent_journal_delete_requires_reason_and_yes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = cli()
        .args([
            "--vault",
            dir.path().to_str().expect("path"),
            "repair",
            "consent-journal",
            "--delete-rowid",
            "0",
        ])
        .output()
        .expect("cairn repair consent-journal");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("required") || stderr.contains("--reason"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p cairn-cli --test cli repair_consent_journal --locked
```

Expected: FAIL because `repair` command does not exist.

- [ ] **Step 3: Add command tree and route**

Add `pub mod repair;` to `lib.rs`.

Add to `command.rs`:

```rust
.subcommand(repair_subcommand())
```

and:

```rust
fn repair_subcommand() -> clap::Command {
    clap::Command::new("repair")
        .about("Operator-driven vault repair commands")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("consent-journal")
                .about("List or delete legacy consent_journal rows blocking migration 0021")
                .arg(clap::Arg::new("delete-rowid").long("delete-rowid").value_name("ROWID").value_parser(clap::value_parser!(i64)))
                .arg(clap::Arg::new("reason").long("reason").value_name("TEXT").requires("delete-rowid"))
                .arg(clap::Arg::new("yes").long("yes").action(clap::ArgAction::SetTrue).requires("delete-rowid"))
                .arg(clap::Arg::new("json").long("json").action(clap::ArgAction::SetTrue))
        )
}
```

In `main.rs`, exclude `"repair"` from `needs_vault_guard`, add:

```rust
Some(("repair", sub)) => cairn_cli::repair::run(sub, explicit_vault.clone()),
```

Create `repair.rs` that resolves the vault path from top-level `--vault` / `CAIRN_VAULT` / current directory, opens `.cairn/cairn.db` with `rusqlite::Connection::open`, calls `list_blockers` or `delete_blocker`, prints JSON or concise human output, and maps errors to exit codes `65`, `74`, `78`.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p cairn-cli --test cli repair_consent_journal --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/repair.rs \
        crates/cairn-cli/src/command.rs \
        crates/cairn-cli/src/main.rs \
        crates/cairn-cli/src/lib.rs \
        crates/cairn-cli/tests/cli.rs
git commit -m "feat(cli): add consent journal repair command (#267)"
```

### Task 5: CLI End-to-End Repair Behavior

**Files:**
- Modify: `crates/cairn-cli/tests/cli.rs`
- Modify: `crates/cairn-cli/src/repair.rs`

- [ ] **Step 1: Write failing E2E CLI test**

Add a helper in `cli.rs` to create a v20 vault DB:

```rust
fn seed_v20_blocked_vault(vault: &std::path::Path) {
    std::fs::create_dir_all(vault.join(".cairn")).expect("mkdir .cairn");
    let db = vault.join(".cairn").join("cairn.db");
    let mut conn = rusqlite::Connection::open(db).expect("open db");
    cairn_store_sqlite::migrations::migrations()
        .to_version(&mut conn, 20)
        .expect("migrate to v20");
    conn.execute(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'rowid-zero', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("seed blocker");
}
```

Add:

```rust
#[test]
fn repair_consent_journal_json_lists_blockers() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_v20_blocked_vault(dir.path());

    let out = cli()
        .args([
            "--vault",
            dir.path().to_str().expect("path"),
            "repair",
            "consent-journal",
            "--json",
        ])
        .output()
        .expect("repair list");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(json["blockers"][0]["rowid"], 0);
    assert_eq!(json["blockers"][0]["blocker_codes"][0], "non_positive_rowid");
}

#[test]
fn repair_consent_journal_delete_then_migrates() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_v20_blocked_vault(dir.path());

    let out = cli()
        .args([
            "--vault",
            dir.path().to_str().expect("path"),
            "repair",
            "consent-journal",
            "--delete-rowid",
            "0",
            "--reason",
            "drop corrupt legacy row",
            "--yes",
            "--json",
        ])
        .output()
        .expect("repair delete");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let db = dir.path().join(".cairn").join("cairn.db");
    let mut conn = rusqlite::Connection::open(db).expect("open db");
    cairn_store_sqlite::migrations::migrations()
        .to_version(&mut conn, 46)
        .expect("migrate after repair");
}
```

- [ ] **Step 2: Run tests to verify they fail if CLI behavior is incomplete**

Run:

```bash
cargo test -p cairn-cli --test cli repair_consent_journal_json_lists_blockers repair_consent_journal_delete_then_migrates --locked
```

Expected: FAIL until `repair.rs` fully resolves vault DB paths and renders JSON.

- [ ] **Step 3: Complete CLI implementation**

Ensure `repair.rs` uses:

```rust
let db_path = vault_path.join(".cairn").join("cairn.db");
```

JSON list shape:

```json
{ "blockers": [ ... ] }
```

JSON delete shape:

```json
{ "deleted": { ...receipt... } }
```

Human list output should include rowid and blocker codes. Human delete output should include repair id and target rowid.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p cairn-cli --test cli repair_consent_journal --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/repair.rs crates/cairn-cli/tests/cli.rs
git commit -m "test(cli): cover consent journal repair flow (#267)"
```

### Task 6: Full Verification

**Files:**
- All changed files.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all --check
```

Expected: PASS.

- [ ] **Step 2: Store tests**

Run:

```bash
cargo test -p cairn-store-sqlite --test migrations --locked
cargo test -p cairn-store-sqlite --test consent_journal_repair --locked
```

Expected: PASS.

- [ ] **Step 3: CLI tests**

Run:

```bash
cargo test -p cairn-cli --test cli repair_consent_journal --locked
```

Expected: PASS.

- [ ] **Step 4: Boundary and workspace verification**

Run:

```bash
./scripts/check-core-boundary.sh
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --no-fail-fast --locked
```

Expected: PASS.

- [ ] **Step 5: Commit any final fixes**

If formatting or clippy required changes:

```bash
git add -A
git commit -m "fix: polish consent journal repair implementation (#267)"
```

## Self-Review

Spec coverage:

- Enumeration of rowid/timestamp/drift blockers: Task 2.
- Deletion-only remediation: Task 3 and Task 4.
- Controlled trigger bypass under `BEGIN IMMEDIATE`: Task 3.
- Audit logging: Task 1 and Task 3.
- Mirror reset marker: Task 3.
- CLI ground truth: Task 4 and Task 5.
- SDK-consumable path: Task 2 and Task 3 expose the public store API.
- Generated MCP wrapper: deferred until the IDL grows a repair namespace, to avoid hand-maintaining an MCP-only command that diverges from generated surfaces.
- Tests and verification: Task 1 through Task 6.

Placeholder scan: no `TBD`, `TODO`, "similar to", incomplete code placeholders, or vague "add error handling" steps remain.

Type consistency: public names are consistently `BlockerCode`, `ConsentJournalRepairRow`, `ConsentJournalRepairReceipt`, `list_blockers`, and `delete_blocker`.
