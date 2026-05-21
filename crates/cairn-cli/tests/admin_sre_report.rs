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

fn json_stdout(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("json stdout")
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
fn admin_sre_report_surfaces_metric_parse_errors_safely() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null}
not json SECRET_PRIVATE_TOKEN /Users/alice private body query text
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
    let json = json_stdout(&output);
    let gates = json["gates"]["gates"].as_array().expect("gates");
    let parse_gate = gates
        .iter()
        .find(|gate| gate["name"] == "metric_parse_errors")
        .expect("metric_parse_errors gate");
    assert_eq!(parse_gate["status"], "warning");
    assert_eq!(parse_gate["measured"], 1.0);
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_counts_search_verb_invocation_failures() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"verb_invocation","ts_ms":1,"verb":"search","surface":"mcp","mode":"semantic","status":"rejected","latency_ms":77,"error":"provider_unavailable","budget_used_ratio":null,"degradation_state":"partial","private":"SECRET_PRIVATE_TOKEN /Users/alice private body query text"}
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
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let semantic = modes
        .iter()
        .find(|mode| mode["mode"] == "semantic")
        .expect("semantic mode");
    assert_eq!(semantic["invocations"], 1);
    assert_eq!(semantic["failed"], 1);
    assert_eq!(semantic["degraded"], 1);
    assert_eq!(semantic["status"], "fail");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_does_not_double_count_completed_cli_search() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":1,"mode":"keyword","hit_count":1,"latency_ms":41,"degradation_state":"none","error":null}
{"event":"verb_invocation","ts_ms":1,"verb":"search","surface":"cli","mode":"keyword","status":"committed","latency_ms":41,"error":null,"budget_used_ratio":null,"degradation_state":"none"}
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
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let keyword = modes
        .iter()
        .find(|mode| mode["mode"] == "keyword")
        .expect("keyword mode");
    assert_eq!(keyword["invocations"], 1);
}

#[test]
fn admin_sre_report_tolerates_unknown_metric_events() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"future_metric","private":"SECRET_PRIVATE_TOKEN /Users/alice private body query text"}
{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null}
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
    let json = json_stdout(&output);
    let gates = json["gates"]["gates"].as_array().expect("gates");
    assert!(
        gates
            .iter()
            .all(|gate| gate["name"] != "metric_parse_errors"),
        "json: {json}"
    );
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_marks_unobserved_search_unknown() {
    let dir = bootstrap_vault();

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
    let json = json_stdout(&output);
    assert_eq!(json["search"]["status"], "unknown");
    assert!(
        json["search"]["modes"]
            .as_array()
            .expect("search modes")
            .iter()
            .any(|mode| mode["invocations"] == 0 && mode["status"] == "unknown"),
        "json: {json}"
    );
}

#[test]
fn admin_sre_report_marks_unadvertised_search_modes() {
    let dir = bootstrap_vault();
    let config = dir.path().join(".cairn/config.yaml");
    let raw = std::fs::read_to_string(&config).expect("read config");
    assert!(
        raw.contains("local_embeddings: true"),
        "bootstrap config: {raw}"
    );
    std::fs::write(
        &config,
        raw.replace("local_embeddings: true", "local_embeddings: false"),
    )
    .expect("write config");

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
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let advertised = |name: &str| {
        modes
            .iter()
            .find(|mode| mode["mode"] == name)
            .expect("mode")["advertised"]
            .as_bool()
            .expect("advertised bool")
    };
    assert!(advertised("keyword"));
    assert!(!advertised("semantic"));
    assert!(!advertised("hybrid"));
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
fn admin_sre_report_human_reports_warning_when_degraded_search_present() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
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
    assert!(stdout.contains("SRE status: warning"), "stdout: {stdout}");
}

#[test]
fn admin_sre_report_search_warns_when_observed_mode_degrades() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
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
    let json = json_stdout(&output);
    assert_eq!(json["search"]["status"], "warning");
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
fn admin_sre_report_bad_bench_dir_error_is_path_free() {
    let dir = bootstrap_vault();
    let missing = dir
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");

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
    assert_forbidden_fragments_absent(&stderr);
}

