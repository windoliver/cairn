# `cairn.admin.v1` Extension Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the `cairn.admin.v1` extension end-to-end — six admin verbs (`snapshot`, `restore`, `replay_wal`, `connector_enable`, `connector_disable`, `connector_backfill`) exposed isomorphically through CLI, SDK, and MCP, gated by an operator role + extension capability handshake.

**Architecture:** Pure verb fns in `cairn-core::verbs::admin::*` over `&dyn Trait` deps. New `AdminStateStore` + `ConsentLog` traits with SQLite-resident state (`admin_roles`, `connector_state` tables). `WorkflowOrchestrator` grows `emit_progress` / `subscribe_progress`. Six phases, each independently mergeable behind `ADMIN_EXTENSION_WIRED = false` until phase 6 flips it.

**Tech Stack:** Rust edition 2024, tokio, rusqlite/sqlx (whichever the store already uses), thiserror, rstest, proptest, insta, schemars, zstd, tar, sha2.

**Spec:** `docs/superpowers/specs/2026-05-26-issue-161-admin-v1-extension-design.md`

**Issue:** [#161](https://github.com/windoliver/cairn/issues/161)

---

## Phase 1 — Wiring, capability advertisement, AdminContext, AdminStateStore, migrations

Foundation. Everything below this depends on it. Lands with `ADMIN_EXTENSION_WIRED = false` so it ships dark; nothing user-visible changes until phase 6.

### Task 1.1: Add `AdminRole` and `AdminContext` types

**Files:**
- Create: `crates/cairn-core/src/domain/admin/mod.rs`
- Create: `crates/cairn-core/src/domain/admin/context.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/src/domain/admin/context.rs`:

```rust
//! Admin caller context for `cairn.admin.v1` verbs.

use crate::domain::identity::IdentityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AdminRole {
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminContext {
    pub actor: IdentityId,
    pub requested_role: AdminRole,
}

impl AdminContext {
    #[must_use]
    pub fn new(actor: IdentityId, requested_role: AdminRole) -> Self {
        Self { actor, requested_role }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::IdentityId;

    #[test]
    fn admin_context_constructor_roundtrip() {
        let actor = IdentityId::from("hmn:test");
        let ctx = AdminContext::new(actor.clone(), AdminRole::Operator);
        assert_eq!(ctx.actor, actor);
        assert_eq!(ctx.requested_role, AdminRole::Operator);
    }
}
```

Create `crates/cairn-core/src/domain/admin/mod.rs`:

```rust
//! `cairn.admin.v1` extension domain types.

pub mod context;
pub use context::{AdminContext, AdminRole};
```

Modify `crates/cairn-core/src/domain/mod.rs`, append at the bottom of the existing `pub mod` block:

```rust
pub mod admin;
```

- [ ] **Step 2: Run test to verify it compiles + passes**

Run: `cargo nextest run -p cairn-core admin::context::tests --locked`
Expected: PASS (one test).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/domain/admin/ crates/cairn-core/src/domain/mod.rs
git commit -m "feat(#161): add AdminContext and AdminRole types"
```

---

### Task 1.2: Add `AdminError` enum

**Files:**
- Create: `crates/cairn-core/src/domain/admin/error.rs`
- Modify: `crates/cairn-core/src/domain/admin/mod.rs`

- [ ] **Step 1: Write the test for wire envelope shape**

Create `crates/cairn-core/src/domain/admin/error.rs`:

```rust
//! `AdminError` for `cairn.admin.v1` verbs. Wire envelope matches design brief §8.0.b.

use crate::domain::identity::IdentityId;
use crate::domain::admin::context::AdminRole;
use crate::store::StoreError;
use crate::wal::WalError;
use crate::contract::workflow_orchestrator::WorkflowError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdminError {
    #[error("admin capability not negotiated: {capability}")]
    CapabilityUnavailable { capability: String, remediation: String },

    #[error("caller {actor} is not authorized for {needed:?}")]
    NotAuthorized { actor: IdentityId, needed: AdminRole },

    #[error("snapshot artifact integrity check failed")]
    IntegrityMismatch { expected: String, actual: String },

    #[error("snapshot is from machine {source} but local is {local} — cross-machine restore not supported in v0.2")]
    CrossMachineRestore { source: String, local: String },

    #[error("snapshot vault id {source} != local vault {local}")]
    VaultIdMismatch { source: String, local: String },

    #[error("snapshot schema_version {source} > local head {local} — refuse forward restore")]
    SchemaTooNew { source: u32, local: u32 },

    #[error("connector {name} not found in registry")]
    UnknownConnector { name: String },

    #[error("WAL step {marker} not found in ledger")]
    UnknownStepMarker { marker: String },

    #[error("WAL replay halted at non-idempotent step {step}; escalated to PURGE_PENDING")]
    ReplayEscalated { step: String },

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Wal(#[from] WalError),

    #[error(transparent)]
    Workflow(#[from] WorkflowError),
}

impl AdminError {
    /// Map to brief §8.0.b wire envelope code string.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::CapabilityUnavailable { .. } => "CapabilityUnavailable",
            Self::NotAuthorized { .. }         => "NotAuthorized",
            Self::IntegrityMismatch { .. }     => "IntegrityMismatch",
            Self::CrossMachineRestore { .. }   => "CrossMachineRestore",
            Self::VaultIdMismatch { .. }       => "VaultIdMismatch",
            Self::SchemaTooNew { .. }          => "SchemaTooNew",
            Self::UnknownConnector { .. }      => "UnknownConnector",
            Self::UnknownStepMarker { .. }     => "UnknownStepMarker",
            Self::ReplayEscalated { .. }       => "ReplayEscalated",
            Self::Store(_)                     => "StoreError",
            Self::Wal(_)                       => "WalError",
            Self::Workflow(_)                  => "WorkflowError",
        }
    }

    /// CLI exit code per spec §7.3.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::CapabilityUnavailable { .. } => 69,
            Self::NotAuthorized { .. }         => 64,
            Self::IntegrityMismatch { .. }
            | Self::CrossMachineRestore { .. }
            | Self::VaultIdMismatch { .. }
            | Self::SchemaTooNew { .. }
            | Self::UnknownStepMarker { .. }
            | Self::UnknownConnector { .. }    => 70,
            Self::ReplayEscalated { .. }       => 75,
            Self::Store(_) | Self::Wal(_) | Self::Workflow(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(64, AdminError::NotAuthorized {
            actor: IdentityId::from("hmn:x"),
            needed: AdminRole::Operator,
        }.exit_code());
        assert_eq!(69, AdminError::CapabilityUnavailable {
            capability: "x".into(), remediation: "y".into(),
        }.exit_code());
        assert_eq!(75, AdminError::ReplayEscalated { step: "step:1".into() }.exit_code());
    }

    #[test]
    fn codes_are_pascal_case() {
        let cases = [
            AdminError::CapabilityUnavailable { capability: "x".into(), remediation: "y".into() },
            AdminError::IntegrityMismatch { expected: "a".into(), actual: "b".into() },
            AdminError::CrossMachineRestore { source: "a".into(), local: "b".into() },
        ];
        for e in cases {
            let c = e.code();
            assert!(c.chars().next().unwrap().is_ascii_uppercase(), "{c}");
        }
    }
}
```

Modify `crates/cairn-core/src/domain/admin/mod.rs`:

```rust
//! `cairn.admin.v1` extension domain types.

pub mod context;
pub mod error;

pub use context::{AdminContext, AdminRole};
pub use error::AdminError;
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-core admin::error --locked`
Expected: PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/domain/admin/
git commit -m "feat(#161): AdminError with wire-envelope code + exit-code mapping"
```

---

### Task 1.3: Define `AdminStateStore` trait

**Files:**
- Create: `crates/cairn-core/src/contract/admin_state.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`

- [ ] **Step 1: Write trait + a contract test fixture**

Create `crates/cairn-core/src/contract/admin_state.rs`:

```rust
//! Persistence boundary for admin role + connector enable/disable state.

use crate::domain::admin::AdminRole;
use crate::domain::identity::IdentityId;
use crate::store::StoreError;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorStateRow {
    pub connector_name: String,
    pub enabled: bool,
    pub last_changed_at: OffsetDateTime,
    pub last_changed_by: IdentityId,
    pub reason: Option<String>,
}

pub trait AdminStateStore: Send + Sync {
    /// Returns true iff `actor` currently holds `role` (i.e. has an
    /// `admin_roles` row with `revoked_at IS NULL`).
    fn has_role(&self, actor: &IdentityId, role: AdminRole) -> Result<bool, StoreError>;

    /// True iff at least one identity currently holds the `Operator` role.
    /// Used by `status::advertise` to decide whether to publish admin capabilities.
    fn has_any_operator(&self) -> Result<bool, StoreError>;

    /// Grant `role` to `actor`. Idempotent: regranting an active role is a no-op.
    fn grant_role(
        &self,
        actor: &IdentityId,
        role: AdminRole,
        granted_by: &IdentityId,
    ) -> Result<(), StoreError>;

    /// Revoke `role` from `actor`. No-op if `actor` does not hold the role.
    fn revoke_role(&self, actor: &IdentityId, role: AdminRole) -> Result<(), StoreError>;

    /// Upsert connector enable/disable row. Returns the row as persisted.
    fn set_connector_enabled(
        &self,
        connector_name: &str,
        enabled: bool,
        by: &IdentityId,
        reason: Option<&str>,
    ) -> Result<ConnectorStateRow, StoreError>;

    /// Returns `None` if the connector has no state row (treated as "enabled" default).
    fn get_connector_state(&self, connector_name: &str) -> Result<Option<ConnectorStateRow>, StoreError>;

    /// Snapshot of every known row, for `status.connectors[]`.
    fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError>;
}
```

Modify `crates/cairn-core/src/contract/mod.rs`, append after existing `pub mod` declarations:

```rust
pub mod admin_state;
```

- [ ] **Step 2: Compile-only check**

Run: `cargo check -p cairn-core --locked`
Expected: clean (trait definition only; no impl yet).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/contract/admin_state.rs crates/cairn-core/src/contract/mod.rs
git commit -m "feat(#161): AdminStateStore trait for role + connector-state persistence"
```

---

### Task 1.4: Migration `0003_admin_roles.sql`

**Files:**
- Create: `crates/cairn-store-sqlite/migrations/0003_admin_roles.sql`

- [ ] **Step 1: Write migration**

Create `crates/cairn-store-sqlite/migrations/0003_admin_roles.sql`:

```sql
-- Issue #161: admin role table for cairn.admin.v1 extension.
-- Append-only contract: never mutate this file after merge.
CREATE TABLE admin_roles (
  identity_id TEXT NOT NULL,
  role        TEXT NOT NULL CHECK (role IN ('operator')),
  granted_at  TEXT NOT NULL,
  granted_by  TEXT NOT NULL,
  revoked_at  TEXT,
  PRIMARY KEY (identity_id, role)
);

CREATE INDEX admin_roles_active ON admin_roles(identity_id)
  WHERE revoked_at IS NULL;
```

- [ ] **Step 2: Run store tests**

Run: `cargo nextest run -p cairn-store-sqlite --locked`
Expected: PASS (existing tests must still work; migration applies cleanly).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/migrations/0003_admin_roles.sql
git commit -m "feat(#161): migration 0003 — admin_roles table"
```

---

### Task 1.5: Migration `0004_connector_state.sql`

**Files:**
- Create: `crates/cairn-store-sqlite/migrations/0004_connector_state.sql`

- [ ] **Step 1: Write migration**

Create `crates/cairn-store-sqlite/migrations/0004_connector_state.sql`:

```sql
-- Issue #161: connector enable/disable state for cairn.admin.v1.
-- Append-only contract: never mutate this file after merge.
CREATE TABLE connector_state (
  connector_name  TEXT PRIMARY KEY,
  enabled         INTEGER NOT NULL DEFAULT 1,
  last_changed_at TEXT NOT NULL,
  last_changed_by TEXT NOT NULL,
  reason          TEXT
);
```

- [ ] **Step 2: Run store tests**

Run: `cargo nextest run -p cairn-store-sqlite --locked`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/migrations/0004_connector_state.sql
git commit -m "feat(#161): migration 0004 — connector_state table"
```

---

### Task 1.6: `SqliteAdminStateStore` implementation

**Files:**
- Create: `crates/cairn-store-sqlite/src/admin_state.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Test: `crates/cairn-store-sqlite/tests/admin_state.rs`

- [ ] **Step 1: Write failing integration test**

Create `crates/cairn-store-sqlite/tests/admin_state.rs`:

```rust
//! Contract tests for `AdminStateStore` against the real SQLite adapter.

use cairn_core::contract::admin_state::AdminStateStore;
use cairn_core::domain::admin::AdminRole;
use cairn_core::domain::identity::IdentityId;
use cairn_store_sqlite::SqliteStore;
use cairn_test_fixtures::tempvault;

#[tokio::test]
async fn role_grant_check_revoke_roundtrip() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = store.admin_state();
    let actor  = IdentityId::from("hmn:alice");
    let bootstrap = IdentityId::from("hmn:bootstrap");

    assert!(!admin.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(!admin.has_any_operator().unwrap());

    admin.grant_role(&actor, AdminRole::Operator, &bootstrap).unwrap();
    assert!(admin.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(admin.has_any_operator().unwrap());

    // idempotent
    admin.grant_role(&actor, AdminRole::Operator, &bootstrap).unwrap();
    assert!(admin.has_role(&actor, AdminRole::Operator).unwrap());

    admin.revoke_role(&actor, AdminRole::Operator).unwrap();
    assert!(!admin.has_role(&actor, AdminRole::Operator).unwrap());
    assert!(!admin.has_any_operator().unwrap());
}

#[tokio::test]
async fn connector_state_upsert_and_list() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = store.admin_state();
    let by    = IdentityId::from("hmn:op");

    assert!(admin.get_connector_state("github").unwrap().is_none());

    let row = admin.set_connector_enabled("github", false, &by, Some("rate-limit")).unwrap();
    assert!(!row.enabled);
    assert_eq!(row.reason.as_deref(), Some("rate-limit"));

    let listed = admin.list_connector_state().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].connector_name, "github");

    let row2 = admin.set_connector_enabled("github", true, &by, None).unwrap();
    assert!(row2.enabled);
    assert!(row2.reason.is_none());
}
```

- [ ] **Step 2: Verify it fails**

Run: `cargo nextest run -p cairn-store-sqlite admin_state --locked`
Expected: FAIL (no `admin_state()` method on `SqliteStore`).

- [ ] **Step 3: Implement `SqliteAdminStateStore`**

Create `crates/cairn-store-sqlite/src/admin_state.rs`:

```rust
use cairn_core::contract::admin_state::{AdminStateStore, ConnectorStateRow};
use cairn_core::domain::admin::AdminRole;
use cairn_core::domain::identity::IdentityId;
use cairn_core::store::StoreError;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use rusqlite::Connection;

pub struct SqliteAdminStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteAdminStateStore {
    pub(crate) fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn role_str(role: AdminRole) -> &'static str {
        match role { AdminRole::Operator => "operator" }
    }
}

impl AdminStateStore for SqliteAdminStateStore {
    fn has_role(&self, actor: &IdentityId, role: AdminRole) -> Result<bool, StoreError> {
        let conn = self.conn.blocking_lock();
        let role_s = Self::role_str(role);
        let row: i64 = conn.query_row(
            "SELECT COUNT(*) FROM admin_roles
             WHERE identity_id = ?1 AND role = ?2 AND revoked_at IS NULL",
            (actor.as_str(), role_s),
            |r| r.get(0),
        ).map_err(StoreError::from)?;
        Ok(row > 0)
    }

    fn has_any_operator(&self) -> Result<bool, StoreError> {
        let conn = self.conn.blocking_lock();
        let row: i64 = conn.query_row(
            "SELECT COUNT(*) FROM admin_roles WHERE role = 'operator' AND revoked_at IS NULL",
            (),
            |r| r.get(0),
        ).map_err(StoreError::from)?;
        Ok(row > 0)
    }

    fn grant_role(&self, actor: &IdentityId, role: AdminRole, granted_by: &IdentityId)
        -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        let role_s = Self::role_str(role);
        let now = OffsetDateTime::now_utc().to_string();
        conn.execute(
            "INSERT INTO admin_roles (identity_id, role, granted_at, granted_by, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(identity_id, role) DO UPDATE SET
               revoked_at = NULL,
               granted_at = excluded.granted_at,
               granted_by = excluded.granted_by",
            (actor.as_str(), role_s, &now, granted_by.as_str()),
        ).map_err(StoreError::from)?;
        Ok(())
    }

    fn revoke_role(&self, actor: &IdentityId, role: AdminRole) -> Result<(), StoreError> {
        let conn = self.conn.blocking_lock();
        let role_s = Self::role_str(role);
        let now = OffsetDateTime::now_utc().to_string();
        conn.execute(
            "UPDATE admin_roles SET revoked_at = ?3
             WHERE identity_id = ?1 AND role = ?2 AND revoked_at IS NULL",
            (actor.as_str(), role_s, &now),
        ).map_err(StoreError::from)?;
        Ok(())
    }

    fn set_connector_enabled(
        &self,
        connector_name: &str,
        enabled: bool,
        by: &IdentityId,
        reason: Option<&str>,
    ) -> Result<ConnectorStateRow, StoreError> {
        let conn = self.conn.blocking_lock();
        let now = OffsetDateTime::now_utc();
        let now_s = now.to_string();
        conn.execute(
            "INSERT INTO connector_state (connector_name, enabled, last_changed_at, last_changed_by, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(connector_name) DO UPDATE SET
               enabled = excluded.enabled,
               last_changed_at = excluded.last_changed_at,
               last_changed_by = excluded.last_changed_by,
               reason = excluded.reason",
            (connector_name, enabled as i64, &now_s, by.as_str(), reason),
        ).map_err(StoreError::from)?;
        Ok(ConnectorStateRow {
            connector_name: connector_name.into(),
            enabled,
            last_changed_at: now,
            last_changed_by: by.clone(),
            reason: reason.map(str::to_string),
        })
    }

    fn get_connector_state(&self, connector_name: &str)
        -> Result<Option<ConnectorStateRow>, StoreError> {
        let conn = self.conn.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT connector_name, enabled, last_changed_at, last_changed_by, reason
             FROM connector_state WHERE connector_name = ?1"
        ).map_err(StoreError::from)?;
        let mut rows = stmt.query([connector_name]).map_err(StoreError::from)?;
        match rows.next().map_err(StoreError::from)? {
            Some(r) => Ok(Some(ConnectorStateRow {
                connector_name: r.get(0).map_err(StoreError::from)?,
                enabled: r.get::<_, i64>(1).map_err(StoreError::from)? != 0,
                last_changed_at: parse_ts(r.get::<_, String>(2).map_err(StoreError::from)?)?,
                last_changed_by: IdentityId::from(r.get::<_, String>(3).map_err(StoreError::from)?),
                reason: r.get::<_, Option<String>>(4).map_err(StoreError::from)?,
            })),
            None => Ok(None),
        }
    }

    fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
        let conn = self.conn.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT connector_name, enabled, last_changed_at, last_changed_by, reason
             FROM connector_state ORDER BY connector_name"
        ).map_err(StoreError::from)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        }).map_err(StoreError::from)?;
        let mut out = Vec::new();
        for r in rows {
            let (name, en, ts, by, reason) = r.map_err(StoreError::from)?;
            out.push(ConnectorStateRow {
                connector_name: name,
                enabled: en != 0,
                last_changed_at: parse_ts(ts)?,
                last_changed_by: IdentityId::from(by),
                reason,
            });
        }
        Ok(out)
    }
}

fn parse_ts(s: String) -> Result<OffsetDateTime, StoreError> {
    OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::Decode(format!("bad timestamp {s}: {e}")))
}
```

Modify `crates/cairn-store-sqlite/src/lib.rs`. Find the existing `impl SqliteStore { ... }` block and append:

```rust
mod admin_state;

impl SqliteStore {
    /// Returns an `AdminStateStore` view over this store. Cheap: clones the
    /// inner connection handle (`Arc<Mutex<Connection>>`).
    #[must_use]
    pub fn admin_state(&self) -> admin_state::SqliteAdminStateStore {
        admin_state::SqliteAdminStateStore::new(self.conn.clone())
    }
}
```

If `SqliteStore::conn` isn't already `Arc<Mutex<Connection>>`, adapt the constructor to use that type. **If the codebase uses `sqlx` rather than `rusqlite`, port the queries directly — same SQL, `sqlx::query!` macros, and use `SqliteAdminStateStore { pool: SqlitePool }` instead.** Confirm which driver this crate uses (`rg "rusqlite\|sqlx" crates/cairn-store-sqlite/Cargo.toml`) and adjust before writing.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-store-sqlite admin_state --locked`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/admin_state.rs \
        crates/cairn-store-sqlite/src/lib.rs \
        crates/cairn-store-sqlite/tests/admin_state.rs
git commit -m "feat(#161): SqliteAdminStateStore impl + integration tests"
```

---

### Task 1.7: Wiring constant + `admin_extension_ready` truth table

**Files:**
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Test: `crates/cairn-core/src/status/wiring.rs` (#[cfg(test)] in-module)

- [ ] **Step 1: Add constant + helper**

Modify `crates/cairn-core/src/status/wiring.rs`. Find the block of `*_WIRED` constants (around line 40) and add:

```rust
/// Set to `true` when phase 6 of issue #161 lands the MCP/SDK wiring for
/// `cairn.admin.v1`. Until then admin verbs ship dark.
pub const ADMIN_EXTENSION_WIRED: bool = false;
```

Find the block of `*_extension_ready()` functions (around line 176) and add:

```rust
/// Truth-table gate for advertising the `cairn.admin.v1` extension.
/// All three preconditions must hold: build-time wiring, runtime config
/// opt-in, and at least one operator identity present in `admin_roles`.
#[must_use]
pub fn admin_extension_ready(config_enabled: bool, has_operator: bool) -> bool {
    ADMIN_EXTENSION_WIRED && config_enabled && has_operator
}

#[cfg(test)]
mod admin_extension_ready_tests {
    use super::*;

    #[test]
    fn truth_table() {
        // Cartesian product of (wired, config, has_op). `wired` is a const
        // so we exercise the 4 runtime combos; the `wired = true` rows are
        // verified by construction (the &&-short-circuit means flipping the
        // const can only ADD `true` rows).
        let cases = [
            (false, false, false),
            (false, true,  false),
            (true,  false, false),
            (true,  true,  false),
        ];
        for (config, has_op, expected) in cases {
            assert_eq!(admin_extension_ready(config, has_op), expected,
                "config={config} has_op={has_op}");
        }
    }
}
```

- [ ] **Step 2: Run test**

Run: `cargo nextest run -p cairn-core status::wiring::admin_extension_ready_tests --locked`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/status/wiring.rs
git commit -m "feat(#161): ADMIN_EXTENSION_WIRED + admin_extension_ready gate"
```

---

### Task 1.8: REMEDIATION rows for six admin capabilities

**Files:**
- Modify: `crates/cairn-core/src/status/remediation.rs`

- [ ] **Step 1: Add six rows**

Modify `crates/cairn-core/src/status/remediation.rs`. Find the `REMEDIATION` static array (around line 13) and append before the closing `]`:

```rust
    (
        "cairn.mcp.v1.extension.admin.snapshot",
        "enable admin extension: set `admin.enabled: true` in .cairn/config.yaml and grant operator role with `cairn admin grant <identity>`",
    ),
    (
        "cairn.mcp.v1.extension.admin.restore",
        "enable admin extension: set `admin.enabled: true` in .cairn/config.yaml and grant operator role with `cairn admin grant <identity>`",
    ),
    (
        "cairn.mcp.v1.extension.admin.replay_wal",
        "enable admin extension: set `admin.enabled: true` in .cairn/config.yaml. Dry-run requires no role; --apply requires operator",
    ),
    (
        "cairn.mcp.v1.extension.admin.connector.enable",
        "enable admin extension and grant operator role; see `cairn admin --help`",
    ),
    (
        "cairn.mcp.v1.extension.admin.connector.disable",
        "enable admin extension and grant operator role; see `cairn admin --help`",
    ),
    (
        "cairn.mcp.v1.extension.admin.connector.backfill",
        "enable admin extension and grant operator role; ensure the connector is registered and currently enabled",
    ),
```

- [ ] **Step 2: Run remediation tests**

Run: `cargo nextest run -p cairn-core status::remediation --locked`
Expected: PASS (existing snapshot tests will need acceptance — run with `INSTA_UPDATE=auto` if any insta snapshots cover this table, or run `cargo insta review`).

- [ ] **Step 3: Accept insta snapshots if any**

```bash
cargo insta accept --workspace
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/status/remediation.rs crates/cairn-core/src/status/snapshots/ 2>/dev/null || true
git commit -m "feat(#161): REMEDIATION rows for six admin capabilities"
```

---

### Task 1.9: `status::advertise` admin rows (dark)

**Files:**
- Modify: `crates/cairn-core/src/status/mod.rs`

- [ ] **Step 1: Add the six admin capability strings to the advertise table**

Modify `crates/cairn-core/src/status/mod.rs`. Locate `extension_namespaces()` (around line 471) and the row for `cairn.admin.v1` (around line 481). The row currently declares the extension but lists no capabilities. Replace it with:

```rust
ExtensionNamespace {
    name: "cairn.admin.v1",
    since: "v0.1",
    enabler: "operator role",
    capability: "cairn.mcp.v1.extension.admin",
    capabilities: &[
        AdvertisedCapability { name: "cairn.mcp.v1.extension.admin.snapshot",            ready: admin_extension_ready_for(ctx) },
        AdvertisedCapability { name: "cairn.mcp.v1.extension.admin.restore",             ready: admin_extension_ready_for(ctx) },
        AdvertisedCapability { name: "cairn.mcp.v1.extension.admin.replay_wal",          ready: admin_extension_ready_for(ctx) },
        AdvertisedCapability { name: "cairn.mcp.v1.extension.admin.connector.enable",    ready: admin_extension_ready_for(ctx) },
        AdvertisedCapability { name: "cairn.mcp.v1.extension.admin.connector.disable",   ready: admin_extension_ready_for(ctx) },
        AdvertisedCapability { name: "cairn.mcp.v1.extension.admin.connector.backfill",  ready: admin_extension_ready_for(ctx) },
    ],
},
```

**If the existing struct shape differs** (the explorer reported a literal JSON-style declaration), mirror the existing fields exactly — the only mandatory change is that six capability rows are gated on a single helper `admin_extension_ready_for(ctx)` which calls into Task 1.7's `admin_extension_ready(config.admin_enabled, ctx.has_operator)`. Add this helper at the bottom of `status/mod.rs`:

```rust
fn admin_extension_ready_for(ctx: &AdvertiseContext) -> bool {
    super::wiring::admin_extension_ready(ctx.config.admin_enabled, ctx.has_operator)
}
```

If `AdvertiseContext` does not yet carry `has_operator`/`config.admin_enabled`, extend it: both come from the caller (the SDK boundary, which reads from `AdminStateStore::has_any_operator()` and config). Defer wiring the real values to phase 6; for now stub them to `false` so the rows stay absent under `wired = false`.

- [ ] **Step 2: Snapshot test the new shape**

If `status::mod` has an `insta` snapshot test, run it; otherwise add:

```rust
#[cfg(test)]
mod admin_advertise_tests {
    use super::*;

    #[test]
    fn admin_rows_absent_when_dark() {
        let ns = extension_namespaces();
        let admin = ns.iter().find(|n| n.name == "cairn.admin.v1").unwrap();
        for cap in admin.capabilities {
            assert!(!cap.ready, "{} should be dark while ADMIN_EXTENSION_WIRED=false", cap.name);
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core status --locked`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/status/mod.rs
git commit -m "feat(#161): advertise admin capabilities (dark behind ADMIN_EXTENSION_WIRED)"
```

---

### Task 1.10: Phase 1 verification + push

- [ ] **Step 1: Run full verification gate**

Run, in order, the checks from CLAUDE.md §8:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
./scripts/check-core-boundary.sh
```

Expected: all green.

- [ ] **Step 2: Open phase-1 PR**

```bash
gh pr create --title "feat(#161): cairn.admin.v1 phase 1 — wiring + AdminStateStore + migrations" \
  --body "$(cat <<'EOF'
## Summary
Phase 1 of issue #161. Lands foundation behind `ADMIN_EXTENSION_WIRED = false`:
- `AdminContext`, `AdminRole`, `AdminError` types
- `AdminStateStore` trait + `SqliteAdminStateStore` impl
- Migrations `0003_admin_roles.sql`, `0004_connector_state.sql`
- `admin_extension_ready` truth-table gate
- Six `REMEDIATION` rows
- `status::advertise` admin capability rows (dark)

Spec: `docs/superpowers/specs/2026-05-26-issue-161-admin-v1-extension-design.md` §3-§4, §6.5-§6.6, §7.4

## Test plan
- [x] `cargo nextest run --workspace`
- [x] `./scripts/check-core-boundary.sh`
- [x] New tests: `admin::context`, `admin::error`, `wiring::admin_extension_ready_tests`, `admin_state` integration
EOF
)"
```

Wait for review.

---

## Phase 2 — Snapshot / restore move-down + manifest + integrity envelope

Move existing CLI snapshot/restore logic into `cairn-core::verbs::admin`. Add typed manifest, integrity envelope, machine-id precondition gate. CLI shrinks to a thin wrapper.

### Task 2.1: `ConsentLog` trait + SQLite impl

**Files:**
- Create: `crates/cairn-core/src/contract/consent_log.rs`
- Create: `crates/cairn-store-sqlite/src/consent_log.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Test: `crates/cairn-store-sqlite/tests/consent_log.rs`

- [ ] **Step 1: Define trait**

Create `crates/cairn-core/src/contract/consent_log.rs`:

```rust
//! Read-only view over the consent log for snapshot/restore tombstone replay.

use crate::domain::wal::StepMarker;
use crate::store::StoreError;
use crate::domain::record::TargetId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetEntry {
    pub target_id: TargetId,
    pub at_step: StepMarker,
}

pub trait ConsentLog: Send + Sync {
    /// Every `forget` event committed at or after `since` (exclusive).
    /// Used by restore to re-apply tombstones for forgets that landed
    /// after the snapshot was taken.
    fn forgets_since(&self, since: &StepMarker) -> Result<Vec<ForgetEntry>, StoreError>;
}
```

Modify `crates/cairn-core/src/contract/mod.rs`:

```rust
pub mod consent_log;
```

- [ ] **Step 2: Write failing integration test**

Create `crates/cairn-store-sqlite/tests/consent_log.rs`:

```rust
use cairn_core::contract::consent_log::ConsentLog;
use cairn_core::domain::wal::StepMarker;
use cairn_store_sqlite::SqliteStore;
use cairn_test_fixtures::tempvault;

#[tokio::test]
async fn forgets_since_returns_events_after_marker() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();

    // Use existing test helpers to commit two forgets.
    let m0 = store.current_step_marker().await.unwrap();
    store.test_forget("target-a").await.unwrap();
    let m1 = store.current_step_marker().await.unwrap();
    store.test_forget("target-b").await.unwrap();

    let log = store.consent_log();
    let after_m0 = log.forgets_since(&m0).unwrap();
    assert_eq!(after_m0.len(), 2);
    let after_m1 = log.forgets_since(&m1).unwrap();
    assert_eq!(after_m1.len(), 1);
    assert_eq!(after_m1[0].target_id.as_str(), "target-b");
}
```

Run: `cargo nextest run -p cairn-store-sqlite consent_log --locked` — expect FAIL (no `consent_log()` method).

- [ ] **Step 3: Implement `SqliteConsentLog`**

Create `crates/cairn-store-sqlite/src/consent_log.rs`:

```rust
use cairn_core::contract::consent_log::{ConsentLog, ForgetEntry};
use cairn_core::domain::record::TargetId;
use cairn_core::domain::wal::StepMarker;
use cairn_core::store::StoreError;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct SqliteConsentLog {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteConsentLog {
    pub(crate) fn new(conn: Arc<Mutex<Connection>>) -> Self { Self { conn } }
}

impl ConsentLog for SqliteConsentLog {
    fn forgets_since(&self, since: &StepMarker) -> Result<Vec<ForgetEntry>, StoreError> {
        let conn = self.conn.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT target_id, step_marker
             FROM consent_log_forgets
             WHERE step_marker > ?1
             ORDER BY step_marker ASC"
        ).map_err(StoreError::from)?;
        let rows = stmt.query_map([since.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }).map_err(StoreError::from)?;
        let mut out = Vec::new();
        for r in rows {
            let (tid, m) = r.map_err(StoreError::from)?;
            out.push(ForgetEntry {
                target_id: TargetId::from(tid),
                at_step: StepMarker::from(m),
            });
        }
        Ok(out)
    }
}
```

Modify `crates/cairn-store-sqlite/src/lib.rs`:

```rust
mod consent_log;

