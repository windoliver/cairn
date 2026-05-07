//! Snapshot the first 8 dims of `MockEmbedder` output.
//! Catches regressions in `mock_vector` (blake3 hash, fp conversion).

use cairn_embeddings_local::model::EmbeddingModel;
use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};

#[test]
fn mock_bge_query_hello_world_stable() {
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    let v = m.embed_query("hello world").unwrap();
    assert_eq!(v.len(), 384);
    insta::assert_debug_snapshot!("mock_bge_query_hello_world_first8", &v[..8]);
}

#[test]
fn mock_bge_document_hello_world_stable() {
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    let v = m.embed_document("hello world").unwrap();
    assert_eq!(v.len(), 384);
    insta::assert_debug_snapshot!("mock_bge_document_hello_world_first8", &v[..8]);
}

#[test]
fn mock_vector_is_normalised() {
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    let v = m.embed_query("normalisation check").unwrap();
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-5,
        "vector should be L2-normalised, norm={norm}"
    );
}

#[test]
fn mock_different_inputs_differ() {
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    let v1 = m.embed_query("hello").unwrap();
    let v2 = m.embed_query("world").unwrap();
    assert_ne!(v1, v2, "different inputs must produce different vectors");
}
