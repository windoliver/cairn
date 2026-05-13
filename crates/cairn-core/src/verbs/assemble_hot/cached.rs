//! Cache-aware hot-prefix assembly. See issue #83 / spec §5.3.

use std::path::Path;
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::config::HotMemoryConfig;
use crate::contract::hot_prefix_cache::{CachedPrefix, HotPrefixCache};
use crate::contract::metrics::MetricsSink;
use crate::domain::Identity;
use crate::domain::hot_prefix::SourceWatermarks;
use crate::domain::metrics::MetricEvent;
use crate::generated::verbs::assemble_hot::AssembleHotData;
use crate::verbs::assemble_hot::assembler::{AssembleHotError, assemble_hot_from_bodies};

/// Relative paths whose contents go into the cache's `fs_fingerprint`.
/// Edits to these files invalidate cached prefixes even when they
/// bypass any Cairn write hook. See codex review round 1 finding 2 and
/// round 2 finding 4.
const FS_FINGERPRINT_PATHS: &[&str] = &["purpose.md", "index.md", ".cairn/config.yaml"];

/// Per-file size cap when hashing fingerprint contents. Matches the
/// assembler's absolute hard cap (`segments::MAX_BYTES`, 4 MiB) so any
/// byte that can affect assembly contributes to the fingerprint.
///
/// Codex review round 3 finding 2: prior cap was 1 MiB, but the
/// assembler supports prefixes up to 4 MiB. Edits past 1 MiB in
/// `purpose.md` / `index.md` / `config.yaml` could change the
/// assembled prefix while leaving the fingerprint unchanged.
const FS_FINGERPRINT_MAX_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB

