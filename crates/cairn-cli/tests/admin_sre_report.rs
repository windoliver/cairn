//! Integration coverage for `cairn admin sre report`.

use std::process::Command;

fn cairn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn admin_sre_report_json_is_body_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bootstrap = cairn()
        .args([
            "bootstrap",
            "--vault-path",
            dir.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("bootstrap");
    assert!(bootstrap.status.success());

    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null}
{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":3,"latency_ms":42,"degradation_state":"partial","error":null}
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
    assert!(!stdout.contains("SECRET_PRIVATE_TOKEN"));
}

#[test]
fn admin_sre_report_human_summarizes_sections() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bootstrap = cairn()
        .args([
            "bootstrap",
            "--vault-path",
            dir.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("bootstrap");
    assert!(bootstrap.status.success());

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
}
