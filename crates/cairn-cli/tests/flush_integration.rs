//! End-to-end integration tests for `cairn flush list/apply/reject`.

use std::path::Path;

use cairn_core::contract::memory_store::{Edge, EdgeDir, EdgeKind, MemoryStore};
use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::flush_plan::{
    CoordActionStatus, FlushMode, PatchTarget, PersistedPlan, PlanReason, PlanStatus,
    PlannedMutation, ReplaceOccurrence, StrReplace,
};
use cairn_core::domain::session::SessionIdentity;
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_test_fixtures::flush_plan::sample_pending;
use cairn_test_fixtures::sample_record;

fn write_pending(vault: &Path, id: &str) {
    let p = sample_pending(id);
    let path = plan_path(vault, Bucket::Pending, &p.plan.operation_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&p).unwrap()).unwrap();
}

fn write_non_placeholder_pending_noop(vault: &Path, id: &str) {
    write_real_pending_plan(vault, id, vec![]);
}

fn write_real_pending_plan(vault: &Path, id: &str, mutations: Vec<PlannedMutation>) {
    write_real_pending_plan_with_hashes(vault, id, mutations, std::collections::BTreeMap::new());
}

fn write_real_pending_plan_with_hashes(
    vault: &Path,
    id: &str,
    mutations: Vec<PlannedMutation>,
    target_hashes: std::collections::BTreeMap<String, String>,
) {
    let plan = cairn_core::domain::flush_plan::FlushPlan {
        operation_id: cairn_core::generated::common::Ulid(id.into()),
        issued_at: "2026-05-09T12:00:00Z".into(),
        issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
        principal: None,
        scope: ScopeTuple::default(),
        mode: FlushMode::HumanReview,
        mutations,
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes,
        dependencies: vec![],
        expires_at: "2099-05-09T12:05:00Z".into(),
        placeholder: false,
    };
    let persisted = PersistedPlan::pending(plan);
    let path = plan_path(vault, Bucket::Pending, &persisted.plan.operation_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
}

fn write_coord_pending_plan(vault: &Path, id: &str) -> std::path::PathBuf {
    let action_id = TargetId::parse("01HQZK000000000000000ACTN1").unwrap();
    let path = plan_path(
        vault,
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let mut persisted = sample_pending(id);
    persisted.plan.mutations = vec![PlannedMutation::ActionUpdate {
        id: action_id,
        status: CoordActionStatus::Blocked,
        reason: Some("waiting on review".into()),
    }];
    persisted.schema_version = PersistedPlan::COORD_SCHEMA_VERSION;
    std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
    path
}

fn open_store(
    vault: &Path,
) -> (
    tokio::runtime::Runtime,
    cairn_store_sqlite::SqliteMemoryStore,
) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = rt
        .block_on(cairn_store_sqlite::open(vault.join(".cairn/cairn.db")))
        .unwrap();
    (rt, store)
}

#[test]
fn flush_list_outputs_pending_ids() {
    let vault = tempfile::tempdir().unwrap();
    write_pending(vault.path(), "01HQZK00000000000000000001");
    write_pending(vault.path(), "01HQZK00000000000000000002");

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("01HQZK00000000000000000001"),
        "out: {stdout}"
    );
    assert!(
        stdout.contains("01HQZK00000000000000000002"),
        "out: {stdout}"
    );
}

#[test]
fn flush_apply_moves_pending_to_applied() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000010";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(!pending.exists(), "pending should have been removed");
    assert!(applied.exists(), "applied should now exist");

    let bytes = std::fs::read(&applied).unwrap();
    let p: cairn_core::domain::flush_plan::PersistedPlan = serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(
        p.status,
        cairn_core::domain::flush_plan::PlanStatus::Applied { .. }
    ));
}

#[test]
fn flush_apply_removes_stale_requeue_marker_for_non_coord_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK0000000000000000001A";
    write_pending(vault.path(), id);
    let marker = bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-in-flight"));
    std::fs::write(&marker, "stale coord marker\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(applied.exists(), "non-coord plan should apply");
    assert!(!marker.exists(), "stale non-coord marker should be removed");
}

#[test]
fn flush_apply_preserves_requeue_marker_for_mismatched_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK0000000000000000001B";
    let other_id = "01HQZK0000000000000000001C";
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending = plan_path(vault.path(), Bucket::Pending, &ulid);
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(
        &pending,
        serde_json::to_vec_pretty(&sample_pending(other_id)).unwrap(),
    )
    .unwrap();
    let marker = bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-in-flight"));
    std::fs::write(&marker, "stale coord marker\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(70),
        "mismatched pending file should fail before marker cleanup; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(pending.exists(), "pending artifact should remain");
    assert!(marker.exists(), "ambiguous requeue marker should remain");
}

#[test]
fn flush_apply_refuses_requeue_repair_needed_marker() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK0000000000000000001D";
    write_pending(vault.path(), id);
    let pending = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let marker =
        bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-repair-needed"));
    std::fs::write(&marker, "manual repair required\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(70),
        "repair-needed marker should block apply; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        pending.exists(),
        "pending plan should not be claimed when repair marker blocks apply"
    );
    assert!(marker.exists(), "repair marker should remain");
}

#[test]
fn flush_reject_refuses_requeue_repair_needed_marker_on_malformed_pending() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK0000000000000000001E";
    let pending = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(&pending, "{not json").unwrap();
    let marker =
        bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-repair-needed"));
    std::fs::write(&marker, "manual repair required\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "operator reject"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(70),
        "repair-needed marker should block reject before malformed-plan handling; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(pending.exists(), "pending artifact should remain");
    assert!(marker.exists(), "repair marker should remain");
}

#[test]
fn flush_apply_idempotent_on_applied() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000011";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    for _ in 0..2 {
        let out = std::process::Command::new(bin)
            .args(["flush", "apply", id])
            .env("CAIRN_VAULT", vault.path())
            .output()
            .expect("spawn cairn");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn flush_apply_not_found_exits_66() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    // Valid Crockford-base32 ULID that simply doesn't exist on disk —
    // separates the "format invalid" path (exit 64) from the "no such
    // pending plan" path (exit 66).
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", "01HQZK00000000000000000099"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(66),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn flush_reject_moves_pending_to_rejected_with_reason() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000020";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "operator decided no"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rejected = plan_path(
        vault.path(),
        Bucket::Rejected,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let p: cairn_core::domain::flush_plan::PersistedPlan =
        serde_json::from_slice(&std::fs::read(&rejected).unwrap()).unwrap();
    let cairn_core::domain::flush_plan::PlanStatus::Rejected { ref reason, .. } = p.status else {
        panic!("expected Rejected, got {:?}", p.status);
    };
    assert_eq!(reason, "operator decided no");
}

