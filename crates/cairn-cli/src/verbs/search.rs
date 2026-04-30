//! `cairn search` handler.
//!
//! Dispatches to keyword (`--mode keyword`) or semantic (`--mode semantic`)
//! search depending on the parsed mode flag. Hybrid is stubbed as
//! `CapabilityUnavailable` — follow-up issue.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::contract::memory_store::{MemoryStore as _, SemanticSearchArgs};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::verbs::search::SearchArgsMode;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, new_operation_id, unimplemented_response};

/// Run `cairn search`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    // Parse mode from the generated --mode flag.
    let mode = sub
        .get_one::<SearchArgsMode>("mode")
        .copied()
        .unwrap_or(SearchArgsMode::Keyword);

    match mode {
        SearchArgsMode::Keyword => run_keyword(json),
        SearchArgsMode::Semantic => run_semantic(sub, json),
        SearchArgsMode::Hybrid => {
            let op_id = new_operation_id();
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": {
                        "code": "CapabilityUnavailable",
                        "message": "hybrid search is not yet implemented (follow-up issue)"
                    }
                }));
            } else {
                human_error(
                    "search",
                    "CapabilityUnavailable",
                    "hybrid search is not yet implemented (follow-up issue)",
                    &op_id,
                );
            }
            ExitCode::from(69) // EX_UNAVAILABLE
        }
        // Non-exhaustive arm for future modes.
        _ => {
            let resp = unimplemented_response(ResponseVerb::Search);
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "search",
                    "Internal",
                    "unknown search mode",
                    &resp.operation_id,
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run_keyword(json: bool) -> ExitCode {
    // Keyword search requires the store — not yet wired at P0.
    // TODO(#46): wire store and dispatch to search_keyword.
    let resp = unimplemented_response(ResponseVerb::Search);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "search",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}

#[allow(clippy::too_many_lines)]
fn run_semantic(sub: &ArgMatches, json: bool) -> ExitCode {
    let query = sub.get_one::<String>("query").cloned().unwrap_or_default();
    let limit: usize = sub
        .get_one::<i64>("limit")
        .copied()
        .map_or(10, |l| usize::try_from(l.max(1)).unwrap_or(1));

    // Resolve vault root from CAIRN_VAULT or CWD heuristic.
    // TODO(#46): wire the registry-resolved vault path here.
    let vault_root = if let Ok(p) = std::env::var("CAIRN_VAULT") {
        std::path::PathBuf::from(p)
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    };
    let db_path = vault_root.join(".cairn").join("cairn.db");

    // Load config.
    let config = match crate::config::load(&vault_root, &crate::config::CliOverrides::default()) {
        Ok(c) => c,
        Err(e) => {
            let op_id = new_operation_id();
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "ConfigError", "message": format!("{e:#}") }
                }));
            } else {
                human_error("search", "ConfigError", &format!("{e:#}"), &op_id);
            }
            return ExitCode::from(78); // EX_CONFIG
        }
    };

    // Probe model presence (pure stat, no I/O).
    let models_root = vault_root.join(".cairn").join("models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let kind = config.search.embedding_model;
    let model_present = cache.is_present(kind);
    let caps = config.capabilities(model_present);

    if caps.semantic_search {
        // Run async dispatch on a single-thread runtime (short-lived CLI verb).
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let op_id = new_operation_id();
                let msg = format!("runtime build: {e}");
                if json {
                    emit_json(&serde_json::json!({
                        "operation_id": op_id.0,
                        "verb": "search",
                        "status": "error",
                        "error": { "code": "Internal", "message": msg }
                    }));
                } else {
                    human_error("search", "Internal", &msg, &op_id);
                }
                return ExitCode::FAILURE;
            }
        };

        rt.block_on(async move {
            run_semantic_async(&vault_root, &db_path, query, limit, json, kind).await
        })
    } else {
        let op_id = new_operation_id();
        let msg = if config.search.local_embeddings {
            format!(
                "embedding model '{}' not on disk — run `cairn admin model fetch` or `cairn bootstrap`",
                kind.as_str()
            )
        } else {
            "local_embeddings is disabled in config — set search.local_embeddings: true".to_owned()
        };
        if json {
            emit_json(&serde_json::json!({
                "operation_id": op_id.0,
                "verb": "search",
                "status": "error",
                "error": { "code": "CapabilityUnavailable", "message": msg }
            }));
        } else {
            human_error("search", "CapabilityUnavailable", &msg, &op_id);
        }
        ExitCode::from(69) // EX_UNAVAILABLE
    }
}

#[allow(clippy::too_many_lines)]
async fn run_semantic_async(
    vault_root: &Path,
    db_path: &Path,
    query: String,
    limit: usize,
    json: bool,
    kind: cairn_core::config::EmbeddingModelKind,
) -> ExitCode {
    use anyhow::Context as _;

    // Load model (CPU-bound).
    let models_root = vault_root.join(".cairn").join("models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let embedder = match tokio::task::spawn_blocking(move || cache.ensure(kind))
        .await
        .context("join error")
        .and_then(|r| r.context("model load failed"))
    {
        Ok(e) => e,
        Err(e) => {
            let op_id = new_operation_id();
            let msg = format!("{e:#}");
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "Internal", "message": msg }
                }));
            } else {
                human_error("search", "Internal", &msg, &op_id);
            }
            return ExitCode::FAILURE;
        }
    };

    // Open store with embedder.
    let store =
        match cairn_store_sqlite::open_with_embedder(db_path, Some(Arc::clone(&embedder))).await {
            Ok(s) => s,
            Err(e) => {
                let op_id = new_operation_id();
                let msg = format!("store open: {e}");
                if json {
                    emit_json(&serde_json::json!({
                        "operation_id": op_id.0,
                        "verb": "search",
                        "status": "error",
                        "error": { "code": "Internal", "message": msg }
                    }));
                } else {
                    human_error("search", "Internal", &msg, &op_id);
                }
                return ExitCode::FAILURE;
            }
        };

    // Build visibility allowlist (P0: all tiers).
    let visibility_allowlist = vec![
        MemoryVisibility::Private,
        MemoryVisibility::Session,
        MemoryVisibility::Project,
        MemoryVisibility::Team,
        MemoryVisibility::Org,
        MemoryVisibility::Public,
    ];

    let args = SemanticSearchArgs {
        query,
        filter: None,
        visibility_allowlist,
        limit,
        model_label: kind.as_str().to_owned(),
    };

    match store.search_semantic(&args).await {
        Ok(page) => render_semantic_results(&page, json),
        Err(e) => {
            let op_id = new_operation_id();
            let msg = format!("{e}");
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "Internal", "message": msg }
                }));
            } else {
                human_error("search", "Internal", &msg, &op_id);
            }
            ExitCode::FAILURE
        }
    }
}

fn render_semantic_results(
    page: &cairn_core::contract::memory_store::SemanticSearchPage,
    json: bool,
) -> ExitCode {
    if json {
        let hits: Vec<serde_json::Value> = page
            .candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "record_id": c.record_id.as_str(),
                    "score": c.semantic_distance,
                    "snippet": c.snippet,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "hits": hits })).unwrap_or_default()
        );
    } else if page.candidates.is_empty() {
        println!("search: no results");
    } else {
        for (i, c) in page.candidates.iter().enumerate() {
            let dist = c
                .semantic_distance
                .map_or_else(|| "n/a".to_owned(), |d| format!("{d:.4}"));
            println!(
                "{}. [{}] dist={} {}",
                i + 1,
                c.record_id.as_str(),
                dist,
                c.snippet
            );
        }
    }
    ExitCode::SUCCESS
}
