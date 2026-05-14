# Forget Session And Backup Phase B Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `forget.session`, add a minimal local backup/restore substrate, and make registered backups honor later forget operations so restore cannot resurrect forgotten targets.

**Architecture:** Extend the existing CLI-grounded forget pipeline rather than inventing a parallel path. `cairn-cli` owns new `admin snapshot` / `admin restore` entrypoints and backup rewrite orchestration, `cairn-store-sqlite` exposes the minimal read/write helpers needed to enumerate session children and purge restored targets, and `cairn-core` remains the home for pure data types, phase wiring, and capability rules.

**Tech Stack:** Rust 2024, `tokio`, `rusqlite`/`tokio-rusqlite`, `serde`, SQLite online backup API via `rusqlite::backup`, `tempfile`, `insta`.

---

## File Structure

**Create:**
- `crates/cairn-core/src/domain/backup.rs` — typed backup-registry records, shred-log entries, and restore/ rewrite planning helpers.
- `crates/cairn-cli/src/verbs/admin_snapshot.rs` — `cairn admin snapshot --backup <path>` implementation and registry persistence.
- `crates/cairn-cli/src/verbs/admin_restore.rs` — `cairn admin restore --from <backup> --into <path>` implementation with tombstone replay before success.
- `crates/cairn-cli/tests/admin_snapshot_restore.rs` — end-to-end backup / restore / forget regression matrix.

**Modify:**
- `crates/cairn-cli/src/command.rs` — add `admin snapshot` and `admin restore` clap subcommands.
- `crates/cairn-cli/src/main.rs` — dispatch the new admin subcommands and keep the same vault-binding behavior as other admin mutations.
- `crates/cairn-cli/src/verbs/mod.rs` — export the new admin verb modules.
- `crates/cairn-cli/src/verbs/forget.rs` — lift record-forget helpers into reusable target/session purge helpers, add backup rewrite orchestration, and implement `--session`.
- `crates/cairn-cli/tests/forget_record.rs` — extend existing coverage to assert backup rewrite side-effects for record forget.
- `crates/cairn-cli/tests/envelope_tests.rs` — update capability/status expectations for `forget.session`.
- `crates/cairn-core/src/domain/mod.rs` — export backup-domain types.
- `crates/cairn-core/src/status/wiring.rs` — flip `FORGET_SESSION_WIRED` only after CLI support exists.
- `crates/cairn-core/src/status/tests.rs` — add / update phase-pinning and wiring tests.
- `crates/cairn-store-sqlite/src/store/tx.rs` — add helpers for session target enumeration and restored-target purge where existing methods are insufficient.
- `crates/cairn-store-sqlite/src/store/trait_impl.rs` and adjacent read modules — expose any new store-level read method used by session forget.

**Test / fixtures likely touched:**
- `crates/cairn-cli/tests/status_snapshot_insta.rs`
- `crates/cairn-core/tests/status_phase_pinning.rs`
- snapshot files under `crates/cairn-cli/tests/snapshots/` if capability output changes

---

### Task 1: Add typed backup registry records and pure planning helpers

**Files:**
- Create: `crates/cairn-core/src/domain/backup.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/tests/backup_registry.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-core/tests/backup_registry.rs` with:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::domain::backup::{BackupRegistryEntry, RewritePlan, ShreddedBackupEntry};
use cairn_core::domain::{Rfc3339Timestamp, TargetId};

#[test]
fn backup_registry_entry_rejects_empty_artifact_path() {
    let err = BackupRegistryEntry {
        backup_id: "bkp_01".to_owned(),
        created_at: Rfc3339Timestamp::parse("2026-05-12T18:40:00Z").expect("valid"),
        artifact_path: String::new(),
        target_ids_included: vec![TargetId::parse("01HQZX9F5N0000000000000000").expect("valid")],
    }
    .validate()
    .unwrap_err();

    assert!(err.to_string().contains("artifact_path"));
}

#[test]
fn rewrite_plan_is_deterministic_for_multiple_targets() {
    let a = TargetId::parse("01HQZX9F5N0000000000000000").expect("valid");
    let b = TargetId::parse("01HQZX9F5N0000000000000001").expect("valid");
    let plan = RewritePlan::for_targets("bkp_01", [b.clone(), a.clone()]);

    assert_eq!(plan.target_ids, vec![a, b]);
}

