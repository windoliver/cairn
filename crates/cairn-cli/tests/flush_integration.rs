//! End-to-end integration tests for `cairn flush list/apply/reject`.

use std::path::Path;

use cairn_core::domain::flush_plan::store::{Bucket, plan_path};
use cairn_test_fixtures::flush_plan::sample_pending;

fn write_pending(vault: &Path, id: &str) {
    let p = sample_pending(id);
    let path = plan_path(vault, Bucket::Pending, &p.plan.operation_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&p).unwrap()).unwrap();
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
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", "01HQZK0000000000000000NONE"])
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
    let summaries: serde_json::Value = serde_json::from_slice(&list_out.stdout).unwrap();
    let id = summaries[0]["id"].as_str().expect("plan id").to_owned();

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
