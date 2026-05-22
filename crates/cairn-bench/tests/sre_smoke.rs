//! SRE release-gate smoke tests.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::process::ExitStatus;

use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
}

fn assert_exit(status: ExitStatus, expected: i32) {
    assert_eq!(
        status.code(),
        Some(expected),
        "expected exit {expected}, got {status}"
    );
}

fn read_report(out_dir: &Path) -> Value {
    let raw = std::fs::read_to_string(out_dir.join("sre.json")).expect("read sre.json");
    serde_json::from_str(&raw).expect("valid json")
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("missing check `{name}` in {}", report["checks"]))
}

fn write_criterion_sample(criterion_dir: &Path, bench: &str, sample_ms: &[f64]) {
    let sample_dir = criterion_dir.join(bench).join("new");
    std::fs::create_dir_all(&sample_dir).expect("create criterion sample dir");
    let times = sample_ms
        .iter()
        .map(|sample| sample * 1_000_000.0)
        .collect::<Vec<_>>();
    let iters = vec![1.0; sample_ms.len()];
    std::fs::write(
        sample_dir.join("sample.json"),
        serde_json::json!({
            "times": times,
            "iters": iters,
        })
        .to_string(),
    )
    .expect("write sample.json");
}

fn write_criterion_estimate(criterion_dir: &Path, bench: &str, estimate_ms: f64) {
    let estimate_dir = criterion_dir.join(bench).join("new");
    std::fs::create_dir_all(&estimate_dir).expect("create criterion estimate dir");
    let point_estimate_ns = estimate_ms * 1_000_000.0;
    std::fs::write(
        estimate_dir.join("estimates.json"),
        format!(r#"{{"median":{{"point_estimate":{point_estimate_ns}}}}}"#),
    )
    .expect("write estimates.json");
}

fn write_workflow_db(path: &Path, queued_age_ms: i64) {
    let now_ms = now_epoch_ms();
    write_workflow_db_with_times(path, now_ms - queued_age_ms, now_ms);
}

fn write_workflow_db_with_times(path: &Path, next_run_at: i64, updated_at: i64) {
    let conn = rusqlite::Connection::open(path).expect("open workflow db");
    conn.execute_batch(
        "CREATE TABLE workflow_jobs (
            job_id TEXT NOT NULL PRIMARY KEY,
            kind TEXT NOT NULL,
            payload BLOB NOT NULL,
            state TEXT NOT NULL,
            attempts INTEGER NOT NULL,
            delivery_count INTEGER NOT NULL,
            max_attempts INTEGER NOT NULL,
            base_backoff_ms INTEGER NOT NULL,
            backoff_multiplier INTEGER NOT NULL,
            max_backoff_ms INTEGER NOT NULL,
            next_run_at INTEGER NOT NULL,
            enqueued_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            lease_owner TEXT,
            lease_nonce TEXT,
            lease_started INTEGER,
            lease_expires_at INTEGER,
            failure_class TEXT,
            dead_letter_at_ms INTEGER,
            completed_at_ms INTEGER,
            last_error TEXT
        );",
    )
    .expect("create workflow_jobs");
    conn.execute(
        "INSERT INTO workflow_jobs (
            job_id, kind, payload, state, attempts, delivery_count, max_attempts,
            base_backoff_ms, backoff_multiplier, max_backoff_ms, next_run_at,
            enqueued_at, updated_at
        ) VALUES (?1, ?2, x'', 'queued', 0, 0, 3, 1, 2, 60000, ?3, ?4, ?4)",
        rusqlite::params!["queued-migration", "expire.tier", next_run_at, updated_at],
    )
    .expect("insert workflow row");
}

fn now_epoch_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_millis(),
    )
    .expect("epoch millis fit i64")
}

#[cfg(unix)]
fn write_fake_cargo(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("cargo");
    std::fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$CAIRN_FAKE_CARGO_LOG"
if [ "${1:-}" = "bench" ]; then
  sample_root="${CAIRN_FAKE_CARGO_SAMPLE_ROOT:-$CRITERION_HOME}"
  sample_dir="$sample_root/cold_rehydrate_p95/new"
  mkdir -p "$sample_dir"
  printf '%s\n' '{"times":[1800000000,2100000000,2500000000],"iters":[1,1,1]}' > "$sample_dir/sample.json"
fi
"#,
    )
    .expect("write fake cargo");
    let mut perms = std::fs::metadata(&script)
        .expect("fake cargo metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod fake cargo");
    script
}

#[test]
fn top_level_help_lists_sre() {
    let output = cli().args(["--help"]).output().expect("run");
    assert!(
        output.status.success(),
        "help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sre"), "expected `sre` in --help output");
}