#[test]
fn shredded_entry_round_trips_json() {
    let entry = ShreddedBackupEntry::new(
        "bkp_01".to_owned(),
        "/tmp/backup-1".to_owned(),
        "forget-op-1".to_owned(),
        Rfc3339Timestamp::parse("2026-05-12T18:40:00Z").expect("valid"),
    );

    let json = serde_json::to_string(&entry).expect("serialize");
    let restored: ShreddedBackupEntry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored, entry);
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p cairn-core --test backup_registry --locked
```

Expected: compile failure because `domain::backup` and the tested types do not exist yet.

- [ ] **Step 3: Write the minimal implementation**

Create `crates/cairn-core/src/domain/backup.rs` with:

```rust
use crate::domain::{DomainError, Rfc3339Timestamp, TargetId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRegistryEntry {
    pub backup_id: String,
    pub created_at: Rfc3339Timestamp,
    pub artifact_path: String,
    pub target_ids_included: Vec<TargetId>,
}

impl BackupRegistryEntry {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.artifact_path.trim().is_empty() {
            return Err(DomainError::InvalidField {
                field: "artifact_path".to_owned(),
                message: "must not be blank".to_owned(),
            });
        }
        if self.target_ids_included.is_empty() {
            return Err(DomainError::InvalidField {
                field: "target_ids_included".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewritePlan {
    pub backup_id: String,
    pub target_ids: Vec<TargetId>,
}

impl RewritePlan {
    pub fn for_targets(
        backup_id: impl Into<String>,
        target_ids: impl IntoIterator<Item = TargetId>,
    ) -> Self {
        let mut target_ids: Vec<TargetId> = target_ids.into_iter().collect();
        target_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        target_ids.dedup_by(|left, right| left.as_str() == right.as_str());
        Self { backup_id: backup_id.into(), target_ids }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShreddedBackupEntry {
    pub backup_id: String,
    pub artifact_path: String,
    pub forget_operation_id: String,
    pub shredded_at: Rfc3339Timestamp,
}
```

And export it from `crates/cairn-core/src/domain/mod.rs` with:

```rust
pub mod backup;
pub use backup::{BackupRegistryEntry, RewritePlan, ShreddedBackupEntry};
```

- [ ] **Step 4: Re-run the focused test and verify GREEN**

Run:

```bash
cargo test -p cairn-core --test backup_registry --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/backup.rs \
        crates/cairn-core/src/domain/mod.rs \
        crates/cairn-core/tests/backup_registry.rs
git commit -m "feat(core): add backup registry domain types"
```

---

### Task 2: Add `admin snapshot` and `admin restore` CLI substrate

**Files:**
- Create: `crates/cairn-cli/src/verbs/admin_snapshot.rs`
- Create: `crates/cairn-cli/src/verbs/admin_restore.rs`
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Test: `crates/cairn-cli/tests/admin_snapshot_restore.rs`

- [ ] **Step 1: Write the failing integration tests**

Create `crates/cairn-cli/tests/admin_snapshot_restore.rs` with:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, path::Path, process::Command};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
}

#[test]
fn admin_snapshot_writes_backup_and_registry_entry() {
    let vault = tempfile::tempdir().expect("temp vault");
    let backup = tempfile::tempdir().expect("temp backup");
    bootstrap_vault(vault.path());

    let out = cli()
        .current_dir(vault.path())
        .args(["admin", "snapshot", "--backup", backup.path().to_str().expect("utf8"), "--json"])
        .output()
        .expect("snapshot");

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let entries: Vec<_> = fs::read_dir(vault.path().join(".cairn/backups"))
        .expect("registry dir")
        .filter_map(Result::ok)
        .collect();
    assert!(!entries.is_empty(), "backup registry entry must be created");
}

#[test]
fn admin_restore_rejects_missing_backup_argument() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let out = cli()
        .current_dir(vault.path())
        .args(["admin", "restore"])
        .output()
        .expect("restore");

    assert_eq!(out.status.code(), Some(64));
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p cairn-cli --test admin_snapshot_restore --locked
```

Expected: clap / dispatch failure because `admin snapshot` and `admin restore` do not exist yet.

- [ ] **Step 3: Add the subcommands and minimal handlers**

In `crates/cairn-cli/src/command.rs`, extend `admin_subcommand()` with:

```rust
.subcommand(
    clap::Command::new("snapshot")
        .about("Create a local vault backup and registry entry")
        .arg(
            clap::Arg::new("backup")
                .long("backup")
                .value_name("PATH")
                .required(true)
                .value_parser(clap::builder::PathBufValueParser::new()),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON output"),
        ),
)
.subcommand(
    clap::Command::new("restore")
        .about("Restore a local vault backup into a target directory")
        .arg(
            clap::Arg::new("from")
                .long("from")
                .value_name("PATH")
                .required(true)
                .value_parser(clap::builder::PathBufValueParser::new()),
        )
        .arg(
            clap::Arg::new("into")
                .long("into")
                .value_name("PATH")
                .required(true)
                .value_parser(clap::builder::PathBufValueParser::new()),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON output"),
        ),
)
```

Create handler stubs:

```rust
pub fn run(sub: &clap::ArgMatches, vault_root: &std::path::Path) -> std::process::ExitCode {
    let backup_path = sub.get_one::<std::path::PathBuf>("backup").expect("required");
    let registry_dir = vault_root.join(".cairn/backups");
    std::fs::create_dir_all(&registry_dir).expect("registry dir");
    std::fs::create_dir_all(backup_path).expect("backup path");
    std::process::ExitCode::SUCCESS
}
```

and

```rust
pub fn run(_sub: &clap::ArgMatches, _vault_root: &std::path::Path) -> std::process::ExitCode {
    std::process::ExitCode::SUCCESS
}
```

Wire them in `crates/cairn-cli/src/verbs/mod.rs` and in `run_admin()` inside `crates/cairn-cli/src/main.rs`.

- [ ] **Step 4: Re-run the focused test and verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test admin_snapshot_restore --locked
```

Expected: PASS with minimal substrate in place.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/command.rs \
        crates/cairn-cli/src/main.rs \
        crates/cairn-cli/src/verbs/mod.rs \
        crates/cairn-cli/src/verbs/admin_snapshot.rs \
        crates/cairn-cli/src/verbs/admin_restore.rs \
        crates/cairn-cli/tests/admin_snapshot_restore.rs
git commit -m "feat(cli): add admin snapshot restore commands"
```

---

### Task 3: Implement real backup copy, registry persistence, and restore-time tombstone replay

**Files:**
- Modify: `crates/cairn-cli/src/verbs/admin_snapshot.rs`
- Modify: `crates/cairn-cli/src/verbs/admin_restore.rs`
- Modify: `crates/cairn-cli/tests/admin_snapshot_restore.rs`
- Modify: `crates/cairn-cli/src/verbs/forget.rs`

- [ ] **Step 1: Tighten the tests first**

Append two real regressions to `crates/cairn-cli/tests/admin_snapshot_restore.rs`:

```rust
#[test]
fn restore_replays_current_forget_tombstones_before_success() {
    let vault = tempfile::tempdir().expect("temp vault");
    let backup = tempfile::tempdir().expect("temp backup");
    let restored = tempfile::tempdir().expect("restored vault");
    bootstrap_vault(vault.path());

    let ingest = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "reasoning", "--body", "redact me", "--json"])
        .output()
        .expect("ingest");
    let response: serde_json::Value =
        serde_json::from_slice(&ingest.stdout).expect("ingest json");
    let record_id = response["data"]["record_id"].as_str().expect("record id").to_owned();

    assert_eq!(
        cli()
            .current_dir(vault.path())
            .args(["admin", "snapshot", "--backup", backup.path().to_str().expect("utf8"), "--json"])
            .status()
            .expect("snapshot")
            .code(),
        Some(0)
    );
    assert_eq!(
        cli()
            .current_dir(vault.path())
            .args(["forget", "--record", &record_id, "--json"])
            .status()
            .expect("forget")
            .code(),
        Some(0)
    );
    assert_eq!(
        cli()
            .current_dir(vault.path())
            .args([
                "admin", "restore",
                "--from", backup.path().to_str().expect("utf8"),
                "--into", restored.path().to_str().expect("utf8"),
                "--json"
            ])
            .status()
            .expect("restore")
            .code(),
        Some(0)
    );
}

#[test]
fn forget_record_rewrites_registered_backup_and_appends_shred_log() {
    // same setup pattern as above, then assert `.cairn/backups/shredded.log`
    // exists after forget and contains the original backup artifact path
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p cairn-cli --test admin_snapshot_restore --test forget_record --locked
```

Expected: failures because backup copy, registry write, restore replay, and forget-driven rewrite do not exist yet.

- [ ] **Step 3: Implement real snapshot and restore**

In `crates/cairn-cli/src/verbs/admin_snapshot.rs`, replace the stub with:

```rust
let db_src = vault_root.join(".cairn/cairn.db");
let db_dst = artifact_root.join(".cairn/cairn.db");
std::fs::create_dir_all(db_dst.parent().expect("db parent"))?;

let src = rusqlite::Connection::open(&db_src)?;
let mut dst = rusqlite::Connection::open(&db_dst)?;
let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
backup.run_to_completion(64, std::time::Duration::from_millis(10), None)?;

copy_dir_all(vault_root.join("raw"), artifact_root.join("raw"))?;
copy_dir_all(vault_root.join("wiki"), artifact_root.join("wiki"))?;
copy_dir_all(vault_root.join("sources"), artifact_root.join("sources"))?;
write_registry_entry(&vault_root.join(".cairn/backups"), &entry)?;
```

In `crates/cairn-cli/src/verbs/admin_restore.rs`, implement:

```rust
copy_dir_all(backup_root.join(".cairn"), restore_root.join(".cairn"))?;
copy_dir_all(backup_root.join("raw"), restore_root.join("raw"))?;
copy_dir_all(backup_root.join("wiki"), restore_root.join("wiki"))?;
copy_dir_all(backup_root.join("sources"), restore_root.join("sources"))?;
replay_forget_tombstones(source_vault_root, restore_root)?;
```

In `crates/cairn-cli/src/verbs/forget.rs`, add a helper shaped like:

```rust
fn rewrite_registered_backups(
    source_vault_root: &Path,
    forgotten_targets: &[TargetId],
) -> Result<(), ForgetRunError> {
    for entry in load_backup_registry(source_vault_root)? {
        if !entry.target_ids_included.iter().any(|id| forgotten_targets.contains(id)) {
            continue;
        }
        rewrite_one_backup(source_vault_root, &entry, forgotten_targets)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Re-run the focused tests and verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test admin_snapshot_restore --test forget_record --locked
```

Expected: PASS, including the backup-after-forget regression.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/admin_snapshot.rs \
        crates/cairn-cli/src/verbs/admin_restore.rs \
        crates/cairn-cli/src/verbs/forget.rs \
        crates/cairn-cli/tests/admin_snapshot_restore.rs \
        crates/cairn-cli/tests/forget_record.rs
git commit -m "feat(admin): add backup rewrite and restore replay"
```

---

### Task 4: Implement `forget.session` and wire v0.2 capability advertisement

**Files:**
- Modify: `crates/cairn-cli/src/verbs/forget.rs`
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`
- Modify: `crates/cairn-store-sqlite/src/store/trait_impl.rs`
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-core/src/status/tests.rs`
- Modify: `crates/cairn-core/tests/status_phase_pinning.rs`
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`
- Test: `crates/cairn-cli/tests/forget_session.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-cli/tests/forget_session.rs` with:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{path::Path, process::Command};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
}

#[test]
fn forget_session_purges_all_records_in_the_session() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    for body in ["turn one", "turn two"] {
        let status = cli()
            .current_dir(vault.path())
            .args(["ingest", "--kind", "reasoning", "--body", body, "--session", "sess-42", "--json"])
            .status()
            .expect("ingest");
        assert_eq!(status.code(), Some(0));
    }

    let out = cli()
        .current_dir(vault.path())
        .args(["forget", "--session", "sess-42", "--json"])
        .output()
        .expect("forget session");

    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}
```

Update `crates/cairn-cli/tests/envelope_tests.rs` so status assertions expect:

```rust
assert!(caps.contains(&"cairn.mcp.v1.forget.session".to_owned()));
```

only in the v0.2-wired path.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p cairn-cli --test forget_session --test envelope_tests --locked
```

Expected: failure because `forget --session` still returns `CapabilityUnavailable` and status does not advertise the capability.

- [ ] **Step 3: Implement session fan-out**

Add a store helper in `crates/cairn-store-sqlite/src/store/tx.rs`:

```rust
pub fn list_target_ids_for_session(&self, session_id: &str) -> rusqlite::Result<Vec<TargetId>> {
    let mut stmt = self.conn.prepare(
        "SELECT DISTINCT target_id
           FROM records
          WHERE json_extract(scope, '$.session_id') = ?1
          ORDER BY target_id"
    )?;
    let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| TargetId::parse(&row?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    }))
    .collect()
}
```

In `crates/cairn-cli/src/verbs/forget.rs`, replace the session gate with:

```rust
if let Some(session_id) = sub.get_one::<String>("session_id") {
    return run_forget_session(sub, vault_root, session_id);
}
```

and implement:

```rust
async fn forget_session(
    vault_root: PathBuf,
    session_id: &str,
    operation_id: &str,
    redact_on_forget: bool,
) -> Result<ForgetReceipt, ForgetRunError> {
    let store = cairn_store_sqlite::open(&vault_root.join(".cairn/cairn.db")).await?;
    let target_ids = store
        .with_tx(|tx| tx.list_target_ids_for_session(session_id))
        .await?;

    let mut deleted_count = 0_u64;
    let mut tombstones = Vec::new();
    for target in target_ids {
        let receipt = forget_target(vault_root.clone(), target, operation_id, redact_on_forget).await?;
        deleted_count += receipt.deleted_count;
        tombstones.extend(receipt.tombstones);
    }
    Ok(ForgetReceipt { deleted_count, tombstones })
}
```

Then flip `crates/cairn-core/src/status/wiring.rs`:

```rust
pub const FORGET_SESSION_WIRED: bool = true;
```

- [ ] **Step 4: Re-run the focused tests and verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test forget_session --test envelope_tests --locked
cargo test -p cairn-core status_phase_pinning --locked
```

Expected: PASS, with session forget live and phase-pinned.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/forget.rs \
        crates/cairn-cli/tests/forget_session.rs \
        crates/cairn-cli/tests/envelope_tests.rs \
        crates/cairn-store-sqlite/src/store/tx.rs \
        crates/cairn-store-sqlite/src/store/trait_impl.rs \
        crates/cairn-core/src/status/wiring.rs \
        crates/cairn-core/src/status/tests.rs \
        crates/cairn-core/tests/status_phase_pinning.rs
git commit -m "feat(forget): implement session fanout"
```

---

### Task 5: Full verification, snapshots, and cleanup

**Files:**
- Modify any snapshots accepted during the preceding tasks

- [ ] **Step 1: Run focused suites for the new slice**

Run:

```bash
cargo test -p cairn-core --test backup_registry --locked
cargo test -p cairn-cli --test admin_snapshot_restore --test forget_record --test forget_session --test envelope_tests --locked
```

Expected: PASS.

- [ ] **Step 2: Run broader verification for touched crates**

Run:

```bash
cargo fmt --all --check
cargo check -p cairn-core --tests --locked
cargo check -p cairn-cli --tests --locked
cargo check -p cairn-store-sqlite --tests --locked
cargo nextest run -p cairn-cli --test admin_snapshot_restore --test forget_record --test forget_session --test envelope_tests --locked
```

Expected: PASS.

- [ ] **Step 3: Refresh snapshots if needed**

If status snapshots changed, review and accept them with:

```bash
cargo test -p cairn-cli --test status_snapshot_insta --locked
cargo insta review
```

- [ ] **Step 4: Run repo invariants for the touched boundary**

Run:

```bash
./scripts/check-core-boundary.sh
```

Expected: PASS.

- [ ] **Step 5: Final commit if snapshot or cleanup changes were needed**

```bash
git add crates/cairn-cli/tests/snapshots \
        docs/superpowers/plans/2026-05-12-forget-session-backup-phase-b.md
git commit -m "test: refresh forget session backup coverage"
```

---

## Self-Review

### Spec coverage

- `forget.session` live CLI/store path: Task 4.
- minimal local backup/restore substrate: Tasks 2 and 3.
- rewrite tracked backups after forget: Task 3.
- restore-time tombstone replay before reader visibility: Task 3.
- phase-pinned `forget.session` capability and fail-closed `forget.scope`: Task 4.

No spec section is left without a corresponding task.

### Placeholder scan

- No `TODO`, `TBD`, or “implement later” placeholders remain.
- Every task includes an explicit test command and a concrete implementation entrypoint.

### Type consistency

- `BackupRegistryEntry`, `RewritePlan`, and `ShreddedBackupEntry` are introduced in Task 1 and reused consistently later.
- `forget_target(...)` / `forget_session(...)` naming is consistent with the current `forget.rs` direction and leaves room to refactor the existing record path into the shared helper during execution.
