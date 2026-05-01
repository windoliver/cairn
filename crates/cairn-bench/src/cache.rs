//! Disk-backed embedding cache. Maps `(model_label, slug, content_hash) → vector`.
//!
//! Format: serde-JSON-encoded [`BTreeMap`]. One cache file per run.
//! Re-runs with the same fixture skip the network/inference cost.
//!
//! Task 11 wires `load`/`save` bookends into the bench binary; threading
//! the cache through the actual `EmbeddingModel::embed_document` calls is
//! deferred to a follow-up so this module ships with a stable surface
//! today.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Composite key identifying a single cached vector.
///
/// `model_label` matches `EmbeddingModelKind::as_str` (e.g.
/// `"bge-small-en-v1.5"`). `slug` is the corpus page slug — stable across
/// runs of the same fixture. `content_hash` is a SHA-256 hex digest of the
/// embedded text and guards against silent fixture mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    /// Active embedding-model label (kebab-case).
    pub model_label: String,
    /// Corpus page slug.
    pub slug: String,
    /// SHA-256 hex digest of the embedded text.
    pub content_hash: String,
}

/// SHA-256 hex digest of `text`. Stable across processes and platforms.
#[must_use]
pub fn content_hash(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// Disk-backed map from [`CacheKey`] to embedding vector.
///
/// Loaded once per bench run, mutated as new vectors arrive, then flushed
/// to disk before the binary exits. The on-disk format is JSON; binary
/// formats were considered but JSON keeps the file inspectable from `jq`
/// without an extra tool.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(from = "EmbeddingCacheWire", into = "EmbeddingCacheWire")]
pub struct EmbeddingCache {
    entries: BTreeMap<CacheKey, Vec<f32>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct EmbeddingCacheWire {
    entries: Vec<(CacheKey, Vec<f32>)>,
}

impl From<EmbeddingCacheWire> for EmbeddingCache {
    fn from(w: EmbeddingCacheWire) -> Self {
        Self {
            entries: w.entries.into_iter().collect(),
        }
    }
}

impl From<EmbeddingCache> for EmbeddingCacheWire {
    fn from(c: EmbeddingCache) -> Self {
        Self {
            entries: c.entries.into_iter().collect(),
        }
    }
}

impl EmbeddingCache {
    /// Load a cache from `path`, or return an empty cache if `path` does
    /// not yet exist.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` exists but cannot be read or fails to
    /// parse as JSON.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read(path).with_context(|| format!("read cache at {}", path.display()))?;
        let cache = serde_json::from_slice(&raw).context("parse cache")?;
        Ok(cache)
    }

    /// Persist this cache to `path`, creating the parent directory if
    /// necessary.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory cannot be created or the
    /// file cannot be written.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent()
            && !p.as_os_str().is_empty()
        {
            std::fs::create_dir_all(p)
                .with_context(|| format!("create cache parent {}", p.display()))?;
        }
        let raw = serde_json::to_vec(self).context("serialize cache")?;
        std::fs::write(path, raw).with_context(|| format!("write cache to {}", path.display()))?;
        Ok(())
    }

    /// Look up a cached vector by key.
    #[must_use]
    pub fn get(&self, k: &CacheKey) -> Option<&Vec<f32>> {
        self.entries.get(k)
    }

    /// Insert or replace a cache entry.
    pub fn put(&mut self, k: CacheKey, v: Vec<f32>) {
        self.entries.insert(k, v);
    }

    /// Number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the cache holds zero entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheKey, EmbeddingCache, content_hash};

    #[test]
    fn content_hash_is_stable() {
        let a = content_hash("hello world");
        let b = content_hash("hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // SHA-256 hex
    }

    #[test]
    fn empty_cache_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.json");
        let cache = EmbeddingCache::default();
        cache.save(&path).expect("save");
        let loaded = EmbeddingCache::load(&path).expect("load");
        assert!(loaded.is_empty());
    }

    #[test]
    fn populated_cache_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.json");
        let key = CacheKey {
            model_label: "bge-small-en-v1.5".to_owned(),
            slug: "alice".to_owned(),
            content_hash: content_hash("body"),
        };
        let mut cache = EmbeddingCache::default();
        cache.put(key.clone(), vec![0.1, 0.2, 0.3]);
        cache.save(&path).expect("save");
        let loaded = EmbeddingCache::load(&path).expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.get(&key).map(Vec::as_slice),
            Some(&[0.1, 0.2, 0.3][..])
        );
    }

    #[test]
    fn load_missing_path_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist.json");
        let loaded = EmbeddingCache::load(&path).expect("load");
        assert!(loaded.is_empty());
    }
}
