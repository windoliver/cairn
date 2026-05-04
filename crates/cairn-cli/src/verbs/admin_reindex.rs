//! `cairn admin reindex --semantic [--all]`
//!
//! Drains the `pending_embeddings` queue, optionally re-enqueueing all active
//! records first (for model swap). Requires the embedding model to be present
//! on disk; run `cairn admin model fetch` first if needed.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::config::CairnConfig;
use clap::ArgMatches;
use serde::Serialize;

use super::envelope::{human_error, new_operation_id};

#[derive(Debug, Serialize)]
struct ReindexOutput {
    drained: usize,
    failed: usize,
    remaining: usize,
}

#[derive(Debug, Serialize)]
struct RebuildOutput {
    fts_rebuilt: u64,
    drained: usize,
    failed: usize,
    remaining: usize,
}

/// Emit an error and return the given exit code. Shared by the two error-exit
/// patterns in this module so the logic stays in one place.
fn bail(json: bool, verb: &str, code: &str, msg: &str, exit: u8) -> ExitCode {
    let op_id = new_operation_id();
    if json {
        eprintln!(
            "{}",
            serde_json::json!({
                "operation_id": op_id.0,
                "error": { "code": code, "message": msg }
            })
        );
    } else {
        human_error(verb, code, msg, &op_id);
    }
    ExitCode::from(exit)
}

/// Run `cairn admin reindex`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path, config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let semantic = sub.get_flag("semantic");
    let all = sub.get_flag("all");
    let from_db = sub.get_flag("from-db");

    if !semantic && !from_db {
        return bail(
            json,
            "admin reindex",
            "UsageError",
            "specify --semantic or --from-db",
            64, // EX_USAGE
        );
    }

    // Probe model presence. Skipped when CAIRN_MOCK_EMBEDDER=1 (test-only
    // escape hatch that uses MockEmbedder in place of on-disk model weights).
    let models_root = vault_root.join(".cairn").join("models");
    let kind = config.search.embedding_model;
    let mock_embedder = std::env::var("CAIRN_MOCK_EMBEDDER").as_deref() == Ok("1");
    if !mock_embedder {
        let cache = cairn_embeddings_local::ModelCache::new(&models_root);
        if !cache.is_present(kind) {
            let msg = format!(
                "embedding model '{}' not on disk — run `cairn admin model fetch` first",
                kind.as_str()
            );
            return bail(json, "admin reindex", "CapabilityUnavailable", &msg, 69);
        }
    }

    let db_path = vault_root.join(".cairn").join("cairn.db");

    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return bail(
            json,
            "admin reindex",
            "Internal",
            "could not build tokio runtime",
            1,
        );
    };

    if from_db {
        return rt.block_on(async move {
            run_rebuild_from_db_async(&db_path, &models_root, kind, json).await
        });
    }

    rt.block_on(async move { run_reindex_async(&db_path, &models_root, kind, all, json).await })
}

#[allow(clippy::too_many_lines)]
async fn run_reindex_async(
    db_path: &Path,
    models_root: &Path,
    kind: cairn_core::config::EmbeddingModelKind,
    enqueue_all: bool,
    json: bool,
) -> ExitCode {
    use anyhow::Context as _;

    // Load embedder (CPU-bound). CAIRN_MOCK_EMBEDDER=1 bypasses disk load for tests.
    let embedder: Arc<dyn cairn_embeddings_local::EmbeddingModel> =
        if std::env::var("CAIRN_MOCK_EMBEDDER").as_deref() == Ok("1") {
            Arc::new(cairn_embeddings_local::MockEmbedder::new(kind))
        } else {
            let cache = cairn_embeddings_local::ModelCache::new(models_root);
            match tokio::task::spawn_blocking(move || cache.ensure(kind))
                .await
                .context("join error")
                .and_then(|r| r.context("model load failed"))
            {
                Ok(e) => e,
                Err(e) => {
                    let msg = format!("{e:#}");
                    return bail(json, "admin reindex", "Internal", &msg, 1);
                }
            }
        };

    // Open store with embedder (background drain loop included).
    let store =
        match cairn_store_sqlite::open_with_embedder(db_path, Some(Arc::clone(&embedder))).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("store open: {e}");
                return bail(json, "admin reindex", "Internal", &msg, 1);
            }
        };

    // If --all: re-enqueue every active record (model swap path).
    if enqueue_all {
        let Some(raw) = store.raw_conn_for_admin() else {
            return bail(
                json,
                "admin reindex",
                "Internal",
                "store connection not available",
                1,
            );
        };
        let conn = Arc::clone(raw);
        if let Err(e) = conn
            .call(|c| {
                c.execute(
                    "INSERT INTO pending_embeddings(record_id, reason, attempt_count, enqueued_at)
                       SELECT r.record_id, 'model_swap', 0, strftime('%s','now')
                         FROM records r
                        WHERE r.active = 1 AND r.tombstoned = 0
                       ON CONFLICT(record_id) DO UPDATE
                         SET reason = 'model_swap', attempt_count = 0",
                    [],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
        {
            let msg = format!("enqueue all: {e}");
            return bail(json, "admin reindex", "Internal", &msg, 1);
        }
    }

    // Drain loop — up to 10_000 passes.
    let Some(raw_conn) = store.raw_conn_for_admin() else {
        return bail(
            json,
            "admin reindex",
            "Internal",
            "store connection not available",
            1,
        );
    };
    let conn = Arc::clone(raw_conn);

    let mut total_drained = 0usize;
    let mut total_failed = 0usize;

    for _ in 0..10_000u32 {
        match cairn_store_sqlite::drain_once(Arc::clone(&conn), Arc::clone(&embedder)).await {
            Ok(stats) => {
                total_drained += stats.drained;
                total_failed += stats.failed;
                if stats.remaining == 0 || (stats.drained == 0 && stats.failed == 0) {
                    let out = ReindexOutput {
                        drained: total_drained,
                        failed: total_failed,
                        remaining: stats.remaining,
                    };
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&out)
                                .expect("invariant: ReindexOutput is always serializable")
                        );
                    } else {
                        println!(
                            "cairn admin reindex: drained={} failed={} remaining={}",
                            out.drained, out.failed, out.remaining
                        );
                    }
                    return ExitCode::SUCCESS;
                }
            }
            Err(e) => {
                let msg = format!("drain_once: {e}");
                return bail(json, "admin reindex", "Internal", &msg, 1);
            }
        }
    }

    bail(
        json,
        "admin reindex",
        "Internal",
        "reindex did not converge — check for poison-pill rows (attempt_count > 5)",
        1,
    )
}

