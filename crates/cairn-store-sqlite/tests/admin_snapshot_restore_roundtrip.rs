//! AC#2: round-trip snapshot→restore on the same machine preserves
//! record state and tombstones exactly.
//!
//! Schema used: `records` table (migration 0001), columns verified against
//! `src/migrations/sql/0001_records.sql`.
//! Seeded rows cover five active records and two tombstoned records.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test — panics surface immediately as test failures"
)]
#![allow(missing_docs)]

use cairn_core::contract::AdminStateStore as _;
use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::verbs::admin::{restore, snapshot};
use cairn_store_sqlite::{
    FileBackupRegistry, SqliteAdminStateStore, SqliteConsentLog, SqliteSnapshotApplier,
    SqliteSnapshotMetadata, SqliteSnapshotProducer, SqliteSnapshotReader,
};

const TEST_MACHINE: &str = "MACHINE-TEST-A";
const TEST_VAULT: &str = "vault-roundtrip";

/// Fingerprint row: `(record_id, target_id, body_hash, active, tombstoned)`.
/// Ordered by `record_id` for deterministic comparison.
type Row = (String, String, String, i64, i64);

fn seed_records(conn: &rusqlite::Connection, active: usize, tombstoned: usize) {
    // Insert active rows.
    for i in 0..active {
        let rid = format!("01REC0ACTIVE{i:012}");
        let tid = format!("01TGT0ACTIVE{i:012}");
        let hash = format!("sha256:active{i:064}");
        conn.execute(
            "INSERT INTO records \
               (record_id, target_id, version, path, kind, class, visibility, \
                scope, actor_chain, body, body_hash, created_at, updated_at, \
                active, tombstoned, is_static) \
             VALUES (?1, ?2, 1, 'test/active', 'user', 'semantic', 'private', \
                     '{\"user\":\"hmn:test\"}', '[]', 'body text', ?3, 1, 1, 1, 0, 0)",
            rusqlite::params![rid, tid, hash],
        )
        .unwrap_or_else(|e| panic!("seed active {i}: {e}"));
    }

    // Insert tombstoned rows.
    for i in 0..tombstoned {
        let rid = format!("01REC0TOMBST{i:012}");
        let tid = format!("01TGT0TOMBST{i:012}");
        let hash = format!("sha256:tombstone{i:060}");
        conn.execute(
            "INSERT INTO records \
               (record_id, target_id, version, path, kind, class, visibility, \
                scope, actor_chain, body, body_hash, created_at, updated_at, \
                active, tombstoned, is_static) \
             VALUES (?1, ?2, 1, 'test/tombstoned', 'user', 'semantic', 'private', \
                     '{\"user\":\"hmn:test\"}', '[]', 'tombstoned body', ?3, 1, 1, 1, 1, 0)",
            rusqlite::params![rid, tid, hash],
        )
        .unwrap_or_else(|e| panic!("seed tombstone {i}: {e}"));
    }
}

