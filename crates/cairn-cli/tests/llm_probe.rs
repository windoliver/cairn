//! End-to-end tests for `cairn llm probe`.
//!
//! Spawns the real `cairn` binary against a `wiremock` server and asserts
//! the exit codes pinned by ADR 0001 §"Error codes (stable, machine-readable)".

use assert_cmd::Command;
use predicates::str::contains;
use std::fs;

/// Helper: write a `.cairn/config.yaml` to `dir` pointing at `base_url`.
fn write_config(dir: &std::path::Path, base_url: &str) {
    let cairn_dir = dir.join(".cairn");
    fs::create_dir_all(&cairn_dir).expect("mkdir .cairn");
    let yaml = format!(
        "llm:\n  provider: openai-compatible\n  base_url: {base_url}\n  \
         model: gpt-4o-mini\n  api_key: test-key\n"
    );
    fs::write(cairn_dir.join("config.yaml"), yaml).expect("write config");
}

/// Helper: minimal valid `OpenAI` chat completion response.
fn chat_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1_700_000_000u64,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
    })
}

#[test]
fn probe_exits_78_when_no_provider_configured() {
    let vault = tempfile::tempdir().expect("tempdir");
    // No .cairn/config.yaml — defaults apply, provider is None.

    let mut cmd = Command::cargo_bin("cairn").expect("cargo bin cairn");
    let assert = cmd
        .current_dir(vault.path())
        .args(["llm", "probe"])
        .env_remove("CAIRN_VAULT")
        .env_remove("CAIRN_REGISTRY")
        .env_remove("OPENROUTER_API_KEY")
        .assert();

    assert.code(78).stderr(contains("llm.not_configured"));
}

#[tokio::test]
async fn probe_exits_0_with_text_response() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("ping")))
        .mount(&server)
        .await;

    let vault = tempfile::tempdir().expect("tempdir");
    write_config(vault.path(), &server.uri());

    let vault_path = vault.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cairn")
            .expect("cargo bin cairn")
            .current_dir(&vault_path)
            .args(["llm", "probe"])
            .env_remove("CAIRN_VAULT")
            .env_remove("CAIRN_REGISTRY")
            .output()
            .expect("run cairn")
    })
    .await
    .expect("blocking task");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ok"), "stdout: {stdout}");
    assert!(stdout.contains("ping"), "stdout: {stdout}");
}

#[tokio::test]
async fn probe_exits_77_on_401() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let body = serde_json::json!({
        "error": {
            "message": "Incorrect API key provided",
            "code": "invalid_api_key",
            "type": "invalid_request_error"
        }
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(body))
        .mount(&server)
        .await;

    let vault = tempfile::tempdir().expect("tempdir");
    write_config(vault.path(), &server.uri());

    let vault_path = vault.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cairn")
            .expect("cargo bin cairn")
            .current_dir(&vault_path)
            .args(["llm", "probe", "--json"])
            .env_remove("CAIRN_VAULT")
            .env_remove("CAIRN_REGISTRY")
            .output()
            .expect("run cairn")
    })
    .await
    .expect("blocking task");

    assert_eq!(
        output.status.code(),
        Some(77),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("llm.auth_denied"), "stderr: {stderr}");
}

#[test]
fn probe_exits_69_on_unreachable_endpoint() {
    let vault = tempfile::tempdir().expect("tempdir");
    // Port 1 is reserved and refuses connections.
    write_config(vault.path(), "http://127.0.0.1:1");

    let mut cmd = Command::cargo_bin("cairn").expect("cargo bin cairn");
    let assert = cmd
        .current_dir(vault.path())
        .args(["llm", "probe"])
        .env_remove("CAIRN_VAULT")
        .env_remove("CAIRN_REGISTRY")
        .timeout(std::time::Duration::from_secs(30))
        .assert();

    assert.code(69).stderr(contains("llm.provider_unreachable"));
}

