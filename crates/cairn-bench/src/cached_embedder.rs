//! Wraps an [`EmbeddingModel`] with a content-addressed [`EmbeddingCache`]
//! so re-runs of the bench don't re-pay inference / API cost, and a
//! pre-warm step can populate the cache via the embedder's batch path
//! (e.g. `OpenAiEmbedder::embed_documents`) to avoid per-call rate
//! limits during ingestion.
//!
//! Cache keying: `(model_label, "", sha256(text))`. The `slug` field of
//! [`CacheKey`] is intentionally empty in this path — we key by content
//! hash alone so re-orderings of the corpus don't invalidate the cache.

use std::sync::{Arc, Mutex};

use cairn_core::config::EmbeddingModelKind;
use cairn_embeddings_local::{EmbeddingError, EmbeddingModel};

use crate::cache::{CacheKey, EmbeddingCache, content_hash};

/// Wraps an `EmbeddingModel` with a disk-backed cache. Cache hits skip
/// the underlying embedder entirely; misses fall through and store the
/// result on return.
pub struct CachedEmbedder {
    inner: Arc<dyn EmbeddingModel>,
    cache: Arc<Mutex<EmbeddingCache>>,
    model_label: String,
}

impl CachedEmbedder {
    /// Build a wrapper around `inner`, keyed by `model_label`.
    #[must_use]
    pub fn new(
        inner: Arc<dyn EmbeddingModel>,
        cache: Arc<Mutex<EmbeddingCache>>,
        model_label: String,
    ) -> Self {
        Self {
            inner,
            cache,
            model_label,
        }
    }

    fn key_for(&self, text: &str) -> CacheKey {
        CacheKey {
            model_label: self.model_label.clone(),
            slug: String::new(),
            content_hash: content_hash(text),
            // Dimension-aware keying: same text + same model label
            // produced at two different `dimensions` (BGE/OpenAI MRL)
            // are different cache entries. Without this, a 384-dim
            // entry from a prior run would silently match a 1536-dim
            // run and the store would later reject the wrong-size
            // vector at upsert time.
            dim: u32::try_from(self.inner.dim()).unwrap_or(u32::MAX),
        }
    }

    /// Borrow the inner embedder. Used during pre-warm to invoke the
    /// embedder's batch path directly.
    #[must_use]
    pub fn inner(&self) -> &Arc<dyn EmbeddingModel> {
        &self.inner
    }

