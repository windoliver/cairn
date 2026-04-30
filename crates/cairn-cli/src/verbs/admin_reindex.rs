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

    if !semantic {
        return bail(
            json,
            "admin reindex",
            "UsageError",
            "specify --semantic (hybrid is a future option)",
            64, // EX_USAGE
        );
    }

    // Probe model presence.
    let models_root = vault_root.join(".cairn").join("models");
    let kind = config.search.embedding_model;
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    if !cache.is_present(kind) {
        let msg = format!(
            "embedding model '{}' not on disk — run `cairn admin model fetch` first",
            kind.as_str()
        );
        return bail(json, "admin reindex", "CapabilityUnavailable", &msg, 69);
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

    // Load embedder (CPU-bound).
    let cache = cairn_embeddings_local::ModelCache::new(models_root);
    let embedder = match tokio::task::spawn_blocking(move || cache.ensure(kind))
        .await
        .context("join error")
        .and_then(|r| r.context("model load failed"))
    {
        Ok(e) => e,
        Err(e) => {
            let msg = format!("{e:#}");
            return bail(json, "admin reindex", "Internal", &msg, 1);
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