fn fingerprint_records(conn: &rusqlite::Connection) -> Vec<Row> {
    let mut stmt = conn
        .prepare(
            "SELECT record_id, target_id, body_hash, active, tombstoned \
               FROM records \
              ORDER BY record_id ASC",
        )
        .expect("prepare fingerprint query");

    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })
    .expect("execute fingerprint query")
    .map(|r| r.expect("fingerprint row"))
    .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "end-to-end AC#2 integration test exercises six adapter impls in sequence"
)]
fn snapshot_restore_roundtrip_preserves_records() {
    // ── 1. Set up temp vault dir with .cairn/cairn.db ──────────────────────
    let temp = tempfile::tempdir().expect("tempdir");
    let vault_root = temp.path().to_path_buf();
    std::fs::create_dir_all(vault_root.join(".cairn")).unwrap();
    let db_path = vault_root.join(".cairn/cairn.db");

    // Open via open_sync so all migrations are applied.
    let conn = cairn_store_sqlite::open_sync(&db_path).expect("open store");
    seed_records(&conn, /* active= */ 5, /* tombstoned= */ 2);
    let pre = fingerprint_records(&conn);
    assert_eq!(pre.len(), 7, "expect 5 active + 2 tombstoned seeded");
    // Release the file lock before snapshot reads the file.
    conn.close().expect("close connection before snapshot");

    // ── 2. Build admin state and grant operator role ───────────────────────
    let admin = SqliteAdminStateStore::open_in_memory().expect("admin store");
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant operator");

    let ctx = AdminContext::new(op.clone(), AdminRole::Operator);

    // ── 3. Snapshot ────────────────────────────────────────────────────────
    let out_dir = temp.path().join("backups");
    let meta = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
    let registry = FileBackupRegistry::new(vault_root.clone());

    let snap_req = snapshot::SnapshotRequest {
        out_dir: out_dir.clone(),
        label: Some("rt".into()),
        local_machine_id: TEST_MACHINE.into(),
        backup_kind: "snapshot".into(),
    };
    let snap_resp =
        snapshot::run(&ctx, &snap_req, &admin, &meta, &producer, &registry).expect("snapshot run");

    // Snapshot manifest record count should equal active-non-tombstoned rows.
    assert_eq!(
        snap_resp.manifest.record_count, 5,
        "manifest.record_count must equal active non-tombstoned rows"
    );
    // Tombstone count includes tombstoned rows.
    assert_eq!(
        snap_resp.manifest.tombstone_count, 2,
        "manifest.tombstone_count must equal tombstoned rows"
    );

    // ── 4. Mutate the live DB to prove the restore actually replaces it ─────
    // Write a new row that should NOT appear in the post-restore fingerprint.
    // This shows the restore overwrites the mutated DB with the snapshot copy.
    {
        let conn_mut = cairn_store_sqlite::open_sync(&db_path).expect("reopen db for mutation");
        conn_mut
            .execute(
                "INSERT INTO records \
                   (record_id, target_id, version, path, kind, class, visibility, \
                    scope, actor_chain, body, body_hash, created_at, updated_at, \
                    active, tombstoned, is_static) \
                 VALUES ('01REC0MUTATD000000000000', '01TGT0MUTATD000000000000', \
                         1, 'test/mutated', 'user', 'semantic', 'private', \
                         '{\"user\":\"hmn:test\"}', '[]', 'mutated body', \
                         'sha256:mutated', 1, 1, 1, 0, 0)",
                [],
            )
            .expect("insert mutation row");
        // Verify the mutation row is present before restore.
        let count: i64 = conn_mut
            .query_row(
                "SELECT COUNT(*) FROM records WHERE record_id = '01REC0MUTATD000000000000'",
                [],
                |r| r.get(0),
            )
            .expect("count mutated row");
        assert_eq!(count, 1, "mutated row must be present before restore");
        conn_mut.close().expect("close mutated db");
    }

    // ── 5. Restore ─────────────────────────────────────────────────────────
    // The live DB still exists (with the mutation row) — the restore will
    // overwrite it via the swap_in atomic rename. The metadata provider reads
    // the live DB for vault_id and migration_heads (precondition gate).
    let meta2 = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());

    let reader = SqliteSnapshotReader;
    let applier = SqliteSnapshotApplier::new(vault_root.clone());

    // ConsentLog: after swap_in the restored DB IS the live DB, so we pass
    // db_path for both sides. No records were forgotten in this round-trip so
    // apply_post_restore_purge will return 0.
    let consent = SqliteConsentLog::new(db_path.clone());

    let restore_req = restore::RestoreRequest {
        artifact_path: snap_resp.artifact_path.clone(),
        dry_run: false,
        local_machine_id: TEST_MACHINE.into(),
    };

    // Reuse the same registry the snapshot wrote to: restore verifies the
    // artifact's recomputed digest against the trusted `file_digest` anchor.
    let restore_resp = restore::run(
        &ctx,
        &restore_req,
        &admin,
        &meta2,
        &reader,
        &applier,
        &consent,
        &registry,
    )
    .expect("restore run");

    // ── 6. Verify ──────────────────────────────────────────────────────────
    // After swap_in the live DB is the restored copy; open it and fingerprint.
    let conn2 = cairn_store_sqlite::open_sync(&db_path).expect("reopen restored store");
    let post = fingerprint_records(&conn2);

    assert_eq!(
        pre, post,
        "record fingerprint must be bit-identical after round-trip"
    );
    assert_eq!(
        restore_resp.restored_records, snap_resp.manifest.record_count,
        "RestoreResponse.restored_records must equal manifest.record_count"
    );
    assert_eq!(
        restore_resp.tombstones_replayed, 0,
        "no records were forgotten; tombstones_replayed must be 0"
    );

    // Spot-check that tombstoned rows are present and active rows are present.
    let active_count = post.iter().filter(|r| r.3 == 1 && r.4 == 0).count();
    let tombstone_count = post.iter().filter(|r| r.4 == 1).count();
    assert_eq!(
        active_count, 5,
        "restored DB must contain 5 active non-tombstoned rows"
    );
    assert_eq!(
        tombstone_count, 2,
        "restored DB must contain 2 tombstoned rows"
    );
}