#[tokio::test]
async fn probe_retries_on_429_then_succeeds() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // First two requests: 429 (rate-limited). Third: 200.
    let rate_limit_body = serde_json::json!({
        "error": {
            "message": "Rate limit exceeded",
            "code": "rate_limit_exceeded",
            "type": "rate_limit_exceeded"
        }
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(rate_limit_body))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("survived")))
        .mount(&server)
        .await;

    let vault = tempfile::tempdir().expect("tempdir");
    write_config(vault.path(), &server.uri());

    let vault_path = vault.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cairn")
            .expect("cargo bin cairn")
            .current_dir(&vault_path)
            .args(["llm", "probe"])
            .env_remove("CAIRN_VAULT")
            .env_remove("CAIRN_REGISTRY")
            .output()
            .expect("run cairn")
    })
    .await
    .expect("blocking task");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("survived"), "stdout: {stdout}");
}

#[tokio::test]
async fn probe_exits_69_when_429_retries_exhausted() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let rate_limit_body = serde_json::json!({
        "error": {
            "message": "Rate limit exceeded",
            "code": "rate_limit_exceeded",
            "type": "rate_limit_exceeded"
        }
    });
    // Always 429 — must exhaust the standard retry policy (3 retries) and
    // surface as ProviderUnreachable.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(rate_limit_body))
        .mount(&server)
        .await;

    let vault = tempfile::tempdir().expect("tempdir");
    write_config(vault.path(), &server.uri());

    let vault_path = vault.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cairn")
            .expect("cargo bin cairn")
            .current_dir(&vault_path)
            .args(["llm", "probe", "--json"])
            .env_remove("CAIRN_VAULT")
            .env_remove("CAIRN_REGISTRY")
            .output()
            .expect("run cairn")
    })
    .await
    .expect("blocking task");

    assert_eq!(
        output.status.code(),
        Some(69),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("llm.provider_unreachable"),
        "stderr: {stderr}"
    );
}

#[tokio::test]
async fn probe_exits_1_on_schema_mismatch() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Returns valid JSON but missing the required `kind` field.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_response(r#"{"confidence":0.5}"#)),
        )
        .mount(&server)
        .await;

    let vault = tempfile::tempdir().expect("tempdir");
    write_config(vault.path(), &server.uri());

    // Write a JSON Schema file requiring `kind` and `confidence`.
    let schema_file = vault.path().join("schema.json");
    fs::write(
        &schema_file,
        r#"{
            "type": "object",
            "required": ["kind", "confidence"],
            "properties": {
                "kind": { "type": "string" },
                "confidence": { "type": "number" }
            }
        }"#,
    )
    .expect("write schema");

    let vault_path = vault.path().to_path_buf();
    let schema_path = schema_file.to_string_lossy().to_string();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cairn")
            .expect("cargo bin cairn")
            .current_dir(&vault_path)
            .args([
                "llm",
                "probe",
                "--json",
                "--schema-file",
                schema_path.as_str(),
            ])
            .env_remove("CAIRN_VAULT")
            .env_remove("CAIRN_REGISTRY")
            .output()
            .expect("run cairn")
    })
    .await
    .expect("blocking task");

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("llm.invalid_json_output"),
        "stderr: {stderr}"
    );
}

#[tokio::test]
async fn probe_succeeds_on_schema_match() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"kind":"feedback","confidence":0.9}"#)),
        )
        .mount(&server)
        .await;

    let vault = tempfile::tempdir().expect("tempdir");
    write_config(vault.path(), &server.uri());

    let schema_file = vault.path().join("schema.json");
    fs::write(
        &schema_file,
        r#"{
            "type": "object",
            "required": ["kind", "confidence"],
            "properties": {
                "kind": { "type": "string" },
                "confidence": { "type": "number" }
            }
        }"#,
    )
    .expect("write schema");

    let vault_path = vault.path().to_path_buf();
    let schema_path = schema_file.to_string_lossy().to_string();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("cairn")
            .expect("cargo bin cairn")
            .current_dir(&vault_path)
            .args([
                "llm",
                "probe",
                "--json",
                "--schema-file",
                schema_path.as_str(),
            ])
            .env_remove("CAIRN_VAULT")
            .env_remove("CAIRN_REGISTRY")
            .output()
            .expect("run cairn")
    })
    .await
    .expect("blocking task");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The probe wraps its output as JSON; the inner JSON reply has escaped
    // quotes. Just assert the core values appear somewhere.
    assert!(stdout.contains("feedback"), "stdout: {stdout}");
    assert!(stdout.contains("\"ok\":true"), "stdout: {stdout}");
}