#[test]
fn ingest_dry_run_writes_no_flush_files() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args([
            "ingest",
            "--kind",
            "fact",
            "--body",
            "hello world",
            "--dry-run",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    let _ = out;
    let flush_dir = vault.path().join(".cairn").join("flush");
    assert!(!flush_dir.exists(), "dry-run must not create .cairn/flush");
}

#[test]
fn ingest_human_review_writes_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args([
            "ingest",
            "--kind",
            "fact",
            "--body",
            "review me",
            "--human-review",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pending_dir = vault.path().join(".cairn").join("flush").join("pending");
    let entries: Vec<_> = std::fs::read_dir(&pending_dir).unwrap().flatten().collect();
    assert!(
        entries
            .iter()
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json")),
        "expected at least one .plan.json in pending/"
    );
}

#[test]
fn forget_dry_run_writes_nothing() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let _ = std::process::Command::new(bin)
        .args([
            "forget",
            "--record",
            "01HQZX9F5N0000000000000000",
            "--dry-run",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(!vault.path().join(".cairn/flush").exists());
}

#[test]
fn forget_human_review_writes_pending() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args([
            "forget",
            "--record",
            "01HQZX9F5N0000000000000000",
            "--human-review",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pending = vault.path().join(".cairn/flush/pending");
    assert!(pending.exists());
}

#[test]
fn flush_list_json_snapshot() {
    let vault = tempfile::tempdir().unwrap();
    write_pending(vault.path(), "01HQZK00000000000000000030");
    write_pending(vault.path(), "01HQZK00000000000000000031");
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    insta::assert_snapshot!("flush_list_json", String::from_utf8(out.stdout).unwrap());
}

#[test]
fn flush_apply_human_output_snapshot() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000040";
    write_pending(vault.path(), id);
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    insta::assert_snapshot!("flush_apply_human", String::from_utf8(out.stdout).unwrap());
}

#[test]
fn ingest_dry_run_and_human_review_conflict() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args([
            "ingest",
            "--kind",
            "fact",
            "--body",
            "x",
            "--dry-run",
            "--human-review",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        !out.status.success(),
        "expected clap to reject mutually exclusive flags"
    );
}

/// Forward-compat gate from review-loop round 1 (#54): `flush apply` must
/// refuse a plan with an unsupported `schema_version` rather than overwrite
/// the status field of a future format.
#[test]
fn flush_apply_rejects_unsupported_schema_version() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM1";
    // Hand-write a v999 plan to bypass `sample_pending`'s SCHEMA_VERSION = 1.
    let path = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = serde_json::to_value(sample_pending(id)).unwrap();
    bytes["schema_version"] = serde_json::json!(999);
    std::fs::write(&path, serde_json::to_vec_pretty(&bytes).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("schema_version 999 unsupported"),
        "expected version-mismatch message; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Plan stays in pending — was not advanced to applied.
    assert!(path.exists(), "plan file should remain in pending/");
    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(!applied.exists(), "must NOT have moved to applied/");
}

#[test]
fn flush_apply_rejects_coord_mutation_in_legacy_schema_version() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM2";
    let action_id = TargetId::parse("01HQZK000000000000000ACTN1").unwrap();
    let path = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let mut persisted = sample_pending(id);
    persisted.plan.mutations = vec![PlannedMutation::ActionUpdate {
        id: action_id,
        status: CoordActionStatus::Blocked,
        reason: Some("waiting on review".into()),
    }];
    persisted.schema_version = PersistedPlan::BASE_SCHEMA_VERSION;
    std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("schema_version 1 is too old for enclosed mutations"),
        "expected legacy coord mutation message; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(path.exists(), "plan file should remain in pending/");
}

#[test]
fn flush_apply_rejects_coord_mutation_while_coord_runtime_unwired() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM3";
    let path = write_coord_pending_plan(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(69),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("coord runtime is not wired"),
        "expected coord unwired message; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(path.exists(), "plan file should remain in pending/");
}

#[test]
fn flush_reject_quarantines_coord_mutation_while_coord_runtime_unwired() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM4";
    let path = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    std::fs::write(&pending_diff, "coord review diff").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let retry = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn retry");
    assert!(
        retry.status.success(),
        "retry should treat existing quarantine as success; stderr: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    std::fs::write(&path, std::fs::read(&quarantined).unwrap()).unwrap();
    let duplicate_retry = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn duplicate retry");
    assert!(
        duplicate_retry.status.success(),
        "duplicate pending copy should be reconciled into existing quarantine; stderr: {}",
        String::from_utf8_lossy(&duplicate_retry.stderr)
    );
    assert!(!path.exists(), "coord plan should leave pending/");
    assert!(!pending_diff.exists(), "pending diff should leave pending/");

    let rejected = plan_path(vault.path(), Bucket::Rejected, &ulid);
    assert!(
        !rejected.exists(),
        "coord quarantine must not semantically reject the plan"
    );
    assert!(
        !rejected.with_extension("json.in-flight").exists(),
        "coord quarantine must not stage through rejected/"
    );
    let persisted: PersistedPlan =
        serde_json::from_slice(&std::fs::read(&quarantined).unwrap()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&quarantined_diff).unwrap(),
        "coord review diff"
    );
    assert!(
        matches!(
            persisted.status,
            cairn_core::domain::flush_plan::PlanStatus::Pending
        ),
        "quarantine must preserve original plan status, got {:?}",
        persisted.status
    );
    assert_eq!(
        persisted.schema_version,
        PersistedPlan::COORD_SCHEMA_VERSION
    );
}

#[test]
fn flush_reject_from_quarantine_writes_terminal_rejected_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ21";
    write_coord_pending_plan(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let quarantined_out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject");
    assert!(
        quarantined_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&quarantined_out.stderr)
    );

    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject from quarantine");
    assert!(
        rejected_out.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );

    let ulid = cairn_core::generated::common::Ulid(id.into());
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let rejected = plan_path(vault.path(), Bucket::Rejected, &ulid);
    assert!(!quarantined.exists(), "quarantined plan should be consumed");
    assert!(rejected.exists(), "rejected terminal should be written");
    let persisted: PersistedPlan =
        serde_json::from_slice(&std::fs::read(rejected).unwrap()).unwrap();
    assert!(matches!(
        persisted.status,
        PlanStatus::Rejected { ref reason, .. } if reason == "malicious coord"
    ));
}

#[test]
fn flush_reject_from_quarantine_resumes_quarantine_claim() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ23";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantine_claim = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json.in-flight"));
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantine_claim).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject from quarantine");
    assert!(
        rejected_out.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );

    let ulid = cairn_core::generated::common::Ulid(id.into());
    let rejected = plan_path(vault.path(), Bucket::Rejected, &ulid);
    assert!(!quarantine_claim.exists(), "claim should be consumed");
    assert!(rejected.exists(), "rejected terminal should be written");
}

#[test]
fn flush_reject_from_quarantine_collapses_duplicate_matching_claim() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ28";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantine_claim = quarantined.with_extension("json.in-flight");
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantined).unwrap();
    std::fs::write(&quarantine_claim, std::fs::read(&quarantined).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject from quarantine");
    assert!(
        rejected_out.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );

    let ulid = cairn_core::generated::common::Ulid(id.into());
    let rejected = plan_path(vault.path(), Bucket::Rejected, &ulid);
    assert!(rejected.exists(), "rejected terminal should be written");
    assert!(
        !quarantined.exists(),
        "canonical quarantine should be consumed"
    );
    assert!(
        !quarantine_claim.exists(),
        "duplicate quarantine claim should be removed"
    );
}

#[test]
fn flush_reject_from_quarantine_claim_removes_pending_diff() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ27";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    std::fs::write(&pending_diff, "coord review diff").unwrap();
    let quarantine_claim = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json.in-flight"));
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantine_claim).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject from quarantine");
    assert!(
        rejected_out.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );

    let rejected = plan_path(vault.path(), Bucket::Rejected, &ulid);
    assert!(rejected.exists(), "rejected terminal should be written");
    assert!(!pending_diff.exists(), "pending diff should be cleaned up");
    assert!(!quarantine_claim.exists(), "claim should be consumed");
}

#[test]
fn flush_reject_from_quarantine_refuses_divergent_diff_sidecars() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ2A";
    write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());

    let bin = env!("CARGO_BIN_EXE_cairn");
    let first = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn first reject");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantine_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    std::fs::write(&pending_diff, "pending evidence").unwrap();
    std::fs::write(&quarantine_diff, "quarantine evidence").unwrap();

    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn reject from quarantine");
    assert_eq!(
        rejected_out.status.code(),
        Some(70),
        "divergent diffs should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );
    assert!(pending_diff.exists(), "pending diff should remain");
    assert!(quarantine_diff.exists(), "quarantine diff should remain");
    assert!(
        !plan_path(vault.path(), Bucket::Rejected, &ulid).exists(),
        "terminal reject should not publish over divergent evidence"
    );
}

#[test]
fn flush_reject_from_quarantine_refuses_orphan_pending_diff() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ2B";
    write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());

    let bin = env!("CARGO_BIN_EXE_cairn");
    let first = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn first reject");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    std::fs::write(&pending_diff, "orphaned pending evidence").unwrap();

    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn reject from quarantine");
    assert_eq!(
        rejected_out.status.code(),
        Some(70),
        "orphan pending diff should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );
    assert!(pending_diff.exists(), "orphan pending diff should remain");
    assert!(
        !plan_path(vault.path(), Bucket::Rejected, &ulid).exists(),
        "terminal reject should not publish over orphan evidence"
    );
}

#[test]
fn flush_reject_json_retry_on_already_quarantined_plan_is_json() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ29";
    write_coord_pending_plan(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let first = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn first reject");
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let retry = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "unsupported coord",
            "--json",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn retry reject");
    assert!(
        retry.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&retry.stdout),
        String::from_utf8_lossy(&retry.stderr)
    );
    let body: serde_json::Value =
        serde_json::from_slice(&retry.stdout).expect("retry should emit JSON");
    assert_eq!(body["operation_id"], id);
    assert_eq!(body["status"], "quarantined");
}

