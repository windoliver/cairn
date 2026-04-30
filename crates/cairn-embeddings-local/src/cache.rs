//! `ModelCache` stub — implemented in Task 4.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::config::EmbeddingModelKind;

use crate::EmbeddingError;
use crate::EmbeddingModel;

/// Manages model files under `.cairn/models/<kind>/`.
pub struct ModelCache {
    root: PathBuf,
}

/// Result of a successful model fetch.
pub struct FetchReport {
    /// Model kind that was fetched.
    pub kind: EmbeddingModelKind,
    /// Bytes downloaded from the Hub (0 if already cached).
    pub bytes_downloaded: u64,
    /// blake3 hex digest of the fetched files.
    pub integrity: String,
    /// `true` if the model was already on disk and intact.
    pub already_cached: bool,
}

impl ModelCache {
    /// Create a cache rooted at `models_root` (typically `.cairn/models/`).
    #[must_use]
    pub fn new(models_root: &Path) -> Self {
        Self { root: models_root.to_owned() }
    }

    /// Path to the directory for a given model.
    #[must_use]
    pub fn model_dir(&self, kind: EmbeddingModelKind) -> PathBuf {
        self.root.join(kind.as_str())
    }

    /// `true` iff the `.integrity` marker exists.
    #[must_use]
    pub fn is_present(&self, kind: EmbeddingModelKind) -> bool {
        self.model_dir(kind).join(".integrity").exists()
    }

    /// Load the model into memory. Returns `Err(ModelNotFetched)` if not on disk.
    /// Wrap in `tokio::task::spawn_blocking` when calling from async code.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::ModelNotFetched`] if the model has not been
    /// downloaded yet, or any I/O / inference error encountered during load.
    pub fn ensure(&self, kind: EmbeddingModelKind) -> Result<Arc<dyn EmbeddingModel>, EmbeddingError> {
        if !self.is_present(kind) {
            return Err(EmbeddingError::ModelNotFetched { kind });
        }
        // Full impl in Task 4.
        Err(EmbeddingError::ModelNotFetched { kind })
    }

    /// Download model files from `HuggingFace` Hub (huggingface.co). Idempotent.
    /// Wrap in `tokio::task::spawn_blocking` when calling from async code.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Network`] on Hub download failure, or
    /// [`EmbeddingError::Io`] on filesystem errors.
    pub fn fetch(&self, kind: EmbeddingModelKind) -> Result<FetchReport, EmbeddingError> {
        // Full impl in Task 4.
        Err(EmbeddingError::ModelNotFetched { kind })
    }
}