/// Round-3 adversarial review #3: a snapshot taken while a WAL-mode
/// connection is still OPEN — committed rows live in the `-wal` sidecar, not
/// yet checkpointed into the main db file — must still capture those rows.
/// The producer uses `VACUUM INTO`, which reads a transactionally-consistent
/// committed image (WAL frames included); the old `std::fs::read` of the main
/// db file would have silently dropped them.
#[test]
fn snapshot_captures_committed_rows_with_open_wal_connection() {
    let temp = tempfile::tempdir().expect("tempdir");
    let vault_root = temp.path().to_path_buf();
    std::fs::create_dir_all(vault_root.join(".cairn")).unwrap();
    let db_path = vault_root.join(".cairn/cairn.db");

    // Seed + commit, but DELIBERATELY keep the connection open so the
    // committed frames stay in the -wal sidecar (no checkpoint on close).
    let conn = cairn_store_sqlite::open_sync(&db_path).expect("open store");
    seed_records(&conn, /* active= */ 3, /* tombstoned= */ 1);
    let pre = fingerprint_records(&conn);
    assert_eq!(pre.len(), 4, "expect 3 active + 1 tombstoned seeded");

    let admin = SqliteAdminStateStore::open_in_memory().expect("admin store");
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant operator");
    let ctx = AdminContext::new(op.clone(), AdminRole::Operator);

    let out_dir = temp.path().join("backups");
    let meta = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
    let registry = FileBackupRegistry::new(vault_root.clone());
    let snap_req = snapshot::SnapshotRequest {
        out_dir: out_dir.clone(),
        label: Some("wal-open".into()),
        local_machine_id: TEST_MACHINE.into(),
        backup_kind: "snapshot".into(),
    };
    // Snapshot runs WHILE `conn` is still open.
    let snap_resp = snapshot::run(&ctx, &snap_req, &admin, &meta, &producer, &registry)
        .expect("snapshot with open WAL connection");
    assert_eq!(
        snap_resp.manifest.record_count, 3,
        "manifest must count the 3 WAL-committed active rows"
    );

    // Now close the live connection and restore the snapshot into the vault.
    conn.close().expect("close after snapshot");
    let meta2 = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let reader = SqliteSnapshotReader;
    let applier = SqliteSnapshotApplier::new(vault_root.clone());
    let consent = SqliteConsentLog::new(db_path.clone());
    let restore_req = restore::RestoreRequest {
        artifact_path: snap_resp.artifact_path.clone(),
        dry_run: false,
        local_machine_id: TEST_MACHINE.into(),
    };
    restore::run(
        &ctx,
        &restore_req,
        &admin,
        &meta2,
        &reader,
        &applier,
        &consent,
        &registry,
    )
    .expect("restore run");

    // The restored DB IS the snapshot's db; its rows must match what was
    // committed (and only in the WAL) at snapshot time.
    let conn2 = cairn_store_sqlite::open_sync(&db_path).expect("reopen restored store");
    let post = fingerprint_records(&conn2);
    assert_eq!(
        pre, post,
        "WAL-committed rows must survive snapshot→restore (VACUUM INTO captured them)"
    );
}

/// Round-3 adversarial review #4: a snapshot artifact whose `cairn.db` member
/// has been tampered with after sealing — while its `manifest.json` stays
/// byte-identical so the manifest self-check still passes — must be rejected
/// at the restore precondition gate with `IntegrityMismatch`, before any
/// `swap_in`. The trusted anchor is the backup registry's recorded
/// `file_digest`, which the tampered artifact no longer hashes to.
#[test]
fn restore_rejects_artifact_with_tampered_db_member() {
    let temp = tempfile::tempdir().expect("tempdir");
    let vault_root = temp.path().to_path_buf();
    std::fs::create_dir_all(vault_root.join(".cairn")).unwrap();
    let db_path = vault_root.join(".cairn/cairn.db");

    let conn = cairn_store_sqlite::open_sync(&db_path).expect("open store");
    seed_records(&conn, /* active= */ 2, /* tombstoned= */ 0);
    conn.close().expect("close before snapshot");

    let admin = SqliteAdminStateStore::open_in_memory().expect("admin store");
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant operator");
    let ctx = AdminContext::new(op.clone(), AdminRole::Operator);

    let out_dir = temp.path().join("backups");
    let meta = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
    let registry = FileBackupRegistry::new(vault_root.clone());
    let snap_req = snapshot::SnapshotRequest {
        out_dir: out_dir.clone(),
        label: Some("tamper".into()),
        local_machine_id: TEST_MACHINE.into(),
        backup_kind: "snapshot".into(),
    };
    let snap_resp =
        snapshot::run(&ctx, &snap_req, &admin, &meta, &producer, &registry).expect("snapshot run");

    // Tamper the cairn.db member in place (manifest.json kept identical).
    tamper_db_member(&snap_resp.artifact_path);

    let meta2 = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let reader = SqliteSnapshotReader;
    let applier = SqliteSnapshotApplier::new(vault_root.clone());
    let consent = SqliteConsentLog::new(db_path.clone());
    let restore_req = restore::RestoreRequest {
        artifact_path: snap_resp.artifact_path.clone(),
        dry_run: false,
        local_machine_id: TEST_MACHINE.into(),
    };
    let err = restore::run(
        &ctx,
        &restore_req,
        &admin,
        &meta2,
        &reader,
        &applier,
        &consent,
        &registry,
    )
    .expect_err("restore must refuse a tampered artifact");
    assert!(
        matches!(
            err,
            cairn_core::domain::admin::AdminError::IntegrityMismatch { .. }
        ),
        "expected IntegrityMismatch for tampered cairn.db, got {err:?}"
    );
}