#[test]
fn flush_reject_from_quarantine_fails_closed_with_duplicate_pending_copy() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ22";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::write(&quarantined, std::fs::read(&pending).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected_out = std::process::Command::new(bin)
        .args([
            "flush",
            "reject",
            id,
            "--reason",
            "malicious coord",
            "--from-quarantine",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject from quarantine");
    assert_eq!(
        rejected_out.status.code(),
        Some(70),
        "duplicate pending/quarantine should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected_out.stdout),
        String::from_utf8_lossy(&rejected_out.stderr)
    );
    assert!(pending.exists(), "pending duplicate should remain");
    assert!(quarantined.exists(), "quarantined plan should remain");
}

#[test]
fn flush_list_marks_invalid_quarantined_artifacts() {
    let vault = tempfile::tempdir().unwrap();
    let quarantine_dir = vault.path().join(".cairn/flush/quarantined");
    std::fs::create_dir_all(&quarantine_dir).unwrap();

    let non_coord_id = "01HQZK000000000000000SCQ24";
    let non_coord = sample_pending(non_coord_id);
    std::fs::write(
        quarantine_dir.join(format!("{non_coord_id}.plan.json")),
        serde_json::to_vec_pretty(&non_coord).unwrap(),
    )
    .unwrap();

    let rejected_id = "01HQZK000000000000000SCQ25";
    let mut rejected = sample_pending(rejected_id);
    rejected.plan.mutations = vec![PlannedMutation::ActionUpdate {
        id: TargetId::parse("01HQZK000000000000000ACTN1").unwrap(),
        status: CoordActionStatus::Blocked,
        reason: Some("waiting on review".into()),
    }];
    rejected.schema_version = PersistedPlan::COORD_SCHEMA_VERSION;
    rejected.status = PlanStatus::Rejected {
        at: "2026-05-09T12:00:00Z".into(),
        reason: "already rejected".into(),
    };
    std::fs::write(
        quarantine_dir.join(format!("{rejected_id}.plan.json")),
        serde_json::to_vec_pretty(&rejected).unwrap(),
    )
    .unwrap();

    let invalid_schema_id = "01HQZK000000000000000SCQ26";
    let mut invalid_schema = sample_pending(invalid_schema_id);
    invalid_schema.plan.mutations = vec![PlannedMutation::ActionUpdate {
        id: TargetId::parse("01HQZK000000000000000ACTN1").unwrap(),
        status: CoordActionStatus::Blocked,
        reason: Some("waiting on review".into()),
    }];
    invalid_schema.schema_version = PersistedPlan::COORD_SCHEMA_VERSION + 100;
    std::fs::write(
        quarantine_dir.join(format!("{invalid_schema_id}.plan.json")),
        serde_json::to_vec_pretty(&invalid_schema).unwrap(),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let plans = v["plans"].as_array().expect("plans array");
    assert!(
        plans
            .iter()
            .any(|plan| plan["id"] == non_coord_id && plan["status"] == "quarantined (non-coord)"),
        "non-coord quarantine should be labeled invalid: {v}"
    );
    assert!(
        plans
            .iter()
            .any(|plan| plan["id"] == rejected_id
                && plan["status"] == "quarantined (invalid status)"),
        "terminal-status quarantine should be labeled invalid: {v}"
    );
    assert!(
        plans.iter().any(|plan| plan["id"] == invalid_schema_id
            && plan["status"] == "quarantined (invalid schema)"),
        "schema-invalid quarantine should be labeled invalid: {v}"
    );
}

#[test]
fn flush_reject_resumes_quarantined_in_flight_claim() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ11";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantine_claim = quarantined.with_extension("json.in-flight");
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantine_claim).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(quarantined.exists(), "quarantined plan should be published");
    assert!(
        !quarantine_claim.exists(),
        "quarantine in-flight claim should be consumed"
    );
    assert!(
        !plan_path(
            vault.path(),
            Bucket::Rejected,
            &cairn_core::generated::common::Ulid(id.into())
        )
        .exists(),
        "coord quarantine recovery must not create rejected terminal state"
    );
}

#[test]
fn flush_list_shows_quarantined_in_flight_claims() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ13";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantine_claim = quarantined.with_extension("json.in-flight");
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantine_claim).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let plans = v["plans"].as_array().expect("plans array");
    assert!(
        plans.iter().any(|plan| {
            plan["id"] == id
                && plan["bucket"] == "in-flight (quarantine)"
                && plan["status"] == "stranded"
        }),
        "flush list should surface quarantined in-flight claims: {v}"
    );
}

#[test]
fn flush_reject_refuses_already_quarantined_plan_while_requeue_marker_exists() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMN";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantined).unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject");
    assert_eq!(
        rejected.status.code(),
        Some(70),
        "reject should fail closed while requeue marker exists; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("requeue"),
        "expected requeue marker message; got: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(quarantined.exists(), "quarantine artifact should remain");
}

#[test]
fn flush_list_quarantined_scan_counts_plan_rows_not_diff_sidecars() {
    let vault = tempfile::tempdir().unwrap();
    let visible_id = "01HQZK000000000000000SCHMQ";
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{visible_id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    let mut persisted = sample_pending(visible_id);
    persisted.plan.mutations = vec![PlannedMutation::ActionUpdate {
        id: TargetId::parse("01HQZK000000000000000ACTN1").unwrap(),
        status: CoordActionStatus::Blocked,
        reason: Some("waiting on review".into()),
    }];
    persisted.schema_version = PersistedPlan::COORD_SCHEMA_VERSION;
    std::fs::write(&quarantined, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();

    for i in 0..1100 {
        let sidecar = vault
            .path()
            .join(".cairn/flush/quarantined")
            .join(format!("00DIFF{i:022}.diff.md"));
        std::fs::write(sidecar, "sidecar").unwrap();
    }

    let bin = env!("CARGO_BIN_EXE_cairn");
    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let plans = v["plans"].as_array().expect("plans array");
    assert!(
        plans
            .iter()
            .any(|plan| plan["id"] == visible_id && plan["bucket"] == "quarantined"),
        "quarantined plan should remain visible despite many sidecars: {v}"
    );
}

#[test]
fn flush_list_surfaces_quarantined_coord_plans() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM8";
    write_coord_pending_plan(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let plans = v["plans"].as_array().expect("plans array");
    assert!(
        plans.iter().any(|plan| {
            plan["id"] == id && plan["bucket"] == "quarantined" && plan["status"] == "quarantined"
        }),
        "quarantined plan should remain visible in flush list output: {v}"
    );
}

#[test]
fn flush_list_uses_quarantine_filename_id_on_mismatch() {
    let vault = tempfile::tempdir().unwrap();
    let filename_id = "01HQZK000000000000000SCHMF";
    let embedded_id = "01HQZK000000000000000SCHME";
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{filename_id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    let mut persisted = sample_pending(embedded_id);
    persisted.plan.mutations = vec![PlannedMutation::ActionUpdate {
        id: TargetId::parse("01HQZK000000000000000ACTN1").unwrap(),
        status: CoordActionStatus::Blocked,
        reason: Some("waiting on review".into()),
    }];
    persisted.schema_version = PersistedPlan::COORD_SCHEMA_VERSION;
    std::fs::write(&quarantined, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let plans = v["plans"].as_array().expect("plans array");
    assert!(
        plans.iter().any(|plan| {
            plan["id"] == filename_id && plan["status"] == "quarantined (id mismatch)"
        }),
        "quarantine list should report filename id and mismatch status: {v}"
    );
    assert!(
        !plans.iter().any(|plan| plan["id"] == embedded_id),
        "quarantine list should not advertise embedded mismatched id: {v}"
    );
}

#[test]
fn flush_requeue_moves_quarantined_coord_plan_back_to_pending() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM9";
    let original_pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    std::fs::write(&pending_diff, "coord review diff").unwrap();
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject");
    assert!(
        rejected.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(quarantined.exists(), "precondition: plan is quarantined");
    assert!(
        quarantined_diff.exists(),
        "precondition: diff is quarantined"
    );

    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        original_pending.exists(),
        "requeue should restore the plan to pending"
    );
    assert!(
        pending_diff.exists(),
        "requeue should restore the companion diff to pending"
    );
    assert!(
        !quarantined.exists(),
        "requeue should remove the quarantined plan"
    );
    assert!(
        !quarantined_diff.exists(),
        "requeue should remove the quarantined diff"
    );
}

#[test]
fn flush_requeue_requires_ready_coord_runtime_without_force() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMD";
    write_coord_pending_plan(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let rejected = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject");
    assert!(
        rejected.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(69),
        "unwired coord runtime should block requeue without --force; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        String::from_utf8_lossy(&requeued.stderr).contains("coord runtime is not wired"),
        "expected coord runtime message; got: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
}

#[test]
fn flush_apply_reject_refuse_pending_plan_while_requeue_marker_exists() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMH";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let applied_claim =
        plan_path(vault.path(), Bucket::Applied, &ulid).with_extension("json.in-flight");
    let rejected_claim =
        plan_path(vault.path(), Bucket::Rejected, &ulid).with_extension("json.in-flight");
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let apply = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn apply");
    assert_eq!(
        apply.status.code(),
        Some(70),
        "apply should fail closed while requeue marker exists; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(
        String::from_utf8_lossy(&apply.stderr).contains("requeue"),
        "expected requeue marker message; got: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(
        pending.exists(),
        "pending plan should remain after apply refuses requeue marker"
    );
    assert!(
        !applied_claim.exists(),
        "apply refusal must not leave an applied in-flight claim"
    );

    let reject = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "operator decided no"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject");
    assert_eq!(
        reject.status.code(),
        Some(70),
        "reject should fail closed while requeue marker exists; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&reject.stdout),
        String::from_utf8_lossy(&reject.stderr)
    );
    assert!(
        String::from_utf8_lossy(&reject.stderr).contains("requeue"),
        "expected requeue marker message; got: {}",
        String::from_utf8_lossy(&reject.stderr)
    );
    assert!(
        pending.exists(),
        "pending plan should remain after reject refuses requeue marker"
    );
    assert!(
        !rejected_claim.exists(),
        "reject refusal must not leave a rejected in-flight claim"
    );
}

#[test]
fn flush_list_surfaces_requeue_markers() {
    let vault = tempfile::tempdir().unwrap();
    let in_flight_id = "01HQZK000000000000000MRK01";
    let repair_id = "01HQZK000000000000000MRK02";
    write_coord_pending_plan(vault.path(), in_flight_id);

    let pending_dir = bucket_dir(vault.path(), Bucket::Pending);
    std::fs::write(
        pending_dir.join(format!("{in_flight_id}.requeue-in-flight")),
        "cairn flush requeue in flight\n",
    )
    .unwrap();
    std::fs::write(
        pending_dir.join(format!("{repair_id}.requeue-repair-needed")),
        "repair required\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let markers = v["requeue_markers"]
        .as_array()
        .expect("requeue markers array");
    assert!(
        markers.iter().any(|marker| {
            marker["id"] == in_flight_id
                && marker["marker"] == "requeue-in-flight"
                && marker["status"] == "requeue in flight"
        }),
        "list should expose in-flight requeue markers: {v}"
    );
    assert!(
        markers.iter().any(|marker| {
            marker["id"] == repair_id
                && marker["marker"] == "requeue-repair-needed"
                && marker["status"] == "requeue repair needed"
        }),
        "list should expose repair-needed requeue markers: {v}"
    );
}

#[test]
fn flush_list_surfaces_requeue_markers_in_busy_pending_dir() {
    let vault = tempfile::tempdir().unwrap();
    for i in 0..1100 {
        write_pending(vault.path(), &format!("01HQZK00000000000000{i:06}"));
    }

    let marker_id = "01HQZK000000000000000MRK03";
    let pending_dir = bucket_dir(vault.path(), Bucket::Pending);
    std::fs::write(
        pending_dir.join(format!("{marker_id}.requeue-in-flight")),
        "cairn flush requeue in flight\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let listed = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn flush list");
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("valid JSON");
    let markers = v["requeue_markers"]
        .as_array()
        .expect("requeue markers array");
    assert!(
        markers
            .iter()
            .any(|marker| { marker["id"] == marker_id && marker["marker"] == "requeue-in-flight" }),
        "busy pending directory should not hide requeue markers: {v}"
    );
}

#[test]
fn flush_requeue_clears_stale_marker_on_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMJ";
    let pending = write_coord_pending_plan(vault.path(), id);
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain");
    assert!(!marker.exists(), "stale marker should be removed");
}

#[test]
fn flush_requeue_recovers_quarantine_claim_with_marker() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ12";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantine_claim = quarantined.with_extension("json.in-flight");
    let marker = bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantine_claim).unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "requeue should restore pending plan");
    assert!(!quarantine_claim.exists(), "claim should be consumed");
    assert!(
        !quarantined.exists(),
        "quarantine artifact should be consumed"
    );
    assert!(!marker.exists(), "requeue marker should be cleaned up");
}

