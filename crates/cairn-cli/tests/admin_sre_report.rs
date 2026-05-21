//! Integration coverage for `cairn admin sre report`.

use std::process::Command;

fn cairn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let bootstrap = cairn()
        .args([
            "bootstrap",
            "--vault-path",
            dir.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    dir
}

fn assert_forbidden_fragments_absent(output: &str) {
    for fragment in [
        "SECRET_PRIVATE_TOKEN",
        "/Users/alice",
        "private body",
        "query text",
    ] {
        assert!(
            !output.contains(fragment),
            "output leaked forbidden fragment {fragment:?}: {output}"
        );
    }
}

#[test]
fn admin_sre_report_json_is_body_free() {
    let dir = bootstrap_vault();

    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null,"raw_query":"query text","source_path":"/Users/alice/private body"}
{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":3,"latency_ms":42,"degradation_state":"partial","error":"SECRET_PRIVATE_TOKEN"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"rehydration\""));
    assert!(stdout.contains("\"sample_count\":1"));
    assert!(stdout.contains("\"mode\":\"semantic\""));
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_summarizes_sections() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"none","error":"SECRET_PRIVATE_TOKEN /Users/alice private body query text"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status:"));
    assert!(stdout.contains("workflow:"));
    assert!(stdout.contains("rehydration:"));
    assert!(stdout.contains("projection:"));
    assert!(stdout.contains("search:"));
    assert!(stdout.contains("gates:"));
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_reports_unknown_when_sections_unknown() {
    let dir = bootstrap_vault();

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status: unknown"), "stdout: {stdout}");
}

#[test]
fn admin_sre_report_rejects_bad_bench_report_dir() {
    let dir = bootstrap_vault();
    let missing = dir.path().join("missing-bench-reports");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--bench-report-dir",
            missing.to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bench-report-dir"), "stderr: {stderr}");
}

#[test]
fn admin_sre_report_uses_bench_sre_gates() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"migration_backlog","status":"fail","measured":742000,"threshold":600000,"unit":"ms","detail":"fixture"}],"private":"/Users/alice SECRET_PRIVATE_TOKEN private body query text"}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"migration_backlog\""));
    assert!(stdout.contains("\"status\":\"fail\""));
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_rejects_malformed_bench_sre_json() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(bench.path().join("sre.json"), r#"{"checks":["#)
        .expect("write malformed bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sre.json"), "stderr: {stderr}");
}

#[test]
fn admin_sre_report_preserves_unknown_gate_rollup() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"future_gate","status":"unknown","measured":1,"threshold":2,"unit":"ms","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"name\":\"future_gate\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"gates\":{\"status\":\"unknown\""),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_sre_report_prioritizes_unknown_over_warning_gate_rollup() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"future_gate","status":"unknown","measured":1,"threshold":2,"unit":"ms","detail":"fixture"},{"name":"migration_backlog","status":"warning","measured":500000,"threshold":600000,"unit":"ms","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"gates\":{\"status\":\"unknown\""),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_sre_report_scrubs_imported_gate_labels() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"/Users/alice/private body","status":"fail","measured":1,"threshold":0,"unit":"SECRET_PRIVATE_TOKEN","detail":"query text from /Users/alice"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("redacted"), "stdout: {stdout}");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_scrubs_unsafe_vault_label() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&vault).expect("create vault dir");
    let bootstrap = cairn()
        .args(["bootstrap", "--vault-path", vault.to_str().expect("utf8")])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );

    let output = cairn()
        .current_dir(&vault)
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"name\":\"local_vault\""),
        "stdout: {stdout}"
    );
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_rolls_up_rehydration_failures() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":5000,"bytes_restored":1000,"record_count":2,"error":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status: fail"), "stdout: {stdout}");
}