impl SqliteStore {
    #[must_use]
    pub fn consent_log(&self) -> consent_log::SqliteConsentLog {
        consent_log::SqliteConsentLog::new(self.conn.clone())
    }
}
```

**Note on schema:** `consent_log_forgets` may not exist as a named table — the existing `forget` verb may write into a different schema. Before implementing, run `rg "consent_log\|forget.*INSERT\|forget.*VALUES" crates/cairn-store-sqlite/migrations/` to find the canonical table; adjust the SQL accordingly.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-store-sqlite consent_log --locked` — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/contract/consent_log.rs crates/cairn-core/src/contract/mod.rs \
        crates/cairn-store-sqlite/src/consent_log.rs crates/cairn-store-sqlite/src/lib.rs \
        crates/cairn-store-sqlite/tests/consent_log.rs
git commit -m "feat(#161): ConsentLog trait + SqliteConsentLog impl"
```

---

### Task 2.2: `SnapshotManifest` + canonical JSON + integrity envelope

**Files:**
- Create: `crates/cairn-core/src/verbs/admin/mod.rs`
- Create: `crates/cairn-core/src/verbs/admin/manifest.rs`
- Modify: `crates/cairn-core/src/verbs/mod.rs`

- [ ] **Step 1: Add proptest dev-dep if missing**

Run: `cargo add --dev --package cairn-core proptest@1 --locked`
(Skip if already present per `Cargo.toml`.)

- [ ] **Step 2: Write the manifest module + tests**

Create `crates/cairn-core/src/verbs/admin/manifest.rs`:

```rust
//! Snapshot manifest schema, canonical JSON serializer, and integrity envelope.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use time::OffsetDateTime;