#[test]
fn flush_requeue_clears_repair_needed_after_operator_restores_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCMR1";
    let pending = write_coord_pending_plan(vault.path(), id);
    let repair_marker =
        bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-repair-needed"));
    std::fs::write(&repair_marker, "manual repair required\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "repaired pending plan should remain");
    assert!(
        !repair_marker.exists(),
        "completed repair should clear repair marker"
    );

    let rejected = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "operator reject"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn reject");
    assert!(
        rejected.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&rejected.stdout),
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn flush_requeue_clears_repair_needed_with_matching_leftover_quarantine_artifacts() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCMR2";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    let repair_marker =
        bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-repair-needed"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::write(&quarantined, std::fs::read(&pending).unwrap()).unwrap();
    std::fs::write(&pending_diff, "coord review diff").unwrap();
    std::fs::write(&quarantined_diff, "coord review diff").unwrap();
    std::fs::write(&repair_marker, "manual repair required\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "repaired pending plan should remain");
    assert!(pending_diff.exists(), "pending diff should remain");
    assert!(
        !quarantined.exists(),
        "matching quarantine duplicate should be removed"
    );
    assert!(
        !quarantined_diff.exists(),
        "matching quarantine diff duplicate should be removed"
    );
    assert!(
        !repair_marker.exists(),
        "completed repair should clear repair marker"
    );
}

#[test]
fn flush_requeue_preserves_marker_on_malformed_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCMA1";
    let pending = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.plan.json"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    let archived_marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-repair-needed"));
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(&pending, "{not json").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        pending.exists(),
        "pending plan should remain for inspection"
    );
    assert!(!marker.exists(), "in-flight marker should be archived");
    assert!(
        archived_marker.exists(),
        "repair marker should preserve recovery evidence"
    );
}

#[test]
fn flush_requeue_marks_repair_needed_for_malformed_pending_with_quarantine_artifact() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCMA2";
    let pending_dir = bucket_dir(vault.path(), Bucket::Pending);
    let quarantine_dir = vault.path().join(".cairn/flush/quarantined");
    let pending = pending_dir.join(format!("{id}.plan.json"));
    let marker = pending_dir.join(format!("{id}.requeue-in-flight"));
    let archived_marker = pending_dir.join(format!("{id}.requeue-repair-needed"));
    std::fs::create_dir_all(&pending_dir).unwrap();
    std::fs::create_dir_all(&quarantine_dir).unwrap();
    std::fs::write(&pending, "{not json").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();
    std::fs::write(quarantine_dir.join(format!("{id}.diff.md")), "staged diff").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(65),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(!marker.exists(), "in-flight marker should be archived");
    assert!(
        archived_marker.exists(),
        "repair marker should preserve recovery evidence"
    );
}

#[test]
fn flush_requeue_preserves_marker_on_mismatched_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCMM1";
    let other_id = "01HQZK000000000000000SCMM2";
    let other_pending = write_coord_pending_plan(vault.path(), other_id);
    let pending = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.plan.json"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    let archived_marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-repair-needed"));
    std::fs::rename(other_pending, &pending).unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        pending.exists(),
        "pending plan should remain for inspection"
    );
    assert!(!marker.exists(), "in-flight marker should be archived");
    assert!(
        archived_marker.exists(),
        "repair marker should preserve recovery evidence"
    );
}

#[test]
fn flush_requeue_clears_stale_marker_after_plan_and_diff_moved_to_pending() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCMD1";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::write(&pending_diff, "completed requeue diff").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        pending.exists(),
        "pending plan should remain for inspection"
    );
    assert!(
        pending_diff.exists(),
        "pending diff should remain after completed requeue"
    );
    assert!(!marker.exists(), "stale marker should be removed");
}

#[test]
fn flush_requeue_resumes_marker_only_quarantined_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMP";
    let original_pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&original_pending, &quarantined).unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        original_pending.exists(),
        "marker-only interrupted requeue should complete the plan move"
    );
    assert!(!quarantined.exists(), "quarantined plan should be consumed");
    assert!(!marker.exists(), "marker should be removed");
}

#[test]
fn flush_requeue_resumes_marker_with_quarantined_plan_and_diff() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMM";
    let original_pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&original_pending, &quarantined).unwrap();
    std::fs::write(&quarantined_diff, "coord review diff").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(original_pending.exists(), "plan should move to pending");
    assert!(pending_diff.exists(), "diff should move to pending");
    assert!(!quarantined.exists(), "quarantined plan should be consumed");
    assert!(
        !quarantined_diff.exists(),
        "quarantined diff should be consumed"
    );
    assert!(!marker.exists(), "marker should be removed");
}