#[test]
fn admin_sre_report_unbound_vault_error_is_path_free() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&vault).expect("create vault dir");

    let output = cairn()
        .args([
            "--vault",
            vault.to_str().expect("utf8"),
            "admin",
            "sre",
            "report",
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cairn admin sre report:"),
        "stderr: {stderr}"
    );
    assert_forbidden_fragments_absent(&stderr);
}

#[test]
fn admin_sre_report_vault_resolution_error_is_path_free() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "--vault",
            "SECRET_PRIVATE_TOKEN private body query text",
            "admin",
            "sre",
            "report",
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("vault resolution error") || stderr.contains("cairn admin sre report"));
    assert_forbidden_fragments_absent(&stderr);
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
fn admin_sre_report_keeps_projection_lag_fixture_gate_name() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"projection_lag_fixture","status":"warning","measured":2,"threshold":0,"unit":"count","detail":"fixture"}]}"#,
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
    let json = json_stdout(&output);
    let gates = json["gates"]["gates"].as_array().expect("gates");
    let projection_gate = gates
        .iter()
        .find(|gate| gate["name"] == "projection_lag_fixture")
        .expect("projection lag gate");
    assert_eq!(projection_gate["status"], "warning");
}

#[test]
fn admin_sre_report_scrubs_stable_looking_untrusted_labels() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent.path().join("payroll-vault");
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
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"customer_acme_board","status":"fail","measured":1,"threshold":0,"unit":"session_01JABC","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(&vault)
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
        stdout.contains("\"name\":\"local_vault\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"name\":\"redacted_gate\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("\"unit\":\"redacted\""), "stdout: {stdout}");
    assert!(!stdout.contains("payroll-vault"), "stdout: {stdout}");
    assert!(!stdout.contains("customer_acme_board"), "stdout: {stdout}");
    assert!(!stdout.contains("session_01JABC"), "stdout: {stdout}");
}

#[test]
fn admin_sre_report_redacts_stable_looking_imported_gate_detail() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"migration_backlog","status":"fail","measured":1,"threshold":0,"unit":"ms","detail":"customer_acme_board"}]}"#,
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
    assert!(!stdout.contains("customer_acme_board"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"detail\":\"redacted\""),
        "stdout: {stdout}"
    );
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
fn admin_sre_report_rejects_schema_invalid_sre_json() {
    let dir = bootstrap_vault();
    let parent = tempfile::tempdir().expect("bench parent");
    let bench = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&bench).expect("create bench dir");

    for body in [r#"{}"#, r#"{"checks":[{}]}"#] {
        std::fs::write(bench.join("sre.json"), body).expect("write schema-invalid report");
        let output = cairn()
            .current_dir(dir.path())
            .args([
                "admin",
                "sre",
                "report",
                "--bench-report-dir",
                bench.to_str().expect("utf8"),
            ])
            .output()
            .expect("run sre report");

        assert!(!output.status.success(), "body: {body}");
        assert_eq!(output.status.code(), Some(78), "body: {body}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("sre.json"), "stderr: {stderr}");
        assert_forbidden_fragments_absent(&stderr);
    }
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
        stdout.contains("\"name\":\"redacted_gate\""),
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
fn admin_sre_report_human_includes_safe_actionable_details() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
"#,
    )
    .expect("write metrics");
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
    assert!(stdout.contains("semantic"), "stdout: {stdout}");
    assert!(stdout.contains("degraded 1/1"), "stdout: {stdout}");
    assert!(stdout.contains("migration_backlog"), "stdout: {stdout}");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_shows_search_failures_and_degradations() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":1,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":43,"degradation_state":"partial","error":"provider_unavailable"}
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
    assert!(stdout.contains("semantic"), "stdout: {stdout}");
    assert!(stdout.contains("failed 1/2"), "stdout: {stdout}");
    assert!(stdout.contains("degraded 2/2"), "stdout: {stdout}");
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