#[allow(clippy::too_many_lines)]
async fn run_rebuild_from_db_async(
    db_path: &Path,
    models_root: &Path,
    kind: cairn_core::config::EmbeddingModelKind,
    json: bool,
) -> ExitCode {
    use anyhow::Context as _;

    // Load embedder (CPU-bound). CAIRN_MOCK_EMBEDDER=1 bypasses disk load for tests.
    let embedder: Arc<dyn cairn_embeddings_local::EmbeddingModel> =
        if std::env::var("CAIRN_MOCK_EMBEDDER").as_deref() == Ok("1") {
            Arc::new(cairn_embeddings_local::MockEmbedder::new(kind))
        } else {
            let cache = cairn_embeddings_local::ModelCache::new(models_root);
            match tokio::task::spawn_blocking(move || cache.ensure(kind))
                .await
                .context("join error")
                .and_then(|r| r.context("model load failed"))
            {
                Ok(e) => e,
                Err(e) => {
                    return bail(json, "admin reindex", "Internal", &format!("{e:#}"), 1);
                }
            }
        };

    // Open store with embedder.
    let store =
        match cairn_store_sqlite::open_with_embedder(db_path, Some(Arc::clone(&embedder))).await {
            Ok(s) => s,
            Err(e) => {
                return bail(
                    json,
                    "admin reindex",
                    "Internal",
                    &format!("store open: {e}"),
                    1,
                );
            }
        };

    let Some(raw_conn) = store.raw_conn_for_admin() else {
        return bail(
            json,
            "admin reindex",
            "Internal",
            "store connection not available",
            1,
        );
    };
    let conn = Arc::clone(raw_conn);

    // Rebuild FTS5 + vector indexes from the authoritative records table.
    let stats = match cairn_store_sqlite::rebuild_from_db(Arc::clone(&conn)).await {
        Ok(s) => s,
        Err(e) => {
            return bail(
                json,
                "admin reindex",
                "Internal",
                &format!("rebuild_from_db: {e}"),
                1,
            );
        }
    };

    // Drain embedding queue — up to 10_000 passes.
    let mut total_drained = 0usize;
    let mut total_failed = 0usize;
    let mut last_remaining = 0usize;

    for _ in 0..10_000u32 {
        match cairn_store_sqlite::drain_once(Arc::clone(&conn), Arc::clone(&embedder)).await {
            Ok(s) => {
                total_drained += s.drained;
                total_failed += s.failed;
                last_remaining = s.remaining;
                if s.remaining == 0 || (s.drained == 0 && s.failed == 0) {
                    break;
                }
            }
            Err(e) => {
                return bail(
                    json,
                    "admin reindex",
                    "Internal",
                    &format!("drain_once: {e}"),
                    1,
                );
            }
        }
    }

    let out = RebuildOutput {
        fts_rebuilt: stats.fts_rebuilt,
        drained: total_drained,
        failed: total_failed,
        remaining: last_remaining,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .expect("invariant: RebuildOutput is always serializable")
        );
    } else {
        println!(
            "cairn admin reindex --from-db: fts_rebuilt={} drained={} failed={} remaining={}",
            out.fts_rebuilt, out.drained, out.failed, out.remaining
        );
    }
    ExitCode::SUCCESS
}
