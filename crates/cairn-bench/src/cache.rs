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
/// embedded text and guards against silent fixture mutation. `dim` is
/// the requested embedding dimensionality — included so a 384-dim cache
/// entry from a previous run cannot match a 1536-dim run with the same
/// model label (text-embedding-3-large supports both via the `OpenAI`
/// `dimensions` parameter).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    /// Active embedding-model label (kebab-case).
    pub model_label: String,
    /// Corpus page slug.
    pub slug: String,
    /// SHA-256 hex digest of the embedded text.
    pub content_hash: String,
    /// Requested vector dimensionality. Defaults to `0` for legacy
    /// pre-dim cache files; `CachedEmbedder::lookup_or_compute` rejects
    /// any hit whose stored vector length disagrees with the live
    /// embedder, so a `0` here matches nothing on the validation side.
    #[serde(default)]
    pub dim: u32,
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

/// RAII helper that releases an `fs4` exclusive flock when dropped.
/// Used by [`EmbeddingCache::save`] so the lock is released on every
/// return path (including `?` early-return after partial work).
struct LockGuard<'a>(&'a std::fs::File);
impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        use fs4::fs_std::FileExt;
        let _ = FileExt::unlock(self.0);
    }
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
    /// A malformed file (truncated JSON from a crash mid-`save`,
    /// schema drift, etc.) is logged and treated as a miss — we hand
    /// back an empty cache so the bench rebuilds rather than aborting
    /// the entire run on a corrupt sidecar. Read errors that aren't
    /// "file missing" still propagate (permission denied, I/O error).
    ///
    /// # Errors
    ///
    /// Returns an error if `path` exists but cannot be read.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read(path).with_context(|| format!("read cache at {}", path.display()))?;
        match serde_json::from_slice::<Self>(&raw) {
            Ok(cache) => Ok(cache),
            Err(e) => {
                // Quarantine: a corrupt cache should not crash the
                // whole run. The next `save` will overwrite atomically.
                eprintln!(
                    "warn: embedding cache at {} is unparseable ({e}); starting empty",
                    path.display(),
                );
                Ok(Self::default())
            }
        }
    }

    /// Persist this cache to `path`, creating the parent directory if
    /// necessary.
    ///
    /// Crash-safe AND merge-on-write: re-reads the on-disk cache (if
    /// any) under our own entries, then serializes to a sibling temp
    /// file, fsyncs, and atomically renames. Two parallel bench
    /// processes can therefore each compute disjoint embeddings
    /// without the later persist clobbering the earlier one's adds —
    /// the merge picks our entry on a key collision (we just produced
    /// it) and otherwise preserves whatever the other writer wrote.
    /// A crash, kill, or disk-full event between the temp-file write
    /// and the rename leaves the previous good cache untouched.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory cannot be created, the
    /// existing on-disk cache cannot be re-read, or any of the (open-
    /// temp / write / fsync / rename) steps fail.
    pub fn save(&self, path: &Path) -> Result<()> {
        use fs4::fs_std::FileExt;
        use std::io::Write;
        if let Some(p) = path.parent()
            && !p.as_os_str().is_empty()
        {
            std::fs::create_dir_all(p)
                .with_context(|| format!("create cache parent {}", p.display()))?;
        }
        // Acquire an OS-level exclusive lock on a sidecar file before
        // load-merge-persist. Two parallel bench processes therefore
        // serialize through this critical section and the second one
        // observes the first's already-persisted entries during its
        // reload. Without the lock, both processes can `load` the
        // pre-save snapshot, each merge their own additions on top,
        // and the later `rename` silently drops the earlier writer's
        // entries (TOCTOU on the destination).
        let lock_path = match path.extension() {
            Some(ext) => {
                let mut s = path.as_os_str().to_owned();
                s.push(".");
                s.push(ext);
                s.push(".lock");
                std::path::PathBuf::from(s)
            }
            None => path.with_extension("lock"),
        };
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open lock at {}", lock_path.display()))?;
        FileExt::lock_exclusive(&lock_file)
            .with_context(|| format!("acquire exclusive lock {}", lock_path.display()))?;
        // RAII guard releases the lock on the function's return path,
        // including on the early `?` returns below.
        let _guard = LockGuard(&lock_file);
        // Reload the on-disk snapshot and let our entries win on
        // collision. Holding the flock makes this load-merge-persist
        // sequence atomic with respect to other bench processes.
        let mut merged = Self::load(path).context("reload cache for merge")?;
        for (k, v) in &self.entries {
            merged.entries.insert(k.clone(), v.clone());
        }
        let raw = serde_json::to_vec(&merged).context("serialize cache")?;
        // The temp file MUST live in the same directory as `path` so
        // the final `rename` is a same-filesystem atomic operation.
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
        let mut tmp = if let Some(d) = dir {
            tempfile::NamedTempFile::new_in(d)
        } else {
            tempfile::NamedTempFile::new_in(".")
        }
        .with_context(|| format!("create temp cache near {}", path.display()))?;
        tmp.write_all(&raw)
            .with_context(|| format!("write temp cache near {}", path.display()))?;
        // Force the bytes to disk before rename so a power loss after
        // the rename cannot leave an empty/zero-length file behind.
        tmp.as_file_mut()
            .sync_all()
            .with_context(|| format!("fsync temp cache near {}", path.display()))?;
        tmp.persist(path)
            .with_context(|| format!("rename temp cache to {}", path.display()))?;
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
            dim: 384,
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

    /// Regression for Codex round-7 finding #2 (load half): a
    /// truncated/garbage cache file (simulating a `save` interrupted
    /// by SIGKILL or an unrelated tool corrupting the JSON) must not
    /// abort the bench run — the loader logs and returns an empty
    /// cache so the next pass rebuilds atomically.
    #[test]
    fn load_returns_empty_on_unparseable_cache_instead_of_aborting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.json");
        std::fs::write(&path, b"{\"entries\": [\"truncated").expect("write garbage");
        let loaded = EmbeddingCache::load(&path).expect("load must not error");
        assert!(loaded.is_empty(), "corrupt cache must yield empty");
    }

    /// Regression for Codex round-7 finding #2 (save half): the cache
    /// is written via a sibling temp file + atomic rename. After a
    /// successful `save`, no temp leftovers should remain in the
    /// directory — and the file at `path` must contain the new bytes.
    #[test]
    fn save_is_atomic_and_leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.json");

        let key = CacheKey {
            model_label: "bge-small-en-v1.5".to_owned(),
            slug: String::new(),
            content_hash: content_hash("hello"),
            dim: 384,
        };
        let mut cache = EmbeddingCache::default();
        cache.put(key.clone(), vec![1.0, 2.0, 3.0]);
        cache.save(&path).expect("save");

        // The destination must be readable...
        let loaded = EmbeddingCache::load(&path).expect("reload");
        assert_eq!(loaded.len(), 1);
        // ...and there must be no temp leftovers. The dir holds the
        // destination plus the sidecar `*.lock` (zero-byte advisory
        // lock file) — but no `tmp*` partial-write files.
        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let leftovers: Vec<&String> = entries
            .iter()
            .filter(|n| {
                let p = std::path::Path::new(n);
                let ext_is = |target: &str| -> bool {
                    p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(target))
                };
                !ext_is("json") && !ext_is("lock")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "unexpected temp leftovers after atomic save: {leftovers:?} (full dir: {entries:?})",
        );
    }

    /// Regression for Codex round-8 finding #2: two parallel bench
    /// processes can each `load`, add disjoint entries, then `save`.
    /// Before merge-on-save, the later writer would clobber the
    /// earlier writer's additions because both started from the same
    /// pre-save snapshot. After merge-on-save, the second writer
    /// re-reads the on-disk file, merges its own additions on top,
    /// and persists the union.
    #[test]
    fn save_merges_with_concurrent_on_disk_additions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.json");

        // Both writers start from an empty cache (loaded once).
        let mut writer_a = EmbeddingCache::default();
        let mut writer_b = EmbeddingCache::default();

        let key_a = CacheKey {
            model_label: "m".to_owned(),
            slug: String::new(),
            content_hash: content_hash("a"),
            dim: 4,
        };
        let key_b = CacheKey {
            model_label: "m".to_owned(),
            slug: String::new(),
            content_hash: content_hash("b"),
            dim: 4,
        };

        writer_a.put(key_a.clone(), vec![1.0, 1.0, 1.0, 1.0]);
        writer_b.put(key_b.clone(), vec![2.0, 2.0, 2.0, 2.0]);

        // A persists first, then B persists. Without the merge, B
        // would overwrite the file with only its own entry and A's
        // would be lost.
        writer_a.save(&path).expect("save a");
        writer_b.save(&path).expect("save b");

        let final_cache = EmbeddingCache::load(&path).expect("reload");
        assert_eq!(final_cache.len(), 2, "merge must preserve both writers' adds");
        assert!(final_cache.get(&key_a).is_some(), "writer A's entry lost");
        assert!(final_cache.get(&key_b).is_some(), "writer B's entry lost");
    }

    /// Regression for Codex round-9 finding #2 (barriered concurrent
    /// writers): two threads that BOTH read the same pre-save snapshot
    /// and then both attempt to save must not lose either thread's
    /// additions. The exclusive flock around load-merge-persist
    /// serializes the two saves; whichever thread enters the
    /// critical section second observes the first thread's persisted
    /// entries during its reload and includes them in the rewrite.
    #[test]
    fn save_under_overlapping_writer_schedule_preserves_both_writers() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cache.json");
        // Both writers start from the (empty) on-disk snapshot.
        EmbeddingCache::default().save(&path).expect("seed");

        let barrier = Arc::new(Barrier::new(2));
        let path_a = path.clone();
        let path_b = path.clone();
        let bar_a = Arc::clone(&barrier);
        let bar_b = Arc::clone(&barrier);

        let key_a = CacheKey {
            model_label: "m".to_owned(),
            slug: String::new(),
            content_hash: content_hash("a"),
            dim: 4,
        };
        let key_b = CacheKey {
            model_label: "m".to_owned(),
            slug: String::new(),
            content_hash: content_hash("b"),
            dim: 4,
        };

        let owned_a = key_a.clone();
        let t_a = thread::spawn(move || {
            // Both threads load before either saves — the worst-case
            // window the lock has to close.
            let _seen = EmbeddingCache::load(&path_a).expect("load a");
            let mut local = EmbeddingCache::default();
            local.put(owned_a, vec![1.0; 4]);
            bar_a.wait();
            local.save(&path_a).expect("save a");
        });
        let owned_b = key_b.clone();
        let t_b = thread::spawn(move || {
            let _seen = EmbeddingCache::load(&path_b).expect("load b");
            let mut local = EmbeddingCache::default();
            local.put(owned_b, vec![2.0; 4]);
            bar_b.wait();
            local.save(&path_b).expect("save b");
        });
        t_a.join().expect("join a");
        t_b.join().expect("join b");

        let final_cache = EmbeddingCache::load(&path).expect("reload");
        assert!(
            final_cache.get(&key_a).is_some(),
            "writer A lost: {:?}",
            final_cache.len(),
        );
        assert!(
            final_cache.get(&key_b).is_some(),
            "writer B lost: {:?}",
            final_cache.len(),
        );
    }
}
