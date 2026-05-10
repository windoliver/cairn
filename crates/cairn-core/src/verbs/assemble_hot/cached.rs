//! Cache-aware hot-prefix assembly. See issue #83 / spec §5.3.

use std::time::Instant;

use crate::config::HotMemoryConfig;
use crate::contract::hot_prefix_cache::{CachedPrefix, HotPrefixCache};
use crate::contract::metrics::MetricsSink;
use crate::domain::Identity;
use crate::domain::hot_prefix::SourceWatermarks;
use crate::domain::metrics::MetricEvent;
use crate::generated::verbs::assemble_hot::AssembleHotData;
use crate::verbs::assemble_hot::assembler::{AssembleHotError, assemble_hot_from_bodies};

/// Errors `cached_assemble` may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CachedAssembleError {
    /// Underlying assembler failure.
    #[error("assemble: {0}")]
    Assemble(#[from] AssembleHotError),
}

/// Stable hash over the canonical JSON encoding of the recipe.
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

/// Cache-aware `assemble_hot`. See module docs.
pub async fn cached_assemble(
    config: &HotMemoryConfig,
    agent: &Identity,
    vault_id: &str,
    cache: &dyn HotPrefixCache,
    metrics: &dyn MetricsSink,
    load_bodies: impl FnOnce() -> Result<Vec<String>, AssembleHotError>,
) -> Result<AssembleHotData, CachedAssembleError> {
    let recipe_hash = recipe_hash_canonical(&config.recipe);
    let started = Instant::now();

    // Read live watermarks; failure → bypass cache, still assemble + emit.
    let wm_now: SourceWatermarks = match cache.current_watermarks().await {
        Ok(wm) => wm,
        Err(e) => {
            tracing::warn!(error = %e, "hot-prefix cache: watermark read failed; bypassing cache");
            return assemble_and_emit(
                config,
                agent,
                vault_id,
                &recipe_hash,
                metrics,
                started,
                false,
                SourceWatermarks::default(),
                load_bodies,
            )
            .await;
        }
    };

    // Cache lookup; non-corrupt errors bypass the cache for this call.
    let cache_get = cache.get(agent, &recipe_hash).await;
    if let Ok(Some(entry)) = &cache_get {
        if entry.watermarks.matches(&wm_now) {
            let latency_ms = elapsed_ms(started);
            emit_event(
                metrics,
                vault_id,
                agent,
                &recipe_hash,
                entry.bytes,
                u64::from(config.max_bytes),
                latency_ms,
                true,
                wm_now,
            )
            .await;
            return Ok(into_assemble_hot_data(entry.clone()));
        }
    } else if let Err(e) = &cache_get {
        tracing::warn!(error = %e, "hot-prefix cache: get failed; bypassing cache for this call");
    }

    // Miss path: load + assemble + put + emit.
    let bodies = load_bodies()?;
    let data = assemble_hot_from_bodies(config, bodies, None)?;
    let latency_ms = elapsed_ms(started);

    let entry = CachedPrefix {
        prefix: data.prefix.as_bytes().to_vec(),
        segments: data.segments.clone().unwrap_or_default(),
        bytes: data.bytes,
        watermarks: wm_now,
        assembled_at_ms: now_ms(),
        assembly_latency_ms: latency_ms,
    };
    if let Err(e) = cache.put(agent, &recipe_hash, &entry).await {
        tracing::warn!(error = %e, "hot-prefix cache: put failed");
    }

    emit_event(
        metrics,
        vault_id,
        agent,
        &recipe_hash,
        data.bytes,
        u64::from(config.max_bytes),
        latency_ms,
        false,
        wm_now,
    )
    .await;

    Ok(data)
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal helper; all args required"
)]
async fn assemble_and_emit(
    config: &HotMemoryConfig,
    agent: &Identity,
    vault_id: &str,
    recipe_hash: &str,
    metrics: &dyn MetricsSink,
    started: Instant,
    cache_hit: bool,
    wm: SourceWatermarks,
    load_bodies: impl FnOnce() -> Result<Vec<String>, AssembleHotError>,
) -> Result<AssembleHotData, CachedAssembleError> {
    let bodies = load_bodies()?;
    let data = assemble_hot_from_bodies(config, bodies, None)?;
    let latency_ms = elapsed_ms(started);
    emit_event(
        metrics,
        vault_id,
        agent,
        recipe_hash,
        data.bytes,
        u64::from(config.max_bytes),
        latency_ms,
        cache_hit,
        wm,
    )
    .await;
    Ok(data)
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal helper; all args required"
)]
async fn emit_event(
    metrics: &dyn MetricsSink,
    vault_id: &str,
    agent: &Identity,
    recipe_hash: &str,
    bytes: u64,
    budget: u64,
    latency_ms: u64,
    cache_hit: bool,
    wm: SourceWatermarks,
) {
    let event = MetricEvent::HotPrefixAssembled {
        ts_ms: now_ms(),
        vault_id: vault_id.to_owned(),
        agent_id: agent.to_string(),
        recipe_hash: recipe_hash.to_owned(),
        latency_ms,
        bytes,
        budget_bytes: budget,
        budget_used_ratio: budget_ratio(bytes, budget),
        cache_hit,
        watermarks: wm,
    };
    if let Err(e) = metrics.emit(event).await {
        tracing::warn!(error = %e, "metrics sink emit failed");
    }
}

