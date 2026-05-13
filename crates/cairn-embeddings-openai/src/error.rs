//! Error type for `OpenAI` embedding calls. Maps into
//! [`cairn_embeddings_local::EmbeddingError`].
//!
//! The local error enum has no `String`-bearing inference variant
//! (`Inference` carries `candle_core::Error`), so parse / shape failures
//! are routed through `EmbeddingError::Network`. If a future task adds a
//! cloud-shaped variant, route there instead.

use cairn_embeddings_local::EmbeddingError;
use thiserror::Error;

/// Errors raised by [`crate::OpenAiEmbedder`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OpenAiEmbeddingError {
    /// `OpenAI` rejected the credentials (HTTP 401 or 403).
    #[error("authentication failed (HTTP {status})")]
    AuthFailed {
        /// Raw HTTP status (401 for invalid keys, 403 for org/region blocks).
        status: u16,
    },
    /// `OpenAI` rate-limited the request (HTTP 429), and we exhausted retries.
    #[error("rate limited (HTTP 429)")]
    RateLimited,
    /// Server error after the configured number of retries.
    #[error("server error (HTTP {status}) after {retries} retries")]
    Server {
        /// Last observed HTTP status code (5xx).
        status: u16,
        /// Number of retries attempted before giving up.
        retries: u32,
    },
    /// Generic network / transport error (DNS, TLS, dropped connection).
    #[error("network error: {0}")]
    Network(String),
    /// Response body did not deserialize as a valid embedding payload.
    #[error("response parse error: {0}")]
    Parse(String),
    /// `OPENAI_API_KEY` was missing from the environment.
    #[error("OPENAI_API_KEY not set or empty")]
    MissingKey,
    /// Server returned a payload with the wrong number of vectors.
    #[error("model returned wrong number of vectors: expected {expected}, got {got}")]
    BadResponseShape {
        /// Number of vectors the caller asked for.
        expected: usize,
        /// Number of vectors the server returned.
        got: usize,
    },
}

impl From<OpenAiEmbeddingError> for EmbeddingError {
    fn from(e: OpenAiEmbeddingError) -> Self {
        // `EmbeddingError::Inference` only carries `candle_core::Error`, not a
        // String, so Parse / BadResponseShape collapse into Network rather than
        // synthesising a candle error. This matches the spirit of the variant
        // (a runtime failure surfacing to the verb layer) without requiring a
        // new String-bearing variant on the local error.
        Self::Network(e.to_string())
    }
}
