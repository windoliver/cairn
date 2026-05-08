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

    // The mock returns a stub 3-element vector; pin `target_dim` to 3
    // so the new per-vector dim validation accepts it. Production paths
    // leave `target_dim` at the model default (1536).
    let embedder = OpenAiEmbedder::new(
        "test-key",
        &server.uri(),
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .expect("construct embedder")
    .with_dim(3);

    let embedder_clone = embedder.clone();
    let v = tokio::task::spawn_blocking(move || embedder_clone.embed_query("hello"))
        .await
        .expect("spawn_blocking joined")
        .expect("embed_query succeeded");
    assert_eq!(v, vec![0.1_f32, 0.2_f32, 0.3_f32]);
}

/// Negative case: when the API returns a vector whose length differs
/// from `target_dim`, the embedder must surface `BadResponseShape`
/// rather than caching/returning a wrong-dim vector that would later
/// be rejected by the sqlite-vec INSERT path.
#[tokio::test]
async fn embed_query_rejects_dim_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "embedding": [0.1_f32, 0.2_f32, 0.3_f32], "index": 0 }
            ],
            "model": "text-embedding-3-large"
        })))
        .mount(&server)
        .await;
    // target_dim defaults to 1536; the mock returns 3.
    let embedder = OpenAiEmbedder::new(
        "test-key",
        &server.uri(),
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .expect("construct embedder");
    let embedder_clone = embedder.clone();
    let res = tokio::task::spawn_blocking(move || embedder_clone.embed_query("hello"))
        .await
        .expect("spawn_blocking joined");
    let err = res.expect_err("dim mismatch must error");
    let msg = err.to_string();
    assert!(
        msg.contains("1536") && msg.contains('3'),
        "expected dim-mismatch error message, got: {msg}"
    );
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

/// Privacy invariant: the Authorization header is marked sensitive in
/// `OpenAiEmbedder::new`, so reqwest must redact the bearer token from the
/// embedder's `Debug` output. Belt-and-suspenders smoke check on top of
/// reqwest's documented `set_sensitive` behavior.
#[tokio::test]
async fn debug_does_not_leak_api_key() {
    let embedder = OpenAiEmbedder::new(
        "sk-super-secret-do-not-leak",
        "https://example.invalid",
        EmbeddingModelKind::OpenAiTextEmbedding3Large,
    )
    .expect("construct embedder");
    let dbg = format!("{embedder:?}");
    assert!(
        !dbg.contains("sk-super-secret-do-not-leak"),
        "Debug output leaked api key: {dbg}"
    );
    assert!(
        !dbg.contains("Bearer "),
        "Debug output leaked Bearer prefix: {dbg}"
    );
}
