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
///
/// Uses blake3's extended output function (XOF) to fill `384 * 4` bytes,
/// then interprets each 4-byte group as a signed `i32` in little-endian and
/// converts to `f32`. This avoids the subnormal-collapse problem that would
/// arise from treating arbitrary hash bytes directly as raw `f32` bit patterns.
#[must_use]
pub fn mock_vector(text: &str) -> Vec<f32> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    let mut xof = hasher.finalize_xof();
    let needed = 384 * 4;
    let mut raw = vec![0u8; needed];
    xof.fill(&mut raw);
    // Interpret each 4-byte XOF chunk as a normal f32 in the range [1.0, 2.0)
    // by injecting bits 22..0 into the mantissa and setting a fixed exponent (127).
    // Bit 31 provides the sign (-1.0 or +1.0 range). This avoids both subnormals
    // and the cast-precision-loss lint from i32-to-f32 casting.
    let mut v: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| {
            let bits = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            // Force exponent to 127 (biased), keep sign and lower 23 mantissa bits.
            let normal_bits = (bits & 0x8000_0000) | (127 << 23) | (bits & 0x007f_ffff);
            f32::from_bits(normal_bits)
        })
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