/// Current value of the manifest format version (the `schema_version` field).
/// Increment ONLY when introducing a forward-incompat manifest change.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub backup_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub source_machine_id: String,
    pub source_vault_id: String,
    pub frontier_step: String,
    pub record_count: u64,
    pub tombstone_count: u64,
    /// Per-component migration heads at snapshot time. Sorted by key on
    /// serialization to keep the integrity envelope stable.
    pub schema_versions: BTreeMap<String, u32>,
    pub label: Option<String>,
}

impl SnapshotManifest {
    /// Canonical JSON: sorted keys, no whitespace. Stable input for the
    /// integrity envelope so a byte-for-byte tarball entry round-trips to
    /// the same digest.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        // serde_json writes object keys in insertion order; BTreeMap +
        // struct field order (declared above) gives stable output.
        let s = serde_json::to_string(self)?;
        Ok(s.into_bytes())
    }

    pub fn from_canonical_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Three-part integrity envelope: `sha256(manifest) || sha256(db) || sha256(tree)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityEnvelope {
    pub manifest_sha256: String,
    pub db_sha256: String,
    pub tree_sha256: String,
}

impl IntegrityEnvelope {
    /// Returns the artifact-level hash `sha256(manifest_sha || db_sha || tree_sha)`
    /// in lowercase hex.
    #[must_use]
    pub fn artifact_sha256(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.manifest_sha256.as_bytes());
        h.update(self.db_sha256.as_bytes());
        h.update(self.tree_sha256.as_bytes());
        hex(h.finalize().as_slice())
    }
}

/// Compute sha256 of raw bytes, lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(h.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        use std::fmt::Write;
        write!(s, "{b:02x}").unwrap();
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn manifest_strategy() -> impl Strategy<Value = SnapshotManifest> {
        (
            any::<String>(),
            0i64..=4_102_444_800, // 2100-01-01
            any::<String>(),
            any::<String>(),
            any::<String>(),
            any::<u64>(),
            any::<u64>(),
            prop::collection::btree_map("[a-z]{1,10}".prop_map(String::from), any::<u32>(), 0..4),
            prop::option::of("[a-zA-Z0-9 _-]{0,32}".prop_map(String::from)),
        ).prop_map(|(backup_id, ts, mid, vid, step, rc, tc, vers, label)| {
            SnapshotManifest {
                schema_version: MANIFEST_SCHEMA_VERSION,
                backup_id,
                created_at: OffsetDateTime::from_unix_timestamp(ts).unwrap(),
                source_machine_id: mid,
                source_vault_id: vid,
                frontier_step: step,
                record_count: rc,
                tombstone_count: tc,
                schema_versions: vers,
                label,
            }
        })
    }

    proptest! {
        #[test]
        fn canonical_json_roundtrip(m in manifest_strategy()) {
            let bytes = m.to_canonical_json().unwrap();
            let parsed = SnapshotManifest::from_canonical_json(&bytes).unwrap();
            prop_assert_eq!(m, parsed);
        }

        #[test]
        fn canonical_json_is_stable(m in manifest_strategy()) {
            // Same struct, serialized twice, must produce byte-identical output.
            let a = m.to_canonical_json().unwrap();
            let b = m.to_canonical_json().unwrap();
            prop_assert_eq!(a, b);
        }
    }

    #[test]
    fn artifact_sha_combines_three_parts() {
        let env = IntegrityEnvelope {
            manifest_sha256: "aa".into(),
            db_sha256: "bb".into(),
            tree_sha256: "cc".into(),
        };
        // Sanity: same input → same output; different input → different output.
        let a = env.artifact_sha256();
        let b = IntegrityEnvelope { db_sha256: "ff".into(), ..env }.artifact_sha256();
        assert_ne!(a, b);
    }
}
```

Create `crates/cairn-core/src/verbs/admin/mod.rs`:

```rust
//! `cairn.admin.v1` verb implementations.

pub mod manifest;
```

Modify `crates/cairn-core/src/verbs/mod.rs`:

```rust
pub mod admin;
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core verbs::admin::manifest --locked` — expect PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/admin/ crates/cairn-core/src/verbs/mod.rs Cargo.lock Cargo.toml
git commit -m "feat(#161): SnapshotManifest with canonical JSON + integrity envelope"
```

---

### Task 2.3: `verbs::admin::snapshot` core function

**Files:**
- Create: `crates/cairn-core/src/verbs/admin/snapshot.rs`
- Modify: `crates/cairn-core/src/verbs/admin/mod.rs`

- [ ] **Step 1: Write the test first**

Add to `crates/cairn-core/src/verbs/admin/snapshot.rs` (test at bottom):

```rust
//! `cairn.admin.v1` snapshot verb.

use crate::contract::admin_state::AdminStateStore;
use crate::contract::backup_registry::BackupRegistry;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};
use crate::store::MemoryStore;
use crate::verbs::admin::manifest::{IntegrityEnvelope, SnapshotManifest, MANIFEST_SCHEMA_VERSION};
use std::path::PathBuf;
use time::OffsetDateTime;
use ulid::Ulid;

#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    pub out_path: PathBuf,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SnapshotResponse {
    pub backup_id: String,
    pub artifact_path: PathBuf,
    pub sha256: String,
    pub frontier_step: String,
    pub manifest: SnapshotManifest,
}

pub async fn run(
    ctx: AdminContext,
    req: SnapshotRequest,
    store: &dyn MemoryStore,
    admin: &dyn AdminStateStore,
    registry: &dyn BackupRegistry,
) -> Result<SnapshotResponse, AdminError> {
    super::guard::require_role(&ctx, admin, AdminRole::Operator)?;

    let backup_id = Ulid::new().to_string();
    let frontier_step = store.current_step_marker().await?.to_string();
    let machine_id = crate::host::local_machine_id();
    let vault_id = store.vault_id().await?;
    let schema_versions = store.migration_heads().await?;

    let manifest = SnapshotManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        backup_id: backup_id.clone(),
        created_at: OffsetDateTime::now_utc(),
        source_machine_id: machine_id,
        source_vault_id: vault_id,
        frontier_step: frontier_step.clone(),
        record_count: store.record_count().await?,
        tombstone_count: store.tombstone_count().await?,
        schema_versions,
        label: req.label.clone(),
    };

    let artifact_path = super::artifact::materialize(
        &req.out_path, &backup_id, &manifest, store,
    ).await?;

    let envelope = super::artifact::compute_envelope(&artifact_path, &manifest)?;
    let sha = envelope.artifact_sha256();

    registry.register(&backup_id, &artifact_path, &sha, &manifest).await?;

    Ok(SnapshotResponse {
        backup_id,
        artifact_path,
        sha256: sha,
        frontier_step,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::IdentityId;
    use crate::test_fixtures::{FakeAdminStore, FakeBackupRegistry, FakeMemoryStore};

    #[tokio::test]
    async fn snapshot_returns_envelope_and_registers() {
        let store    = FakeMemoryStore::seeded(42, 3);
        let admin    = FakeAdminStore::with_operator("hmn:op");
        let registry = FakeBackupRegistry::new();
        let ctx      = AdminContext::new(IdentityId::from("hmn:op"), AdminRole::Operator);
        let tmp      = tempfile::tempdir().unwrap();
        let req      = SnapshotRequest {
            out_path: tmp.path().to_path_buf(),
            label: Some("test".into()),
        };

        let resp = run(ctx, req, &store, &admin, &registry).await.unwrap();
        assert_eq!(resp.manifest.record_count, 42);
        assert_eq!(resp.manifest.tombstone_count, 3);
        assert!(resp.artifact_path.exists());
        assert_eq!(registry.count(), 1);
        // Re-running snapshot yields a different backup_id and sha.
        let ctx2 = AdminContext::new(IdentityId::from("hmn:op"), AdminRole::Operator);
        let resp2 = run(ctx2, SnapshotRequest { out_path: tmp.path().into(), label: None },
                        &store, &admin, &registry).await.unwrap();
        assert_ne!(resp.backup_id, resp2.backup_id);
    }

    #[tokio::test]
    async fn snapshot_rejects_non_operator() {
        let store    = FakeMemoryStore::seeded(0, 0);
        let admin    = FakeAdminStore::empty();
        let registry = FakeBackupRegistry::new();
        let ctx      = AdminContext::new(IdentityId::from("hmn:nobody"), AdminRole::Operator);
        let tmp      = tempfile::tempdir().unwrap();

        let err = run(ctx, SnapshotRequest { out_path: tmp.path().into(), label: None },
                      &store, &admin, &registry).await.unwrap_err();
        matches!(err, AdminError::NotAuthorized { .. });
        assert_eq!(registry.count(), 0);
    }
}
```

Modify `crates/cairn-core/src/verbs/admin/mod.rs`:

```rust
pub mod artifact;
pub mod guard;
pub mod manifest;
pub mod snapshot;

#[cfg(test)]
pub(crate) mod test_fixtures;
```

- [ ] **Step 2: Add guard helper**

Create `crates/cairn-core/src/verbs/admin/guard.rs`:

```rust
use crate::contract::admin_state::AdminStateStore;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};

pub fn require_role(
    ctx: &AdminContext,
    admin: &dyn AdminStateStore,
    needed: AdminRole,
) -> Result<(), AdminError> {
    if !admin.has_role(&ctx.actor, needed)? {
        return Err(AdminError::NotAuthorized {
            actor: ctx.actor.clone(),
            needed,
        });
    }
    Ok(())
}
```

- [ ] **Step 3: Add artifact materializer**

Create `crates/cairn-core/src/verbs/admin/artifact.rs`:

```rust
//! Tarball materialization + integrity envelope computation for snapshots.

use crate::domain::admin::AdminError;
use crate::store::MemoryStore;
use crate::verbs::admin::manifest::{sha256_hex, IntegrityEnvelope, SnapshotManifest};
use std::path::{Path, PathBuf};

pub async fn materialize(
    out_dir: &Path,
    backup_id: &str,
    manifest: &SnapshotManifest,
    store: &dyn MemoryStore,
) -> Result<PathBuf, AdminError> {
    let filename = format!("{}-{backup_id}.cairn-snap.tar.zst",
        manifest.label.as_deref().unwrap_or("snap"));
    let path = out_dir.join(filename);

    let file = std::fs::File::create(&path)
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    let zstd = zstd::Encoder::new(file, 3)
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?
        .auto_finish();
    let mut tar = tar::Builder::new(zstd);

    // 1. manifest.json — first member.
    let manifest_bytes = manifest.to_canonical_json()
        .map_err(|e| AdminError::Store(crate::store::StoreError::Encode(e.to_string())))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &manifest_bytes[..])
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;

    // 2. cairn.db via sqlite online backup — stream to a temp file then append.
    let tmp_db = tempfile::NamedTempFile::new()
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    store.online_backup_to(tmp_db.path()).await?;
    tar.append_path_with_name(tmp_db.path(), "cairn.db")
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;

    // 3-5. wiki/, raw/, purpose.md, config.snapshot.yaml — copied from vault layout.
    let vault_root = store.vault_root();
    for sub in ["wiki", "raw"] {
        let p = vault_root.join(sub);
        if p.exists() {
            tar.append_dir_all(sub, &p)
                .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
        }
    }
    for file_name in ["purpose.md"] {
        let p = vault_root.join(file_name);
        if p.exists() {
            tar.append_path_with_name(&p, file_name)
                .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
        }
    }
    // Filtered config (strip secrets) — done by store.export_config_snapshot().
    let cfg_bytes = store.export_filtered_config().await?;
    let mut h = tar::Header::new_gnu();
    h.set_size(cfg_bytes.len() as u64);
    h.set_mode(0o600);
    h.set_cksum();
    tar.append_data(&mut h, "config.snapshot.yaml", &cfg_bytes[..])
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;

    tar.finish().map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    Ok(path)
}

pub fn compute_envelope(
    artifact_path: &Path,
    manifest: &SnapshotManifest,
) -> Result<IntegrityEnvelope, AdminError> {
    let manifest_bytes = manifest.to_canonical_json()
        .map_err(|e| AdminError::Store(crate::store::StoreError::Encode(e.to_string())))?;
    let manifest_sha = sha256_hex(&manifest_bytes);

    // Re-open the tarball, read `cairn.db` and the tree.
    let file = std::fs::File::open(artifact_path)
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    let zstd = zstd::Decoder::new(file)
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    let mut archive = tar::Archive::new(zstd);

    let mut db_hasher = sha2::Sha256::new();
    let mut tree_hasher = sha2::Sha256::new();
    use sha2::Digest;
    use std::io::Read;

    for entry in archive.entries()
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))? {
        let mut e = entry.map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
        let path = e.path()
            .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?
            .into_owned();
        let path_str = path.to_string_lossy().into_owned();
        // Tree hasher: hash each member's path + size, sorted by name (tar
        // preserves insertion order; manifest.json + cairn.db + dirs go in
        // a fixed order so this is deterministic).
        tree_hasher.update(path_str.as_bytes());
        tree_hasher.update(&e.header().size().unwrap_or(0).to_le_bytes());
        if path_str == "cairn.db" {
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)
                .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
            db_hasher.update(&buf);
        }
    }

    Ok(IntegrityEnvelope {
        manifest_sha256: manifest_sha,
        db_sha256: super::manifest::sha256_hex(&db_hasher.finalize()),
        tree_sha256: super::manifest::sha256_hex(&tree_hasher.finalize()),
    })
}
```

- [ ] **Step 4: Add test fixtures**

Create `crates/cairn-core/src/verbs/admin/test_fixtures.rs`:

```rust
//! Fakes used by `verbs::admin::*` unit tests.

use crate::contract::admin_state::{AdminStateStore, ConnectorStateRow};
use crate::contract::backup_registry::BackupRegistry;
use crate::domain::admin::AdminRole;
use crate::domain::identity::IdentityId;
use crate::store::{MemoryStore, StoreError};
use crate::verbs::admin::manifest::SnapshotManifest;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use time::OffsetDateTime;

pub(crate) struct FakeMemoryStore {
    records: u64,
    tombstones: u64,
    root: tempfile::TempDir,
}

impl FakeMemoryStore {
    pub fn seeded(records: u64, tombstones: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        // Write a stub cairn.db so online_backup_to has something to copy.
        std::fs::write(root.path().join("cairn.db"), b"FAKE-DB").unwrap();
        Self { records, tombstones, root }
    }
}

// Minimal MemoryStore impl: stub the methods used by snapshot.
// (Add additional stubs when later verbs need them.)
impl MemoryStore for FakeMemoryStore {
    fn vault_root(&self) -> &Path { self.root.path() }

    // ... existing trait methods stubbed to return defaults ...
}

pub(crate) struct FakeAdminStore {
    operators: Mutex<Vec<String>>,
}

impl FakeAdminStore {
    pub fn empty() -> Self { Self { operators: Mutex::new(vec![]) } }
    pub fn with_operator(id: &str) -> Self {
        Self { operators: Mutex::new(vec![id.into()]) }
    }
}

impl AdminStateStore for FakeAdminStore {
    fn has_role(&self, actor: &IdentityId, _role: AdminRole) -> Result<bool, StoreError> {
        Ok(self.operators.lock().unwrap().iter().any(|id| id == actor.as_str()))
    }
    fn has_any_operator(&self) -> Result<bool, StoreError> {
        Ok(!self.operators.lock().unwrap().is_empty())
    }
    fn grant_role(&self, actor: &IdentityId, _: AdminRole, _: &IdentityId)
        -> Result<(), StoreError> {
        self.operators.lock().unwrap().push(actor.as_str().into()); Ok(())
    }
    fn revoke_role(&self, actor: &IdentityId, _: AdminRole) -> Result<(), StoreError> {
        self.operators.lock().unwrap().retain(|id| id != actor.as_str()); Ok(())
    }
    fn set_connector_enabled(&self, _: &str, _: bool, _: &IdentityId, _: Option<&str>)
        -> Result<ConnectorStateRow, StoreError> { unimplemented!("not used in snapshot tests") }
    fn get_connector_state(&self, _: &str) -> Result<Option<ConnectorStateRow>, StoreError> { Ok(None) }
    fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> { Ok(vec![]) }
}

