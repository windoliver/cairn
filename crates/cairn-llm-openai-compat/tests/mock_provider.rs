//! Integration tests using wiremock to verify the `complete()` text and JSON schema paths.

use cairn_core::{
    config::{LlmConfig, LlmProvider},
    contract::{CompletionOutput, CompletionRequest, LlmError},
};
use cairn_llm_openai_compat::build_llm_provider;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Minimal valid `OpenAI` chat completion response body for `content`.
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

#[tokio::test]
async fn text_completion_no_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("Hello!")))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let req = CompletionRequest::builder()
        .prompt("Say hello".to_string())
        .build();
    let out = provider.complete(&req).await.unwrap();
    assert!(matches!(out, CompletionOutput::Text(ref s) if s == "Hello!"));
}

#[tokio::test]
async fn json_completion_schema_match() {
    let server = MockServer::start().await;
    let body_content = r#"{"kind":"feedback","confidence":0.9}"#;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(body_content)))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "required": ["kind", "confidence"],
        "properties": {
            "kind": { "type": "string" },
            "confidence": { "type": "number" }
        }
    });
    let req = CompletionRequest::builder()
        .prompt("Extract memory".to_string())
        .schema(schema)
        .build();
    let out = provider.complete(&req).await.unwrap();
    assert!(
        matches!(out, CompletionOutput::Json(ref v) if v["kind"] == "feedback"),
        "expected Json with kind=feedback, got {out:?}"
    );
}

#[tokio::test]
async fn json_completion_schema_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"kind":"feedback"}"#)),
        )
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "required": ["kind", "confidence"],
        "properties": {
            "kind": { "type": "string" },
            "confidence": { "type": "number" }
        }
    });
    let req = CompletionRequest::builder()
        .prompt("Extract memory".to_string())
        .schema(schema)
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::InvalidJsonOutput { .. }),
        "expected InvalidJsonOutput, got {err:?}"
    );
}

#[tokio::test]
async fn json_completion_unparseable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("not json {")))
        .mount(&server)
        .await;

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("test-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let schema = serde_json::json!({ "type": "object" });
    let req = CompletionRequest::builder()
        .prompt("Extract memory".to_string())
        .schema(schema)
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(matches!(err, LlmError::InvalidJsonOutput { .. }));
}
