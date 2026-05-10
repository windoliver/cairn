//! Cache-aware hot-prefix assembly. See issue #83 / spec §5.3.

// Imports are stubs for Task 10's implementation; suppress the
// unused-import lints so clippy -D warnings stays clean.
#[allow(unused_imports, reason = "all used in Task 10 implementation")]
use std::time::Instant;

use crate::config::HotMemoryConfig;
#[allow(unused_imports, reason = "CachedPrefix used in Task 10 implementation")]
use crate::contract::hot_prefix_cache::{CachedPrefix, HotPrefixCache};
use crate::contract::metrics::MetricsSink;
use crate::domain::Identity;
#[allow(unused_imports, reason = "used in Task 10 implementation")]
use crate::domain::hot_prefix::SourceWatermarks;
#[allow(unused_imports, reason = "used in Task 10 implementation")]
use crate::domain::metrics::MetricEvent;
use crate::generated::verbs::assemble_hot::AssembleHotData;
use crate::verbs::assemble_hot::assembler::AssembleHotError;

/// Errors `cached_assemble` may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CachedAssembleError {
    /// Underlying assembler failure (loader returned error, segment
    /// build failed, budget exceeded).
    #[error("assemble: {0}")]
    Assemble(#[from] AssembleHotError),
}

/// Stable hash over the canonical JSON encoding of the recipe.
///
/// Two identical recipes hash to the same value across platforms and
/// across cairn versions, as long as the recipe shape doesn't change.
#[must_use]
pub fn recipe_hash_canonical(recipe: &[crate::config::HotMemoryRecipeStep]) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(recipe).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    hex_lower(&h.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Cache-aware `assemble_hot`.
///
/// On HIT: returns the cached prefix; emits one `HotPrefixAssembled`
/// with `cache_hit: true`.
/// On MISS: calls `load_bodies`, reassembles, stores the result via
/// `cache.put`, emits `cache_hit: false`.
/// Cache failures (Backend / Corrupt) bypass the cache; the verb
/// still returns the assembled prefix.
pub async fn cached_assemble(
    config: &HotMemoryConfig,
    agent: &Identity,
    vault_id: &str,
    cache: &dyn HotPrefixCache,
    metrics: &dyn MetricsSink,
    load_bodies: impl FnOnce() -> Result<Vec<String>, AssembleHotError>,
) -> Result<AssembleHotData, CachedAssembleError> {
    let _ = (config, agent, vault_id, cache, metrics, load_bodies);
    todo!("implementation in Task 10")
}

#[cfg(test)]
#[allow(clippy::todo, reason = "scaffolding for Task 10")]
mod tests {
    #[allow(unused_imports, reason = "will be needed by Task 10 implementations")]
    use super::*;

    #[tokio::test]
    async fn cache_hit_returns_cached_prefix_and_emits_hit_metric() {
        todo!()
    }

    #[tokio::test]
    async fn cache_miss_assembles_puts_and_emits_miss_metric() {
        todo!()
    }

    #[tokio::test]
    async fn cache_miss_when_watermarks_diverge() {
        todo!()
    }

    #[tokio::test]
    async fn backend_error_on_get_falls_back_to_assembly() {
        todo!()
    }

    #[tokio::test]
    async fn put_failure_does_not_break_the_verb() {
        todo!()
    }

    #[tokio::test]
    async fn metrics_sink_failure_is_swallowed() {
        todo!()
    }
}
