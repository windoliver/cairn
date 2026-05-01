//! Integration tests using wiremock to verify the `complete()` text and JSON schema paths.

use cairn_core::{
    config::{LlmConfig, LlmProvider},
    contract::{
        CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
    },
};
use cairn_llm_openai_compat::OpenAiCompatProvider;
use cairn_llm_openai_compat::build_llm_provider;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
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
            ResponseTemplate::new(200).set_body_json(chat_response(r#"{"kind":"feedback"}"#)),
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

#[tokio::test]
async fn endpoint_returns_401() {
    let server = MockServer::start().await;
    // Realistic OpenAI-shaped 401 body — async-openai parses it as
    // OpenAIError::ApiError, which our mapper recognises by the code field.
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

    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some(server.uri()),
        model: Some("gpt-4o-mini".into()),
        api_key: Some("bad-key".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let req = CompletionRequest::builder()
        .prompt("hi".to_string())
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::AuthDenied),
        "expected AuthDenied, got {err:?}"
    );
}

#[tokio::test]
async fn json_mode_not_supported_no_http_call() {
    // Build a provider with json_mode=false using the test constructor.
    let provider = OpenAiCompatProvider::with_capabilities(
        "http://127.0.0.1:1", // unreachable — should never be called
        "any-model",
        LLMProviderCapabilities {
            json_mode: false,
            streaming: false,
            tool_calls: false,
        },
    );
    let schema = serde_json::json!({ "type": "object" });
    let req = CompletionRequest::builder()
        .prompt("hi".to_string())
        .schema(schema)
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::CapabilityMissing { ref capability } if capability == "json_mode"),
        "expected CapabilityMissing(json_mode), got {err:?}"
    );
}

#[tokio::test]
async fn budget_exceeded_on_finish_reason_length() {
    let server = MockServer::start().await;
    let mut resp = chat_response("truncat");
    resp["choices"][0]["finish_reason"] = serde_json::json!("length");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(resp))
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
        .prompt("hi".to_string())
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::BudgetExceeded),
        "expected BudgetExceeded, got {err:?}"
    );
}

#[tokio::test]
async fn endpoint_unreachable() {
    // Port 1 is reserved and guaranteed to refuse connections.
    let config = LlmConfig {
        provider: Some(LlmProvider::OpenaiCompatible),
        base_url: Some("http://127.0.0.1:1".into()),
        model: Some("any".into()),
        api_key: Some("any".into()),
    };
    let provider = build_llm_provider(&config).unwrap();
    let req = CompletionRequest::builder()
        .prompt("hi".to_string())
        .build();
    let err = provider.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, LlmError::ProviderUnreachable { .. }),
        "expected ProviderUnreachable, got {err:?}"
    );
}