    /// Pre-fill the cache for `texts` whose hashes are not yet present.
    /// Calls `batch_fn` exactly once with the slice of uncached texts;
    /// `batch_fn` returns one vector per input in the same order.
    ///
    /// # Errors
    ///
    /// Propagates `batch_fn`'s error or any cache-mutation panic.
    pub fn prewarm_with<F>(&self, texts: &[&str], batch_fn: F) -> Result<usize, EmbeddingError>
    where
        F: FnOnce(&[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError>,
    {
        let want = self.inner.dim();
        let mut to_embed: Vec<&str> = Vec::new();
        let mut to_embed_keys: Vec<CacheKey> = Vec::new();
        {
            let cache = self
                .cache
                .lock()
                .map_err(|e| EmbeddingError::Network(format!("cache poisoned: {e}")))?;
            for t in texts {
                let key = self.key_for(t);
                // Treat a dim-mismatched cache entry as a miss.
                // Belt-and-suspenders against legacy `dim = 0` keys.
                let valid_hit = cache.get(&key).is_some_and(|v| v.len() == want);
                if !valid_hit {
                    to_embed.push(*t);
                    to_embed_keys.push(key);
                }
            }
        }
        if to_embed.is_empty() {
            return Ok(0);
        }
        let vectors = batch_fn(&to_embed)?;
        if vectors.len() != to_embed.len() {
            return Err(EmbeddingError::Network(format!(
                "prewarm: batch returned {} vectors, expected {}",
                vectors.len(),
                to_embed.len(),
            )));
        }
        // Validate every returned vector's length BEFORE mutating the
        // cache. A bad/regressed batch provider that emits the wrong
        // dim (e.g. an OpenAI request that silently dropped the
        // `dimensions` field) must not poison the cache — otherwise
        // `lookup_or_compute` would later reject those entries and
        // fall back to per-record sequential embeds, defeating the
        // rate-limit avoidance prewarm exists for.
        for (i, v) in vectors.iter().enumerate() {
            if v.len() != want {
                return Err(EmbeddingError::Network(format!(
                    "prewarm: vector {i} has dim {} but embedder expects {want}",
                    v.len(),
                )));
            }
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|e| EmbeddingError::Network(format!("cache poisoned: {e}")))?;
        for (key, v) in to_embed_keys.into_iter().zip(vectors) {
            cache.put(key, v);
        }
        Ok(to_embed.len())
    }

    fn lookup_or_compute(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let key = self.key_for(text);
        let want = self.inner.dim();
        // Hit path. The cache key already carries `dim`, so a wrong-dim
        // entry won't even match, but we still validate the vector
        // length defensively in case an older cache file is loaded that
        // pre-dates the dim-keyed format (legacy `dim = 0` entries).
        if let Ok(c) = self.cache.lock()
            && let Some(v) = c.get(&key)
            && v.len() == want
        {
            return Ok(v.clone());
        }
        let v = self.inner.embed_document(text)?;
        if let Ok(mut c) = self.cache.lock() {
            c.put(key, v.clone());
        }
        Ok(v)
    }
}

impl EmbeddingModel for CachedEmbedder {
    fn kind(&self) -> EmbeddingModelKind {
        self.inner.kind()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.lookup_or_compute(text)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        // Queries are typically short and high-volume distinct; honor the
        // cache anyway (text-equality match) but the hit rate is low.
        self.lookup_or_compute(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{CachedEmbedder, EmbeddingCache};
    use cairn_core::config::EmbeddingModelKind;
    use cairn_embeddings_local::{EmbeddingError, EmbeddingModel};
    use std::sync::{Arc, Mutex};

    /// Stand-in embedder that reports `dim()` but is never actually
    /// invoked by these tests — `prewarm_with`'s validation runs ahead
    /// of any per-record call.
    struct FixedDim(usize);
    impl EmbeddingModel for FixedDim {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::BgeSmallEnV1_5
        }
        fn dim(&self) -> usize {
            self.0
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; self.0])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; self.0])
        }
    }

    /// Regression for Codex round-4 finding #3: a regressed batch
    /// provider that returns vectors with the wrong dim must not
    /// poison the cache. Pre-fix, `prewarm_with` only checked the
    /// vector count and inserted every entry; later `lookup_or_compute`
    /// would reject the wrong-dim hits and fall back to per-record
    /// embeds, defeating the rate-limit avoidance prewarm exists for.
    #[test]
    fn prewarm_with_wrong_dim_batch_returns_error_and_leaves_cache_clean() {
        let inner: Arc<dyn EmbeddingModel> = Arc::new(FixedDim(384));
        let cache = Arc::new(Mutex::new(EmbeddingCache::default()));
        let embedder = CachedEmbedder::new(inner, Arc::clone(&cache), "bge-small-en-v1.5".into());

        let texts = vec!["alice", "bob"];
        let res = embedder.prewarm_with(&texts, |inputs| {
            // Simulate a misconfigured backend: returns 1536-dim
            // vectors when the embedder advertises 384-dim.
            Ok(inputs.iter().map(|_| vec![0.0_f32; 1536]).collect())
        });
        let err = res.expect_err("must reject wrong-dim batch");
        let msg = err.to_string();
        assert!(
            msg.contains("1536") && msg.contains("384"),
            "error must mention observed and expected dims, got: {msg}",
        );
        let c = cache.lock().expect("cache lock");
        assert!(
            c.is_empty(),
            "cache must remain untouched on wrong-dim batch; len={}",
            c.len(),
        );
    }
}