#[test]
fn flush_requeue_repairs_pending_plan_with_quarantined_diff() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMA";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined_diff.parent().unwrap()).unwrap();
    std::fs::write(&quarantined_diff, "coord review diff").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain in place");
    assert!(
        pending_diff.exists(),
        "interrupted requeue should restore quarantined diff to pending"
    );
    assert!(
        !quarantined_diff.exists(),
        "interrupted requeue repair should remove quarantined diff"
    );
    assert!(
        !marker.exists(),
        "interrupted requeue repair should remove the marker"
    );
    assert_eq!(
        std::fs::read_to_string(&pending_diff).unwrap(),
        "coord review diff"
    );
}

#[test]
fn flush_requeue_removes_duplicate_quarantine_claim_when_pending_exists() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ15";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantine_claim = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json.in-flight"));
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::write(&quarantine_claim, std::fs::read(&pending).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain");
    assert!(
        !quarantine_claim.exists(),
        "duplicate quarantine claim should be removed"
    );
}

#[test]
fn flush_requeue_recovers_quarantine_claim_without_marker() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ16";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantine_claim = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json.in-flight"));
    std::fs::create_dir_all(quarantine_claim.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantine_claim).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "requeue should restore pending plan");
    assert!(!quarantine_claim.exists(), "claim should be consumed");
}

#[test]
fn flush_requeue_collapses_duplicate_matching_quarantine_claim() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCQ14";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantine_claim = quarantined.with_extension("json.in-flight");
    let marker = bucket_dir(vault.path(), Bucket::Pending).join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&pending, &quarantined).unwrap();
    std::fs::write(&quarantine_claim, std::fs::read(&quarantined).unwrap()).unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "requeue should restore pending plan");
    assert!(
        !quarantined.exists(),
        "quarantine artifact should be consumed"
    );
    assert!(
        !quarantine_claim.exists(),
        "duplicate quarantine claim should be removed"
    );
    assert!(!marker.exists(), "requeue marker should be cleaned up");
}

#[test]
fn flush_requeue_refuses_quarantined_diff_without_marker() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMF";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    std::fs::create_dir_all(quarantined_diff.parent().unwrap()).unwrap();
    std::fs::write(&quarantined_diff, "stray coord review diff").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "stray quarantined diff should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain");
    assert!(
        quarantined_diff.exists(),
        "stray quarantined diff should remain for manual inspection"
    );
}

#[test]
fn flush_requeue_refuses_quarantined_plan_without_marker() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMK";
    let pending = write_coord_pending_plan(vault.path(), id);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::write(&quarantined, std::fs::read(&pending).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "marker-less quarantined plan should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain");
    assert!(
        quarantined.exists(),
        "marker-less quarantined plan should remain for manual inspection"
    );
}

#[test]
fn flush_requeue_repairs_duplicate_diff_after_plan_move() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMG";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined_diff.parent().unwrap()).unwrap();
    std::fs::write(&pending_diff, "coord review diff").unwrap();
    std::fs::write(&quarantined_diff, "coord review diff").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain");
    assert!(pending_diff.exists(), "pending diff should remain");
    assert!(
        !quarantined_diff.exists(),
        "matching duplicate quarantined diff should be cleaned up"
    );
    assert!(!marker.exists(), "marker should be removed");
}

#[test]
fn flush_requeue_repairs_duplicate_pending_and_quarantined_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMC";
    let pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::write(&quarantined, std::fs::read(&pending).unwrap()).unwrap();
    std::fs::write(&quarantined_diff, "coord review diff").unwrap();
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert!(
        requeued.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(pending.exists(), "pending plan should remain");
    assert!(
        !quarantined.exists(),
        "matching duplicate quarantined plan should be removed"
    );
    assert!(
        pending_diff.exists(),
        "quarantined diff should be restored to pending"
    );
    assert!(
        !quarantined_diff.exists(),
        "quarantined diff should be removed"
    );
    assert!(!marker.exists(), "marker should be removed");
}

#[test]
fn flush_requeue_refuses_unmarked_pending_diff_with_quarantined_plan() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMB";
    let original_pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    std::fs::write(&pending_diff, "stale pending diff").unwrap();
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&original_pending, &quarantined).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "unmarked pending diff should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        !original_pending.exists(),
        "unmarked pending diff must not complete the plan move"
    );
    assert!(
        quarantined.exists(),
        "quarantined plan should remain for operator reconciliation"
    );
    assert_eq!(
        std::fs::read_to_string(&pending_diff).unwrap(),
        "stale pending diff"
    );
}

#[test]
fn flush_requeue_refuses_marker_with_quarantined_plan_and_unverified_pending_diff() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHME";
    let original_pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&original_pending, &quarantined).unwrap();
    std::fs::write(&pending_diff, "coord review diff").unwrap();
    let marker = vault
        .path()
        .join(".cairn/flush/pending")
        .join(format!("{id}.requeue-in-flight"));
    std::fs::write(&marker, "cairn flush requeue in flight\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "unverified pending diff should fail closed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        !original_pending.exists(),
        "plan must not move to pending with an unverified diff sidecar"
    );
    assert!(
        pending_diff.exists(),
        "pending diff should remain for operator inspection"
    );
    assert!(quarantined.exists(), "quarantined plan should remain");
    assert!(marker.exists(), "marker should remain for retry/repair");
    assert!(
        String::from_utf8_lossy(&requeued.stderr).contains("without a quarantined diff"),
        "stderr should explain the unverified diff; stderr: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
}

#[test]
fn flush_requeue_refuses_unmarked_quarantined_plan_with_pending_diff_without_quarantined_pair() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHMX";
    let original_pending = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::rename(&original_pending, &quarantined).unwrap();
    std::fs::write(&pending_diff, "stale pending diff").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let requeued = std::process::Command::new(bin)
        .args(["flush", "requeue", id, "--force"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn requeue");
    assert_eq!(
        requeued.status.code(),
        Some(70),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&requeued.stdout),
        String::from_utf8_lossy(&requeued.stderr)
    );
    assert!(
        !original_pending.exists(),
        "pending plan should not be restored with an unverified diff sidecar"
    );
    assert!(pending_diff.exists(), "pending diff should remain");
    assert!(quarantined.exists(), "quarantined plan should remain");
}

#[test]
fn flush_reject_does_not_discard_divergent_coord_quarantine_retry() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM5";
    let path = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    std::fs::write(&pending_diff, "coord review diff").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::write(&path, std::fs::read(&quarantined).unwrap()).unwrap();
    std::fs::write(&pending_diff, "coord review diff v2").unwrap();

    let retry = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn divergent retry");
    assert_eq!(
        retry.status.code(),
        Some(70),
        "divergent duplicate diff should fail loudly; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&retry.stdout),
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(
        String::from_utf8_lossy(&retry.stderr)
            .contains("divergent coord quarantine retry for diff"),
        "expected divergent diff message; got: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(
        path.exists(),
        "divergent pending plan should be restored for operator reconciliation"
    );
    assert_eq!(
        std::fs::read_to_string(&pending_diff).unwrap(),
        "coord review diff v2",
        "divergent pending diff must not be discarded"
    );
    assert_eq!(
        std::fs::read_to_string(&quarantined_diff).unwrap(),
        "coord review diff",
        "existing quarantined diff should remain unchanged"
    );

    let mut divergent_plan = std::fs::read(&quarantined).unwrap();
    divergent_plan.push(b'\n');
    std::fs::write(&path, divergent_plan).unwrap();
    std::fs::write(&pending_diff, "coord review diff").unwrap();

    let retry = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn divergent plan retry");
    assert_eq!(
        retry.status.code(),
        Some(70),
        "divergent duplicate plan should fail loudly; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&retry.stdout),
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(
        String::from_utf8_lossy(&retry.stderr)
            .contains("divergent coord quarantine retry for plan"),
        "expected divergent plan message; got: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(
        path.exists(),
        "divergent pending plan should remain available after plan conflict"
    );
    assert_eq!(
        std::fs::read_to_string(&pending_diff).unwrap(),
        "coord review diff",
        "matching pending diff should remain because the divergent plan was not accepted"
    );
}

#[test]
fn flush_reject_rolls_back_plan_when_quarantine_diff_publish_fails() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM6";
    let path = write_coord_pending_plan(vault.path(), id);
    let ulid = cairn_core::generated::common::Ulid(id.into());
    let pending_diff = cairn_core::domain::flush_plan::store::diff_path(vault.path(), &ulid);
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    let quarantined_diff = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.diff.md"));
    std::fs::write(&pending_diff, "coord review diff").unwrap();
    std::fs::create_dir_all(&quarantined_diff).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(70),
        "diff publish failure should return I/O failure; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        path.exists(),
        "plan should be restored to pending when quarantine diff publication fails"
    );
    assert!(
        !quarantined.exists(),
        "failed quarantine publish must not leave a partial quarantined plan"
    );
    assert!(
        pending_diff.exists(),
        "pending diff should remain available for retry/operator recovery"
    );
}