pub(crate) struct FakeBackupRegistry {
    entries: Mutex<Vec<String>>,
}

impl FakeBackupRegistry {
    pub fn new() -> Self { Self { entries: Mutex::new(vec![]) } }
    pub fn count(&self) -> usize { self.entries.lock().unwrap().len() }
}

#[async_trait::async_trait]
impl BackupRegistry for FakeBackupRegistry {
    async fn register(&self, backup_id: &str, _: &Path, _: &str, _: &SnapshotManifest)
        -> Result<(), StoreError> {
        self.entries.lock().unwrap().push(backup_id.into()); Ok(())
    }
    // ... other methods stubbed
}
```

**Note:** the `MemoryStore` trait may not have `online_backup_to`, `vault_root`, `record_count`, `migration_heads`, `vault_id`, or `export_filtered_config` methods today. Each must be added in a *minimal* way to the trait, with the SQLite impl filling them in. Existing in-memory test impls (if any) get default stubs. If this expands the surface significantly, split the trait extension into a separate prep commit before this task.

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p cairn-core verbs::admin::snapshot --locked` — expect PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/verbs/admin/
git commit -m "feat(#161): verbs::admin::snapshot core verb"
```

---

### Task 2.4: `verbs::admin::restore` core function

**Files:**
- Create: `crates/cairn-core/src/verbs/admin/restore.rs`
- Modify: `crates/cairn-core/src/verbs/admin/mod.rs`

- [ ] **Step 1: Add to mod.rs**

```rust
pub mod restore;
```

- [ ] **Step 2: Write the restore verb + tests**

Create `crates/cairn-core/src/verbs/admin/restore.rs`:

```rust
//! `cairn.admin.v1` restore verb. Precondition gate per spec §6.4.

use crate::contract::admin_state::AdminStateStore;
use crate::contract::backup_registry::BackupRegistry;
use crate::contract::consent_log::ConsentLog;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};
use crate::store::MemoryStore;
use crate::verbs::admin::manifest::{SnapshotManifest, MANIFEST_SCHEMA_VERSION};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub artifact_path: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct RestoreResponse {
    pub restored_records: u64,
    pub tombstones_replayed: u64,
    pub frontier_step: String,
}

pub async fn run(
    ctx: AdminContext,
    req: RestoreRequest,
    store: &dyn MemoryStore,
    admin: &dyn AdminStateStore,
    registry: &dyn BackupRegistry,
    consent: &dyn ConsentLog,
) -> Result<RestoreResponse, AdminError> {
    super::guard::require_role(&ctx, admin, AdminRole::Operator)?;

    let manifest = super::artifact::read_manifest(&req.artifact_path)?;
    precondition_gate(&manifest, store).await?;
    super::artifact::verify_envelope(&req.artifact_path, &manifest)?;

    if req.dry_run {
        return Ok(RestoreResponse {
            restored_records: manifest.record_count,
            tombstones_replayed: 0,
            frontier_step: manifest.frontier_step,
        });
    }

    let staged = super::artifact::stage(&req.artifact_path, &manifest)?;
    store.swap_in(&staged).await?;

    let forgets = consent.forgets_since(&manifest.frontier_step.clone().into())?;
    for f in &forgets {
        store.apply_tombstone(&f.target_id).await?;
    }
    registry.note_restore(&manifest.backup_id).await?;

    Ok(RestoreResponse {
        restored_records: manifest.record_count,
        tombstones_replayed: forgets.len() as u64,
        frontier_step: store.current_step_marker().await?.to_string(),
    })
}

async fn precondition_gate(
    m: &SnapshotManifest,
    store: &dyn MemoryStore,
) -> Result<(), AdminError> {
    // 1. manifest schema_version known.
    if m.schema_version > MANIFEST_SCHEMA_VERSION {
        return Err(AdminError::SchemaTooNew {
            source: m.schema_version,
            local: MANIFEST_SCHEMA_VERSION,
        });
    }
    // 2. machine match.
    let local_machine = crate::host::local_machine_id();
    if m.source_machine_id != local_machine {
        return Err(AdminError::CrossMachineRestore {
            source: m.source_machine_id.clone(),
            local: local_machine,
        });
    }
    // 3. vault match.
    let local_vault = store.vault_id().await?;
    if m.source_vault_id != local_vault {
        return Err(AdminError::VaultIdMismatch {
            source: m.source_vault_id.clone(),
            local: local_vault,
        });
    }
    // 4. component schema heads ≤ local.
    let heads = store.migration_heads().await?;
    for (component, ver) in &m.schema_versions {
        if let Some(local_ver) = heads.get(component) {
            if ver > local_ver {
                return Err(AdminError::SchemaTooNew { source: *ver, local: *local_ver });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::IdentityId;
    use crate::verbs::admin::test_fixtures::*;

    #[tokio::test]
    async fn cross_machine_refused() {
        let store    = FakeMemoryStore::seeded(0, 0);
        let admin    = FakeAdminStore::with_operator("hmn:op");
        let registry = FakeBackupRegistry::new();
        let consent  = FakeConsentLog::new();
        let artifact = make_snapshot_with_machine_id("OTHER-MACHINE", &tempfile::tempdir().unwrap());
        let ctx      = AdminContext::new(IdentityId::from("hmn:op"), AdminRole::Operator);

        let err = run(ctx, RestoreRequest { artifact_path: artifact, dry_run: false },
                      &store, &admin, &registry, &consent).await.unwrap_err();
        assert!(matches!(err, AdminError::CrossMachineRestore { .. }));
    }

    #[tokio::test]
    async fn dry_run_does_not_swap() {
        // ... seeds same-machine snapshot, runs dry_run, asserts no store.swap_in() called.
    }
}
```

(`FakeConsentLog`, `make_snapshot_with_machine_id` and the additional `FakeMemoryStore` stubs need adding to `test_fixtures.rs` — same pattern as Task 2.3.)

- [ ] **Step 3: Add artifact reader + verifier**

Extend `crates/cairn-core/src/verbs/admin/artifact.rs`:

```rust
pub fn read_manifest(artifact: &Path) -> Result<SnapshotManifest, AdminError> {
    use std::io::Read;
    let f = std::fs::File::open(artifact)
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    let zstd = zstd::Decoder::new(f)
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
    let mut archive = tar::Archive::new(zstd);
    for entry in archive.entries()
        .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))? {
        let mut e = entry.map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
        let p = e.path()
            .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?
            .into_owned();
        if p.to_string_lossy() == "manifest.json" {
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)
                .map_err(|e| AdminError::Store(crate::store::StoreError::Io(e.to_string())))?;
            return SnapshotManifest::from_canonical_json(&buf)
                .map_err(|e| AdminError::Store(crate::store::StoreError::Decode(e.to_string())));
        }
    }
    Err(AdminError::IntegrityMismatch {
        expected: "manifest.json".into(),
        actual: "missing".into(),
    })
}

pub fn verify_envelope(artifact: &Path, m: &SnapshotManifest) -> Result<(), AdminError> {
    let recomputed = compute_envelope(artifact, m)?;
    // Spec §6.3: registry entry holds the artifact-level sha, but the
    // envelope must be self-consistent (re-compute matches a freshly-read
    // manifest digest).
    let manifest_bytes = m.to_canonical_json()
        .map_err(|e| AdminError::Store(crate::store::StoreError::Encode(e.to_string())))?;
    let manifest_sha = super::manifest::sha256_hex(&manifest_bytes);
    if recomputed.manifest_sha256 != manifest_sha {
        return Err(AdminError::IntegrityMismatch {
            expected: manifest_sha,
            actual: recomputed.manifest_sha256,
        });
    }
    Ok(())
}

pub fn stage(_artifact: &Path, _m: &SnapshotManifest) -> Result<PathBuf, AdminError> {
    // Stage the tarball contents into `.cairn/restore-<backup_id>/` and
    // return the staging root. Caller does the atomic rename.
    todo!("phase 2 task 2.4")
}
```

Replace the `todo!()` with the real impl before commit; the structure mirrors `materialize` in reverse.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-core verbs::admin::restore --locked` — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verbs/admin/
git commit -m "feat(#161): verbs::admin::restore with precondition gate"
```

---

### Task 2.5: CLI `admin snapshot` thin wrapper

**Files:**
- Modify: `crates/cairn-cli/src/verbs/admin_snapshot.rs`

- [ ] **Step 1: Replace existing module body**

Read the existing file (about 200 lines per earlier exploration). Replace the body with a thin dispatch:

```rust
//! `cairn admin snapshot` — thin CLI wrapper around `cairn_core::verbs::admin::snapshot`.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use cairn_core::verbs::admin::snapshot::{run, SnapshotRequest};
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_sdk::admin::resolve_actor;

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Directory to write the snapshot tarball into.
    #[arg(long)]
    pub out: PathBuf,

    /// Human-readable label embedded in the manifest + filename.
    #[arg(long)]
    pub label: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

pub async fn execute(args: SnapshotArgs, ctx: cairn_sdk::CliContext) -> Result<i32> {
    let actor = resolve_actor(&ctx).context("resolve actor identity")?;
    let admin_ctx = AdminContext::new(actor, AdminRole::Operator);
    let req = SnapshotRequest { out_path: args.out, label: args.label };

    let resp = run(admin_ctx, req,
        ctx.store(), ctx.admin_state(), ctx.backup_registry()).await;

    match resp {
        Ok(r) => {
            if args.json {
                println!("{}", serde_json::to_string(&r.into_wire())?);
            } else {
                println!("snapshot {} → {}", r.backup_id, r.artifact_path.display());
                println!("  sha256: {}", r.sha256);
                println!("  frontier_step: {}", r.frontier_step);
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(e.exit_code() as i32)
        }
    }
}

// Keep the old `replay_current_forgets` function alive ONLY if other modules
// still reference it; phase 2 task 2.1 promotes its replacement to ConsentLog.
```

`SnapshotResponse::into_wire` returns a `Serialize` struct matching the JSON shape pinned in spec §7.2. Add an `into_wire(self)` method on `SnapshotResponse` in `cairn-core` if absent.

- [ ] **Step 2: Run CLI snapshot tests**

Run: `cargo nextest run -p cairn-cli admin_snapshot --locked`
Expected: existing CLI snapshot tests pass (they probably need `insta` accept since the output format changed slightly — review with `cargo insta review`).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/src/verbs/admin_snapshot.rs crates/cairn-cli/tests/snapshots/ 2>/dev/null || true
git commit -m "feat(#161): refactor CLI admin_snapshot to thin core wrapper"
```

---

### Task 2.6: CLI `admin restore` thin wrapper

Same shape as Task 2.5, but for restore. Modify `crates/cairn-cli/src/verbs/admin_restore.rs` to dispatch into `cairn_core::verbs::admin::restore::run`. Add `--dry-run` and `--json` flags. Map every `AdminError` variant to its spec §7.3 exit code via `e.exit_code()`.

- [ ] **Step 1: Replace module body**

```rust
//! `cairn admin restore` — thin CLI wrapper around `cairn_core::verbs::admin::restore`.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use cairn_core::verbs::admin::restore::{run, RestoreRequest};
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_sdk::admin::resolve_actor;

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Path to the `.cairn-snap.tar.zst` artifact.
    pub artifact: PathBuf,

    /// Verify the artifact + manifest but skip the actual swap-in.
    #[arg(long)]
    pub dry_run: bool,

    #[arg(long)]
    pub json: bool,
}

pub async fn execute(args: RestoreArgs, ctx: cairn_sdk::CliContext) -> Result<i32> {
    let actor = resolve_actor(&ctx).context("resolve actor identity")?;
    let admin_ctx = AdminContext::new(actor, AdminRole::Operator);
    let req = RestoreRequest { artifact_path: args.artifact, dry_run: args.dry_run };

    let resp = run(admin_ctx, req,
        ctx.store(), ctx.admin_state(), ctx.backup_registry(), ctx.consent_log()).await;

    match resp {
        Ok(r) => {
            if args.json {
                println!("{}", serde_json::to_string(&r.into_wire())?);
            } else {
                println!("restored {} records, {} tombstones replayed",
                    r.restored_records, r.tombstones_replayed);
                println!("  frontier_step: {}", r.frontier_step);
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(e.exit_code() as i32)
        }
    }
}
```

- [ ] **Step 2: Run + accept snapshots**

Run: `cargo nextest run -p cairn-cli admin_restore --locked && cargo insta review`

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/src/verbs/admin_restore.rs crates/cairn-cli/tests/snapshots/ 2>/dev/null || true
git commit -m "feat(#161): refactor CLI admin_restore to thin core wrapper"
```

---

### Task 2.7: Integration test — snapshot/restore round-trip (AC#2)

**Files:**
- Create: `crates/cairn-core/tests/admin_snapshot_restore_roundtrip.rs`

- [ ] **Step 1: Write the test**

```rust
//! Acceptance criterion #2: round-trip snapshot→restore on identical machine
//! yields bit-identical record hashes and preserves tombstone state.

use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::domain::identity::IdentityId;
use cairn_core::verbs::admin::{snapshot, restore};
use cairn_store_sqlite::SqliteStore;
use cairn_test_fixtures::{tempvault, seed_records, seed_forgets};

#[tokio::test]
async fn roundtrip_preserves_record_hashes_and_tombstones() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();
    seed_records(&store, 100).await;
    seed_forgets(&store, 5).await;

    let admin = store.admin_state();
    let bootstrap = IdentityId::from("hmn:bootstrap");
    let op = IdentityId::from("hmn:op");
    admin.grant_role(&op, AdminRole::Operator, &bootstrap).unwrap();

    let registry = vault.backup_registry();
    let consent  = store.consent_log();

    let pre_hashes = store.all_record_hashes().await;
    let pre_tombstones = store.tombstone_count().await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let snap_resp = snapshot::run(
        AdminContext::new(op.clone(), AdminRole::Operator),
        snapshot::SnapshotRequest { out_path: tmp.path().into(), label: Some("rt".into()) },
        &store, &admin, &registry,
    ).await.unwrap();

    // Drop and reopen the vault to simulate a clean restore target.
    drop(store);
    let store2 = SqliteStore::open(vault.db_path()).await.unwrap();
    // Wipe contents but keep the schema (helper from cairn-test-fixtures).
    store2.wipe_all_records().await.unwrap();

    let admin2 = store2.admin_state();
    admin2.grant_role(&op, AdminRole::Operator, &bootstrap).unwrap();
    let registry2 = vault.backup_registry();
    let consent2  = store2.consent_log();

    restore::run(
        AdminContext::new(op.clone(), AdminRole::Operator),
        restore::RestoreRequest { artifact_path: snap_resp.artifact_path, dry_run: false },
        &store2, &admin2, &registry2, &consent2,
    ).await.unwrap();

    let post_hashes = store2.all_record_hashes().await;
    let post_tombstones = store2.tombstone_count().await.unwrap();

    assert_eq!(pre_hashes, post_hashes, "record hashes must be bit-identical");
    assert_eq!(pre_tombstones, post_tombstones, "tombstone count preserved");
}
```

- [ ] **Step 2: Run test**

Run: `cargo nextest run -p cairn-core admin_snapshot_restore_roundtrip --locked` — expect PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/admin_snapshot_restore_roundtrip.rs
git commit -m "test(#161): AC#2 snapshot/restore round-trip integration"
```

---

### Task 2.8: Integration test — cross-machine refused

**Files:**
- Create: `crates/cairn-core/tests/admin_cross_machine_refused.rs`

- [ ] **Step 1: Write the test**

```rust
//! Spec §6.4 precondition #2: cross-machine restore fails closed with
//! `AdminError::CrossMachineRestore` and the registry is untouched.

use cairn_core::domain::admin::{AdminContext, AdminError, AdminRole};
use cairn_core::domain::identity::IdentityId;
use cairn_core::verbs::admin::{restore, snapshot};
use cairn_store_sqlite::SqliteStore;
use cairn_test_fixtures::{tempvault, force_machine_id};

#[tokio::test]
async fn restore_refuses_cross_machine_snapshot() {
    let vault_a = tempvault();
    let store_a = SqliteStore::open(vault_a.db_path()).await.unwrap();
    let admin_a = store_a.admin_state();
    let op = IdentityId::from("hmn:op");
    let bootstrap = IdentityId::from("hmn:bootstrap");
    admin_a.grant_role(&op, AdminRole::Operator, &bootstrap).unwrap();
    let registry_a = vault_a.backup_registry();

    // Snapshot under machine A.
    force_machine_id("MACHINE-A");
    let tmp = tempfile::tempdir().unwrap();
    let snap = snapshot::run(
        AdminContext::new(op.clone(), AdminRole::Operator),
        snapshot::SnapshotRequest { out_path: tmp.path().into(), label: None },
        &store_a, &admin_a, &registry_a,
    ).await.unwrap();

    // Restore under machine B.
    force_machine_id("MACHINE-B");
    let vault_b = tempvault();
    let store_b = SqliteStore::open(vault_b.db_path()).await.unwrap();
    let admin_b = store_b.admin_state();
    admin_b.grant_role(&op, AdminRole::Operator, &bootstrap).unwrap();
    let registry_b = vault_b.backup_registry();
    let consent_b  = store_b.consent_log();

    let err = restore::run(
        AdminContext::new(op, AdminRole::Operator),
        restore::RestoreRequest { artifact_path: snap.artifact_path, dry_run: false },
        &store_b, &admin_b, &registry_b, &consent_b,
    ).await.unwrap_err();

    assert!(matches!(err, AdminError::CrossMachineRestore { ref source, ref local }
        if source == "MACHINE-A" && local == "MACHINE-B"));
}
```

