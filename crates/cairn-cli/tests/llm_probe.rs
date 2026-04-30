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
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
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
        .timeout(std::time::Duration::from_secs(15))
        .assert();

    assert.code(69).stderr(contains("llm.provider_unreachable"));
}
