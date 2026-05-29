//! Spec §6.4 precondition #2: cross-machine restore fails closed with
//! `AdminError::CrossMachineRestore` and the live DB is untouched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test — panics surface immediately as test failures"
)]
#![allow(missing_docs)]

use cairn_core::contract::AdminStateStore as _;
use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminError, AdminRole};
use cairn_core::verbs::admin::{restore, snapshot};
use cairn_store_sqlite::{
    FileBackupRegistry, SqliteAdminStateStore, SqliteConsentLog, SqliteSnapshotApplier,
    SqliteSnapshotMetadata, SqliteSnapshotProducer, SqliteSnapshotReader,
};

const VAULT_ID: &str = "vault-cross-machine";
const MACHINE_A: &str = "MACHINE-A";
const MACHINE_B: &str = "MACHINE-B";

fn seed_minimal_records(conn: &rusqlite::Connection) {
    // 2 active rows — enough to make the snapshot non-trivial.
    // INSERT shape mirrors admin_snapshot_restore_roundtrip.rs.
    for i in 0..2_usize {
        let rid = format!("01REC0CROSS0{i:012}");
        let tid = format!("01TGT0CROSS0{i:012}");
        let hash = format!("sha256:cross{i:065}");
        conn.execute(
            "INSERT INTO records \
               (record_id, target_id, version, path, kind, class, visibility, \
                scope, actor_chain, body, body_hash, created_at, updated_at, \
                active, tombstoned, is_static) \
             VALUES (?1, ?2, 1, 'test/cross', 'user', 'semantic', 'private', \
                     '{\"user\":\"hmn:test\"}', '[]', 'body text', ?3, 1, 1, 1, 0, 0)",
            rusqlite::params![rid, tid, hash],
        )
        .unwrap_or_else(|e| panic!("seed cross {i}: {e}"));
    }
}

#[test]
fn restore_refuses_cross_machine_snapshot() {
    // ── 1. Set up temp vault dir with .cairn/cairn.db ─────────────────────
    let temp = tempfile::tempdir().expect("tempdir");
    let vault_root = temp.path().to_path_buf();
    std::fs::create_dir_all(vault_root.join(".cairn")).unwrap();
    let db_path = vault_root.join(".cairn/cairn.db");

    // Apply migrations and seed 2 active records.
    let conn = cairn_store_sqlite::open_sync(&db_path).expect("open store");
    seed_minimal_records(&conn);
    conn.close().expect("close connection before snapshot");

    // ── 2. Build admin state and grant operator role ───────────────────────
    let admin = SqliteAdminStateStore::open_in_memory().expect("admin store");
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant operator");

    let ctx = AdminContext::new(op.clone(), AdminRole::Operator);

    // ── 3. Snapshot AS MACHINE-A ───────────────────────────────────────────
    let out_dir = temp.path().join("backups");
    let meta = SqliteSnapshotMetadata::new(db_path.clone(), VAULT_ID.to_string());
    let producer = SqliteSnapshotProducer::new(vault_root.clone(), db_path.clone());
    let registry = FileBackupRegistry::new(vault_root.clone());

    let snap_req = snapshot::SnapshotRequest {
        out_dir: out_dir.clone(),
        label: Some("cross-machine".into()),
        local_machine_id: MACHINE_A.into(),
        backup_kind: "snapshot".into(),
    };
    let snap_resp = snapshot::run(&ctx, &snap_req, &admin, &meta, &producer, &registry)
        .expect("snapshot under MACHINE-A");

    // Confirm the manifest recorded machine A.
    assert_eq!(
        snap_resp.manifest.source_machine_id, MACHINE_A,
        "manifest must record the snapshot machine id"
    );

    // ── 4. Capture live DB bytes BEFORE attempted restore ──────────────────
    let db_before = std::fs::read(&db_path).expect("read db before");

    // ── 5. Attempt RESTORE AS MACHINE-B — must fail at precondition gate ──
    let meta2 = SqliteSnapshotMetadata::new(db_path.clone(), VAULT_ID.to_string());
    let reader = SqliteSnapshotReader;
    let applier = SqliteSnapshotApplier::new(vault_root.clone());
    // ConsentLog references db_path for both sides (no forgotten records here).
    let consent = SqliteConsentLog::new(db_path.clone());

    let restore_req = restore::RestoreRequest {
        artifact_path: snap_resp.artifact_path.clone(),
        dry_run: false,
        local_machine_id: MACHINE_B.into(),
    };
    let result = restore::run(
        &ctx,
        &restore_req,
        &admin,
        &meta2,
        &reader,
        &applier,
        &consent,
        &registry,
    );

    // ── 6. Assert typed error ──────────────────────────────────────────────
    match result {
        Err(AdminError::CrossMachineRestore { snapshot, local }) => {
            assert_eq!(snapshot, MACHINE_A, "error must name the snapshot machine");
            assert_eq!(local, MACHINE_B, "error must name the local machine");
        }
        other => panic!("expected CrossMachineRestore, got {other:?}"),
    }

    // ── 7. Assert live DB byte-for-byte unchanged ──────────────────────────
    let db_after = std::fs::read(&db_path).expect("read db after");
    assert_eq!(
        db_before, db_after,
        "live DB must not be mutated when cross-machine gate fails"
    );
}