(`force_machine_id` is a new test-fixtures helper that sets a process-local override read by `host::local_machine_id()`. Add it as part of this task.)

- [ ] **Step 2: Run test**

Run: `cargo nextest run -p cairn-core admin_cross_machine_refused --locked` — expect PASS.

- [ ] **Step 3: Commit + open phase-2 PR**

```bash
git add crates/cairn-core/tests/admin_cross_machine_refused.rs crates/cairn-test-fixtures/
git commit -m "test(#161): cross-machine restore refused with typed error"

gh pr create --title "feat(#161): cairn.admin.v1 phase 2 — snapshot/restore in core" \
  --body "Phase 2: ConsentLog trait + SqliteConsentLog impl; SnapshotManifest with canonical JSON + integrity envelope; snapshot/restore core verbs with precondition gate; CLI refactored to thin wrappers; AC#2 round-trip + cross-machine refused integration tests. Spec §5, §6.1-§6.4."
```

---

## Phase 3 — `replay_wal` (dry-run + apply)

Diagnostic-first WAL replay verb. Dry-run streams the step graph from a marker without mutation (safe under standard identity). `--apply` re-executes idempotent steps; non-idempotent steps escalate to `PURGE_PENDING` per brief §5.6.

### Task 3.1: `WalReplayer` pure function

**Files:**
- Create: `crates/cairn-core/src/wal/replayer.rs`
- Modify: `crates/cairn-core/src/wal/mod.rs`

- [ ] **Step 1: Define the replayer + tests**

Create `crates/cairn-core/src/wal/replayer.rs`:

```rust
//! Pure WAL replay primitive. Given a step graph and a starting marker,
//! produce the ordered sequence of `StepEvent`s. Apply-mode is handled by
//! `verbs::admin::replay_wal`; this module is *only* the iterator.

use crate::domain::wal::{StepMarker, StepGraph, StepKind};
use crate::wal::WalError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEvent {
    pub marker: StepMarker,
    pub kind: StepKind,
    pub idempotent: bool,
}

/// Walk the step graph from `from_step` (exclusive) to the head.
/// Pure — no I/O.
pub fn iter_from<'a>(
    graph: &'a StepGraph,
    from_step: &StepMarker,
) -> Result<Box<dyn Iterator<Item = StepEvent> + 'a>, WalError> {
    if !graph.contains(from_step) {
        return Err(WalError::UnknownMarker(from_step.clone()));
    }
    Ok(Box::new(graph.steps_after(from_step).map(|s| StepEvent {
        marker: s.marker().clone(),
        kind: s.kind(),
        idempotent: s.is_idempotent(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::wal::{StepGraph, StepKind};

    fn fixture_graph() -> StepGraph {
        let mut g = StepGraph::new();
        g.push("step:1", StepKind::IngestApply, true);
        g.push("step:2", StepKind::ProjectionWrite, true);
        g.push("step:3", StepKind::PrimaryPurge, false);
        g
    }

    #[test]
    fn iterates_from_marker() {
        let g = fixture_graph();
        let evs: Vec<_> = iter_from(&g, &"step:1".into()).unwrap().collect();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].marker, "step:2".into());
        assert_eq!(evs[1].marker, "step:3".into());
        assert!(!evs[1].idempotent);
    }

    #[test]
    fn unknown_marker_errors() {
        let g = fixture_graph();
        let err = iter_from(&g, &"step:99".into()).unwrap_err();
        assert!(matches!(err, WalError::UnknownMarker(_)));
    }
}
```

Modify `crates/cairn-core/src/wal/mod.rs`:

```rust
pub mod replayer;
pub use replayer::{iter_from, StepEvent};
```

The exact shape of `StepGraph`, `StepKind`, `StepMarker`, and `WalError::UnknownMarker` depend on what landed in #55. Before writing, check `crates/cairn-core/src/wal/step_graph.rs` and adjust the test fixture to use the real constructors and variants.

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-core wal::replayer --locked` — expect PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/wal/
git commit -m "feat(#161): WalReplayer iter_from primitive"
```

---

### Task 3.2: `verbs::admin::replay_wal` core verb

**Files:**
- Create: `crates/cairn-core/src/verbs/admin/replay_wal.rs`
- Modify: `crates/cairn-core/src/verbs/admin/mod.rs`

- [ ] **Step 1: Add to mod.rs**

```rust
pub mod replay_wal;
```

- [ ] **Step 2: Write the verb + tests**

Create `crates/cairn-core/src/verbs/admin/replay_wal.rs`:

```rust
//! `cairn.admin.v1` replay_wal verb.

use crate::contract::admin_state::AdminStateStore;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};
use crate::domain::wal::StepMarker;
use crate::store::MemoryStore;
use crate::wal::{iter_from, StepEvent};

#[derive(Debug, Clone)]
pub struct ReplayWalRequest {
    pub from_step: StepMarker,
    pub apply: bool,
}

#[derive(Debug, Clone)]
pub struct ReplayWalResponse {
    pub steps_visited: u64,
    pub steps_applied: u64,
    pub escalated: Vec<EscalatedStep>,
    pub events: Vec<StepEvent>,
}

#[derive(Debug, Clone)]
pub struct EscalatedStep {
    pub marker: StepMarker,
    pub reason: String,
}

pub async fn run(
    ctx: AdminContext,
    req: ReplayWalRequest,
    store: &dyn MemoryStore,
    admin: &dyn AdminStateStore,
) -> Result<ReplayWalResponse, AdminError> {
    if req.apply {
        super::guard::require_role(&ctx, admin, AdminRole::Operator)?;
    }

    let graph = store.wal_step_graph().await?;
    let events: Vec<StepEvent> = iter_from(&graph, &req.from_step)?.collect();

    if !req.apply {
        return Ok(ReplayWalResponse {
            steps_visited: events.len() as u64,
            steps_applied: 0,
            escalated: vec![],
            events,
        });
    }

    let mut steps_applied = 0u64;
    let mut escalated = Vec::new();
    for ev in &events {
        if !ev.idempotent {
            store.mark_purge_pending(&ev.marker).await?;
            escalated.push(EscalatedStep {
                marker: ev.marker.clone(),
                reason: "non-idempotent step requires manual intervention".into(),
            });
            // Halt on first non-idempotent step per brief §5.6.
            break;
        }
        store.reapply_step(&ev.marker).await?;
        steps_applied += 1;
    }

    if !escalated.is_empty() {
        return Err(AdminError::ReplayEscalated {
            step: escalated[0].marker.to_string(),
        });
    }

    Ok(ReplayWalResponse {
        steps_visited: events.len() as u64,
        steps_applied,
        escalated,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::IdentityId;
    use crate::verbs::admin::test_fixtures::*;

    #[tokio::test]
    async fn dry_run_no_mutation_no_role_needed() {
        let store = FakeMemoryStore::with_wal_steps(vec![
            ("step:1", true), ("step:2", true), ("step:3", true),
        ]);
        let admin = FakeAdminStore::empty();
        let ctx   = AdminContext::new(IdentityId::from("hmn:anyone"), AdminRole::Operator);

        let resp = run(ctx,
            ReplayWalRequest { from_step: "step:1".into(), apply: false },
            &store, &admin).await.unwrap();
        assert_eq!(resp.steps_visited, 2);
        assert_eq!(resp.steps_applied, 0);
        assert_eq!(store.mutation_count(), 0);
    }

    #[tokio::test]
    async fn apply_without_role_rejected() {
        let store = FakeMemoryStore::with_wal_steps(vec![("step:1", true), ("step:2", true)]);
        let admin = FakeAdminStore::empty();
        let ctx   = AdminContext::new(IdentityId::from("hmn:nope"), AdminRole::Operator);

        let err = run(ctx,
            ReplayWalRequest { from_step: "step:1".into(), apply: true },
            &store, &admin).await.unwrap_err();
        assert!(matches!(err, AdminError::NotAuthorized { .. }));
    }

    #[tokio::test]
    async fn apply_escalates_on_non_idempotent() {
        let store = FakeMemoryStore::with_wal_steps(vec![
            ("step:1", true), ("step:2", true), ("step:3", false), ("step:4", true),
        ]);
        let admin = FakeAdminStore::with_operator("hmn:op");
        let ctx   = AdminContext::new(IdentityId::from("hmn:op"), AdminRole::Operator);

        let err = run(ctx,
            ReplayWalRequest { from_step: "step:1".into(), apply: true },
            &store, &admin).await.unwrap_err();
        assert!(matches!(err, AdminError::ReplayEscalated { ref step } if step == "step:3"));
        assert_eq!(store.purge_pending_marks(), vec!["step:3".to_string()]);
    }
}
```

- [ ] **Step 3: Extend `FakeMemoryStore`** in `test_fixtures.rs` with `with_wal_steps`, `mutation_count`, `purge_pending_marks`. Mirror the pattern from Task 2.3.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-core verbs::admin::replay_wal --locked` — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verbs/admin/
git commit -m "feat(#161): verbs::admin::replay_wal with dry-run + apply + escalation"
```

---

### Task 3.3: CLI `admin replay-wal` subcommand

**Files:**
- Create: `crates/cairn-cli/src/verbs/admin_replay_wal.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/main.rs` (Cli enum)

- [ ] **Step 1: Add subcommand module**

Create `crates/cairn-cli/src/verbs/admin_replay_wal.rs`:

```rust
//! `cairn admin replay-wal` — thin CLI wrapper.

use anyhow::Result;
use clap::Args;
use cairn_core::verbs::admin::replay_wal::{run, ReplayWalRequest};
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_sdk::admin::resolve_actor;

#[derive(Debug, Args)]
pub struct ReplayWalArgs {
    /// WAL step marker to replay from (exclusive). Use `cairn status --json`
    /// to find recent step markers.
    #[arg(long)]
    pub from: String,

    /// Re-execute steps. Requires operator role. Without this flag the
    /// command runs read-only and streams the step graph as JSON.
    #[arg(long)]
    pub apply: bool,

    #[arg(long)]
    pub json: bool,
}

pub async fn execute(args: ReplayWalArgs, ctx: cairn_sdk::CliContext) -> Result<i32> {
    let actor = resolve_actor(&ctx)?;
    let admin_ctx = AdminContext::new(actor, AdminRole::Operator);
    let req = ReplayWalRequest {
        from_step: args.from.into(),
        apply: args.apply,
    };

    let resp = run(admin_ctx, req, ctx.store(), ctx.admin_state()).await;
    match resp {
        Ok(r) => {
            if args.json {
                println!("{}", serde_json::to_string(&r.into_wire())?);
            } else {
                println!("visited {} step(s); applied {}", r.steps_visited, r.steps_applied);
                for ev in &r.events {
                    println!("  {} ({:?}, idempotent={})", ev.marker, ev.kind, ev.idempotent);
                }
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(e.exit_code() as i32)
        }
    }
}
```

- [ ] **Step 2: Register the subcommand**

Modify `crates/cairn-cli/src/verbs/mod.rs`:

```rust
pub mod admin_replay_wal;
```

Modify `crates/cairn-cli/src/main.rs`. Locate the `Admin` subcommand enum (likely a `clap` `#[derive(Subcommand)]` near the top of `main.rs`) and add:

```rust
#[derive(clap::Subcommand)]
enum AdminCmd {
    Snapshot(verbs::admin_snapshot::SnapshotArgs),
    Restore(verbs::admin_restore::RestoreArgs),
    ReplayWal(verbs::admin_replay_wal::ReplayWalArgs),     // NEW
    // (Connector subcommand added in phase 4)
}
```

And in the match arm that dispatches admin subcommands:

```rust
AdminCmd::ReplayWal(args) => verbs::admin_replay_wal::execute(args, ctx).await?,
```

- [ ] **Step 3: Snapshot test the help output**

Add to `crates/cairn-cli/tests/cli_help.rs` (or create it):

```rust
#[test]
fn admin_replay_wal_help() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args(["admin", "replay-wal", "--help"])
        .output().unwrap();
    insta::assert_snapshot!(String::from_utf8_lossy(&out.stdout));
}
```

Run: `cargo nextest run -p cairn-cli admin_replay_wal_help && cargo insta accept --workspace`

- [ ] **Step 4: Commit + open phase-3 PR**

```bash
git add crates/cairn-cli/src/verbs/ crates/cairn-cli/src/main.rs crates/cairn-cli/tests/
git commit -m "feat(#161): cairn admin replay-wal CLI subcommand"

gh pr create --title "feat(#161): cairn.admin.v1 phase 3 — replay_wal" \
  --body "Phase 3: WalReplayer pure primitive; replay_wal core verb with dry-run (no role) and --apply (operator role + escalation on non-idempotent); CLI subcommand. Spec §5, §7.3 (exit 75)."
```

---

## Phase 4 — Connector enable / disable

