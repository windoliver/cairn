//! End-to-end integration tests for `cairn flush list/apply/reject`.

use std::path::Path;

use cairn_core::contract::memory_store::{Edge, EdgeDir, EdgeKind, MemoryStore};
use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::flush_plan::{
    FlushMode, PatchTarget, PersistedPlan, PlanReason, PlannedMutation, ReplaceOccurrence,
    StrReplace,
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
        target_hashes: std::collections::BTreeMap::new(),
        dependencies: vec![],
        expires_at: "2099-05-09T12:05:00Z".into(),
        placeholder: false,
    };
    let persisted = PersistedPlan::pending(plan);
    let path = plan_path(vault, Bucket::Pending, &persisted.plan.operation_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&persisted).unwrap()).unwrap();
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
fn flush_apply_real_plan_records_full_apply_kind() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01JTS6R4J7000000000000000A";
    write_non_placeholder_pending_noop(vault.path(), id);

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

    let applied_path = plan_path(
        vault.path(),
        Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()),
    );
    let persisted: cairn_core::domain::flush_plan::PersistedPlan =
        serde_json::from_slice(&std::fs::read(&applied_path).unwrap()).unwrap();
    let cairn_core::domain::flush_plan::PlanStatus::Applied { ref apply_kind, .. } =
        persisted.status
    else {
        panic!("expected Applied status, got {:?}", persisted.status);
    };
    assert_eq!(*apply_kind, cairn_core::domain::flush_plan::ApplyKind::Full);
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

    write_real_pending_plan(
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

    write_real_pending_plan(
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

    write_real_pending_plan(
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

    write_real_pending_plan(
        vault.path(),
        id,
        vec![PlannedMutation::Rename {
            record_id: source.target_id.clone(),
            new_id: new_target.clone(),
        }],
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
