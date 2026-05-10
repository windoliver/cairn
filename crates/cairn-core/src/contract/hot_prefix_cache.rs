//! `HotPrefixCache` contract — see issue #83 / brief §7.
//!
//! Implementations:
//! - `cairn-store-sqlite::SqliteHotPrefixCache` (production)
//! - test mock in `cairn-core/src/verbs/assemble_hot/cached.rs`

use crate::domain::Identity;
use crate::domain::hot_prefix::{SourceClass, SourceWatermarks};
use crate::generated::verbs::assemble_hot::HotSegment;

/// A previously-assembled hot prefix entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedPrefix {
    /// Raw assembled bytes.
    pub prefix: Vec<u8>,
    /// Wire-shape segment metadata.
    pub segments: Vec<HotSegment>,
    /// `prefix.len()` at assembly time.
    pub bytes: u64,
    /// Watermark snapshot — must match the live watermarks for this
    /// entry to count as a hit.
    pub watermarks: SourceWatermarks,
    /// Wall-clock millis when the entry was put.
    pub assembled_at_ms: i64,
    /// Latency observed during the original cache-miss assembly.
    pub assembly_latency_ms: u64,
}

/// Errors a cache backend may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CacheError {
    /// Backend-level failure (rusqlite, fs, etc.).
    #[error("hot-prefix cache backend: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A row was unparseable. Caller should delete + reassemble.
    #[error("cache row corrupt: {reason}")]
    Corrupt {
        /// Why the row is unparseable.
        reason: String,
    },
    /// `hot_source_watermarks` returned fewer than the expected six rows.
    #[error("watermark schema mismatch: missing class {class:?}")]
    WatermarkSchemaMismatch {
        /// Which class was missing.
        class: SourceClass,
    },
}

/// Per-(agent, recipe-hash) hot-prefix cache + per-class watermark counter.
#[async_trait::async_trait]
pub trait HotPrefixCache: Send + Sync {
    /// Read every source-class counter. Six rows; missing rows
    /// surface as `WatermarkSchemaMismatch`.
    async fn current_watermarks(&self) -> Result<SourceWatermarks, CacheError>;

    /// Look up a cached entry. `Ok(None)` for cache miss.
    async fn get(
        &self,
        agent: &Identity,
        recipe_hash: &str,
    ) -> Result<Option<CachedPrefix>, CacheError>;

    /// Store an entry. `INSERT OR REPLACE` semantics.
    async fn put(
        &self,
        agent: &Identity,
        recipe_hash: &str,
        entry: &CachedPrefix,
    ) -> Result<(), CacheError>;

    /// Bump every listed class. Called from inside an enclosing write
    /// transaction by the store; out-of-tx callers exist only for
    /// non-record-bound triggers (config write, summary write).
    async fn bump(&self, classes: &[SourceClass]) -> Result<SourceWatermarks, CacheError>;
}