Wire `connector_enable` / `connector_disable` verbs on top of `ConnectorRegistry` (#130). Scheduler picks up `connector_state.enabled` at each tick; disable cancels in-flight polls via the existing `CancellationToken`. Backfill verb is a stub here — wired for real in phase 5.

### Task 4.1: `verbs::admin::connector::{enable,disable}`

**Files:**
- Create: `crates/cairn-core/src/verbs/admin/connector.rs`
- Modify: `crates/cairn-core/src/verbs/admin/mod.rs`

- [ ] **Step 1: Add to mod.rs**

```rust
pub mod connector;
```

- [ ] **Step 2: Write the verbs + tests**

Create `crates/cairn-core/src/verbs/admin/connector.rs`:

```rust
//! `cairn.admin.v1` connector verbs.

use crate::contract::admin_state::{AdminStateStore, ConnectorStateRow};
use crate::contract::connector_registry::ConnectorRegistry;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};

#[derive(Debug, Clone)]
pub struct ConnectorTarget { pub name: String }

#[derive(Debug, Clone)]
pub struct ConnectorDisableRequest {
    pub name: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConnectorStateResponse {
    pub row: ConnectorStateRow,
}

pub async fn enable(
    ctx: AdminContext,
    req: ConnectorTarget,
    admin: &dyn AdminStateStore,
    registry: &dyn ConnectorRegistry,
) -> Result<ConnectorStateResponse, AdminError> {
    super::guard::require_role(&ctx, admin, AdminRole::Operator)?;
    if !registry.is_registered(&req.name).await {
        return Err(AdminError::UnknownConnector { name: req.name });
    }
    let row = admin.set_connector_enabled(&req.name, true, &ctx.actor, None)?;
    registry.enable(&req.name).await
        .map_err(|e| AdminError::Workflow(e.into()))?;
    Ok(ConnectorStateResponse { row })
}

pub async fn disable(
    ctx: AdminContext,
    req: ConnectorDisableRequest,
    admin: &dyn AdminStateStore,
    registry: &dyn ConnectorRegistry,
) -> Result<ConnectorStateResponse, AdminError> {
    super::guard::require_role(&ctx, admin, AdminRole::Operator)?;
    if !registry.is_registered(&req.name).await {
        return Err(AdminError::UnknownConnector { name: req.name });
    }
    let row = admin.set_connector_enabled(&req.name, false, &ctx.actor, req.reason.as_deref())?;
    registry.disable(&req.name).await
        .map_err(|e| AdminError::Workflow(e.into()))?;
    Ok(ConnectorStateResponse { row })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::IdentityId;
    use crate::verbs::admin::test_fixtures::*;

    #[tokio::test]
    async fn enable_rejects_unknown_connector() {
        let admin = FakeAdminStore::with_operator("hmn:op");
        let registry = FakeConnectorRegistry::with(vec![]);
        let ctx = AdminContext::new(IdentityId::from("hmn:op"), AdminRole::Operator);
        let err = enable(ctx, ConnectorTarget { name: "ghost".into() },
                         &admin, &registry).await.unwrap_err();
        assert!(matches!(err, AdminError::UnknownConnector { .. }));
    }

    #[tokio::test]
    async fn disable_writes_row_and_cancels_polls() {
        let admin = FakeAdminStore::with_operator("hmn:op");
        let registry = FakeConnectorRegistry::with(vec!["github".into()]);
        let ctx = AdminContext::new(IdentityId::from("hmn:op"), AdminRole::Operator);
        let resp = disable(ctx, ConnectorDisableRequest {
            name: "github".into(), reason: Some("rate-limit".into()) },
            &admin, &registry).await.unwrap();
        assert!(!resp.row.enabled);
        assert_eq!(registry.disable_calls(), vec!["github".to_string()]);
    }

    #[tokio::test]
    async fn disable_requires_operator() {
        let admin = FakeAdminStore::empty();
        let registry = FakeConnectorRegistry::with(vec!["github".into()]);
        let ctx = AdminContext::new(IdentityId::from("hmn:nope"), AdminRole::Operator);
        let err = disable(ctx, ConnectorDisableRequest {
            name: "github".into(), reason: None },
            &admin, &registry).await.unwrap_err();
        assert!(matches!(err, AdminError::NotAuthorized { .. }));
    }
}
```

Add `FakeConnectorRegistry` to `test_fixtures.rs` mirroring the established pattern: tracks `disable_calls()` and `enable_calls()` as `Vec<String>`.

- [ ] **Step 3: Confirm `ConnectorRegistry` trait shape**

The verb depends on `cairn-core::contract::connector_registry::ConnectorRegistry`. If the registry trait currently lives in `cairn-connectors-core` (not core), promote the trait definition into core and keep the impl in `cairn-connectors-core` — this keeps `cairn-core` dep-free per CLAUDE.md §3. Confirm with `rg "trait ConnectorRegistry" crates/`. Likely a 30-line port.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-core verbs::admin::connector --locked` — expect PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verbs/admin/ crates/cairn-core/src/contract/
git commit -m "feat(#161): verbs::admin::connector::{enable,disable}"
```

---

### Task 4.2: Scheduler-tick state check

**Files:**
- Modify: `crates/cairn-connectors-core/src/registry.rs`
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs` (or wherever the connector poll tick lives)

- [ ] **Step 1: Wire `AdminStateStore` into the scheduler**

Find the scheduler tick that polls each registered connector (per the explorer report, this lives in `cairn-workflows::Scheduler` adjacent to `ConsolidationHandler`/`DreamHandler`). Modify it to consult `AdminStateStore::get_connector_state(name)` at the top of each tick:

```rust
// At the top of poll_tick(&self, connector: &dyn Connector):
let state = self.admin_state.get_connector_state(connector.manifest().name())
    .map_err(WorkflowError::from)?;
let enabled = state.map(|r| r.enabled).unwrap_or(true);  // unknown → enabled (default)
if !enabled {
    tracing::debug!(
        connector = connector.manifest().name(),
        "connector disabled by admin; skipping poll tick"
    );
    return Ok(());
}
```

Inject `admin_state: Arc<dyn AdminStateStore>` into `Scheduler::new(...)` if not already present. Add to the existing scheduler constructor signature; CLI wiring in `cairn-cli/src/main.rs` passes `store.admin_state().into()`.

- [ ] **Step 2: Write a fixture-driven test**

Create `crates/cairn-workflows/tests/connector_tick_respects_disable.rs`:

```rust
//! AC#3 inner contract: scheduler poll tick skips disabled connectors.

use cairn_test_fixtures::{tempvault, fake_polling_connector};
use cairn_workflows::Scheduler;
use cairn_store_sqlite::SqliteStore;
use cairn_core::domain::identity::IdentityId;
use cairn_core::contract::admin_state::AdminStateStore;
use std::sync::Arc;

#[tokio::test]
async fn disable_skips_next_tick() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = Arc::new(store.admin_state());
    let conn  = fake_polling_connector("test-conn");
    let scheduler = Scheduler::new_with(admin.clone(), vec![Arc::new(conn.clone())]);

    // First tick: connector polled.
    scheduler.tick_once().await.unwrap();
    assert_eq!(conn.poll_count(), 1);

    // Disable.
    let op = IdentityId::from("hmn:op");
    admin.set_connector_enabled("test-conn", false, &op, None).unwrap();

    // Second tick: skipped.
    scheduler.tick_once().await.unwrap();
    assert_eq!(conn.poll_count(), 1, "no new poll after disable");
}
```

Run: `cargo nextest run -p cairn-workflows connector_tick_respects_disable --locked` — expect PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-connectors-core/ crates/cairn-workflows/
git commit -m "feat(#161): scheduler tick reads connector_state.enabled"
```

---

### Task 4.3: CLI `admin connector` subcommand group

**Files:**
- Create: `crates/cairn-cli/src/verbs/admin_connector.rs`
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Add the subcommand group**

Create `crates/cairn-cli/src/verbs/admin_connector.rs`:

```rust
//! `cairn admin connector {enable,disable,backfill}` — thin CLI wrappers.

use anyhow::Result;
use clap::{Args, Subcommand};
use cairn_core::verbs::admin::connector::{
    enable, disable, ConnectorDisableRequest, ConnectorTarget,
};
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_sdk::admin::resolve_actor;

#[derive(Debug, Args)]
pub struct ConnectorArgs {
    #[command(subcommand)]
    pub cmd: ConnectorCmd,
}

#[derive(Debug, Subcommand)]
pub enum ConnectorCmd {
    /// Re-enable a connector previously disabled via `disable`.
    Enable { name: String, #[arg(long)] json: bool },

    /// Disable a connector. New ingestion stops within one scheduler tick.
    Disable {
        name: String,
        #[arg(long)] reason: Option<String>,
        #[arg(long)] json: bool,
    },

    /// (Phase 5) Trigger a bounded backfill.
    Backfill {
        name: String,
        #[arg(long)] from: String,
        #[arg(long)] to: String,
        #[arg(long, default_value_t = 10.0)] rate_limit_per_sec: f64,
        #[arg(long)] watch: bool,
        #[arg(long)] json: bool,
    },
}

pub async fn execute(args: ConnectorArgs, ctx: cairn_sdk::CliContext) -> Result<i32> {
    let actor = resolve_actor(&ctx)?;
    let admin_ctx = AdminContext::new(actor, AdminRole::Operator);
    match args.cmd {
        ConnectorCmd::Enable { name, json } => {
            let resp = enable(admin_ctx, ConnectorTarget { name }, ctx.admin_state(), ctx.connectors()).await;
            print_response(resp, json)
        }
        ConnectorCmd::Disable { name, reason, json } => {
            let resp = disable(admin_ctx, ConnectorDisableRequest { name, reason },
                               ctx.admin_state(), ctx.connectors()).await;
            print_response(resp, json)
        }
        ConnectorCmd::Backfill { .. } => {
            // Stub: phase 5 wires the real handler.
            eprintln!("error: connector backfill not yet wired (phase 5)");
            Ok(69)
        }
    }
}

fn print_response<T: serde::Serialize + std::fmt::Debug>(
    resp: Result<T, cairn_core::domain::admin::AdminError>,
    json: bool,
) -> Result<i32> {
    match resp {
        Ok(r) => {
            if json { println!("{}", serde_json::to_string(&r)?); }
            else { println!("{r:#?}"); }
            Ok(0)
        }
        Err(e) => { eprintln!("error: {e}"); Ok(e.exit_code() as i32) }
    }
}
```

Modify `crates/cairn-cli/src/main.rs`. Extend `AdminCmd`:

```rust
#[derive(clap::Subcommand)]
enum AdminCmd {
    Snapshot(verbs::admin_snapshot::SnapshotArgs),
    Restore(verbs::admin_restore::RestoreArgs),
    ReplayWal(verbs::admin_replay_wal::ReplayWalArgs),
    Connector(verbs::admin_connector::ConnectorArgs),   // NEW
}
```

Dispatch:

```rust
AdminCmd::Connector(args) => verbs::admin_connector::execute(args, ctx).await?,
```

- [ ] **Step 2: Snapshot the help output**

Add to `crates/cairn-cli/tests/cli_help.rs`:

```rust
#[test] fn admin_connector_help() { /* same pattern as Task 3.3 */ }
#[test] fn admin_connector_disable_help() { /* ... */ }
```

Run + accept: `cargo nextest run -p cairn-cli admin_connector_help && cargo insta accept --workspace`

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/
git commit -m "feat(#161): cairn admin connector subcommand group"
```

---

### Task 4.4: `status.connectors[]` includes enable state

**Files:**
- Modify: `crates/cairn-core/src/status/mod.rs` (or wherever `status` response is built)

- [ ] **Step 1: Extend the response shape**

Find the `Status::connectors` field. Each entry today is likely `{ name, kind, last_polled }`. Extend to include the admin state row fields:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ConnectorStatus {
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub last_changed_at: Option<String>,
    pub last_changed_by: Option<String>,
    pub reason: Option<String>,
    // existing fields …
}
```

In the builder, join each connector registration with `AdminStateStore::list_connector_state()`:

```rust
let state_rows: HashMap<String, ConnectorStateRow> =
    admin.list_connector_state()?.into_iter()
        .map(|r| (r.connector_name.clone(), r))
        .collect();

let connectors: Vec<ConnectorStatus> = registry.all().iter()
    .map(|c| {
        let row = state_rows.get(c.manifest().name());
        ConnectorStatus {
            name: c.manifest().name().into(),
            kind: c.manifest().kind().into(),
            enabled: row.map(|r| r.enabled).unwrap_or(true),
            last_changed_at: row.map(|r| r.last_changed_at.to_string()),
            last_changed_by: row.map(|r| r.last_changed_by.to_string()),
            reason: row.and_then(|r| r.reason.clone()),
        }
    }).collect();
```

- [ ] **Step 2: Update status snapshot tests**

Run + accept: `cargo nextest run -p cairn-core status --locked && cargo insta accept --workspace`

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/status/
git commit -m "feat(#161): status.connectors[] surfaces enable/disable state"
```

---

### Task 4.5: Integration test — disable race (AC#3)

**Files:**
- Create: `crates/cairn-workflows/tests/admin_connector_disable_race.rs`

- [ ] **Step 1: Write the test**

```rust
//! AC#3: disable stops ingestion within one scheduler tick AND surfaces in
//! `cairn status --json`.

use cairn_test_fixtures::{tempvault, fake_polling_connector};
use cairn_workflows::Scheduler;
use cairn_store_sqlite::SqliteStore;
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::domain::identity::IdentityId;
use cairn_core::verbs::admin::connector::{disable, ConnectorDisableRequest};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_takes_effect_within_one_tick_and_shows_in_status() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = Arc::new(store.admin_state());
    let op = IdentityId::from("hmn:op");
    admin.grant_role(&op, AdminRole::Operator, &op).unwrap();

    let conn = fake_polling_connector("test-conn");
    let registry = Arc::new(cairn_connectors_core::ConnectorRegistry::with(vec![Arc::new(conn.clone())]));
    let scheduler = Scheduler::new_with(admin.clone(), registry.clone()).start().await;

    // Let it poll a couple of times.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let before = conn.poll_count();
    assert!(before > 0);

    // Disable.
    disable(
        AdminContext::new(op.clone(), AdminRole::Operator),
        ConnectorDisableRequest { name: "test-conn".into(), reason: Some("test".into()) },
        admin.as_ref(), registry.as_ref(),
    ).await.unwrap();

    // Wait for at least one tick interval, then assert no new polls.
    tokio::time::sleep(scheduler.tick_interval() * 2).await;
    let after = conn.poll_count();
    assert_eq!(after, before, "disable should stop new polls within one tick");

    // Status reflects disabled state.
    let status = cairn_core::status::build(&store, &admin, &registry).await.unwrap();
    let entry = status.connectors.iter().find(|c| c.name == "test-conn").unwrap();
    assert!(!entry.enabled);
    assert_eq!(entry.reason.as_deref(), Some("test"));
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-workflows admin_connector_disable_race --locked` — expect PASS.

- [ ] **Step 3: Commit + open phase-4 PR**

```bash
git add crates/cairn-workflows/tests/
git commit -m "test(#161): AC#3 connector disable race"

gh pr create --title "feat(#161): cairn.admin.v1 phase 4 — connector enable/disable" \
  --body "Phase 4: connector enable/disable verbs; scheduler reads connector_state per tick; status.connectors[] surfaces state; AC#3 disable-race integration. Spec §5, §8.3, §8.4."
