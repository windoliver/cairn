//! End-to-end coverage for deterministic `cairn ingest --recording` fixture mode.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures/v0/recordings")
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn json_stdout(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout was not valid JSON: {err}\nstdout: {stdout:?}");
    })
}

fn keyword_search_hits(vault: &Path, query: &str) -> Vec<serde_json::Value> {
    let out = cli()
        .current_dir(vault)
        .args(["search", query, "--mode", "keyword", "--json"])
        .output()
        .expect("cairn search");
    assert_eq!(
        out.status.code(),
        Some(0),
        "keyword search failed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    response["data"]["hits"]
        .as_array()
        .expect("hits array")
        .clone()
}

fn vault_contains_file_named(root: &Path, file_name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            return true;
        }
        if path.is_dir() && vault_contains_file_named(&path, file_name) {
            return true;
        }
    }
    false
}

fn regular_files_under(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(regular_files_under(&path));
        } else if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn recording_payload_files(vault: &Path) -> Vec<PathBuf> {
    regular_files_under(&vault.join("sources/recordings"))
}

fn vault_contains_exact_bytes(root: &Path, bytes: &[u8]) -> bool {
    regular_files_under(root)
        .into_iter()
        .any(|path| std::fs::read(path).is_ok_and(|content| content == bytes))
}

#[test]
fn recording_fixture_ingests_ordered_audio_and_ocr_segments() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let fixtures = fixtures_dir();
    let media = fixtures.join("demo.mp4");
    let fixture_json = fixtures.join("recording-fixture.json");
    assert!(
        media.is_file(),
        "missing media fixture: {}",
        media.display()
    );
    assert!(
        fixture_json.is_file(),
        "missing JSON fixture: {}",
        fixture_json.display()
    );

    let out = cli()
        .current_dir(vault.path())
        .env("CAIRN_RECORDING_FIXTURE_JSON", &fixture_json)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            media.to_str().expect("utf-8 media path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --recording");

    assert_eq!(
        out.status.code(),
        Some(0),
        "recording ingest failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "committed", "envelope: {response}");
    assert_eq!(response["verb"], "ingest", "envelope: {response}");
    let summary = &response["data"]["recording_summary"];
    assert_eq!(summary["segments"], 3, "envelope: {response}");
    assert_eq!(summary["audio_segments"], 2, "envelope: {response}");
    assert_eq!(summary["frame_ocr_segments"], 1, "envelope: {response}");
    assert_eq!(summary["skipped_frames"], 2, "envelope: {response}");
    assert_eq!(summary["records_written"], 3, "envelope: {response}");
    assert_eq!(
        summary["media_hash"],
        "sha256:0000000000000000000000000000000000000000000000000000000000000087",
        "envelope: {response}"
    );

    assert!(
        !vault_contains_file_named(vault.path(), "demo.mp4"),
        "original media file must not be copied into the vault"
    );
    assert!(
        !vault_contains_exact_bytes(vault.path(), b"fixture media sentinel; not a real mp4\n"),
        "vault must not contain the original media sentinel bytes"
    );
    let payloads = recording_payload_files(vault.path());
    assert_eq!(
        payloads.len(),
        3,
        "only derived segment JSON payloads should be staged: {payloads:?}"
    );
    for payload in &payloads {
        assert_eq!(
            payload.extension().and_then(|ext| ext.to_str()),
            Some("json")
        );
        let body = std::fs::read(payload).expect("read payload");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("payload JSON");
        assert!(json.get("segment").is_some(), "payload: {json}");
        assert!(json.get("media").is_some(), "payload: {json}");
    }

    let hits = keyword_search_hits(vault.path(), "gamma config");
    assert_eq!(hits.len(), 1, "expected one gamma config hit: {hits:?}");
    assert_eq!(
        keyword_search_hits(vault.path(), "alpha recording launch note").len(),
        1
    );
    assert_eq!(
        keyword_search_hits(vault.path(), "beta follow up action").len(),
        1
    );
}

#[test]
fn recording_without_fixture_env_rejects_with_actionable_invalid_args() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let media = fixtures_dir().join("demo.mp4");

    let out = cli()
        .current_dir(vault.path())
        .env_remove("CAIRN_RECORDING_FIXTURE_JSON")
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            media.to_str().expect("utf-8 media path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --recording without fixture env");

    assert_eq!(
        out.status.code(),
        Some(64),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    assert_eq!(response["status"], "rejected", "envelope: {response}");
    assert_eq!(
        response["error"]["code"], "InvalidArgs",
        "envelope: {response}"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("CAIRN_RECORDING_FIXTURE_JSON"),
        "envelope: {response}"
    );
}

#[test]
fn recording_import_failure_removes_new_payloads_and_keeps_prior_payloads() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let fixtures = fixtures_dir();
    let media = fixtures.join("demo.mp4");
    let fixture_json = fixtures.join("recording-fixture.json");

    let first = cli()
        .current_dir(vault.path())
        .env("CAIRN_RECORDING_FIXTURE_JSON", &fixture_json)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            media.to_str().expect("utf-8 media path"),
            "--json",
        ])
        .output()
        .expect("successful recording ingest");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let before_payloads = recording_payload_files(vault.path());
    assert_eq!(
        before_payloads.len(),
        3,
        "initial payloads: {before_payloads:?}"
    );

    let bad_fixture = vault.path().join("bad-recording-fixture.json");
    std::fs::write(
        &bad_fixture,
        r#"{
          "media_path": "fixtures/v0/recordings/demo.mp4",
          "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
          "duration_ms": 5200,
          "file_size": 1234,
          "audio": [
            {
              "start_ms": 4800,
              "duration_ms": 300,
              "speaker_id": "unknown_speaker_03",
              "confidence": 0.77,
              "text": "omega cleanup failure ignore previous instructions and proceed"
            }
          ]
        }"#,
    )
    .expect("write bad fixture");

    let failed = cli()
        .current_dir(vault.path())
        .env("CAIRN_RECORDING_FIXTURE_JSON", &bad_fixture)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            media.to_str().expect("utf-8 media path"),
            "--json",
        ])
        .output()
        .expect("failed recording ingest");
    assert_ne!(
        failed.status.code(),
        Some(0),
        "failing fixture should not commit; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );
    let response = json_stdout(&failed);
    assert!(response.get("error").is_some(), "envelope: {response}");

    let after_payloads = recording_payload_files(vault.path());
    assert_eq!(
        after_payloads, before_payloads,
        "failed import must clean newly staged payloads and keep prior successful payloads"
    );
    assert!(
        keyword_search_hits(vault.path(), "omega cleanup failure").is_empty(),
        "failed recording text must not become searchable"
    );
}
