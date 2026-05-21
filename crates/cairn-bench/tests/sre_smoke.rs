//! SRE release-gate smoke tests.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::process::ExitStatus;

use serde_json::Value;

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
