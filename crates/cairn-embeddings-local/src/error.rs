//! Error type for the local embedding runtime.

use std::path::PathBuf;

use cairn_core::config::EmbeddingModelKind;

/// Errors from the local embedding runtime.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum EmbeddingError {
    /// Model files not present on disk. Run `cairn bootstrap` or
    /// `cairn admin model fetch` to download.
    #[error("embedding model not fetched: {kind:?} — run `cairn bootstrap`")]
    ModelNotFetched {
        /// The model that has not been fetched yet.
        kind: EmbeddingModelKind,
    },

    /// On-disk file digest does not match the compiled-in constant.
    #[error("integrity mismatch at {path}: run `cairn admin model fetch --force`")]
    IntegrityMismatch {
        /// Path to the file whose digest did not match.
        path: PathBuf,
    },

    /// Tokenizer error (wrapped as String to avoid leaking the dep).
    #[error("tokenizer error: {0}")]
    Tokenizer(String),

    /// Candle inference error.
    #[error("inference error: {0}")]
    Inference(#[from] candle_core::Error),

    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// `HuggingFace` Hub (huggingface.co) network error.
    #[error("hf-hub error: {0}")]
    Network(String),
}

impl From<tokenizers::Error> for EmbeddingError {
    fn from(e: tokenizers::Error) -> Self {
        Self::Tokenizer(e.to_string())
    }
}