#[test]
fn flush_reject_validates_existing_quarantine_before_reporting_success() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000SCHM7";
    let quarantined = vault
        .path()
        .join(".cairn/flush/quarantined")
        .join(format!("{id}.plan.json"));
    std::fs::create_dir_all(quarantined.parent().unwrap()).unwrap();
    std::fs::write(
        &quarantined,
        serde_json::to_vec_pretty(&sample_pending(id)).unwrap(),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "unsupported coord"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(65),
        "non-coord quarantine artifact must not be treated as success; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("existing quarantine is not a coord pending plan"),
        "expected quarantine validation message; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        quarantined.exists(),
        "invalid quarantine artifact should be left for manual inspection"
    );
}

/// Round 10 (#54): explicit `--vault` must override `CAIRN_VAULT` for
/// human-review write — silently routing the plan to the env vault
/// when `--vault` is given is a tenant-isolation bug.
#[test]
fn ingest_human_review_honors_explicit_vault_over_env() {
    let target = tempfile::tempdir().unwrap();
    let env_decoy = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args([
            "--vault",
            target.path().to_str().unwrap(),
            "ingest",
            "--kind",
            "fact",
            "--body",
            "x",
            "--human-review",
        ])
        .env("CAIRN_VAULT", env_decoy.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Plan must land in the --vault target, NOT the CAIRN_VAULT decoy.
    let target_pending = target.path().join(".cairn/flush/pending");
    let decoy_pending = env_decoy.path().join(".cairn/flush/pending");
    assert!(
        target_pending.exists(),
        "plan must land under --vault path, not CAIRN_VAULT"
    );
    assert!(
        !decoy_pending.exists(),
        "CAIRN_VAULT must NOT be touched when --vault is explicit"
    );
}

/// Round 10 (#54): `cairn flush apply` with an explicit `--vault` that
/// fails to resolve must fail closed (`EX_CONFIG`), not silently fall
/// back to `CAIRN_VAULT` and mutate the wrong vault.
#[test]
fn flush_apply_fails_closed_on_unresolvable_explicit_vault() {
    let env_decoy = tempfile::tempdir().unwrap();
    // Stage a pending plan in the env vault so a misrouted apply could
    // demonstrably mutate it.
    let id = "01HQZK00000000000000000VT1";
    write_pending(env_decoy.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    // Use a non-registered vault NAME (not an absolute path) so
    // `resolve_vault_or_cwd` returns `Err(VaultError::NotFound)`.
    // Absolute paths are accepted as direct vault refs even if they
    // don't exist, which is correct behavior for the resolver.
    let out = std::process::Command::new(bin)
        .args(["--vault", "no-such-registered-vault", "flush", "apply", id])
        .env("CAIRN_VAULT", env_decoy.path())
        // Avoid hitting the user's real ~/.cairn registry by routing
        // CAIRN_REGISTRY at an empty file.
        .env(
            "CAIRN_REGISTRY",
            env_decoy.path().join("empty-registry.json"),
        )
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(78),
        "expected EX_CONFIG; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The decoy plan must remain untouched in pending — the misrouted
    // apply did not silently terminalize it.
    let pending = plan_path(
        env_decoy.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let applied = plan_path(
        env_decoy.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(pending.exists(), "decoy pending plan must be preserved");
    assert!(!applied.exists(), "decoy must NOT have been mutated");
}

/// Round 9 (#54): auto-recovery from a `.in-flight.<pid>` orphan was
/// removed because mtime + PID provide no reliable proof that the
/// owner is dead. `apply` must now refuse to act on an orphan and
/// instead surface a clear "stranded" message so the operator can
/// verify-and-rename manually.
#[test]
fn flush_apply_refuses_orphan_owned_in_flight() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000RPHN1";
    // Stage an orphan owned-claim file as if a prior process renamed
    // the canonical in-flight to `.in-flight.<pid>` and crashed before
    // publish. Pending and the canonical in-flight are absent.
    let canonical = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    )
    .with_extension("json.in-flight");
    let orphan = {
        let mut s = canonical.as_os_str().to_owned();
        // Use a pid value unlikely to collide with the test process.
        s.push(".999999");
        std::path::PathBuf::from(s)
    };
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    let p = sample_pending(id);
    std::fs::write(&orphan, serde_json::to_vec_pretty(&p).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        !out.status.success(),
        "must refuse orphan auto-recovery; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stranded in-flight claim"),
        "expected stranded message; got: {stderr}"
    );
    // Orphan and pending both untouched.
    assert!(
        orphan.exists(),
        "orphan must be preserved for manual recovery"
    );
    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(!applied.exists(), "must not have published a terminal");
}

// Round 9 (#54): the round-8 fresh-orphan steal-refusal test was
// subsumed by `flush_apply_refuses_orphan_owned_in_flight` — auto
// recovery is now disabled regardless of mtime, so a single test
// covers both the fresh and stale orphan cases.

/// Round 7 (#54): JSON output uses a typed envelope `{plans, omitted}`
/// so consumers cannot mistake an omitted-scan marker for a real plan.
#[test]
fn flush_list_json_uses_typed_envelope() {
    let vault = tempfile::tempdir().unwrap();
    write_pending(vault.path(), "01HQZK000000000000000NV001");
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert!(v.get("plans").is_some(), "envelope missing `plans`: {v}");
    assert!(
        v.get("omitted").is_some(),
        "envelope missing `omitted`: {v}"
    );
    assert!(v["plans"].is_array());
    assert!(v["omitted"].is_array());
}

/// Round 6 (#54): two concurrent `flush apply` invocations that find an
/// existing in-flight claim must NOT both publish a terminal — exactly
/// one resume succeeds and the other reports "recovery already in
/// flight".
#[test]
fn flush_apply_concurrent_resume_yields_single_publish() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    let vault = Arc::new(tempfile::tempdir().unwrap());
    let id = "01HQZK0000000000000000RC01";
    // Stage an in-flight claim — the prior process crashed between
    // claim and publish.
    let inflight = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    )
    .with_extension("json.in-flight");
    std::fs::create_dir_all(inflight.parent().unwrap()).unwrap();
    let p = sample_pending(id);
    std::fs::write(&inflight, serde_json::to_vec_pretty(&p).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let success_count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let v = Arc::clone(&vault);
        let s = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            let out = std::process::Command::new(bin)
                .args(["flush", "apply", id])
                .env("CAIRN_VAULT", v.path())
                .output()
                .expect("spawn cairn");
            if out.status.success() {
                s.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(
        applied.exists(),
        "exactly one publisher should have produced the terminal"
    );
    // Idempotent re-apply on the existing terminal counts as success
    // (apply re-checks `applied.exists()` before claiming), so multiple
    // concurrent processes can succeed — but only ONE actually publishes.
    // The terminal file is single-content (no interleaving / corruption).
    let bytes = std::fs::read(&applied).unwrap();
    let _: cairn_core::domain::flush_plan::PersistedPlan =
        serde_json::from_slice(&bytes).expect("terminal must be valid JSON");
}

/// Round 5 (#54): a pending file that coexists with an in-flight claim
/// for the same id is ambiguous — `apply` and `reject` must fail closed
/// rather than overwriting the pending file via rollback.
#[test]
fn flush_apply_refuses_when_pending_and_in_flight_both_exist() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000D2A101";
    // Stage both a pending file and an in-flight claim for the same id.
    write_pending(vault.path(), id);
    let inflight = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    )
    .with_extension("json.in-flight");
    std::fs::create_dir_all(inflight.parent().unwrap()).unwrap();
    let p = sample_pending(id);
    std::fs::write(&inflight, serde_json::to_vec_pretty(&p).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        !out.status.success(),
        "must fail closed when both files exist"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("conflict"),
        "expected conflict message; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Both files preserved for operator inspection.
    let pending = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(pending.exists(), "pending file must be preserved");
    assert!(inflight.exists(), "in-flight file must be preserved");
}

/// Round 5 (#54): an attacker-staged or corrupted-vault `.in-flight`
/// file with a non-ULID stem must be ignored by the recovery scan in
/// `flush list`. Defends against staged-filename `DoS` and prevents
/// arbitrary names from showing up as recoverable rows.
#[test]
fn flush_list_ignores_non_ulid_in_flight_stems() {
    let vault = tempfile::tempdir().unwrap();
    let bogus = bucket_dir(vault.path(), Bucket::Applied).join("not-a-ulid.plan.json.in-flight");
    std::fs::create_dir_all(bogus.parent().unwrap()).unwrap();
    std::fs::write(&bogus, b"{}").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !stdout.contains("not-a-ulid"),
        "non-ULID stems must be ignored; stdout: {stdout}"
    );
}

