//! Tests for `ModelCache` on-disk layout.

use cairn_embeddings_local::{EmbeddingModelKind, ModelCache};
use tempfile::TempDir;

#[test]
fn is_present_false_before_fetch() {
    let dir = TempDir::new().unwrap();
    let cache = ModelCache::new(dir.path());
    assert!(!cache.is_present(EmbeddingModelKind::BgeSmallEnV1_5));
    assert!(!cache.is_present(EmbeddingModelKind::AllMiniLmL6V2));
}

#[test]
fn is_present_true_after_integrity_marker() {
    let dir = TempDir::new().unwrap();
    let cache = ModelCache::new(dir.path());
    let model_dir = cache.model_dir(EmbeddingModelKind::BgeSmallEnV1_5);
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join(".integrity"), "abc123").unwrap();
    assert!(cache.is_present(EmbeddingModelKind::BgeSmallEnV1_5));
    assert!(!cache.is_present(EmbeddingModelKind::AllMiniLmL6V2));
}

#[test]
fn ensure_returns_model_not_fetched_when_absent() {
    let dir = TempDir::new().unwrap();
    let cache = ModelCache::new(dir.path());
    match cache.ensure(EmbeddingModelKind::BgeSmallEnV1_5) {
        Ok(_) => panic!("expected Err when model not fetched"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("not fetched") || msg.contains("bge"),
                "unexpected error: {msg}"
            );
        }
    }
}

#[test]
fn fetch_already_cached_returns_no_bytes() {
    let dir = TempDir::new().unwrap();
    let cache = ModelCache::new(dir.path());
    // Simulate already-cached by writing an integrity file.
    let model_dir = cache.model_dir(EmbeddingModelKind::BgeSmallEnV1_5);
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join(".integrity"), "existing_digest").unwrap();

    let report = cache.fetch(EmbeddingModelKind::BgeSmallEnV1_5).unwrap();
    assert!(report.already_cached);
    assert_eq!(report.bytes_downloaded, 0);
    assert_eq!(report.integrity, "existing_digest");
}