/// Round-4 adversarial review #1: a SAME-LENGTH content edit to a vault-tree
/// member (`raw/`/`wiki/`) must be rejected at restore. The tree hash now folds
/// in file *contents*, so the recomputed artifact digest no longer matches the
/// trusted registry anchor — even though every member's name and size are
/// unchanged.
#[test]
fn restore_rejects_artifact_with_tampered_vault_tree_member() {
    let temp = tempfile::tempdir().expect("tempdir");
    let vault_root = temp.path().to_path_buf();
    std::fs::create_dir_all(vault_root.join(".cairn")).unwrap();
    std::fs::create_dir_all(vault_root.join("raw")).unwrap();
    std::fs::write(vault_root.join("raw/note.md"), b"HELLO").expect("seed raw note");
    let db_path = vault_root.join(".cairn/cairn.db");

    let conn = cairn_store_sqlite::open_sync(&db_path).expect("open store");
    seed_records(&conn, /* active= */ 1, /* tombstoned= */ 0);
    conn.close().expect("close before snapshot");

    let admin = SqliteAdminStateStore::open_in_memory().expect("admin store");
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant operator");
    let ctx = AdminContext::new(op.clone(), AdminRole::Operator);

    let out_dir = temp.path().join("backups");
    let meta = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
    let registry = FileBackupRegistry::new(vault_root.clone());
    let snap_req = snapshot::SnapshotRequest {
        out_dir: out_dir.clone(),
        label: Some("tamper-tree".into()),
        local_machine_id: TEST_MACHINE.into(),
        backup_kind: "snapshot".into(),
    };
    let snap_resp =
        snapshot::run(&ctx, &snap_req, &admin, &meta, &producer, &registry).expect("snapshot run");

    // Rewrite raw/note.md to DIFFERENT content of the SAME length (HELLO→WORLD).
    rewrite_member(&snap_resp.artifact_path, "raw/note.md", |d| {
        assert_eq!(d, b"HELLO", "expected the seeded raw note content");
        b"WORLD".to_vec()
    });

    let meta2 = SqliteSnapshotMetadata::new(db_path.clone(), TEST_VAULT.to_string());
    let reader = SqliteSnapshotReader;
    let applier = SqliteSnapshotApplier::new(vault_root.clone());
    let consent = SqliteConsentLog::new(db_path.clone());
    let restore_req = restore::RestoreRequest {
        artifact_path: snap_resp.artifact_path.clone(),
        dry_run: false,
        local_machine_id: TEST_MACHINE.into(),
    };
    let err = restore::run(
        &ctx,
        &restore_req,
        &admin,
        &meta2,
        &reader,
        &applier,
        &consent,
        &registry,
    )
    .expect_err("restore must refuse a same-length vault-tree tamper");
    assert!(
        matches!(
            err,
            cairn_core::domain::admin::AdminError::IntegrityMismatch { .. }
        ),
        "expected IntegrityMismatch for tampered raw/note.md, got {err:?}"
    );
}

/// Append one byte to the `cairn.db` tar member (changing its sha256) while
/// leaving every other member byte-identical. Models post-seal DB tampering.
fn tamper_db_member(path: &std::path::Path) {
    rewrite_member(path, "cairn.db", |mut d| {
        d.push(0xFF);
        d
    });
}

/// Re-pack the artifact at `path`, replacing the `target` member's bytes with
/// `mutate(old_bytes)` and leaving all other members byte-identical. Used to
/// model post-seal tampering of a specific member.
fn rewrite_member(path: &std::path::Path, target: &str, mutate: impl Fn(Vec<u8>) -> Vec<u8>) {
    use std::io::Read as _;

    // Read every member into memory.
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let file = std::fs::File::open(path).expect("open artifact");
        let mut archive = tar::Archive::new(file);
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let name = entry.path().expect("path").to_string_lossy().into_owned();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).expect("read member");
            members.push((name, buf));
        }
    }

    // Re-pack, mutating only the target member.
    let file = std::fs::File::create(path).expect("recreate artifact");
    let mut builder = tar::Builder::new(file);
    for (name, data) in members {
        let data = if name == target { mutate(data) } else { data };
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &name, &data[..])
            .expect("append member");
    }
    builder.finish().expect("finish tar");
}
