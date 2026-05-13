//! Verify prefix asymmetry for BGE (query ≠ document) and its absence for `MiniLM`.
//! Uses `MockEmbedder` since real models need `real-models` feature.

use cairn_embeddings_local::model::EmbeddingModel;
use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};

#[test]
fn mock_embedder_query_equals_document() {
    // MockEmbedder has no asymmetric prefix — both paths call mock_vector directly.
    let m = MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5);
    assert_eq!(
        m.embed_query("foo").unwrap(),
        m.embed_document("foo").unwrap()
    );
}

/// Real BGE asymmetry test — only runs with `--features real-models` and
/// when `CAIRN_TEST_MODELS_DIR` env var points to a directory with the model.
#[cfg(feature = "real-models")]
#[test]
fn bge_query_differs_from_document() {
    use cairn_embeddings_local::ModelCache;
    let dir = std::env::var("CAIRN_TEST_MODELS_DIR")
        .expect("CAIRN_TEST_MODELS_DIR must be set for real-models tests");
    let cache = ModelCache::new(std::path::Path::new(&dir));
    let model = cache.ensure(EmbeddingModelKind::BgeSmallEnV1_5).unwrap();
    let qv = model.embed_query("foo").unwrap();
    let dv = model.embed_document("foo").unwrap();
    assert_ne!(
        qv, dv,
        "BGE must produce different vectors for query vs document"
    );
}
