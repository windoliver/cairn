//! `ModelCache` — on-disk layout, integrity verification, hf-hub download.

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
        Self {
            root: models_root.to_owned(),
        }
    }

    /// Path to the directory for a given model.
    #[must_use]
    pub fn model_dir(&self, kind: EmbeddingModelKind) -> PathBuf {
        self.root.join(kind.as_str())
    }

    /// `true` iff the `.integrity` marker exists (fetch was completed).
    #[must_use]
    pub fn is_present(&self, kind: EmbeddingModelKind) -> bool {
        self.model_dir(kind).join(".integrity").exists()
    }

    /// Load the model into memory.
    ///
    /// Returns `Err(ModelNotFetched)` if the model files are not on disk.
    /// Call `fetch` first (via `cairn bootstrap` or `cairn admin model fetch`).
    ///
    /// CPU-bound — wrap in `tokio::task::spawn_blocking` when calling from async code.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::ModelNotFetched`] if the model has not been
    /// downloaded yet, or any I/O / inference error encountered during load.
    pub fn ensure(
        &self,
        kind: EmbeddingModelKind,
    ) -> Result<Arc<dyn EmbeddingModel>, EmbeddingError> {
        if !self.is_present(kind) {
            return Err(EmbeddingError::ModelNotFetched { kind });
        }
        let dir = self.model_dir(kind);
        // EmbeddingModelKind is #[non_exhaustive]; the wildcard arm is unreachable
        // today but required to compile against future variants added upstream.
        match kind {
            EmbeddingModelKind::BgeSmallEnV1_5 => {
                let m = crate::bge::BgeSmall::load(&dir)?;
                Ok(Arc::new(m))
            }
            EmbeddingModelKind::AllMiniLmL6V2 => {
                let m = crate::minilm::MiniLm::load(&dir)?;
                Ok(Arc::new(m))
            }
            // Safety valve for future variants; fail at runtime rather than silently
            // returning a wrong model.
            _ => Err(EmbeddingError::ModelNotFetched { kind }),
        }
    }

    /// Download model files from `HuggingFace` Hub.
    ///
    /// Idempotent: if `.integrity` already exists, returns `already_cached: true` immediately.
    /// Network + IO — wrap in `tokio::task::spawn_blocking` when calling from async code.
    ///
    /// # Errors
    ///
    /// Returns [`EmbeddingError::Network`] on Hub download failure, or
    /// [`EmbeddingError::Io`] on filesystem errors.
    pub fn fetch(&self, kind: EmbeddingModelKind) -> Result<FetchReport, EmbeddingError> {
        if self.is_present(kind) {
            let integrity = std::fs::read_to_string(self.model_dir(kind).join(".integrity"))
                .unwrap_or_default();
            return Ok(FetchReport {
                kind,
                bytes_downloaded: 0,
                integrity,
                already_cached: true,
            });
        }

        let dir = self.model_dir(kind);
        let tmp = dir.with_extension("tmp");
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(&tmp)?;

        let api =
            hf_hub::api::sync::Api::new().map_err(|e| EmbeddingError::Network(e.to_string()))?;
        let repo = api.model(kind.hf_repo().to_owned());

        let mut bytes_downloaded: u64 = 0;
        for filename in ["config.json", "tokenizer.json", "model.safetensors"] {
            let src = repo
                .get(filename)
                .map_err(|e| EmbeddingError::Network(e.to_string()))?;
            let dst = tmp.join(filename);
            let len = std::fs::copy(&src, &dst)?;
            bytes_downloaded += len;
        }

        // Compute blake3 digest over all three files in deterministic order.
        let mut hasher = blake3::Hasher::new();
        for filename in ["config.json", "tokenizer.json", "model.safetensors"] {
            let data = std::fs::read(tmp.join(filename))?;
            hasher.update(&data);
        }
        let digest = hasher.finalize().to_hex().to_string();
        std::fs::write(tmp.join(".integrity"), &digest)?;

        // Atomic rename: tmp → canonical dir.
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::rename(&tmp, &dir)?;

        Ok(FetchReport {
            kind,
            bytes_downloaded,
            integrity: digest,
            already_cached: false,
        })
    }
}
