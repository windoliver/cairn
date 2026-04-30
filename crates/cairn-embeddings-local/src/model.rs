//! `EmbeddingModel` trait + `MockEmbedder` for tests.

use cairn_core::config::EmbeddingModelKind;

use crate::error::EmbeddingError;

/// Synchronous CPU-bound embedding. Callers wrap in
/// `tokio::task::spawn_blocking`.
pub trait EmbeddingModel: Send + Sync {
    /// Which model variant this instance wraps.
    fn kind(&self) -> EmbeddingModelKind;

    /// Output dimension (both BGE and `MiniLM`: 384).
    fn dim(&self) -> usize;

    /// Embed a document (record body). BGE applies no prefix here.
    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;

    /// Embed a user query. BGE applies asymmetric retrieval prefix;
    /// `MiniLM` treats this identically to `embed_document`.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}

/// Deterministic mock embedder for tests.
///
/// Produces a 384-dim vector from blake3(text) bytes reinterpreted as f32 LE,
/// then L2-normalised. No candle dep, no model files.
pub struct MockEmbedder {
    kind: EmbeddingModelKind,
}

impl MockEmbedder {
    /// Construct a mock that reports itself as the given model kind.
    #[must_use]
    pub fn new(kind: EmbeddingModelKind) -> Self {
        Self { kind }
    }
}

impl EmbeddingModel for MockEmbedder {
    fn kind(&self) -> EmbeddingModelKind {
        self.kind
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(mock_vector(text))
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(mock_vector(text))
    }
}

/// Produce a deterministic normalised 384-dim vector from a string.
/// Uses blake3 hash repeated to fill 384*4 bytes, then reinterpreted as f32 LE.
#[must_use]
pub fn mock_vector(text: &str) -> Vec<f32> {
    let hash = blake3::hash(text.as_bytes());
    let bytes = hash.as_bytes();
    let needed = 384 * 4;
    let extended: Vec<u8> = bytes.iter().cycle().take(needed).copied().collect();
    let mut v: Vec<f32> = extended
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    l2_normalize(&mut v);
    v
}

/// In-place L2 normalisation.
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