#[test]
fn fixtures_only_writes_importable_body_free_sre_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = cli()
        .args([
            "sre",
            "--fixtures-only",
            "--out-dir",
            dir.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");
    assert!(
        output.status.success(),
        "sre gate failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report_path = dir.path().join("sre.json");
    let raw = std::fs::read_to_string(&report_path).expect("read sre.json");
    for fragment in [
        "SECRET_PRIVATE_TOKEN",
        "/Users/alice",
        "private body",
        "query text",
    ] {
        assert!(
            !raw.contains(fragment),
            "sre.json leaked forbidden fragment `{fragment}`: {raw}"
        );
    }

    let report: Value = serde_json::from_str(&raw).expect("valid json");
    assert_eq!(report["schema_version"], 1);
    assert!(report["ok"].as_bool().is_some(), "missing ok bool: {raw}");
    assert!(
        report.get("dashboard").is_some(),
        "missing dashboard: {raw}"
    );
    let checks = report["checks"].as_array().expect("checks array");
    let names = checks
        .iter()
        .map(|check| {
            check["name"]
                .as_str()
                .expect("check name string")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    for expected in [
        "migration_backlog",
        "sre_privacy_scrub",
        "projection_lag_fixture",
    ] {
        assert!(
            names.contains(expected),
            "missing check `{expected}` in {names:?}"
        );
    }

    for check in checks {
        assert!(check["name"].is_string(), "bad check name: {check}");
        assert!(check["status"].is_string(), "bad check status: {check}");
        assert!(check["measured"].is_number(), "bad measured: {check}");
        assert!(check["threshold"].is_number(), "bad threshold: {check}");
        assert!(check["unit"].is_string(), "bad unit: {check}");
        let detail = check.get("detail").and_then(Value::as_str);
        if check["name"] == "sre_privacy_scrub" {
            assert_eq!(detail, Some("redacted"), "unexpected detail: {check}");
        } else {
            assert_eq!(detail, Some("fixture"), "unexpected detail: {check}");
        }
    }
}

#[test]
fn full_mode_missing_criterion_output_exits_missing_input() {
    let criterion = tempfile::tempdir().expect("criterion dir");
    let out = tempfile::tempdir().expect("out dir");
    let output = cli()
        .args([
            "sre",
            "--no-run",
            "--criterion-dir",
            criterion.path().to_str().expect("utf8"),
            "--out-dir",
            out.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");

    assert_exit(output.status, 2);
    let report = read_report(out.path());
    assert_eq!(report["ok"], false);
    assert_eq!(check(&report, "cold_rehydrate_p95")["status"], "unknown");
}

#[test]
fn full_mode_above_slo_criterion_output_fails_cold_rehydrate_gate() {
    let criterion = tempfile::tempdir().expect("criterion dir");
    let out = tempfile::tempdir().expect("out dir");
    write_criterion_sample(
        criterion.path(),
        "cold_rehydrate_p95",
        &[2_000.0, 2_500.0, 3_500.0],
    );

    let output = cli()
        .args([
            "sre",
            "--no-run",
            "--criterion-dir",
            criterion.path().to_str().expect("utf8"),
            "--out-dir",
            out.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");

    assert_exit(output.status, 1);
    let report = read_report(out.path());
    let cold = check(&report, "cold_rehydrate_p95");
    assert_eq!(cold["status"], "fail");
    assert_eq!(cold["measured"], 3_500.0);
    assert_eq!(cold["threshold"], 3_000.0);
}

#[test]
fn full_mode_under_slo_criterion_output_passes_cold_rehydrate_gate() {
    let criterion = tempfile::tempdir().expect("criterion dir");
    let out = tempfile::tempdir().expect("out dir");
    write_criterion_sample(
        criterion.path(),
        "cold_rehydrate_p95",
        &[1_800.0, 2_100.0, 2_500.0],
    );

    let output = cli()
        .args([
            "sre",
            "--no-run",
            "--criterion-dir",
            criterion.path().to_str().expect("utf8"),
            "--out-dir",
            out.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");

    assert!(
        output.status.success(),
        "sre gate failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = read_report(out.path());
    let cold = check(&report, "cold_rehydrate_p95");
    assert_eq!(cold["status"], "ok");
    assert_eq!(cold["measured"], 2_500.0);
    assert_eq!(cold["threshold"], 3_000.0);
}

#[test]
#[cfg(unix)]
fn full_mode_builds_release_cairn_binary_before_lifecycle_bench() {
    let fake_bin = tempfile::tempdir().expect("fake cargo dir");
    write_fake_cargo(fake_bin.path());
    let old_path = std::env::var_os("PATH").expect("PATH");
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .expect("join PATH");
    let log = fake_bin.path().join("cargo.log");
    let out = tempfile::tempdir().expect("out dir");

    let output = cli()
        .env("PATH", path)
        .env("CAIRN_FAKE_CARGO_LOG", &log)
        .args(["sre", "--out-dir", out.path().to_str().expect("utf8")])
        .output()
        .expect("run sre gate");

    assert!(
        output.status.success(),
        "sre gate failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let cargo_log = std::fs::read_to_string(log).expect("read fake cargo log");
    let invocations = cargo_log.lines().collect::<Vec<_>>();
    assert_eq!(
        invocations.first().copied(),
        Some("build -p cairn-cli --bin cairn --release --locked"),
        "full SRE mode must build the release cairn binary before benching: {cargo_log}"
    );
    assert!(
        invocations.contains(&"bench -p cairn-bench --bench lifecycle --locked"),
        "full SRE mode must run lifecycle bench after building cairn: {cargo_log}"
    );
    assert_eq!(
        check(&read_report(out.path()), "cold_rehydrate_p95")["status"],
        "ok"
    );
}

#[test]
#[cfg(unix)]
fn full_mode_imports_criterion_output_from_cargo_bench_default_dir() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let fallback = manifest_dir.join("target/criterion");
    let _ = std::fs::remove_dir_all(fallback.join("cold_rehydrate_p95"));
    let _ = std::fs::remove_dir_all(
        workspace_root
            .join("target/criterion")
            .join("cold_rehydrate_p95"),
    );
    write_criterion_sample(
        &workspace_root.join("target/criterion"),
        "cold_rehydrate_p95",
        &[4_500.0],
    );
    let fake_bin = tempfile::tempdir().expect("fake cargo dir");
    write_fake_cargo(fake_bin.path());
    let old_path = std::env::var_os("PATH").expect("PATH");
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )
    .expect("join PATH");
    let out = tempfile::tempdir().expect("out dir");

    let output = cli()
        .current_dir(workspace_root)
        .env("PATH", path)
        .env("CAIRN_FAKE_CARGO_LOG", fake_bin.path().join("cargo.log"))
        .env("CAIRN_FAKE_CARGO_SAMPLE_ROOT", &fallback)
        .args(["sre", "--out-dir", out.path().to_str().expect("utf8")])
        .output()
        .expect("run sre gate");

    assert!(
        output.status.success(),
        "sre gate failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = read_report(out.path());
    let cold = check(&report, "cold_rehydrate_p95");
    assert_eq!(cold["status"], "ok");
    assert_eq!(cold["measured"], 2_500.0);
}

#[test]
fn workflow_db_backlog_over_slo_fails_migration_gate() {
    let workflow = tempfile::tempdir().expect("workflow dir");
    let workflow_db = workflow.path().join("workflow.sqlite");
    write_workflow_db(&workflow_db, 900_000);
    let out = tempfile::tempdir().expect("out dir");

    let output = cli()
        .args([
            "sre",
            "--fixtures-only",
            "--workflow-db",
            workflow_db.to_str().expect("utf8"),
            "--out-dir",
            out.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");

    assert_exit(output.status, 1);
    let report = read_report(out.path());
    let migration = check(&report, "migration_backlog");
    assert_eq!(migration["status"], "fail");
    assert!(
        migration["measured"].as_f64().expect("measured") >= 900_000.0,
        "migration gate should measure from report time: {migration}"
    );
    assert_eq!(migration["threshold"], 600_000.0);
}

#[test]
fn workflow_db_backlog_uses_report_time_not_row_updated_at() {
    let workflow = tempfile::tempdir().expect("workflow dir");
    let workflow_db = workflow.path().join("workflow.sqlite");
    let now_ms = now_epoch_ms();
    write_workflow_db_with_times(&workflow_db, now_ms - 900_000, now_ms - 900_000);
    let out = tempfile::tempdir().expect("out dir");

    let output = cli()
        .args([
            "sre",
            "--fixtures-only",
            "--workflow-db",
            workflow_db.to_str().expect("utf8"),
            "--out-dir",
            out.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");

    assert_exit(output.status, 1);
    let report = read_report(out.path());
    let migration = check(&report, "migration_backlog");
    assert_eq!(migration["status"], "fail");
    assert!(
        migration["measured"].as_f64().expect("measured") >= 850_000.0,
        "migration gate should age queued work against report time: {migration}"
    );
}

#[test]
fn full_mode_uses_sample_p95_not_estimates_median() {
    let criterion = tempfile::tempdir().expect("criterion dir");
    let out = tempfile::tempdir().expect("out dir");
    write_criterion_estimate(criterion.path(), "cold_rehydrate_p95", 2_000.0);
    write_criterion_sample(
        criterion.path(),
        "cold_rehydrate_p95",
        &[2_000.0, 2_100.0, 3_500.0],
    );

    let output = cli()
        .args([
            "sre",
            "--no-run",
            "--criterion-dir",
            criterion.path().to_str().expect("utf8"),
            "--out-dir",
            out.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre gate");

    assert_exit(output.status, 1);
    let report = read_report(out.path());
    let cold = check(&report, "cold_rehydrate_p95");
    assert_eq!(cold["status"], "fail");
    assert_eq!(cold["measured"], 3_500.0);
}
