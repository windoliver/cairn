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
    let snap_resp = snapshot::run(&ctx, &snap_req, &admin, &meta, &producer, &registry)
        .expect("snapshot run");

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
        let conn_mut =
            cairn_store_sqlite::open_sync(&db_path).expect("reopen db for mutation");
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
    let consent = SqliteConsentLog::new(db_path.clone(), db_path.clone());

    let restore_req = restore::RestoreRequest {
        artifact_path: snap_resp.artifact_path.clone(),
        dry_run: false,
        local_machine_id: TEST_MACHINE.into(),
    };

    let restore_resp =
        restore::run(&ctx, &restore_req, &admin, &meta2, &reader, &applier, &consent)
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