/// Compute a content-hash fingerprint for filesystem-backed hot-memory
/// sources. Returns an empty string when `vault_root` is `None` — the
/// fingerprint check is then a no-op (cache hit relies solely on
/// watermarks).
///
/// Codex review round 2 finding 4: prior versions used `(mtime_ns, size)`
/// which could miss same-size edits on filesystems with coarse mtime
/// granularity. Reading file contents and SHA-256ing them is the only
/// reliable invalidation signal for files that bypass any Cairn write
/// hook.
#[must_use]
pub fn compute_fs_fingerprint(vault_root: Option<&Path>) -> String {
    let Some(root) = vault_root else {
        return String::new();
    };
    let mut hasher = Sha256::new();
    for rel in FS_FINGERPRINT_PATHS {
        hasher.update(rel.as_bytes());
        hasher.update(b"\x00");
        let path = root.join(rel);
        // Include the file's full size (from metadata, unbounded) AND
        // the capped bytes. Codex round 3 finding 2: two files sharing
        // the first cap bytes but with different total sizes must
        // produce different fingerprints — total-size separation makes
        // beyond-cap edits visible as a fingerprint change.
        let full_size: Option<u64> = std::fs::metadata(&path).ok().map(|m| m.len());
        match read_capped(&path, FS_FINGERPRINT_MAX_BYTES) {
            Ok(Some(bytes)) => {
                hasher.update(b"\x01");
                hasher.update(full_size.unwrap_or(0).to_le_bytes());
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
            Ok(None) => {
                hasher.update(b"\x00"); // absent marker
            }
            Err(_) => {
                hasher.update(b"\xff"); // read error — distinct from absent
            }
        }
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Read `path` up to `cap` bytes. Returns `Ok(None)` when the file is
/// absent (`NotFound`) so the caller can distinguish missing from
/// unreadable. Bounded read so a pathological file size cannot OOM the
/// fingerprint pass.
fn read_capped(path: &Path, cap: u64) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.by_ref().take(cap).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// Errors `cached_assemble` may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CachedAssembleError {
    /// Underlying assembler failure.
    #[error("assemble: {0}")]
    Assemble(#[from] AssembleHotError),
}

/// Stable hash over the canonical JSON encoding of the recipe.
///
/// Codex review round 2 finding 1: callers must NOT use this hash as a
/// cache key directly. Use [`cache_key_hash`] which mixes in
/// request-specific inputs (`session_id`, `effective_budget`) that shape
/// the assembled output but aren't in the recipe shape itself.
#[must_use]
pub fn recipe_hash_canonical(recipe: &[crate::config::HotMemoryRecipeStep]) -> String {
    let bytes = serde_json::to_vec(recipe).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    hex_lower(&h.finalize())
}

/// Compose a per-request cache key from `(recipe, session_id, effective_budget)`.
///
/// Codex review round 2 finding 1: `(agent_id, recipe_hash)` alone is
/// not sufficient — `RecentUserSignal` is session-filtered and the
/// effective budget truncates bodies before caching. Two requests with
/// different `session_id` or different `effective_budget` MUST land
/// on distinct cache rows so a session-A warmup cannot leak into a
/// session-B query and a small-budget call cannot poison a later
/// larger-budget call with a truncated prefix.
#[must_use]
pub fn cache_key_hash(
    recipe: &[crate::config::HotMemoryRecipeStep],
    session_id: Option<&str>,
    effective_budget: u64,
) -> String {
    let mut h = Sha256::new();
    let recipe_bytes = serde_json::to_vec(recipe).unwrap_or_default();
    h.update(&recipe_bytes);
    h.update(b"\x00");
    if let Some(s) = session_id {
        h.update(b"sid:");
        h.update(s.as_bytes());
    } else {
        h.update(b"sid:_");
    }
    h.update(b"\x00");
    h.update(b"budget:");
    h.update(effective_budget.to_le_bytes());
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

/// Snapshot of `(watermarks, fs_fingerprint)` captured BEFORE the
/// caller loaded its bodies. Used by [`cached_assemble`] to detect
/// mutations that committed between body-load and assembly so the
/// resulting prefix is not poisoned into the cache.
///
/// Codex review round 3 finding 1: without a pre-load snapshot, a
/// mutation that commits between the caller's body-load and
/// `cached_assemble`'s first watermark read would write a cache row
/// whose `watermarks` match the post-mutation state even though the
/// bodies are pre-mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct PreLoadSnapshot {
    /// Watermarks observed before the caller loaded bodies.
    pub watermarks: SourceWatermarks,
    /// FS fingerprint observed before the caller loaded bodies.
    pub fs_fingerprint: String,
}

/// Take a pre-load snapshot for [`cached_assemble`]. Callers must
/// invoke this BEFORE the body-loading step that the closure will
/// return, then pass the result to `cached_assemble`.
pub async fn pre_load_snapshot(
    cache: &dyn HotPrefixCache,
    vault_root: Option<&Path>,
) -> Result<PreLoadSnapshot, crate::contract::hot_prefix_cache::CacheError> {
    let watermarks = cache.current_watermarks().await?;
    let fs_fingerprint = compute_fs_fingerprint(vault_root);
    Ok(PreLoadSnapshot {
        watermarks,
        fs_fingerprint,
    })
}

/// Cache-aware `assemble_hot`. See module docs.
///
/// `budget_override` caps the assembled output size. When `None`, uses
/// `config.max_bytes`. On cache hit, if the cached entry exceeds the
/// effective budget, the entry is treated as a miss (option b).
///
/// `vault_root` enables the filesystem fingerprint check: edits to
/// `purpose.md`, `index.md`, or `.cairn/config.yaml` invalidate cached
/// prefixes regardless of watermarks. Pass `None` to skip the check
/// (tests, store-only recipes).
///
/// `session_id` is mixed into the cache key (codex review round 2
/// finding 1) so session-filtered recipe steps (`recent_user_signal`)
/// cannot leak across sessions.
///
/// `pre_load` is the `(watermarks, fs_fingerprint)` snapshot captured
/// by the caller BEFORE loading bodies (codex review round 3 finding
/// 1). On miss, `cached_assemble` re-reads both AFTER assembly; if
/// either diverges from `pre_load`, the put is skipped so a mutation
/// that commits between body-load and assembly cannot poison the
/// cache. Pass `None` to skip the pre-load comparison (the prior
/// behaviour: only post-snapshot drift is detected).
#[allow(
    clippy::too_many_arguments,
    reason = "verb entry point; all args required"
)]
pub async fn cached_assemble(
    config: &HotMemoryConfig,
    agent: &Identity,
    vault_id: &str,
    vault_root: Option<&Path>,
    session_id: Option<&str>,
    pre_load: Option<&PreLoadSnapshot>,
    cache: &dyn HotPrefixCache,
    metrics: &dyn MetricsSink,
    budget_override: Option<u64>,
    load_bodies: impl FnOnce() -> Result<Vec<String>, AssembleHotError>,
) -> Result<AssembleHotData, CachedAssembleError> {
    let started = Instant::now();
    let effective_budget = budget_override.unwrap_or_else(|| u64::from(config.max_bytes));
    let recipe_hash = cache_key_hash(&config.recipe, session_id, effective_budget);
    let fs_fingerprint_now = compute_fs_fingerprint(vault_root);

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
                budget_override,
                load_bodies,
            )
            .await;
        }
    };

    // Cache lookup; non-corrupt errors bypass the cache for this call.
    let cache_get = cache.get(agent, &recipe_hash).await;
    if let Ok(Some(entry)) = &cache_get {
        // Treat as miss if cached entry exceeds the effective budget (option b)
        // OR if the filesystem fingerprint has changed since the entry was put.
        let fp_matches = entry.fs_fingerprint == fs_fingerprint_now;
        if entry.watermarks.matches(&wm_now) && entry.bytes <= effective_budget && fp_matches {
            let latency_ms = elapsed_ms(started);
            emit_event(
                metrics,
                vault_id,
                agent,
                &recipe_hash,
                entry.bytes,
                effective_budget,
                latency_ms,
                true,
                wm_now,
            )
            .await;
            return Ok(into_assemble_hot_data(entry.clone(), config));
        }
    } else if let Err(e) = &cache_get {
        tracing::warn!(error = %e, "hot-prefix cache: get failed; bypassing cache for this call");
    }

    // Miss path: load + assemble + put + emit.
    let bodies = load_bodies()?;
    let data = assemble_hot_from_bodies(config, bodies, budget_override)?;
    let latency_ms = elapsed_ms(started);

    // Codex review round 2 finding 3 + round 3 finding 1: bodies were
    // loaded by the caller BEFORE this function's watermark snapshot.
    // A record mutation committing in that window would leave us with
    // a prefix assembled from stale bodies but tagged with fresh
    // watermarks — a poisoned cache row.
    //
    // Two defenses:
    // 1. Re-read watermarks + fingerprint AFTER assembly and compare
    //    to the snapshot we took ENTERING cached_assemble. Catches a
    //    mutation that committed during assembly.
    // 2. If the caller supplied a `pre_load` snapshot (captured BEFORE
    //    body loading), require post-assembly state to match THAT too.
    //    Catches a mutation that committed between caller's body-load
    //    and our internal snapshot.
    let wm_after = cache.current_watermarks().await.ok();
    let fp_after = compute_fs_fingerprint(vault_root);
    let post_matches_entry =
        wm_after.is_some_and(|wm| wm == wm_now) && fp_after == fs_fingerprint_now;
    let post_matches_preload = pre_load.is_none_or(|pre| {
        wm_after.is_some_and(|wm| wm == pre.watermarks) && fp_after == pre.fs_fingerprint
    });
    let snapshot_stable = post_matches_entry && post_matches_preload;

    let entry = CachedPrefix {
        prefix: data.prefix.as_bytes().to_vec(),
        segments: data.segments.clone().unwrap_or_default(),
        bytes: data.bytes,
        watermarks: wm_now,
        assembled_at_ms: now_ms(),
        assembly_latency_ms: latency_ms,
        fs_fingerprint: fs_fingerprint_now,
    };
    if snapshot_stable {
        if let Err(e) = cache.put(agent, &recipe_hash, &entry).await {
            tracing::warn!(error = %e, "hot-prefix cache: put failed");
        }
    } else {
        tracing::warn!(
            "hot-prefix cache: snapshot drifted during assembly; skipping put to avoid poisoning"
        );
    }

    emit_event(
        metrics,
        vault_id,
        agent,
        &recipe_hash,
        data.bytes,
        effective_budget,
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
    budget_override: Option<u64>,
    load_bodies: impl FnOnce() -> Result<Vec<String>, AssembleHotError>,
) -> Result<AssembleHotData, CachedAssembleError> {
    let effective_budget = budget_override.unwrap_or_else(|| u64::from(config.max_bytes));
    let bodies = load_bodies()?;
    let data = assemble_hot_from_bodies(config, bodies, budget_override)?;
    let latency_ms = elapsed_ms(started);
    emit_event(
        metrics,
        vault_id,
        agent,
        recipe_hash,
        data.bytes,
        effective_budget,
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

fn into_assemble_hot_data(entry: CachedPrefix, config: &HotMemoryConfig) -> AssembleHotData {
    AssembleHotData {
        bytes: entry.bytes,
        prefix: String::from_utf8(entry.prefix).unwrap_or_default(),
        segments: Some(entry.segments),
        recipe: if config.default_recipe.is_empty() {
            None
        } else {
            Some(config.default_recipe.clone())
        },
        // `debug` (the --explain trace) is never cached — it is
        // request-specific. The CLI layers it on cache hits via
        // build_explain_debug after cached_assemble returns. See
        // crates/cairn-cli/src/verbs/assemble_hot.rs.
        debug: None,
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
            fs_fingerprint: String::new(),
        });
        let data = cached_assemble(
            &cfg,
            &agent(),
            "v1",
            None,
            None,
            None,
            &cache,
            &metrics,
            None,
            || panic!("loader must not run on cache hit"),
        )
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
        let data = cached_assemble(
            &cfg,
            &agent(),
            "v1",
            None,
            None,
            None,
            &cache,
            &metrics,
            None,
            move || Ok(vec![String::new(); recipe_len]),
        )
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
            fs_fingerprint: String::new(),
        });
        let cfg = config();
        let recipe_len = cfg.recipe.len();
        let data = cached_assemble(
            &cfg,
            &agent(),
            "v1",
            None,
            None,
            None,
            &cache,
            &metrics,
            None,
            move || Ok(vec![String::new(); recipe_len]),
        )
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
        let data = cached_assemble(
            &cfg,
            &agent(),
            "v1",
            None,
            None,
            None,
            &cache,
            &metrics,
            None,
            move || Ok(vec![String::new(); recipe_len]),
        )
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
        let _ = cached_assemble(
            &cfg,
            &agent(),
            "v1",
            None,
            None,
            None,
            &cache,
            &metrics,
            None,
            move || Ok(vec![String::new(); recipe_len]),
        )
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
        let _ = cached_assemble(
            &cfg,
            &agent(),
            "v1",
            None,
            None,
            None,
            &cache,
            &BrokenSink,
            None,
            move || Ok(vec![String::new(); recipe_len]),
        )
        .await
        .expect("verb survives sink failure");
    }
}