fn into_assemble_hot_data(entry: CachedPrefix) -> AssembleHotData {
    AssembleHotData {
        bytes: entry.bytes,
        prefix: String::from_utf8(entry.prefix).unwrap_or_default(),
        segments: Some(entry.segments),
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn now_ms() -> i64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[allow(clippy::cast_precision_loss, reason = "ratio: u64 → f64 acceptable")]
fn budget_ratio(used: u64, budget: u64) -> f64 {
    if budget == 0 {
        0.0
    } else {
        used as f64 / budget as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotMemoryConfig;
    use crate::contract::hot_prefix_cache::CacheError;
    use crate::contract::metrics::CapturingMetricsSink;
    use crate::domain::hot_prefix::SourceClass;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn agent() -> Identity {
        Identity::parse("agt:cairn-cli:default:writer:v1").expect("identity parses")
    }

    fn config() -> HotMemoryConfig {
        HotMemoryConfig::default()
    }

    /// Mock cache for the unit tests. All async work serialised through
    /// tokio mutexes for deterministic interleaving.
    #[derive(Default)]
    struct MockCache {
        entry: tokio::sync::Mutex<Option<CachedPrefix>>,
        watermarks: tokio::sync::Mutex<SourceWatermarks>,
        get_fail: AtomicBool,
        put_fail: AtomicBool,
        wm_fail: AtomicBool,
    }

    #[async_trait::async_trait]
    impl HotPrefixCache for MockCache {
        async fn current_watermarks(&self) -> Result<SourceWatermarks, CacheError> {
            if self.wm_fail.load(Ordering::Relaxed) {
                return Err(CacheError::Backend(Box::<
                    dyn std::error::Error + Send + Sync,
                >::from("wm fail")));
            }
            Ok(*self.watermarks.lock().await)
        }
        async fn get(&self, _a: &Identity, _h: &str) -> Result<Option<CachedPrefix>, CacheError> {
            if self.get_fail.load(Ordering::Relaxed) {
                return Err(CacheError::Backend(Box::<
                    dyn std::error::Error + Send + Sync,
                >::from("get fail")));
            }
            Ok(self.entry.lock().await.clone())
        }
        async fn put(&self, _a: &Identity, _h: &str, e: &CachedPrefix) -> Result<(), CacheError> {
            if self.put_fail.load(Ordering::Relaxed) {
                return Err(CacheError::Backend(Box::<
                    dyn std::error::Error + Send + Sync,
                >::from("put fail")));
            }
            *self.entry.lock().await = Some(e.clone());
            Ok(())
        }
        async fn bump(&self, classes: &[SourceClass]) -> Result<SourceWatermarks, CacheError> {
            let mut wm = self.watermarks.lock().await;
            for c in classes {
                wm.bump(*c);
            }
            Ok(*wm)
        }
    }

    #[tokio::test]
    async fn cache_hit_returns_cached_prefix_and_emits_hit_metric() {
        let cache = MockCache::default();
        let metrics = CapturingMetricsSink::new();
        let cfg = config();
        cache.entry.lock().await.replace(CachedPrefix {
            prefix: b"cached".to_vec(),
            segments: vec![],
            bytes: 6,
            watermarks: SourceWatermarks::default(),
            assembled_at_ms: 0,
            assembly_latency_ms: 1,
        });
        let data = cached_assemble(&cfg, &agent(), "v1", &cache, &metrics, || {
            panic!("loader must not run on cache hit")
        })
        .await
        .expect("assemble");
        assert_eq!(data.prefix, "cached");
        let evs = metrics.snapshot().await;
        assert_eq!(evs.len(), 1);
        let MetricEvent::HotPrefixAssembled { cache_hit, .. } = &evs[0];
        assert!(*cache_hit);
    }

    #[tokio::test]
    async fn cache_miss_assembles_puts_and_emits_miss_metric() {
        let cache = MockCache::default();
        let metrics = CapturingMetricsSink::new();
        let cfg = config();
        let recipe_len = cfg.recipe.len();
        let data = cached_assemble(&cfg, &agent(), "v1", &cache, &metrics, move || {
            Ok(vec![String::new(); recipe_len])
        })
        .await
        .expect("assemble");
        assert_eq!(data.bytes, data.prefix.len() as u64);
        let evs = metrics.snapshot().await;
        assert_eq!(evs.len(), 1);
        let MetricEvent::HotPrefixAssembled { cache_hit, .. } = &evs[0];
        assert!(!*cache_hit);
        assert!(cache.entry.lock().await.is_some());
    }

    #[tokio::test]
    async fn cache_miss_when_watermarks_diverge() {
        let cache = MockCache::default();
        let metrics = CapturingMetricsSink::new();
        let mut stale = SourceWatermarks::default();
        stale.bump(SourceClass::ProfileEvidence);
        cache.entry.lock().await.replace(CachedPrefix {
            prefix: b"stale".to_vec(),
            segments: vec![],
            bytes: 5,
            watermarks: stale,
            assembled_at_ms: 0,
            assembly_latency_ms: 0,
        });
        let cfg = config();
        let recipe_len = cfg.recipe.len();
        let data = cached_assemble(&cfg, &agent(), "v1", &cache, &metrics, move || {
            Ok(vec![String::new(); recipe_len])
        })
        .await
        .expect("assemble");
        assert_ne!(data.prefix, "stale");
        let evs = metrics.snapshot().await;
        let MetricEvent::HotPrefixAssembled { cache_hit, .. } = &evs[0];
        assert!(!*cache_hit);
    }

    #[tokio::test]
    async fn backend_error_on_get_falls_back_to_assembly() {
        let cache = MockCache::default();
        cache.get_fail.store(true, Ordering::Relaxed);
        let metrics = CapturingMetricsSink::new();
        let cfg = config();
        let recipe_len = cfg.recipe.len();
        let data = cached_assemble(&cfg, &agent(), "v1", &cache, &metrics, move || {
            Ok(vec![String::new(); recipe_len])
        })
        .await
        .expect("assemble");
        assert_eq!(data.bytes, data.prefix.len() as u64);
    }

    #[tokio::test]
    async fn put_failure_does_not_break_the_verb() {
        let cache = MockCache::default();
        cache.put_fail.store(true, Ordering::Relaxed);
        let metrics = CapturingMetricsSink::new();
        let cfg = config();
        let recipe_len = cfg.recipe.len();
        let _ = cached_assemble(&cfg, &agent(), "v1", &cache, &metrics, move || {
            Ok(vec![String::new(); recipe_len])
        })
        .await
        .expect("verb survives put failure");
    }

    #[tokio::test]
    async fn metrics_sink_failure_is_swallowed() {
        struct BrokenSink;
        #[async_trait::async_trait]
        impl MetricsSink for BrokenSink {
            async fn emit(
                &self,
                _: MetricEvent,
            ) -> Result<(), crate::contract::metrics::MetricsError> {
                Err(crate::contract::metrics::MetricsError::Io(
                    std::io::Error::other("disk"),
                ))
            }
        }
        let cache = MockCache::default();
        let cfg = config();
        let recipe_len = cfg.recipe.len();
        let _ = cached_assemble(&cfg, &agent(), "v1", &cache, &BrokenSink, move || {
            Ok(vec![String::new(); recipe_len])
        })
        .await
        .expect("verb survives sink failure");
    }
}
