//! CLI coverage for `cairn search --filters`.

#![allow(missing_docs)]

use std::path::Path;
use std::process::Command;

const STALE_DAYS: i64 = 31;
const MILLIS_PER_DAY: i64 = 86_400_000;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    assert_eq!(
        out.status.code(),
        Some(0),
        "command failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn ingest(vault: &Path, kind: &str, body: &str) -> String {
    let out = cli()
        .current_dir(vault)
        .args(["ingest", "--kind", kind, "--body", body, "--json"])
        .output()
        .expect("cairn ingest");
    let json = json_stdout(&out);
    json["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned()
}

fn make_record_stale(vault: &Path, record_id: &str) {
    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open cairn db");
    let rows = conn
        .execute(
            "UPDATE records SET created_at = created_at - ?1 WHERE record_id = ?2",
            rusqlite::params![STALE_DAYS * MILLIS_PER_DAY, record_id],
        )
        .expect("mark record stale");
    assert_eq!(rows, 1, "test fixture must mark exactly one record stale");
}

#[test]
fn search_filters_narrow_keyword_results() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let reference_id = ingest(
        vault.path(),
        "reference",
        "issue62filterwire shared marker reference body",
    );
    let _rule_id = ingest(
        vault.path(),
        "rule",
        "issue62filterwire shared marker rule body",
    );

    let out = cli()
        .current_dir(vault.path())
        .args([
            "search",
            "issue62filterwire",
            "--mode",
            "keyword",
            "--filters",
            r#"{"field":"kind","op":"eq","value":"reference"}"#,
            "--json",
        ])
        .output()
        .expect("cairn search --filters");
    let json = json_stdout(&out);
    let hits = json["data"]["hits"].as_array().expect("hits array");

    assert_eq!(
        hits.len(),
        1,
        "--filters kind=reference must remove the rule hit; response: {json}"
    );
    assert_eq!(hits[0]["record_id"], reference_id);
}

#[test]
fn search_backfills_after_read_filter_exclusions() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());

    let stale_id = ingest(
        vault.path(),
        "reference",
        "issue62backfill alpha shared marker body",
    );
    let first_fresh_id = ingest(
        vault.path(),
        "reference",
        "issue62backfill beta shared marker body",
    );
    let second_fresh_id = ingest(
        vault.path(),
        "reference",
        "issue62backfill gamma shared marker body",
    );
    make_record_stale(vault.path(), &stale_id);

    let out = cli()
        .current_dir(vault.path())
        .args([
            "search",
            "issue62backfill",
            "--mode",
            "keyword",
            "--limit",
            "2",
            "--json",
        ])
        .output()
        .expect("cairn search --limit");
    let json = json_stdout(&out);
    let hits = json["data"]["hits"].as_array().expect("hits array");
    let record_ids: Vec<&str> = hits
        .iter()
        .map(|hit| hit["record_id"].as_str().expect("hit record_id"))
        .collect();

    assert_eq!(
        record_ids,
        vec![first_fresh_id.as_str(), second_fresh_id.as_str()],
        "read-filter exclusions must be replaced from lower-ranked matches; response: {json}"
    );
}

#[test]
fn invalid_search_filter_returns_invalid_filter_envelope() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let _ = ingest(
        vault.path(),
        "reference",
        "issue62invalidfilter shared marker reference body",
    );

    let out = cli()
        .current_dir(vault.path())
        .args([
            "search",
            "issue62invalidfilter",
            "--mode",
            "keyword",
            "--filters",
            r#"{"field":"definitely_not_allowed","op":"eq","value":"x"}"#,
            "--json",
        ])
        .output()
        .expect("cairn search --filters invalid");

    assert_eq!(
        out.status.code(),
        Some(64),
        "invalid filters should be usage errors\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(json["contract"], "cairn.mcp.v1");
    assert_eq!(json["status"], "rejected");
    assert_eq!(json["verb"], "search");
    assert_eq!(json["error"]["code"], "InvalidFilter");
    assert!(
        json["error"]["data"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("definitely_not_allowed")),
        "reason should name the rejected field: {json}",
    );
}
