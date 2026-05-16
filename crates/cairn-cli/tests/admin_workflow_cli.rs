//! CLI integration tests for `cairn admin workflow` — round-3 fixes.
//!
//! Round-3 finding 3.2: every subcommand that accepts `--kind` must
//! refuse non-synthetic values (anything not starting with
//! `test.e2e.`). Otherwise an operator could enqueue a production-kind
//! row that the synthetic registry would dead-letter as `Validation`
//! (run-failing) or — worse — `complete` as `done`, satisfying the
//! `workflow_health` lint's `last_success_ms` check and masking real
//! failures (run-succeeding).
//!
//! Each test:
//!   1. Bootstraps a real Cairn vault so `enforce_vault_binding` passes.
//!   2. Invokes the subcommand with a production `--kind`.
//!   3. Asserts the process exits 69 (`EX_UNAVAILABLE`) and stderr
//!      mentions "refused" + "synthetic".
//!   4. Confirms no `workflow_jobs` row was created.

use assert_cmd::Command;
use std::path::Path;

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn count_workflow_jobs(vault: &Path) -> i64 {
    let db_path = vault.join(".cairn").join("cairn.db");
    if !db_path.exists() {
        return 0;
    }
    let conn = rusqlite::Connection::open(&db_path).expect("open cairn.db");
    // The table may not exist on a freshly bootstrapped vault — the
    // workflow_jobs migration runs lazily. Treat missing-table as
    // zero rows (the refusal aborts before opening the store, so no
    // migration ran).
    conn.query_row("SELECT COUNT(*) FROM workflow_jobs", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap_or(0)
}

fn assert_refused(stderr: &str) {
    assert!(
        stderr.contains("refused"),
        "stderr must mention `refused`: {stderr}"
    );
    assert!(
        stderr.contains("synthetic"),
        "stderr must mention `synthetic`: {stderr}"
    );
    assert!(
        stderr.contains("test.e2e."),
        "stderr must mention the required prefix: {stderr}"
    );
}

#[test]
fn run_failing_rejects_non_synthetic_kind() {
    let temp = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(temp.path());

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", temp.path())
        .args(["admin", "workflow", "run-failing", "--kind", "dream.light"])
        .output()
        .expect("run cli");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        69,
        "expected EX_UNAVAILABLE (69). stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_refused(&stderr);

    // The refusal must fire BEFORE opening the store, so no rows can
    // have been created — workflow_jobs table either doesn't exist or
    // is empty.
    assert_eq!(
        count_workflow_jobs(temp.path()),
        0,
        "no workflow_jobs row may be created when --kind is rejected",
    );
}

#[test]
fn run_succeeding_rejects_non_synthetic_kind() {
    let temp = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(temp.path());

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", temp.path())
        .args([
            "admin",
            "workflow",
            "run-succeeding",
            "--kind",
            "dream.light",
        ])
        .output()
        .expect("run cli");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        69,
        "expected EX_UNAVAILABLE (69). stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_refused(&stderr);
    assert_eq!(
        count_workflow_jobs(temp.path()),
        0,
        "no workflow_jobs row may be created when --kind is rejected",
    );
}

#[test]
fn simulate_crash_rejects_non_synthetic_kind() {
    let temp = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(temp.path());

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", temp.path())
        .args([
            "admin",
            "workflow",
            "simulate-crash",
            "--kind",
            "evaluation.batch",
        ])
        .output()
        .expect("run cli");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        69,
        "expected EX_UNAVAILABLE (69). stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_refused(&stderr);
    assert_eq!(
        count_workflow_jobs(temp.path()),
        0,
        "no workflow_jobs row may be created when --kind is rejected",
    );
}
