//! End-to-end tests for local sensor privacy gates.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily, TerminalContext,
};
use sha2::{Digest as _, Sha256};

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd.env_remove("CAIRN_ISSUER");
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
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout was not valid JSON: {err}\nstdout: {stdout:?}");
    })
}

fn run_sensor_toggle(vault: &Path, action: &str, sensor: &str) -> serde_json::Value {
    let out = cli()
        .current_dir(vault)
        .args(["sensor", action, sensor, "--reason", "e2e_test", "--json"])
        .output()
        .unwrap_or_else(|err| panic!("cairn sensor {action} {sensor}: {err}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "sensor {action} {sensor} failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

fn local_sensor_status(vault: &Path, sensor: &str) -> serde_json::Value {
    let out = cli()
        .current_dir(vault)
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "status failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let response = json_stdout(&out);
    response["sensors"]["local"]
        .as_array()
        .expect("local sensors")
        .iter()
        .find(|row| row["sensor"] == sensor)
        .unwrap_or_else(|| panic!("missing {sensor} status row: {response}"))
        .clone()
}

fn set_screen_config_enabled(vault: &Path) {
    let mut config = cairn_cli::config::load(vault, &cairn_cli::config::CliOverrides::default())
        .expect("load config");
    config.sensors.screen.enabled = true;
    let yaml = yaml_serde::to_string(&config).expect("serialize config");
    std::fs::write(vault.join(".cairn/config.yaml"), yaml).expect("write config");
}

fn set_recording_config_enabled(vault: &Path) {
    let mut config = cairn_cli::config::load(vault, &cairn_cli::config::CliOverrides::default())
        .expect("load config");
    config.sensors.recording.enabled = true;
    let yaml = yaml_serde::to_string(&config).expect("serialize config");
    std::fs::write(vault.join(".cairn/config.yaml"), yaml).expect("write config");
}

fn set_terminal_config_enabled(vault: &Path) {
    let mut config = cairn_cli::config::load(vault, &cairn_cli::config::CliOverrides::default())
        .expect("load config");
    config.sensors.terminal.enabled = true;
    let yaml = yaml_serde::to_string(&config).expect("serialize config");
    std::fs::write(vault.join(".cairn/config.yaml"), yaml).expect("write config");
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures/v0/recordings")
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

fn vault_contains_exact_bytes_outside_sources(root: &Path, bytes: &[u8]) -> bool {
    regular_files_under(root).into_iter().any(|path| {
        let relative = path.strip_prefix(root).unwrap_or(path.as_path());
        !relative.starts_with("sources")
            && std::fs::read(path).is_ok_and(|content| content == bytes)
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write hex");
    }
    out
}

fn terminal_capture_event(payload_ref: &str, body: &[u8]) -> CaptureEvent {
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let sensor =
        Identity::parse("snr:local:terminal:default:v1").expect("valid terminal sensor identity");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("valid event id"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse("2026-05-14T00:00:00Z").expect("valid timestamp"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some("sess-terminal-gate".to_owned()),
            turn_id: Some("turn-terminal-gate".to_owned()),
            tool_id: Some("tool-terminal-gate".to_owned()),
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("valid payload hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse("2026-05-14T00:00:00Z").expect("valid timestamp"),
        payload: CapturePayload::Terminal {
            command: "printf SENTINEL_TERMINAL_BODY".to_owned(),
            exit_code: Some(0),
            context: Some(TerminalContext::NonInteractiveOrStructured),
        },
        source_family: SourceFamily::Terminal,
    }
}

fn hook_pre_tool_event(
    event_id: &str,
    session: &str,
    turn: &str,
    tool_id: &str,
    payload_ref: &str,
    body: &[u8],
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("valid hook sensor identity");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("valid event id"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse("2026-05-14T00:00:00Z").expect("valid timestamp"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session.to_owned()),
            turn_id: Some(turn.to_owned()),
            tool_id: Some(tool_id.to_owned()),
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("valid payload hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse("2026-05-14T00:00:00Z").expect("valid timestamp"),
        payload: CapturePayload::Hook {
            hook_name: "PreToolUse".to_owned(),
            tool_name: Some("shell".to_owned()),
        },
        source_family: SourceFamily::Hook,
    }
}

fn terminal_tool_output_event(
    event_id: &str,
    session: &str,
    turn: &str,
    tool_id: &str,
    payload_ref: &str,
    body: &[u8],
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:terminal:default:v1").expect("valid terminal sensor identity");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("valid event id"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse("2026-05-14T00:00:01Z").expect("valid timestamp"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session.to_owned()),
            turn_id: Some(turn.to_owned()),
            tool_id: Some(tool_id.to_owned()),
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("valid payload hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse("2026-05-14T00:00:01Z").expect("valid timestamp"),
        payload: CapturePayload::Terminal {
            command: "printf E2E_TERMINAL_BODY".to_owned(),
            exit_code: Some(0),
            context: Some(TerminalContext::NonInteractiveOrStructured),
        },
        source_family: SourceFamily::Terminal,
    }
}

fn write_source(vault: &Path, family: &str, name: &str, body: &[u8]) -> String {
    let rel = format!("sources/{family}/{name}.txt");
    let path = vault.join(&rel);
    std::fs::create_dir_all(path.parent().expect("source parent")).expect("source dir");
    std::fs::write(path, body).expect("write source body");
    rel
}

fn write_terminal_turn_jsonl(
    vault: &Path,
    name: &str,
    pre_event_id: &str,
    terminal_event_id: &str,
    terminal_body: &[u8],
) -> PathBuf {
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = format!("turn-{name}");
    let tool_id = "tool-e2e";
    let pre_body = br#"{"tool":"shell","input":"body-free"}"#;
    let pre_ref = write_source(vault, "hook", pre_event_id, pre_body);
    let terminal_ref = write_source(vault, "terminal", terminal_event_id, terminal_body);
    let events = [
        hook_pre_tool_event(pre_event_id, session, &turn, tool_id, &pre_ref, pre_body),
        terminal_tool_output_event(
            terminal_event_id,
            session,
            &turn,
            tool_id,
            &terminal_ref,
            terminal_body,
        ),
    ];
    let jsonl = vault.join(format!("{name}.jsonl"));
    let lines = events
        .iter()
        .map(|event| serde_json::to_string(event).expect("serialize event"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&jsonl, format!("{lines}\n")).expect("write event jsonl");
    jsonl
}

fn run_capture_trace_json(vault: &Path, jsonl: &Path) -> serde_json::Value {
    let out = cli()
        .current_dir(vault)
        .args([
            "capture_trace",
            "--from",
            jsonl.to_str().expect("utf-8 jsonl path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace");
    assert_eq!(
        out.status.code(),
        Some(0),
        "capture_trace failed; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)
}

fn sensor_drop_metrics(vault: &Path) -> Vec<serde_json::Value> {
    let metrics_path = vault.join(".cairn/metrics.jsonl");
    let Ok(metrics) = std::fs::read_to_string(&metrics_path) else {
        return Vec::new();
    };
    metrics
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid metric json"))
        .filter(|row: &serde_json::Value| row["event"] == "sensor_drop")
        .collect()
}

#[test]
fn hook_without_consent_writes_no_trace_artifact() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());

    let out = cli()
        .args([
            "hook",
            "UserPromptSubmit",
            "--vault-path",
            vault.path().to_str().expect("utf-8 vault path"),
            "--payload",
            r#"{"session_id":"sess-1","prompt":"remember this"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook UserPromptSubmit");

    assert_eq!(
        out.status.code(),
        Some(77),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !vault.path().join(".cairn/hooks/traces").exists(),
        "privacy gate must run before trace artifact directory creation"
    );
    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    let metrics = std::fs::read_to_string(&metrics_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", metrics_path.display()));
    assert!(
        metrics.contains("\"event\":\"sensor_drop\""),
        "missing sensor_drop metric: {metrics}"
    );
    assert!(
        metrics.contains("\"reason\":\"privacy_denied\""),
        "missing privacy_denied reason: {metrics}"
    );
}

#[test]
fn hook_denied_metric_drops_unsafe_session_id() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    let unsafe_session_id = "SECRET USER TEXT";

    let out = cli()
        .args([
            "hook",
            "UserPromptSubmit",
            "--vault-path",
            vault.path().to_str().expect("utf-8 vault path"),
            "--payload",
            &format!(r#"{{"session_id":"{unsafe_session_id}","prompt":"remember this"}}"#),
            "--json",
        ])
        .output()
        .expect("cairn hook UserPromptSubmit");

    assert_eq!(
        out.status.code(),
        Some(77),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    let metrics = std::fs::read_to_string(&metrics_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", metrics_path.display()));
    assert!(
        !metrics.contains(unsafe_session_id),
        "denied hook metric leaked unsafe session_id: {metrics}"
    );
    let drops = sensor_drop_metrics(vault.path());
    let hook_drop = drops
        .iter()
        .find(|row| row["sensor"] == "hook" && row["stage"] == "pre_artifact")
        .unwrap_or_else(|| panic!("missing hook drop metric: {drops:?}"));
    assert_eq!(hook_drop.get("session_id"), None);
}

#[test]
fn screen_without_consent_writes_no_png() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    set_screen_config_enabled(vault.path());
    let output = vault.path().join("screen.png");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "screen",
            "capture",
            "--output",
            output.to_str().expect("utf-8 output path"),
            "--json",
        ])
        .output()
        .expect("cairn screen capture");

    assert!(
        matches!(out.status.code(), Some(77 | 78)),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !output.exists(),
        "privacy gate must run before PNG output creation"
    );
    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    let metrics = std::fs::read_to_string(&metrics_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", metrics_path.display()));
    assert!(
        metrics.contains("\"sensor\":\"screen\""),
        "missing screen metric: {metrics}"
    );
    assert!(
        metrics.contains("\"reason\":\"privacy_denied\""),
        "missing privacy_denied reason: {metrics}"
    );
    assert!(
        metrics.contains("\"stage\":\"pre_capture\""),
        "missing pre_capture stage: {metrics}"
    );
}

#[test]
fn recording_without_consent_stages_no_payloads() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    set_recording_config_enabled(vault.path());
    let fixtures = fixtures_dir();
    let media = fixtures.join("demo.mp4");
    let fixture_json = fixtures.join("recording-fixture.json");

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

    assert_ne!(
        out.status.code(),
        Some(0),
        "recording ingest unexpectedly succeeded; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        recording_payload_files(vault.path()).is_empty(),
        "privacy gate must run before derived recording payload staging"
    );
    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    let metrics = std::fs::read_to_string(&metrics_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", metrics_path.display()));
    assert!(
        metrics.contains("\"sensor\":\"recording\""),
        "missing recording metric: {metrics}"
    );
    assert!(
        metrics.contains("\"reason\":\"privacy_denied\""),
        "missing privacy_denied reason: {metrics}"
    );
}

#[test]
fn capture_trace_denies_local_sensor_before_body_resolution() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    set_terminal_config_enabled(vault.path());
    let body = b"SENTINEL_TERMINAL_BODY";
    let payload_ref = "sources/terminal/terminal-body.txt";
    let source_path = vault.path().join(payload_ref);
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("source dir");
    std::fs::write(&source_path, body).expect("write terminal source body");
    let event = terminal_capture_event(payload_ref, body);
    let jsonl = vault.path().join("terminal-events.jsonl");
    std::fs::write(
        &jsonl,
        format!(
            "{}\n",
            serde_json::to_string(&event).expect("serialize capture event")
        ),
    )
    .expect("write event jsonl");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "capture_trace",
            "--from",
            jsonl.to_str().expect("utf-8 jsonl path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("stdout was not valid JSON: {err}\nstdout: {stdout:?}");
    });
    assert_eq!(response["status"], "committed");
    assert_eq!(
        response["data"]["failed_turns"][0]["reason"],
        "sensor_gate:privacy_denied"
    );
    assert!(
        !vault_contains_exact_bytes_outside_sources(vault.path(), body),
        "denied terminal body must not be projected outside sources/"
    );
}

#[test]
fn capture_trace_denied_metric_drops_unsafe_turn_id() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    set_terminal_config_enabled(vault.path());
    let body = b"SENTINEL_TERMINAL_BODY";
    let unsafe_turn_id = "SECRET USER TEXT";
    let payload_ref = write_source(vault.path(), "terminal", "unsafe-turn", body);
    let event = terminal_tool_output_event(
        "01ARZ3NDEKTSV4RRFFQ69G5FC0",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        unsafe_turn_id,
        "tool-terminal-gate",
        &payload_ref,
        body,
    );
    let jsonl = vault.path().join("unsafe-turn-events.jsonl");
    std::fs::write(
        &jsonl,
        format!(
            "{}\n",
            serde_json::to_string(&event).expect("serialize capture event")
        ),
    )
    .expect("write event jsonl");

    let response = run_capture_trace_json(vault.path(), &jsonl);
    assert_eq!(response["status"], "committed");
    assert_eq!(
        response["data"]["failed_turns"][0]["reason"],
        "sensor_gate:privacy_denied"
    );
    let metrics_path = vault.path().join(".cairn/metrics.jsonl");
    let metrics = std::fs::read_to_string(&metrics_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", metrics_path.display()));
    assert!(
        !metrics.contains(unsafe_turn_id),
        "denied capture_trace metric leaked unsafe turn_id: {metrics}"
    );
    let drops = sensor_drop_metrics(vault.path());
    let terminal_drop = drops
        .iter()
        .find(|row| row["sensor"] == "terminal" && row["stage"] == "pre_extraction")
        .unwrap_or_else(|| panic!("missing terminal drop metric: {drops:?}"));
    assert_eq!(terminal_drop.get("turn_id"), None);
}

#[test]
fn sensor_enable_disable_gates_capture_trace_end_to_end() {
    let vault = tempfile::tempdir().expect("tempdir");
    bootstrap_vault(vault.path());
    run_sensor_toggle(vault.path(), "enable", "hook");

    let denied_before_body = b"E2E_TERMINAL_DISABLED_BEFORE_ENABLE";
    let denied_before_jsonl = write_terminal_turn_jsonl(
        vault.path(),
        "before-enable",
        "01ARZ3NDEKTSV4RRFFQ69G5FB0",
        "01ARZ3NDEKTSV4RRFFQ69G5FB1",
        denied_before_body,
    );
    let denied_before = run_capture_trace_json(vault.path(), &denied_before_jsonl);
    assert_eq!(denied_before["status"], "committed");
    assert_eq!(
        denied_before["data"]["failed_turns"][0]["reason"],
        "sensor_gate:disabled"
    );
    assert!(
        !vault_contains_exact_bytes_outside_sources(vault.path(), denied_before_body),
        "disabled terminal body must not be projected outside sources/"
    );
    let terminal_status = local_sensor_status(vault.path(), "terminal");
    assert_eq!(terminal_status["enabled"], false);
    assert_eq!(terminal_status["consent"], "missing");
    assert_eq!(terminal_status["gate"], "disabled");
    assert_eq!(terminal_status["last_drop_reason"], "disabled");

    let enable_terminal = run_sensor_toggle(vault.path(), "enable", "terminal");
    assert_eq!(enable_terminal["status"], "enabled");
    let enabled_status = local_sensor_status(vault.path(), "terminal");
    assert_eq!(enabled_status["enabled"], true);
    assert_eq!(enabled_status["consent"], "enabled");
    assert_eq!(enabled_status["gate"], "allowed");

    let allowed_body = b"E2E_TERMINAL_ALLOWED_AFTER_ENABLE";
    let allowed_jsonl = write_terminal_turn_jsonl(
        vault.path(),
        "after-enable",
        "01ARZ3NDEKTSV4RRFFQ69G5FB2",
        "01ARZ3NDEKTSV4RRFFQ69G5FB3",
        allowed_body,
    );
    let allowed = run_capture_trace_json(vault.path(), &allowed_jsonl);
    assert_eq!(allowed["status"], "committed");
    assert!(
        allowed["data"]["failed_turns"]
            .as_array()
            .expect("failed turns array")
            .is_empty(),
        "enabled terminal import should not fail: {allowed}"
    );

    let disable_terminal = run_sensor_toggle(vault.path(), "disable", "terminal");
    assert_eq!(disable_terminal["status"], "disabled");
    let disabled_status = local_sensor_status(vault.path(), "terminal");
    assert_eq!(disabled_status["enabled"], false);
    assert_eq!(disabled_status["consent"], "disabled");
    assert_eq!(disabled_status["gate"], "disabled");

    let denied_after_body = b"E2E_TERMINAL_DISABLED_AFTER_DISABLE";
    let denied_after_jsonl = write_terminal_turn_jsonl(
        vault.path(),
        "after-disable",
        "01ARZ3NDEKTSV4RRFFQ69G5FB4",
        "01ARZ3NDEKTSV4RRFFQ69G5FB5",
        denied_after_body,
    );
    let denied_after = run_capture_trace_json(vault.path(), &denied_after_jsonl);
    assert_eq!(denied_after["status"], "committed");
    assert_eq!(
        denied_after["data"]["failed_turns"][0]["reason"],
        "sensor_gate:disabled"
    );
    assert!(
        !vault_contains_exact_bytes_outside_sources(vault.path(), denied_after_body),
        "disabled terminal body must not be projected outside sources/"
    );

    let drops = sensor_drop_metrics(vault.path());
    let terminal_disabled_drops = drops
        .iter()
        .filter(|row| row["sensor"] == "terminal" && row["reason"] == "disabled")
        .count();
    assert!(
        terminal_disabled_drops >= 2,
        "expected before/after disabled terminal drops, got {drops:?}"
    );
}