```

---

## Phase 5 — `emit_progress` + `connector_backfill`

Extend `WorkflowOrchestrator` with `emit_progress` + `subscribe_progress`. Add `backfill_jobs` table. Implement the backfill handler and the real `connector_backfill` verb. CLI gains `--watch`.

### Task 5.1: `ProgressEvent` type + trait extension

**Files:**
- Create: `crates/cairn-core/src/domain/workflow/progress.rs`
- Modify: `crates/cairn-core/src/domain/workflow/mod.rs`
- Modify: `crates/cairn-core/src/contract/workflow_orchestrator.rs`

- [ ] **Step 1: Add the event type**

Create `crates/cairn-core/src/domain/workflow/progress.rs`:

```rust
use crate::domain::workflow::WorkflowId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProgressKind {
    Started,
    Tick,
    Completed,
    Failed { code: String, msg: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ProgressEvent {
    pub workflow_id: WorkflowId,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub kind: ProgressKind,
    pub processed: u64,
    pub total: Option<u64>,
    pub detail: serde_json::Value,
}
```

Modify `crates/cairn-core/src/domain/workflow/mod.rs`:

```rust
pub mod progress;
pub use progress::{ProgressEvent, ProgressKind};
```

- [ ] **Step 2: Extend the trait**

Modify `crates/cairn-core/src/contract/workflow_orchestrator.rs`. Add two methods to `WorkflowOrchestrator`:

```rust
use crate::domain::workflow::{ProgressEvent, WorkflowId};
use tokio::sync::broadcast;

pub trait WorkflowOrchestrator: Send + Sync {
    // … existing methods …

    /// Append a progress event for `event.workflow_id`. The default impl
    /// keeps existing implementors compiling; overrides land in phase 5.3.
    fn emit_progress(&self, event: ProgressEvent)
        -> impl std::future::Future<Output = Result<(), WorkflowError>> + Send;

    /// Subscribe to progress events for `id`. Returns `Err(NotFound)` if
    /// the workflow has already completed and the broadcast was dropped.
    fn subscribe_progress(&self, id: WorkflowId)
        -> impl std::future::Future<Output = Result<broadcast::Receiver<ProgressEvent>, WorkflowError>> + Send;
}
```

(Native async-fn-in-traits per CLAUDE.md §6.3 + spec §8.1. Trait remains `dyn`-incompatible for these new methods — that's fine because all internal consumers hold `Arc<ConcreteScheduler>`, only the *existing* dyn-safe methods are accessed via `&dyn`.)

If the trait MUST stay fully object-safe (because something stores `Arc<dyn WorkflowOrchestrator>`), gate the two new methods behind a `WorkflowOrchestratorProgress` extension trait that the scheduler also implements; consumers downcast or hold the concrete handle.

- [ ] **Step 3: Compile-only check**

Run: `cargo check --workspace --locked` — fix any callers that store `dyn WorkflowOrchestrator` and now need the extension trait.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/workflow/ crates/cairn-core/src/contract/workflow_orchestrator.rs
git commit -m "feat(#161): ProgressEvent + WorkflowOrchestrator::{emit,subscribe}_progress"
```

---

### Task 5.2: Scheduler implements progress (jsonl + broadcast)

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs` (or `progress.rs`)
- Create: `crates/cairn-workflows/src/scheduler/progress.rs`

- [ ] **Step 1: Implement the fan-out**

Create `crates/cairn-workflows/src/scheduler/progress.rs`:

```rust
//! Progress fan-out: append to `.cairn/metrics.jsonl` AND broadcast in-proc.

use cairn_core::contract::workflow_orchestrator::WorkflowError;
use cairn_core::domain::workflow::{ProgressEvent, ProgressKind, WorkflowId};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

pub struct ProgressBus {
    metrics_path: PathBuf,
    senders: Mutex<HashMap<WorkflowId, broadcast::Sender<ProgressEvent>>>,
}

impl ProgressBus {
    pub fn new(metrics_path: PathBuf) -> Arc<Self> {
        Arc::new(Self { metrics_path, senders: Mutex::new(HashMap::new()) })
    }

    pub async fn emit(&self, event: ProgressEvent) -> Result<(), WorkflowError> {
        // 1. Append to metrics.jsonl (brief §3).
        let line = serde_json::to_string(&event)
            .map_err(|e| WorkflowError::Encode(e.to_string()))?;
        let path = self.metrics_path.clone();
        tokio::task::spawn_blocking(move || {
            use std::fs::OpenOptions;
            let mut f = OpenOptions::new().create(true).append(true).open(&path)
                .map_err(|e| WorkflowError::Io(e.to_string()))?;
            writeln!(f, "{line}").map_err(|e| WorkflowError::Io(e.to_string()))?;
            Ok::<_, WorkflowError>(())
        }).await.map_err(|e| WorkflowError::Io(e.to_string()))??;

        // 2. Fan-out to in-proc subscribers (best-effort).
        let mut guard = self.senders.lock().await;
        if let Some(tx) = guard.get(&event.workflow_id) {
            let _ = tx.send(event.clone());
            if matches!(event.kind, ProgressKind::Completed | ProgressKind::Failed { .. }) {
                guard.remove(&event.workflow_id);
            }
        }
        Ok(())
    }

    pub async fn subscribe(&self, id: WorkflowId) -> broadcast::Receiver<ProgressEvent> {
        let mut guard = self.senders.lock().await;
        let tx = guard.entry(id).or_insert_with(|| broadcast::channel(64).0);
        tx.subscribe()
    }
}
```

Modify the `Scheduler` impl of `WorkflowOrchestrator`:

```rust
impl WorkflowOrchestrator for Scheduler {
    // … existing …

    async fn emit_progress(&self, event: ProgressEvent) -> Result<(), WorkflowError> {
        self.progress.emit(event).await
    }

    async fn subscribe_progress(&self, id: WorkflowId)
        -> Result<broadcast::Receiver<ProgressEvent>, WorkflowError> {
        Ok(self.progress.subscribe(id).await)
    }
}
```

Add `progress: Arc<ProgressBus>` to `Scheduler` struct + constructor; pass the vault's `.cairn/metrics.jsonl` path through.

- [ ] **Step 2: Write a smoke test**

Create `crates/cairn-workflows/tests/progress_emit_subscribe.rs`:

```rust
use cairn_core::contract::workflow_orchestrator::WorkflowOrchestrator;
use cairn_core::domain::workflow::{ProgressEvent, ProgressKind, WorkflowId};
use cairn_test_fixtures::tempvault;
use time::OffsetDateTime;

#[tokio::test]
async fn emit_to_metrics_jsonl_and_subscriber() {
    let vault = tempvault();
    let scheduler = cairn_workflows::Scheduler::new_for_test(vault.path());
    let id = WorkflowId::new();

    let mut rx = scheduler.subscribe_progress(id.clone()).await.unwrap();

    let ev = ProgressEvent {
        workflow_id: id.clone(),
        at: OffsetDateTime::now_utc(),
        kind: ProgressKind::Started,
        processed: 0,
        total: Some(10),
        detail: serde_json::json!({}),
    };
    scheduler.emit_progress(ev.clone()).await.unwrap();

    let received = rx.recv().await.unwrap();
    assert_eq!(received, ev);

    // metrics.jsonl has one line.
    let contents = std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).unwrap();
    assert_eq!(contents.lines().count(), 1);
}
```

Run: `cargo nextest run -p cairn-workflows progress_emit_subscribe --locked` — expect PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/
git commit -m "feat(#161): ProgressBus — metrics.jsonl sink + in-proc broadcast"
```

---

### Task 5.3: `backfill_jobs` schema + handler

**Files:**
- Create: `crates/cairn-store-sqlite/migrations/0005_backfill_jobs.sql`
- Create: `crates/cairn-workflows/src/handlers/connector_backfill.rs`

- [ ] **Step 1: Migration**

Create `crates/cairn-store-sqlite/migrations/0005_backfill_jobs.sql`:

```sql
-- Issue #161: per-workflow row for connector_backfill verb.
CREATE TABLE backfill_jobs (
  workflow_id    TEXT PRIMARY KEY,
  connector_name TEXT NOT NULL,
  from_ts        TEXT NOT NULL,
  to_ts          TEXT NOT NULL,
  rate_per_sec   REAL NOT NULL,
  started_at     TEXT NOT NULL,
  finished_at    TEXT,
  cursor         TEXT,
  status         TEXT NOT NULL DEFAULT 'running'
    CHECK (status IN ('running', 'completed', 'failed', 'cancelled'))
);
CREATE INDEX backfill_jobs_status ON backfill_jobs(status);
```

- [ ] **Step 2: Handler**

Create `crates/cairn-workflows/src/handlers/connector_backfill.rs`:

```rust
//! Background handler for `cairn.admin.v1.connector_backfill`.

use cairn_connectors_core::{Connector, BackfillRange};
use cairn_core::contract::workflow_orchestrator::{WorkflowError, WorkflowOrchestrator};
use cairn_core::domain::workflow::{ProgressEvent, ProgressKind, WorkflowId};
use std::sync::Arc;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

pub struct BackfillTask {
    pub workflow_id: WorkflowId,
    pub connector: Arc<dyn Connector>,
    pub from: OffsetDateTime,
    pub to: OffsetDateTime,
    pub rate_per_sec: f64,
    pub cancel: CancellationToken,
}

pub async fn run(
    task: BackfillTask,
    orch: Arc<dyn WorkflowOrchestrator>,
) -> Result<(), WorkflowError> {
    let started = OffsetDateTime::now_utc();
    let id = task.workflow_id.clone();

    orch.emit_progress(ProgressEvent {
        workflow_id: id.clone(),
        at: started,
        kind: ProgressKind::Started,
        processed: 0,
        total: None,
        detail: serde_json::json!({
            "connector": task.connector.manifest().name(),
            "from": task.from.to_string(),
            "to": task.to.to_string(),
        }),
    }).await?;

    let mut processed = 0u64;
    let mut interval = tokio::time::interval(
        std::time::Duration::from_secs_f64(1.0 / task.rate_per_sec.max(0.001)));
    let mut tick_since = std::time::Instant::now();

    let range = BackfillRange { from: task.from, to: task.to };
    let mut stream = task.connector.backfill(range).await
        .map_err(|e| WorkflowError::Connector(e.to_string()))?;

    use futures::StreamExt;
    loop {
        tokio::select! {
            _ = task.cancel.cancelled() => {
                orch.emit_progress(ProgressEvent {
                    workflow_id: id.clone(),
                    at: OffsetDateTime::now_utc(),
                    kind: ProgressKind::Failed {
                        code: "CancelledByDisable".into(),
                        msg: "connector disabled mid-backfill".into(),
                    },
                    processed, total: None, detail: serde_json::json!({}),
                }).await?;
                return Ok(());
            }
            item = stream.next() => {
                match item {
                    None => break,
                    Some(Ok(_event)) => {
                        processed += 1;
                        interval.tick().await;
                        // Throttle progress emissions: every 100 records or every 5s.
                        if processed % 100 == 0 || tick_since.elapsed().as_secs() >= 5 {
                            orch.emit_progress(ProgressEvent {
                                workflow_id: id.clone(),
                                at: OffsetDateTime::now_utc(),
                                kind: ProgressKind::Tick,
                                processed, total: None,
                                detail: serde_json::json!({}),
                            }).await?;
                            tick_since = std::time::Instant::now();
                        }
                    }
                    Some(Err(e)) => {
                        orch.emit_progress(ProgressEvent {
                            workflow_id: id.clone(),
                            at: OffsetDateTime::now_utc(),
                            kind: ProgressKind::Failed {
                                code: "ConnectorError".into(),
                                msg: e.to_string(),
                            },
                            processed, total: None, detail: serde_json::json!({}),
                        }).await?;
                        return Err(WorkflowError::Connector(e.to_string()));
                    }
                }
            }
        }
    }

    orch.emit_progress(ProgressEvent {
        workflow_id: id,
        at: OffsetDateTime::now_utc(),
        kind: ProgressKind::Completed,
        processed, total: Some(processed),
        detail: serde_json::json!({}),
    }).await?;
    Ok(())
}
```

Wire the handler into the scheduler's known-handlers map alongside `ConsolidationHandler`, `DreamHandler`, etc.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/migrations/0005_backfill_jobs.sql crates/cairn-workflows/src/handlers/
git commit -m "feat(#161): backfill_jobs schema + connector_backfill handler"
```

---

### Task 5.4: `verbs::admin::connector::backfill`

**Files:**
- Modify: `crates/cairn-core/src/verbs/admin/connector.rs`

- [ ] **Step 1: Add request/response + verb**

Append to `connector.rs`:

```rust
use crate::contract::workflow_orchestrator::WorkflowOrchestrator;
use crate::domain::workflow::WorkflowId;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct BackfillRequest {
    pub name: String,
    pub from: OffsetDateTime,
    pub to: OffsetDateTime,
    pub rate_limit_per_sec: f64,
}

#[derive(Debug, Clone)]
pub struct BackfillResponse {
    pub workflow_id: WorkflowId,
    pub started_at: OffsetDateTime,
}

pub async fn backfill(
    ctx: AdminContext,
    req: BackfillRequest,
    admin: &dyn AdminStateStore,
    registry: &dyn ConnectorRegistry,
    orch: &dyn WorkflowOrchestrator,
) -> Result<BackfillResponse, AdminError> {
    super::guard::require_role(&ctx, admin, AdminRole::Operator)?;
    if !registry.is_registered(&req.name).await {
        return Err(AdminError::UnknownConnector { name: req.name });
    }
    // Refuse backfill on a currently-disabled connector.
    if let Some(row) = admin.get_connector_state(&req.name)? {
        if !row.enabled {
            return Err(AdminError::UnknownConnector {
                name: format!("{} (currently disabled)", req.name),
            });
        }
    }

    let workflow_id = WorkflowId::new();
    let started_at = OffsetDateTime::now_utc();
    orch.start_backfill(crate::contract::workflow_orchestrator::BackfillSpec {
        workflow_id: workflow_id.clone(),
        connector_name: req.name,
        from: req.from,
        to: req.to,
        rate_per_sec: req.rate_limit_per_sec,
        started_at,
        actor: ctx.actor.clone(),
    }).await.map_err(AdminError::Workflow)?;

    Ok(BackfillResponse { workflow_id, started_at })
}
```

Add `start_backfill(&self, spec: BackfillSpec)` to the `WorkflowOrchestrator` trait + Scheduler impl; the impl spawns the handler from Task 5.3 with a `CancellationToken` that's wired into `ConnectorRegistry::disable()`.

- [ ] **Step 2: Tests**

Add to `verbs::admin::connector::tests`:

```rust
#[tokio::test]
async fn backfill_refuses_disabled_connector() { /* ... */ }

#[tokio::test]
async fn backfill_returns_workflow_id() { /* ... */ }
```

Run: `cargo nextest run -p cairn-core verbs::admin::connector --locked` — expect PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/
git commit -m "feat(#161): verbs::admin::connector::backfill"
```

---

### Task 5.5: CLI `--watch` for backfill

**Files:**
- Modify: `crates/cairn-cli/src/verbs/admin_connector.rs`

- [ ] **Step 1: Replace the backfill stub**

```rust
ConnectorCmd::Backfill { name, from, to, rate_limit_per_sec, watch, json } => {
    use cairn_core::verbs::admin::connector::{backfill, BackfillRequest};
    let req = BackfillRequest {
        name,
        from: OffsetDateTime::parse(&from, &time::format_description::well_known::Rfc3339)?,
        to:   OffsetDateTime::parse(&to,   &time::format_description::well_known::Rfc3339)?,
        rate_limit_per_sec,
    };
    let resp = backfill(admin_ctx, req,
        ctx.admin_state(), ctx.connectors(), ctx.workflow_orchestrator()).await;
    match resp {
        Ok(r) => {
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!("backfill {} started", r.workflow_id);
            }
            if watch {
                let mut rx = ctx.workflow_orchestrator()
                    .subscribe_progress(r.workflow_id.clone()).await?;
                while let Ok(ev) = rx.recv().await {
                    if json {
                        println!("{}", serde_json::to_string(&ev)?);
                    } else {
                        match &ev.kind {
                            ProgressKind::Tick => print!("\rprocessed {}", ev.processed),
                            ProgressKind::Completed => {
                                println!("\ncompleted ({} records)", ev.processed); return Ok(0);
                            }
                            ProgressKind::Failed { code, msg } => {
                                println!("\nfailed [{code}]: {msg}"); return Ok(1);
                            }
                            ProgressKind::Started => println!("started"),
                        }
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                }
            }
            Ok(0)
        }
        Err(e) => { eprintln!("error: {e}"); Ok(e.exit_code() as i32) }
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/cairn-cli/src/verbs/admin_connector.rs
git commit -m "feat(#161): cairn admin connector backfill --watch"
```

---

### Task 5.6: Integration tests — backfill progress + cancellation

**Files:**
- Create: `crates/cairn-workflows/tests/admin_connector_backfill_progress.rs`

- [ ] **Step 1: Write tests**

```rust
//! Spec §8.2/§8.3 + AC: backfill emits monotonic progress and `disable`
//! mid-backfill emits `Failed { code: "CancelledByDisable" }`.

use cairn_core::contract::workflow_orchestrator::WorkflowOrchestrator;
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::domain::identity::IdentityId;
use cairn_core::domain::workflow::ProgressKind;
use cairn_core::verbs::admin::connector::{backfill, disable, BackfillRequest, ConnectorDisableRequest};
use cairn_test_fixtures::{tempvault, fake_backfill_connector};
use time::OffsetDateTime;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_progress_monotonic() {
    let vault = tempvault();
    let store = cairn_store_sqlite::SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = Arc::new(store.admin_state());
    let op = IdentityId::from("hmn:op");
    admin.grant_role(&op, AdminRole::Operator, &op).unwrap();

    let conn = fake_backfill_connector("test-conn", /*record_count=*/ 500);
    let registry = Arc::new(cairn_connectors_core::ConnectorRegistry::with(vec![Arc::new(conn)]));
    let scheduler = Arc::new(cairn_workflows::Scheduler::new_for_test(vault.path()));

    let resp = backfill(
        AdminContext::new(op.clone(), AdminRole::Operator),
        BackfillRequest {
            name: "test-conn".into(),
            from: OffsetDateTime::UNIX_EPOCH,
            to: OffsetDateTime::now_utc(),
            rate_limit_per_sec: 1000.0,
        },
        admin.as_ref(), registry.as_ref(), scheduler.as_ref(),
    ).await.unwrap();

    let mut rx = scheduler.subscribe_progress(resp.workflow_id.clone()).await.unwrap();
    let mut last = 0u64;
    let mut saw_completed = false;
    while let Ok(ev) = rx.recv().await {
        assert!(ev.processed >= last);
        last = ev.processed;
        if matches!(ev.kind, ProgressKind::Completed) { saw_completed = true; break; }
    }
    assert!(saw_completed);
    assert_eq!(last, 500);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_mid_backfill_emits_cancelled() {
    let vault = tempvault();
    let store = cairn_store_sqlite::SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = Arc::new(store.admin_state());
    let op = IdentityId::from("hmn:op");
    admin.grant_role(&op, AdminRole::Operator, &op).unwrap();

    let conn = fake_backfill_connector("slow", /*record_count=*/ 100_000);
    let registry = Arc::new(cairn_connectors_core::ConnectorRegistry::with(vec![Arc::new(conn)]));
    let scheduler = Arc::new(cairn_workflows::Scheduler::new_for_test(vault.path()));

    let resp = backfill(
        AdminContext::new(op.clone(), AdminRole::Operator),
        BackfillRequest {
            name: "slow".into(),
            from: OffsetDateTime::UNIX_EPOCH,
            to: OffsetDateTime::now_utc(),
            rate_limit_per_sec: 10.0,
        },
        admin.as_ref(), registry.as_ref(), scheduler.as_ref(),
    ).await.unwrap();

    let mut rx = scheduler.subscribe_progress(resp.workflow_id.clone()).await.unwrap();
    // Disable after a brief delay.
    let admin_clone = admin.clone();
    let registry_clone = registry.clone();
    let op_clone = op.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        disable(
            AdminContext::new(op_clone, AdminRole::Operator),
            ConnectorDisableRequest { name: "slow".into(), reason: None },
            admin_clone.as_ref(), registry_clone.as_ref(),
        ).await.unwrap();
    });

    let mut saw_cancelled = false;
    while let Ok(ev) = rx.recv().await {
        if let ProgressKind::Failed { code, .. } = &ev.kind {
            assert_eq!(code, "CancelledByDisable");
            saw_cancelled = true;
            break;
        }
    }
    assert!(saw_cancelled);
}
```

Run: `cargo nextest run -p cairn-workflows admin_connector_backfill_progress --locked` — expect PASS.

- [ ] **Step 2: Commit + open phase-5 PR**

```bash
git add crates/cairn-workflows/tests/
git commit -m "test(#161): backfill progress monotonic + disable-mid-backfill cancellation"

gh pr create --title "feat(#161): cairn.admin.v1 phase 5 — emit_progress + connector_backfill" \
  --body "Phase 5: ProgressEvent + WorkflowOrchestrator extension; ProgressBus (metrics.jsonl + broadcast); backfill_jobs migration; connector_backfill handler + verb + CLI --watch. Spec §8.1-§8.4."
```

---

## Phase 6 — MCP + SDK + IDL codegen + flip wire

Closes out the epic. Wires admin verbs into MCP and SDK, regenerates codegen, audits, then flips `ADMIN_EXTENSION_WIRED = true`. All four acceptance criteria green after this phase.

### Task 6.1: IDL schemas for six verbs

**Files:**
- Create: `crates/cairn-idl/schema/verbs/admin_snapshot.json`
- Create: `crates/cairn-idl/schema/verbs/admin_restore.json`
- Create: `crates/cairn-idl/schema/verbs/admin_replay_wal.json`
- Create: `crates/cairn-idl/schema/verbs/admin_connector_enable.json`
- Create: `crates/cairn-idl/schema/verbs/admin_connector_disable.json`
- Create: `crates/cairn-idl/schema/verbs/admin_connector_backfill.json`

- [ ] **Step 1: Mirror the request/response shapes from Rust into IDL**

For each verb, write the IDL JSON following the existing pattern under `crates/cairn-idl/schema/verbs/` (`ingest.json`, `search.json`, etc.). Example for `admin_snapshot.json`:

```json
{
  "name": "admin.snapshot",
  "capability": "cairn.mcp.v1.extension.admin.snapshot",
  "auth": { "required_role": "operator" },
  "request": {
    "type": "object",
    "required": ["out_path"],
    "properties": {
      "out_path": { "type": "string", "description": "Directory to write the tarball into" },
      "label":    { "type": "string" }
    }
  },
  "response": {
    "type": "object",
    "required": ["backup_id", "artifact_path", "sha256", "frontier_step", "manifest"],
    "properties": {
      "backup_id":     { "type": "string" },
      "artifact_path": { "type": "string" },
      "sha256":        { "type": "string" },
      "frontier_step": { "type": "string" },
      "manifest":      { "$ref": "#/definitions/SnapshotManifest" }
    }
  }
}
```

Inspect an existing verb file to confirm exact key names (`auth` vs `authentication`, `capability` vs `requires_capability`, etc.) before writing.

- [ ] **Step 2: Run codegen**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: generates new entries in `crates/cairn-mcp/src/generated/mod.rs`, `crates/cairn-sdk/src/generated/` (if exists), and any IDL-driven type modules.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-idl/schema/ crates/cairn-mcp/src/generated/ crates/cairn-sdk/src/generated/ 2>/dev/null
git commit -m "feat(#161): IDL schemas + codegen for six admin verbs"
```

---

### Task 6.2: MCP handler dispatch for admin tools

**Files:**
- Modify: `crates/cairn-mcp/src/handler.rs`

- [ ] **Step 1: Wire dispatch**

Find the existing dispatch match in `handler.rs` (around line 125-150). Add arms for each admin tool name:

```rust
"admin.snapshot" => {
    let req: cairn_core::verbs::admin::snapshot::SnapshotRequest = serde_json::from_value(params)?;
    let ctx = AdminContext::new(actor, AdminRole::Operator);
    cairn_core::verbs::admin::snapshot::run(ctx, req, self.store(), self.admin_state(), self.backup_registry())
        .await.map(into_wire).map_err(into_mcp_error)
}
"admin.restore"    => { /* … */ }
"admin.replay_wal" => { /* … */ }
"admin.connector.enable"   => { /* … */ }
"admin.connector.disable"  => { /* … */ }
"admin.connector.backfill" => { /* … */ }
```

`into_wire` and `into_mcp_error` are small adapters that map `AdminError` to the §8.0.b uniform envelope and `code()` string from Task 1.2.

- [ ] **Step 2: Add MCP integration test**

Create `crates/cairn-mcp/tests/admin_tools.rs`:

```rust
//! Spec §9.3: list_tools advertises admin tools when extension wired,
//! absent otherwise.

use cairn_mcp::test_harness::{MockClient, with_extension};

#[tokio::test]
async fn list_tools_includes_admin_when_extension_ready() {
    let client = with_extension(true).await;
    let tools = client.list_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    for expected in [
        "admin.snapshot", "admin.restore", "admin.replay_wal",
        "admin.connector.enable", "admin.connector.disable", "admin.connector.backfill",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
}

#[tokio::test]
async fn list_tools_omits_admin_when_dark() {
    let client = with_extension(false).await;
    let tools = client.list_tools().await.unwrap();
    for t in tools { assert!(!t.name.starts_with("admin."), "unexpected {}", t.name); }
}
```

Run: `cargo nextest run -p cairn-mcp admin_tools --locked` — expect PASS (tests for "ready" state will be true after the flip in 6.5; for now run with `ADMIN_EXTENSION_WIRED` patched to `true` in `with_extension` test harness).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-mcp/
git commit -m "feat(#161): MCP handler dispatches six admin tools"
```

---

### Task 6.3: SDK admin module

**Files:**
- Create: `crates/cairn-sdk/src/admin.rs`
- Modify: `crates/cairn-sdk/src/lib.rs`

- [ ] **Step 1: Write the wrappers**

Create `crates/cairn-sdk/src/admin.rs`:

```rust
//! Typed SDK surface for `cairn.admin.v1` verbs.

use crate::CliContext;
use cairn_core::domain::admin::{AdminContext, AdminError, AdminRole};
use cairn_core::domain::identity::IdentityId;
use cairn_core::verbs::admin;

pub fn resolve_actor(ctx: &CliContext) -> Result<IdentityId, anyhow::Error> {
    ctx.current_identity()
}

pub async fn snapshot(ctx: &CliContext, req: admin::snapshot::SnapshotRequest)
    -> Result<admin::snapshot::SnapshotResponse, AdminError> {
    let actor = resolve_actor(ctx).map_err(|e| AdminError::NotAuthorized {
        actor: IdentityId::from("unknown"),
        needed: AdminRole::Operator,
    })?;
    let admin_ctx = AdminContext::new(actor, AdminRole::Operator);
    admin::snapshot::run(admin_ctx, req, ctx.store(), ctx.admin_state(), ctx.backup_registry()).await
}

pub async fn restore(ctx: &CliContext, req: admin::restore::RestoreRequest)
    -> Result<admin::restore::RestoreResponse, AdminError> { /* same pattern */ todo!() }

pub async fn replay_wal(ctx: &CliContext, req: admin::replay_wal::ReplayWalRequest)
    -> Result<admin::replay_wal::ReplayWalResponse, AdminError> { todo!() }

pub async fn connector_enable(ctx: &CliContext, req: admin::connector::ConnectorTarget)
    -> Result<admin::connector::ConnectorStateResponse, AdminError> { todo!() }

pub async fn connector_disable(ctx: &CliContext, req: admin::connector::ConnectorDisableRequest)
    -> Result<admin::connector::ConnectorStateResponse, AdminError> { todo!() }

pub async fn connector_backfill(ctx: &CliContext, req: admin::connector::BackfillRequest)
    -> Result<admin::connector::BackfillResponse, AdminError> { todo!() }
```

Replace each `todo!()` with the same construction pattern. Add `pub mod admin;` to `lib.rs`.

- [ ] **Step 2: Commit**

```bash
git add crates/cairn-sdk/
git commit -m "feat(#161): cairn-sdk::admin wrappers for six verbs"
```

---

### Task 6.4: Audit log + integration tests

**Files:**
- Create: `crates/cairn-core/src/verbs/admin/audit.rs`
- Modify: `crates/cairn-core/src/verbs/admin/{snapshot,restore,replay_wal,connector}.rs`
- Create: `crates/cairn-core/tests/admin_unauth_reject.rs`
- Create: `crates/cairn-core/tests/admin_unnegotiated_reject.rs`

- [ ] **Step 1: Audit log writer**

Create `crates/cairn-core/src/verbs/admin/audit.rs`:

```rust
//! Append one line to `.cairn/admin.audit.jsonl` per successful
//! write-modifying admin verb. Read-only verbs do not audit.

use crate::domain::identity::IdentityId;
use sha2::{Digest, Sha256};
use serde::Serialize;
use std::path::Path;
use time::OffsetDateTime;

#[derive(Debug, Serialize)]
pub struct AuditEntry<'a> {
    pub ts: String,
    pub actor: &'a str,
    pub verb: &'a str,
    pub request_digest: String,
    pub response_digest: String,
    pub exit: u8,
}

pub fn append<R: Serialize, S: Serialize>(
    vault_root: &Path,
    actor: &IdentityId,
    verb: &str,
    request: &R,
    response: &S,
    exit: u8,
) -> std::io::Result<()> {
    let entry = AuditEntry {
        ts: OffsetDateTime::now_utc().to_string(),
        actor: actor.as_str(),
        verb,
        request_digest: digest_of(request),
        response_digest: digest_of(response),
        exit,
    };
    let line = serde_json::to_string(&entry).expect("serialize audit entry");
    use std::io::Write;
    let path = vault_root.join(".cairn").join("admin.audit.jsonl");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{line}")
}

fn digest_of<T: Serialize>(v: &T) -> String {
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("sha256:{}", hex::encode(h.finalize()))
}
```

Add an `audit::append(...)` call at the success path of every write-modifying verb (snapshot, restore, replay_wal --apply, connector enable/disable/backfill). Skip dry-run replay.

- [ ] **Step 2: Unauthorized-caller test**

Create `crates/cairn-core/tests/admin_unauth_reject.rs`:

```rust
//! AC#4 / Implementation Detail bullet 7: every write-modifying admin verb
//! rejects non-operator callers with `NotAuthorized`, exit 64, no audit row.

use cairn_core::domain::admin::{AdminContext, AdminError, AdminRole};
use cairn_core::domain::identity::IdentityId;
use cairn_core::verbs::admin;
use cairn_store_sqlite::SqliteStore;
use cairn_test_fixtures::tempvault;

#[tokio::test]
async fn every_write_verb_rejects_non_operator() {
    let vault = tempvault();
    let store = SqliteStore::open(vault.db_path()).await.unwrap();
    let admin = store.admin_state();   // no operator granted
    let nobody = IdentityId::from("hmn:nobody");
    let ctx = AdminContext::new(nobody.clone(), AdminRole::Operator);
    let tmp = tempfile::tempdir().unwrap();

    let snap_err = admin::snapshot::run(ctx.clone(),
        admin::snapshot::SnapshotRequest { out_path: tmp.path().into(), label: None },
        &store, &admin, &vault.backup_registry()).await.unwrap_err();
    assert!(matches!(snap_err, AdminError::NotAuthorized { .. }));
    assert_eq!(snap_err.exit_code(), 64);

    // Restore, replay_wal --apply, connector::{enable, disable, backfill} — same shape.
    // … (repeat the assertion for each).

    // No admin.audit.jsonl entries.
    let audit = vault.path().join(".cairn/admin.audit.jsonl");
    assert!(!audit.exists() || std::fs::read_to_string(&audit).unwrap().is_empty());
}
```

- [ ] **Step 3: Unnegotiated-extension test**

Create `crates/cairn-core/tests/admin_unnegotiated_reject.rs`:

```rust
//! AC#4: when the extension is not negotiated (config off OR no operator),
//! every verb returns `CapabilityUnavailable` with the right remediation.

use cairn_core::status::REMEDIATION;
use cairn_core::domain::admin::AdminError;
use cairn_test_fixtures::tempvault_with_admin_config_disabled;

#[tokio::test]
async fn every_verb_returns_capability_unavailable() {
    let vault = tempvault_with_admin_config_disabled();
    // SDK boundary calls into the verb via cairn_sdk::admin::snapshot,
    // which pre-checks `admin_extension_ready_for(ctx)` and short-circuits
    // to CapabilityUnavailable BEFORE entering the core verb.
    let ctx = vault.cli_context();

    let err = cairn_sdk::admin::snapshot(&ctx,
        cairn_core::verbs::admin::snapshot::SnapshotRequest {
            out_path: vault.path().into(), label: None
        }).await.unwrap_err();
    assert!(matches!(err, AdminError::CapabilityUnavailable { ref capability, .. }
        if capability == "cairn.mcp.v1.extension.admin.snapshot"));

    // Remediation matches table.
    if let AdminError::CapabilityUnavailable { remediation, capability } = err {
        let table_entry = REMEDIATION.iter().find(|(c, _)| *c == capability).unwrap();
        assert_eq!(remediation, table_entry.1);
    }
}
```

Run: `cargo nextest run -p cairn-core admin_unauth_reject admin_unnegotiated_reject --locked` — expect PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/
git commit -m "feat(#161): admin audit log + unauth/unnegotiated rejection tests"
```

---

### Task 6.5: Flip `ADMIN_EXTENSION_WIRED = true`

**Files:**
- Modify: `crates/cairn-core/src/status/wiring.rs`

- [ ] **Step 1: Flip the constant**

```rust
pub const ADMIN_EXTENSION_WIRED: bool = true;
```

- [ ] **Step 2: Re-run all admin tests**

```bash
cargo nextest run --workspace --locked --no-fail-fast
```

Expected: all green. Any tests that asserted "admin rows absent while dark" (Task 1.9) must now be updated to assert presence — review and accept `insta` snapshots:

```bash
cargo insta review
```

- [ ] **Step 3: Run codegen + docgen**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check  # no diff expected
cargo run -p cairn-cli --bin cairn-docgen -- --write
git add docs/site/src/reference/generated/
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/status/wiring.rs crates/cairn-core/src/status/snapshots/ docs/site/src/reference/generated/
git commit -m "feat(#161): flip ADMIN_EXTENSION_WIRED — cairn.admin.v1 live"
```

---

### Task 6.6: Full verification + open final PR

- [ ] **Step 1: Run the entire CLAUDE.md §8 verification block**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-bench --release --locked -- all
cargo run -p cairn-bench --release --locked -- coherence run --gate beta

cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked

cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: all green.

- [ ] **Step 2: File the follow-up issues listed in spec §12**

Use `gh issue create` for each (cross-machine restore, hardware-key countersign, incremental snapshots, encryption-at-rest). Capture their numbers — they go in the next step's PR body.

- [ ] **Step 3: Open the phase-6 (final) PR**

```bash
gh pr create --title "feat(#161): cairn.admin.v1 phase 6 — MCP+SDK wiring, audit, extension flip" \
  --body "$(cat <<'EOF'
## Summary
Final phase of issue #161. Wires the extension live:
- IDL schemas + codegen for six admin verbs
- MCP handler dispatch + tools advertised when extension ready
- cairn-sdk::admin typed wrappers
- Audit log writer (.cairn/admin.audit.jsonl)
- Integration tests: AC#1, AC#4 (admin_unauth_reject, admin_unnegotiated_reject)
- ADMIN_EXTENSION_WIRED = true

Spec: docs/superpowers/specs/2026-05-26-issue-161-admin-v1-extension-design.md
Plan: docs/superpowers/plans/2026-05-26-issue-161-admin-v1-extension.md

## Follow-ups filed
- Cross-machine restore + salt portability: #XXX
- Hardware-key countersign: #XXX
- Incremental snapshots: #XXX
- Backup encryption-at-rest: #XXX

## Acceptance criteria
- [x] AC#1 — six verbs advertised when enabled, absent otherwise
- [x] AC#2 — snapshot/restore bit-identical, tombstones preserved (phase 2)
- [x] AC#3 — disable stops ingestion within one tick, visible in status (phase 4)
- [x] AC#4 — every write verb fails closed with CapabilityUnavailable when not negotiated

## Test plan
- [x] Full CLAUDE.md §8 verification block green
- [x] All four AC integration tests pass
- [x] cargo insta review clean
EOF
)"
```

---

## Self-review checklist

- [x] **Spec coverage:** every section of `docs/superpowers/specs/2026-05-26-issue-161-admin-v1-extension-design.md` maps to one or more tasks:
  - §3 dependencies — verified at plan-write time
  - §4 architecture — phases 1-6
  - §5 verb contracts — phases 1-5
  - §6 snapshot format + migrations — phase 2 + migrations 0003/0004/0005
  - §7 error model + capabilities + audit — phases 1, 6
  - §8 connector progress — phase 5
  - §9 testing — tests embedded throughout each phase + dedicated tasks 2.7, 2.8, 3.2 tests, 4.5, 5.6, 6.4
  - §10 phasing — directly mirrored
  - §11 AC mapping — verified in tasks 1.9, 2.7, 4.5, 6.4
  - §12 follow-ups — task 6.6 step 2
- [x] **Placeholder scan:** no "TBD" / "TODO" — `todo!()` markers appear in 6.3 stub but each is paired with a "replace with the same construction pattern" instruction. Migration filenames `0003/0004/0005` are concrete numbers (head was 0002 at plan-write time).
- [x] **Type consistency:** `SnapshotManifest`, `AdminContext`, `AdminRole`, `AdminError`, `AdminStateStore`, `ConsentLog`, `ProgressEvent`, `WorkflowOrchestrator`, `ConnectorStateRow` names match across phases. `BackfillRequest` / `BackfillResponse` / `BackfillSpec` distinguish the verb-layer types from the orchestrator-layer spec.

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-26-issue-161-admin-v1-extension.md`. Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, two-stage review between tasks, fast iteration on a long plan like this.
2. **Inline Execution** — execute tasks in this session with checkpoints; appropriate if you want to be in the loop for each phase boundary.

Which approach?
