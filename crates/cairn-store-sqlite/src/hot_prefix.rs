//! `HotPrefixCache` implementation backed by the cairn vault's `SQLite`
//! database. Migration 0053 supplies the `hot_prefix_cache` and
//! `hot_source_watermarks` tables.
//!
//! Convention: this cache opens its **own** `AsyncConn` to the existing DB
//! at `<vault_root>/.cairn/cairn.db`. Migrations must already be applied (typically
//! by `cairn_store_sqlite::open`). The dual-connection layout is safe because
//! WAL mode permits concurrent writers; the store and the cache share the same
//! underlying file but coordinate through `SQLite`'s write-lock protocol.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_rusqlite::Connection as AsyncConn;

use cairn_core::contract::hot_prefix_cache::{CacheError, CachedPrefix, HotPrefixCache};
use cairn_core::domain::Identity;
use cairn_core::domain::hot_prefix::{SourceClass, SourceWatermarks};
use cairn_core::generated::verbs::assemble_hot::HotSegment;

/// `SQLite`-backed hot-prefix cache.
///
/// Opens its own connection to `<vault_root>/.cairn/cairn.db` rather than sharing
/// the primary store connection. `SQLite` WAL mode allows multiple concurrent
/// writers, so the two handles do not block each other.
pub struct SqliteHotPrefixCache {
    conn: Arc<AsyncConn>,
}

impl SqliteHotPrefixCache {
    /// Open a fresh connection to `<vault_root>/.cairn/cairn.db`. The DB is
    /// expected to have already been migrated by
    /// `cairn_store_sqlite::open` — this constructor does NOT re-run
    /// migrations.
    ///
    /// # Errors
    /// Returns [`CacheError::Backend`] if the DB file cannot be opened.
    pub async fn open(vault_root: &Path) -> Result<Self, CacheError> {
        let db = vault_root.join(".cairn").join("cairn.db");
        let conn = AsyncConn::open(&db)
            .await
            .map_err(|e| CacheError::Backend(Box::new(e)))?;
        Ok(Self {
            conn: Arc::new(conn),
        })
    }

    /// Construct from a shared connection — used when the store's connection
    /// is reused (avoids opening a second `SQLite` handle).
    #[must_use]
    pub fn from_conn(conn: Arc<AsyncConn>) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl HotPrefixCache for SqliteHotPrefixCache {
    async fn current_watermarks(&self) -> Result<SourceWatermarks, CacheError> {
        let conn = Arc::clone(&self.conn);
        // The closure returns `Result<Result<SourceWatermarks, CacheError>,
        // tokio_rusqlite::Error>` so we can propagate both rusqlite-level
        // errors (the outer `?`) and semantic errors like
        // `WatermarkSchemaMismatch` (the inner `Result`).
        let result: Result<Result<SourceWatermarks, CacheError>, tokio_rusqlite::Error> = conn
            .call(|c| {
                let mut stmt = c.prepare("SELECT class, watermark FROM hot_source_watermarks")?;
                let rows = stmt.query_map([], |r| {
                    let class: String = r.get(0)?;
                    let watermark: i64 = r.get(1)?;
                    Ok((class, watermark))
                })?;
                let mut wm = SourceWatermarks::default();
                let mut seen = [false; 6];
                for row in rows {
                    let (class, watermark) = row?;
                    let Some(c) = SourceClass::parse(&class) else {
                        continue; // unknown class — forward-compat
                    };
                    set_field(&mut wm, c, u64::try_from(watermark).unwrap_or(0));
                    seen[class_index(c)] = true;
                }
                for (i, was_seen) in seen.iter().enumerate() {
                    if !was_seen {
                        return Ok::<_, tokio_rusqlite::Error>(Err(
                            CacheError::WatermarkSchemaMismatch {
                                class: SourceClass::ALL[i],
                            },
                        ));
                    }
                }
                Ok(Ok(wm))
            })
            .await;
        match result {
            Ok(Ok(wm)) => Ok(wm),
            Ok(Err(cache_err)) => Err(cache_err),
            Err(e) => Err(CacheError::Backend(Box::new(e))),
        }
    }

    async fn get(
        &self,
        agent: &Identity,
        recipe_hash: &str,
    ) -> Result<Option<CachedPrefix>, CacheError> {
        let agent_id = agent.to_string();
        let recipe_hash = recipe_hash.to_owned();
        let conn = Arc::clone(&self.conn);
        let result: Result<Result<Option<CachedPrefix>, CacheError>, tokio_rusqlite::Error> = conn
            .call(move |c| {
                let mut stmt = c.prepare(
                    "SELECT prefix, segments_json, bytes, watermarks_json, \
                     assembled_at_ms, assembly_latency_ms \
                     FROM hot_prefix_cache \
                     WHERE agent_id = ?1 AND recipe_hash = ?2",
                )?;
                let row = stmt
                    .query_row((&agent_id, &recipe_hash), |r| {
                        let prefix: Vec<u8> = r.get(0)?;
                        let segments_json: String = r.get(1)?;
                        let bytes: i64 = r.get(2)?;
                        let watermarks_json: String = r.get(3)?;
                        let assembled_at_ms: i64 = r.get(4)?;
                        let assembly_latency_ms: i64 = r.get(5)?;
                        Ok((
                            prefix,
                            segments_json,
                            bytes,
                            watermarks_json,
                            assembled_at_ms,
                            assembly_latency_ms,
                        ))
                    })
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;

                let Some((prefix, seg_json, bytes, wm_json, at, lat)) = row else {
                    return Ok(Ok(None));
                };
                let segments: Vec<HotSegment> = match serde_json::from_str(&seg_json) {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(Err(CacheError::Corrupt {
                            reason: format!("segments: {e}"),
                        }));
                    }
                };
                let watermarks: SourceWatermarks = match serde_json::from_str(&wm_json) {
                    Ok(w) => w,
                    Err(e) => {
                        return Ok(Err(CacheError::Corrupt {
                            reason: format!("watermarks: {e}"),
                        }));
                    }
                };
                Ok(Ok(Some(CachedPrefix {
                    prefix,
                    segments,
                    bytes: u64::try_from(bytes).unwrap_or(0),
                    watermarks,
                    assembled_at_ms: at,
                    assembly_latency_ms: u64::try_from(lat).unwrap_or(0),
                })))
            })
            .await;
        match result {
            Ok(Ok(entry)) => Ok(entry),
            Ok(Err(cache_err)) => Err(cache_err),
            Err(e) => Err(CacheError::Backend(Box::new(e))),
        }
    }

