//! SRE release-gate smoke tests.

use std::collections::BTreeSet;
use std::process::Command;

use serde_json::Value;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
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
        if let Some(detail) = check.get("detail").and_then(Value::as_str) {
            assert_eq!(detail, "fixture", "unexpected detail: {check}");
        }
    }
}