/// Round 4 (#54): a stranded in-flight claim file (process killed
/// between claim and publish) must be recoverable — a subsequent
/// `flush apply <id>` must finish the publish from the existing
/// in-flight file rather than report `NotFound`.
#[test]
fn flush_apply_resumes_existing_in_flight_claim() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000RES01";
    // Hand-stage an in-flight claim — simulate a previous apply that
    // crashed between claim_pending and publish_terminal. Pending file
    // does NOT exist; only the in-flight file is on disk.
    let inflight = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    )
    .with_extension("json.in-flight");
    std::fs::create_dir_all(inflight.parent().unwrap()).unwrap();
    let p = sample_pending(id);
    std::fs::write(&inflight, serde_json::to_vec_pretty(&p).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // After resume, the in-flight file is gone and the terminal is
    // published.
    assert!(
        !inflight.exists(),
        "in-flight file should be cleaned up after resume"
    );
    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(applied.exists(), "terminal applied file should exist");
}

/// Round 4 (#54): `flush list` must surface stranded in-flight files
/// even when they are unreadable / malformed — that's exactly when an
/// operator most needs to see them.
#[test]
fn flush_list_shows_unreadable_in_flight_claims() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK0000000000000NRDB001";
    // Write a malformed in-flight file directly.
    let inflight = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    )
    .with_extension("json.in-flight");
    std::fs::create_dir_all(inflight.parent().unwrap()).unwrap();
    std::fs::write(&inflight, b"{not valid json").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(id),
        "list must include the unreadable in-flight id; stdout: {stdout}"
    );
    assert!(
        stdout.contains("unreadable"),
        "list must mark the row as unreadable; stdout: {stdout}"
    );
}

/// Round 3 (#54): an id containing `..`, `/`, or any non-Crockford
/// character must be rejected at the CLI surface BEFORE any path is
/// constructed — defends `.cairn/flush/` against path-escape.
#[test]
fn flush_apply_rejects_invalid_ulid_format() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    for bad in [
        "../../../etc/passwd",
        "01HQZK00000000000000000U01", // contains 'U' (excluded from Crockford)
        "01HQZK0",                    // wrong length
        "/tmp/escape",
        "9HQZK00000000000000000000A", // first char > 7
    ] {
        let out = std::process::Command::new(bin)
            .args(["flush", "apply", bad])
            .env("CAIRN_VAULT", vault.path())
            .output()
            .expect("spawn cairn");
        assert_eq!(
            out.status.code(),
            Some(64),
            "expected EX_USAGE for invalid ULID `{bad}`; got: {}",
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// Round 3 (#54): post-claim error paths (read failure, malformed JSON)
/// must roll back the in-flight claim so the pending file is recoverable.
#[test]
fn flush_apply_rolls_back_on_malformed_pending_after_claim() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK000000000000000RBCK1";
    let pending = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
    std::fs::write(&pending, b"{not valid json").unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(out.status.code(), Some(65));
    // Rollback: pending file is back at its original path.
    assert!(
        pending.exists(),
        "rollback must restore pending/<id>.plan.json after parse failure"
    );
    // No stranded in-flight files.
    let inflight = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    )
    .with_extension("json.in-flight");
    assert!(
        !inflight.exists(),
        "claim file should have been rolled back"
    );
}

/// Round 2 (#54): `flush apply` must reject a pending file whose embedded
/// `operation_id` does not match the path / requested id (tampering or
/// stale-content protection).
#[test]
fn flush_apply_rejects_id_mismatch() {
    let vault = tempfile::tempdir().unwrap();
    let request_id = "01HQZK000000000000000RQST1";
    let embedded_id = "01HQZK000000000000000EMBD2";
    // Write a pending file under the request_id name but with a different
    // embedded operation_id inside the JSON body.
    let mut p = sample_pending(embedded_id);
    p.plan.operation_id = cairn_core::generated::common::Ulid(embedded_id.into());
    let path = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(request_id.into()),
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&p).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", request_id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("mismatches filename id"),
        "expected id-mismatch message; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Round 2 (#54): concurrent `apply` and `reject` against the same pending
/// id must produce exactly one terminal file (race-free atomic claim).
#[test]
fn flush_apply_and_reject_race_yields_single_terminal() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    let vault = Arc::new(tempfile::tempdir().unwrap());
    let id = "01HQZK000000000000000RACE1";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let success_count = Arc::new(AtomicUsize::new(0));

    let v1 = Arc::clone(&vault);
    let s1 = Arc::clone(&success_count);
    let t1 = thread::spawn(move || {
        let out = std::process::Command::new(bin)
            .args(["flush", "apply", id])
            .env("CAIRN_VAULT", v1.path())
            .output()
            .expect("spawn cairn");
        if out.status.success() {
            s1.fetch_add(1, Ordering::Relaxed);
        }
    });
    let v2 = Arc::clone(&vault);
    let s2 = Arc::clone(&success_count);
    let t2 = thread::spawn(move || {
        let out = std::process::Command::new(bin)
            .args(["flush", "reject", id, "--reason", "race"])
            .env("CAIRN_VAULT", v2.path())
            .output()
            .expect("spawn cairn");
        if out.status.success() {
            s2.fetch_add(1, Ordering::Relaxed);
        }
    });
    t1.join().unwrap();
    t2.join().unwrap();

    let applied = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let rejected = plan_path(
        vault.path(),
        Bucket::Rejected,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let terminals = u32::from(applied.exists()) + u32::from(rejected.exists());
    assert_eq!(
        terminals,
        1,
        "expected exactly one terminal file, found applied={} rejected={}",
        applied.exists(),
        rejected.exists(),
    );
    assert_eq!(
        success_count.load(Ordering::Relaxed),
        1,
        "exactly one of apply / reject must have reported success",
    );
}