    async fn put(
        &self,
        agent: &Identity,
        recipe_hash: &str,
        entry: &CachedPrefix,
    ) -> Result<(), CacheError> {
        let agent_id = agent.to_string();
        let recipe_hash = recipe_hash.to_owned();
        let segments_json =
            serde_json::to_string(&entry.segments).map_err(|e| CacheError::Corrupt {
                reason: format!("seg ser: {e}"),
            })?;
        let watermarks_json =
            serde_json::to_string(&entry.watermarks).map_err(|e| CacheError::Corrupt {
                reason: format!("wm ser: {e}"),
            })?;
        let prefix = entry.prefix.clone();
        let bytes = i64::try_from(entry.bytes).unwrap_or(i64::MAX);
        let assembled_at_ms = entry.assembled_at_ms;
        let assembly_latency_ms = i64::try_from(entry.assembly_latency_ms).unwrap_or(i64::MAX);
        let conn = Arc::clone(&self.conn);
        conn.call(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO hot_prefix_cache \
                 (agent_id, recipe_hash, prefix, segments_json, bytes, \
                  watermarks_json, assembled_at_ms, assembly_latency_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    agent_id,
                    recipe_hash,
                    prefix,
                    segments_json,
                    bytes,
                    watermarks_json,
                    assembled_at_ms,
                    assembly_latency_ms,
                ],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .map_err(|e| CacheError::Backend(Box::new(e)))
    }

    async fn bump(&self, classes: &[SourceClass]) -> Result<SourceWatermarks, CacheError> {
        if classes.is_empty() {
            return self.current_watermarks().await;
        }
        let now_ms = now_unix_ms();
        let class_strs: Vec<String> = classes.iter().map(|c| c.as_db_str().to_owned()).collect();
        let conn = Arc::clone(&self.conn);
        conn.call(move |c| {
            // Build a parameterised IN clause dynamically — rusqlite does
            // not support array binding, so we emit `?1, ?2, …` by hand.
            let placeholders = class_strs
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE hot_source_watermarks \
                 SET watermark = watermark + 1, updated_at_ms = ?1 \
                 WHERE class IN ({placeholders})"
            );
            let mut params_boxed: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(now_ms)];
            for s in &class_strs {
                params_boxed.push(Box::new(s.clone()));
            }
            let params_refs: Vec<&dyn rusqlite::ToSql> = params_boxed
                .iter()
                .map(std::convert::AsRef::as_ref)
                .collect();
            c.execute(&sql, params_refs.as_slice())?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .map_err(|e| CacheError::Backend(Box::new(e)))?;
        self.current_watermarks().await
    }
}

/// Map a [`SourceClass`] to its index in [`SourceClass::ALL`].
///
/// Returns `usize::MAX` for unknown variants introduced by a newer version
/// of the library (forward-compat guard — callers skip out-of-range indices).
fn class_index(c: SourceClass) -> usize {
    match c {
        SourceClass::ProfileEvidence => 0,
        SourceClass::Pinned => 1,
        SourceClass::PurposeIndex => 2,
        SourceClass::Summaries => 3,
        SourceClass::Playbooks => 4,
        SourceClass::Policy => 5,
        // Forward-compat: `SourceClass` is #[non_exhaustive]; unknown
        // variants are skipped by the caller rather than panicking.
        _ => usize::MAX,
    }
}

/// Write a single watermark field by class.
fn set_field(wm: &mut SourceWatermarks, c: SourceClass, v: u64) {
    match c {
        SourceClass::ProfileEvidence => wm.profile_evidence = v,
        SourceClass::Pinned => wm.pinned = v,
        SourceClass::PurposeIndex => wm.purpose_index = v,
        SourceClass::Summaries => wm.summaries = v,
        SourceClass::Playbooks => wm.playbooks = v,
        SourceClass::Policy => wm.policy = v,
        // Forward-compat: unknown variants are silently ignored.
        _ => {}
    }
}

/// Current wall-clock in milliseconds since the Unix epoch.
fn now_unix_ms() -> i64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
