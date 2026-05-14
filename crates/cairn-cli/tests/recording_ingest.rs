//! End-to-end coverage for deterministic `cairn ingest --recording` fixture mode.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd.env_remove("CAIRN_ISSUER");
    cmd
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

fn run_json_ok(vault: &Path, args: &[&str]) -> serde_json::Value {
    let out = cli()
        .current_dir(vault)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "cairn {args:?} failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    json_stdout(&out)
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

fn assert_no_recording_payloads(vault: &Path) {
    let payloads = recording_payload_files(vault);
    assert!(
        payloads.is_empty(),
        "rejected recording ingest must not write derived payloads: {payloads:?}"
    );
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
fn recording_derived_record_forget_removes_search_and_retrieve_text() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let fixtures = fixtures_dir();
    let media = fixtures.join("demo.mp4");
    let fixture_json = fixtures.join("recording-fixture.json");
    let transcript = "alpha recording launch note";

    let ingest = cli()
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
        ingest.status.code(),
        Some(0),
        "recording ingest failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&ingest.stdout),
        String::from_utf8_lossy(&ingest.stderr)
    );

    let hits = keyword_search_hits(vault.path(), transcript);
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one derived recording hit: {hits:?}"
    );
    let record_id = hits[0]["record_id"]
        .as_str()
        .expect("hit record_id")
        .to_owned();

    let before_retrieve = run_json_ok(vault.path(), &["retrieve", &record_id, "--json"]);
    assert_eq!(
        before_retrieve["status"], "committed",
        "envelope: {before_retrieve}"
    );
    assert_eq!(
        before_retrieve["data"]["record_id"], record_id,
        "envelope: {before_retrieve}"
    );
    assert!(
        before_retrieve["data"]["body"]
            .as_str()
            .is_some_and(|body| body.contains(transcript)),
        "retrieve before forget must expose the live recording transcript: {before_retrieve}"
    );

    let forget = run_json_ok(vault.path(), &["forget", "--record", &record_id, "--json"]);
    assert_eq!(forget["status"], "committed", "envelope: {forget}");

    assert!(
        keyword_search_hits(vault.path(), transcript).is_empty(),
        "search after forget must not surface recording transcript"
    );
    let retrieve_out = cli()
        .current_dir(vault.path())
        .args(["retrieve", &record_id, "--json"])
        .output()
        .expect("retrieve after forget");
    assert_eq!(
        retrieve_out.status.code(),
        Some(0),
        "retrieve after forget should commit with empty record data\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&retrieve_out.stderr),
        String::from_utf8_lossy(&retrieve_out.stdout)
    );
    let retrieve = json_stdout(&retrieve_out);
    assert_eq!(retrieve["status"], "committed", "envelope: {retrieve}");
    assert_eq!(
        retrieve["data"]["record_id"], record_id,
        "envelope: {retrieve}"
    );
    assert!(
        retrieve["data"]["body"].is_null(),
        "retrieve after forget must return empty body: {retrieve}"
    );
    assert!(
        !retrieve.to_string().contains(transcript),
        "retrieve after forget must not leak recording text: {retrieve}"
    );
}

#[test]
fn recording_rejects_unsupported_extension_before_fixture_reads() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let unsupported = vault.path().join("note.txt");
    std::fs::write(&unsupported, "not media").expect("write unsupported recording");
    let missing_fixture = vault.path().join("missing-fixture.json");

    let out = cli()
        .current_dir(vault.path())
        .env("CAIRN_RECORDING_FIXTURE_JSON", &missing_fixture)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            unsupported.to_str().expect("utf-8 unsupported path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --recording unsupported extension");

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
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("unsupported recording format"),
        "envelope: {response}"
    );
    assert!(
        message.contains("mp4, m4a, mp3, mkv, webm, wav"),
        "envelope: {response}"
    );
    assert_no_recording_payloads(vault.path());
}

#[test]
fn recording_rejects_missing_fixture_runtime_with_actionable_invalid_args() {
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
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("ffprobe")
            || message.contains("voice-runtime")
            || message.contains("tesseract"),
        "envelope: {response}"
    );
    assert_no_recording_payloads(vault.path());
}

#[test]
fn recording_rejects_corrupt_fixture_before_payload_or_record_writes() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let media = fixtures_dir().join("demo.mp4");
    let corrupt_fixture = vault.path().join("corrupt-recording-fixture.json");
    std::fs::write(&corrupt_fixture, "{ not json").expect("write corrupt fixture");

    let out = cli()
        .current_dir(vault.path())
        .env("CAIRN_RECORDING_FIXTURE_JSON", &corrupt_fixture)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            media.to_str().expect("utf-8 media path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --recording corrupt fixture");

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
            .contains("failed to parse CAIRN_RECORDING_FIXTURE_JSON"),
        "envelope: {response}"
    );
    assert_no_recording_payloads(vault.path());
    assert!(
        keyword_search_hits(vault.path(), "alpha recording launch note").is_empty(),
        "corrupt fixture text must not become searchable"
    );
}

#[test]
fn recording_rejects_fixture_media_mismatch_before_payload_or_record_writes() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let media = fixtures_dir().join("demo.mp4");
    let mismatched_fixture = vault.path().join("mismatched-recording-fixture.json");
    std::fs::write(
        &mismatched_fixture,
        r#"{
          "media_path": "fixtures/v0/recordings/other.mp4",
          "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
          "duration_ms": 5200,
          "file_size": 1234,
          "audio": [
            {
              "start_ms": 0,
              "duration_ms": 1800,
              "speaker_id": "unknown_speaker_01",
              "confidence": 0.91,
              "text": "mismatch should never commit"
            }
          ],
          "frames": []
        }"#,
    )
    .expect("write mismatched fixture");

    let out = cli()
        .current_dir(vault.path())
        .env("CAIRN_RECORDING_FIXTURE_JSON", &mismatched_fixture)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            media.to_str().expect("utf-8 media path"),
            "--json",
        ])
        .output()
        .expect("cairn ingest --recording mismatched fixture");

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
            .contains("recording fixture media_path does not match"),
        "envelope: {response}"
    );
    assert_no_recording_payloads(vault.path());
    assert!(
        keyword_search_hits(vault.path(), "mismatch should never commit").is_empty(),
        "mismatched fixture text must not become searchable"
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
