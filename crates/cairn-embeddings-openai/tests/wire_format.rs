//! Verify the request/response wire format with a wiremock server.
//!
//! No real network is touched; the embedder is pointed at the wiremock URI
//! and the trait method is invoked from a `spawn_blocking` task to mirror
//! how the store calls embedders in production.

use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::EmbeddingModel;
use cairn_embeddings_openai::OpenAiEmbedder;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embed_query_round_trip() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "embedding": [0.1_f32, 0.2_f32, 0.3_f32], "index": 0 }
            ],
            "model": "text-embedding-3-large"
        })))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::new(
        "test-key",
        &server.uri(),
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .expect("construct embedder");

    let embedder_clone = embedder.clone();
    let v = tokio::task::spawn_blocking(move || embedder_clone.embed_query("hello"))
        .await
        .expect("spawn_blocking joined")
        .expect("embed_query succeeded");
    assert_eq!(v, vec![0.1_f32, 0.2_f32, 0.3_f32]);
}

#[tokio::test]
async fn embed_query_returns_auth_error_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "bad key", "type": "auth", "code": "invalid_api_key" }
        })))
        .mount(&server)
        .await;

    let embedder = OpenAiEmbedder::new(
        "wrong-key",
        &server.uri(),
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .expect("construct embedder");

    let embedder_clone = embedder.clone();
    let result = tokio::task::spawn_blocking(move || embedder_clone.embed_query("hello"))
        .await
        .expect("spawn_blocking joined");
    assert!(result.is_err(), "expected auth error, got {result:?}");
}