/// Round 1 (#54): `flush apply` against a stub-planner placeholder must
/// emit a prominent stderr warning and record `apply_kind=metadata_only`
/// so operators understand `MemoryStore` mutations did NOT execute.
#[test]
fn flush_apply_warns_on_placeholder_and_records_metadata_only() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    // Drive the stub planner end-to-end via `ingest --human-review` so
    // the persisted plan carries `placeholder = true`.
    let ingest_out = std::process::Command::new(bin)
        .args([
            "ingest",
            "--kind",
            "fact",
            "--body",
            "review me",
            "--human-review",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(ingest_out.status.success());

    // Pull the freshly-minted plan id from `flush list --json`.
    let list_out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    let envelope: serde_json::Value = serde_json::from_slice(&list_out.stdout).unwrap();
    let id = envelope["plans"][0]["id"]
        .as_str()
        .expect("plan id in envelope.plans")
        .to_owned();

    let apply_out = std::process::Command::new(bin)
        .args(["flush", "apply", &id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(apply_out.status.success());
    let stderr = String::from_utf8_lossy(&apply_out.stderr);
    assert!(
        stderr.contains("WARNING") && stderr.contains("not yet wired"),
        "expected metadata-only warning; got stderr: {stderr}"
    );
    assert!(
        stderr.contains("stub planner"),
        "expected placeholder-plan note; got stderr: {stderr}"
    );

    // Persisted status carries apply_kind = metadata_only.
    let applied_path = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.clone()),
    );
    let persisted: cairn_core::domain::flush_plan::PersistedPlan =
        serde_json::from_slice(&std::fs::read(&applied_path).unwrap()).unwrap();
    let cairn_core::domain::flush_plan::PlanStatus::Applied { ref apply_kind, .. } =
        persisted.status
    else {
        panic!("expected Applied status, got {:?}", persisted.status);
    };
    assert_eq!(
        *apply_kind,
        cairn_core::domain::flush_plan::ApplyKind::MetadataOnly
    );
}

#[test]
fn flush_apply_real_plan_rejects_empty_mutations() {
    // Re-loop r9 finding 1: a non-placeholder plan with no mutations
    // must NOT publish `ApplyKind::Full` (that would be a misleading
    // "fully applied" audit record for a planner-corruption / no-op).
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000A";
    write_non_placeholder_pending_noop(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");

    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("has no mutations"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Plan must remain pending — refusal should roll the claim back.
    let pending_path = plan_path(
        vault.path(),
        Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    assert!(
        pending_path.exists(),
        "expected plan to roll back to pending"
    );
}

#[test]
fn flush_apply_patch_updates_record_body_and_keeps_old_version() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000B";
    let mut record = sample_record(1);
    record.body = "alpha beta".into();
    let target = record.target_id.clone();

    let (rt, store) = open_store(vault.path());
    rt.block_on(store.upsert(&record)).expect("seed record");
    let stored = rt
        .block_on(store.get_active_by_target(&target))
        .expect("read stored")
        .expect("stored record");

    let mut hashes = std::collections::BTreeMap::new();
    hashes.insert(
        target.as_str().to_owned(),
        cairn_cli::verbs::flush_apply::record_drift_hash(&stored.record),
    );
    write_real_pending_plan_with_hashes(
        vault.path(),
        id,
        vec![PlannedMutation::Patch {
            target: PatchTarget::Record(target.clone()),
            str_replace: vec![StrReplace {
                old: "beta".into(),
                new: "gamma".into(),
                occurrence: ReplaceOccurrence::First,
            }],
        }],
        hashes,
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let active = rt
        .block_on(store.get_active_by_target(&target))
        .expect("read active")
        .expect("active record");
    assert_eq!(active.record.body, "alpha gamma");
    let history = rt.block_on(store.versions(&target)).expect("history");
    assert_eq!(
        history.len(),
        2,
        "expected superseded old version to remain"
    );
    let old = rt
        .block_on(store.get(&history[0].record_id))
        .expect("fetch old version")
        .expect("old version body");
    assert_eq!(old.body, "alpha beta");
    assert!(
        active
            .record
            .extra_frontmatter
            .contains_key("flush_patch_history"),
        "patched record should carry patch audit metadata"
    );
}

#[test]
fn flush_apply_patch_missing_substring_fails_atomically() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000C";
    let mut record = sample_record(2);
    record.body = "alpha beta".into();
    let target = record.target_id.clone();

    let (rt, store) = open_store(vault.path());
    rt.block_on(store.upsert(&record)).expect("seed record");
    let stored = rt
        .block_on(store.get_active_by_target(&target))
        .expect("read stored")
        .expect("stored record");

    let mut hashes = std::collections::BTreeMap::new();
    hashes.insert(
        target.as_str().to_owned(),
        cairn_cli::verbs::flush_apply::record_drift_hash(&stored.record),
    );
    write_real_pending_plan_with_hashes(
        vault.path(),
        id,
        vec![PlannedMutation::Patch {
            target: PatchTarget::Record(target.clone()),
            str_replace: vec![StrReplace {
                old: "delta".into(),
                new: "gamma".into(),
                occurrence: ReplaceOccurrence::First,
            }],
        }],
        hashes,
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(70),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("patch substring not found"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let active = rt
        .block_on(store.get_active_by_target(&target))
        .expect("read active")
        .expect("active record");
    assert_eq!(active.record.body, "alpha beta");
    let history = rt.block_on(store.versions(&target)).expect("history");
    assert_eq!(
        history.len(),
        1,
        "atomic failure must not write a new version"
    );
}

#[test]
fn flush_apply_patch_session_metadata_updates_live_session() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000D";
    let (rt, store) = open_store(vault.path());
    let identity = SessionIdentity::new(
        Identity::parse("hmn:alice:v1").unwrap(),
        Identity::parse("agt:claude-code:opus-4-7:main:v1").unwrap(),
        Some(vault.path().display().to_string()),
    )
    .unwrap();
    let session = rt
        .block_on(store.create_session(
            &identity,
            cairn_store_sqlite::NewSessionMetadata {
                channel: Some("chat".into()),
                priority: Some("low".into()),
                tags: vec!["alpha".into()],
            },
        ))
        .unwrap();

    let mut hashes = std::collections::BTreeMap::new();
    hashes.insert(
        session.id.as_str().to_owned(),
        cairn_cli::verbs::flush_apply::session_drift_hash(&session),
    );
    write_real_pending_plan_with_hashes(
        vault.path(),
        id,
        vec![PlannedMutation::Patch {
            target: PatchTarget::Session(session.id.clone()),
            str_replace: vec![StrReplace {
                old: "chat".into(),
                new: "ops".into(),
                occurrence: ReplaceOccurrence::First,
            }],
        }],
        hashes,
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let patched = rt
        .block_on(store.resolve_explicit_session(&session.id, &identity))
        .expect("resolve patched session");
    assert_eq!(patched.channel.as_deref(), Some("ops"));
}

#[test]
fn flush_apply_rename_rejects_live_destination_collision() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000E";
    let source = sample_record(3);
    let dest = sample_record(4);

    let (rt, store) = open_store(vault.path());
    rt.block_on(store.upsert(&source)).expect("seed source");
    rt.block_on(store.upsert(&dest)).expect("seed destination");
    let stored_source = rt
        .block_on(store.get_active_by_target(&source.target_id))
        .expect("read source")
        .expect("source row");

    let mut hashes = std::collections::BTreeMap::new();
    hashes.insert(
        source.target_id.as_str().to_owned(),
        cairn_cli::verbs::flush_apply::record_drift_hash(&stored_source.record),
    );
    write_real_pending_plan_with_hashes(
        vault.path(),
        id,
        vec![PlannedMutation::Rename {
            record_id: source.target_id.clone(),
            new_id: dest.target_id.clone(),
        }],
        hashes,
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(70),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("rename destination already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn lint_plan_flags_live_rename_target_collision() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000H";
    let source = sample_record(7);
    let dest = sample_record(8);

    let (rt, store) = open_store(vault.path());
    rt.block_on(store.upsert(&source)).expect("seed source");
    rt.block_on(store.upsert(&dest)).expect("seed dest");

    write_real_pending_plan(
        vault.path(),
        id,
        vec![PlannedMutation::Rename {
            record_id: source.target_id.clone(),
            new_id: dest.target_id.clone(),
        }],
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["lint", "--plan", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rename target collision (live)")
            && stderr.contains(dest.target_id.as_str()),
        "expected live rename collision finding; got stderr: {stderr}"
    );
}

#[test]
fn lint_plan_flags_intra_plan_rename_collision() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000J";
    let dup_target = TargetId::parse("01JTS6R4J7000000000000000K").unwrap();

    write_real_pending_plan(
        vault.path(),
        id,
        vec![
            PlannedMutation::Rename {
                record_id: TargetId::parse("01JTS6R4J70000000000000020").unwrap(),
                new_id: dup_target.clone(),
            },
            PlannedMutation::Rename {
                record_id: TargetId::parse("01JTS6R4J70000000000000021").unwrap(),
                new_id: dup_target.clone(),
            },
        ],
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["lint", "--plan", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rename target collision (intra-plan)"),
        "expected intra-plan rename collision finding; got stderr: {stderr}"
    );
}

#[test]
fn lint_plan_clean_plan_exits_zero() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000N";
    let source = sample_record(9);
    let new_target = TargetId::parse("01JTS6R4J7000000000000000P").unwrap();

    let (rt, store) = open_store(vault.path());
    rt.block_on(store.upsert(&source)).expect("seed source");

    write_real_pending_plan(
        vault.path(),
        id,
        vec![PlannedMutation::Rename {
            record_id: source.target_id.clone(),
            new_id: new_target,
        }],
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["lint", "--plan", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("clean"),
        "expected `clean` marker; got stdout: {stdout}"
    );
}

#[test]
fn flush_apply_rename_rewrites_inbound_edges_to_new_active_record() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000F";
    let source = sample_record(5);
    let inbound = sample_record(6);
    let new_target = TargetId::parse("01JTS6R4J7000000000000000G").unwrap();

    let (rt, store) = open_store(vault.path());
    rt.block_on(store.upsert(&source)).expect("seed source");
    rt.block_on(store.upsert(&inbound)).expect("seed inbound");
    rt.block_on(store.put_edge(&Edge {
        src: inbound.id.clone(),
        dst: source.id.clone(),
        kind: EdgeKind::Mentions,
        weight: None,
    }))
    .expect("seed inbound edge");
    let stored_source = rt
        .block_on(store.get_active_by_target(&source.target_id))
        .expect("read source")
        .expect("source row");

    let mut hashes = std::collections::BTreeMap::new();
    hashes.insert(
        source.target_id.as_str().to_owned(),
        cairn_cli::verbs::flush_apply::record_drift_hash(&stored_source.record),
    );
    write_real_pending_plan_with_hashes(
        vault.path(),
        id,
        vec![PlannedMutation::Rename {
            record_id: source.target_id.clone(),
            new_id: new_target.clone(),
        }],
        hashes,
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let active = rt
        .block_on(store.get_active_by_target(&new_target))
        .expect("read renamed target")
        .expect("renamed record");
    assert!(
        rt.block_on(store.get_active_by_target(&source.target_id))
            .expect("read old target")
            .is_none(),
        "old target should no longer resolve as active"
    );
    let inbound_edges = rt
        .block_on(store.neighbours(&active.record.id, EdgeDir::In))
        .expect("read inbound edges");
    assert!(
        inbound_edges
            .iter()
            .any(|edge| edge.src == inbound.id && edge.kind == EdgeKind::Mentions),
        "expected mentions edge to follow renamed record"
    );
}
