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
