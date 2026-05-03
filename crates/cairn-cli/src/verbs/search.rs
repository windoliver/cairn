//! `cairn search` handler.
//!
//! Dispatches to keyword (`--mode keyword`), semantic (`--mode semantic`), or
//! hybrid (`--mode hybrid`) search depending on the parsed mode flag.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::config::EmbeddingProvider;
use cairn_core::contract::memory_store::{
    HybridSearchArgs, HybridSearchPage, MemoryStore as _, SemanticSearchArgs,
};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::generated::envelope::ResponseVerb;
use cairn_core::generated::verbs::search::SearchArgsMode;
use cairn_embeddings_local::EmbeddingModel;
use clap::ArgMatches;

use super::envelope::{
    capability_unavailable_response, emit_json, human_error, new_operation_id,
    unimplemented_response,
};
use super::status;

/// Capability that gates `--explain`. Mirrors
/// `crates/cairn-idl/schema/verbs/search.json`'s
/// `args.explain.x-cairn-capability-when-true` annotation; tests in
/// `crates/cairn-idl/tests/schema_files.rs` lock the contract.
const EXPLAIN_CAPABILITY: &str = "cairn.mcp.v1.policy_trace";

/// Run `cairn search`. `--explain` requires the
/// `cairn.mcp.v1.policy_trace` capability to be advertised by `status`;
/// otherwise we fail-closed with `CapabilityUnavailable` (sysexit 69)
/// before any verb dispatch (CLAUDE.md §6.5, §4.6).
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let explain = sub.get_flag("explain");

    if explain && !status::p0_capabilities_advertises(EXPLAIN_CAPABILITY) {
        let resp = capability_unavailable_response(ResponseVerb::Search, EXPLAIN_CAPABILITY);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "search",
                "CapabilityUnavailable",
                &format!("--explain requires {EXPLAIN_CAPABILITY}, which is not advertised"),
                &resp.operation_id,
            );
        }
        return ExitCode::from(69); // EX_UNAVAILABLE
    }

    // Parse mode from the generated --mode flag. The generated subcommand
    // registers `mode` with a `PossibleValuesParser` which yields `String`,
    // so we map the string back to the typed enum here. Default = Keyword.
    let mode = sub
        .get_one::<String>("mode")
        .map_or(SearchArgsMode::Keyword, |s| match s.as_str() {
            "semantic" => SearchArgsMode::Semantic,
            "hybrid" => SearchArgsMode::Hybrid,
            _ => SearchArgsMode::Keyword,
        });

    match mode {
        SearchArgsMode::Keyword => run_keyword(json),
        SearchArgsMode::Semantic => run_semantic(sub, json),
        SearchArgsMode::Hybrid => run_hybrid(sub, json),
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
    // The generated subcommand registers `limit` as `u32`; map to a usize
    // for downstream args. Floor at 1 to avoid degenerate empty-page calls.
    let limit: usize = sub
        .get_one::<u32>("limit")
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
    let provider = config.search.default_provider;

    // Fail-closed if OpenAi is requested but the `openai` feature is not
    // compiled in. CLAUDE.md §4 invariant 6: never silently downgrade.
    if let Some(rc) = openai_feature_gate(provider, json) {
        return rc;
    }

    // For OpenAi we don't need a local model on disk, so the local
    // capability gate doesn't apply. The local case still requires the
    // weights to be present.
    if provider == EmbeddingProvider::OpenAi || caps.semantic_search {
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
            run_semantic_async(&vault_root, &db_path, query, limit, json, kind, provider).await
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
// reason: dispatcher fans CLI flags + config-derived weights into the flat verb call; splitting into a struct buys nothing here
#[allow(clippy::too_many_arguments)]
async fn run_semantic_async(
    vault_root: &Path,
    db_path: &Path,
    query: String,
    limit: usize,
    json: bool,
    kind: cairn_core::config::EmbeddingModelKind,
    provider: EmbeddingProvider,
) -> ExitCode {
    let embedder = match resolve_embedder(vault_root, kind, provider).await {
        Ok(e) => e,
        Err(rc) => return rc.emit(json),
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
        with_explain: false,
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

#[allow(clippy::too_many_lines)]
fn run_hybrid(sub: &ArgMatches, json: bool) -> ExitCode {
    let query = sub.get_one::<String>("query").cloned().unwrap_or_default();
    // The generated subcommand registers `limit` as `u32`; map to a usize
    // for downstream args. Floor at 1 to avoid degenerate empty-page calls.
    let limit: usize = sub
        .get_one::<u32>("limit")
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
    let provider = config.search.default_provider;

    if let Some(rc) = openai_feature_gate(provider, json) {
        return rc;
    }

    if provider == EmbeddingProvider::OpenAi || caps.semantic_search {
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

        let blend = config.search.rerank_blend;
        let fts_weights = config.search.fts_column_weights;
        let rrf_k = config.search.rrf_k;
        let rerank_topk = config.search.rerank_topk;

        rt.block_on(async move {
            run_hybrid_async(
                &vault_root,
                &db_path,
                query,
                limit,
                json,
                kind,
                provider,
                fts_weights,
                blend,
                rrf_k,
                rerank_topk,
            )
            .await
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
// reason: dispatcher fans CLI flags + config-derived weights into the flat verb call; splitting into a struct buys nothing here
#[allow(clippy::too_many_arguments)]
async fn run_hybrid_async(
    vault_root: &Path,
    db_path: &Path,
    query: String,
    limit: usize,
    json: bool,
    kind: cairn_core::config::EmbeddingModelKind,
    provider: EmbeddingProvider,
    fts_column_weights: [f64; 4],
    blend: f32,
    rrf_k: usize,
    rerank_topk: usize,
) -> ExitCode {
    let embedder = match resolve_embedder(vault_root, kind, provider).await {
        Ok(e) => e,
        Err(rc) => return rc.emit(json),
    };

    // Open store with embedder + configured FTS column weights.
    let store = match cairn_store_sqlite::open_with_embedder_and_config(
        db_path,
        Some(Arc::clone(&embedder)),
        fts_column_weights,
    )
    .await
    {
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

    let args = HybridSearchArgs {
        query,
        filter: None,
        visibility_allowlist,
        limit,
        model_label: kind.as_str().to_owned(),
        blend,
        rrf_k,
        rerank_topk,
        with_explain: false,
    };

    match store.search_hybrid(&args).await {
        Ok(page) => render_hybrid_results(&page, json),
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

fn render_hybrid_results(page: &HybridSearchPage, json: bool) -> ExitCode {
    if json {
        let hits: Vec<serde_json::Value> = page
            .candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "record_id": c.record_id.as_str(),
                    "bm25": c.bm25,
                    "semantic_distance": c.semantic_distance,
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
                "{}. [{}] bm25={:.4} dist={} {}",
                i + 1,
                c.record_id.as_str(),
                c.bm25,
                dist,
                c.snippet,
            );
        }
    }
    ExitCode::SUCCESS
}

// ── Embedder resolution ─────────────────────────────────────────────────────

/// Pre-flight gate for the `openai` provider. Returns `Some(ExitCode)` when
/// the caller selected `EmbeddingProvider::OpenAi` but the `openai` Cargo
/// feature was not compiled in. Otherwise returns `None` and the caller
/// continues into normal dispatch.
fn openai_feature_gate(provider: EmbeddingProvider, json: bool) -> Option<ExitCode> {
    if provider != EmbeddingProvider::OpenAi {
        return None;
    }
    #[cfg(feature = "openai")]
    {
        let _ = json; // intentionally unused on this branch
        None
    }
    #[cfg(not(feature = "openai"))]
    {
        let op_id = new_operation_id();
        let msg = "search.default_provider = openai requires the `openai` cargo feature; \
                   rebuild cairn-cli with `--features openai`"
            .to_owned();
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
        Some(ExitCode::from(69)) // EX_UNAVAILABLE
    }
}

/// Reason an embedder could not be constructed. Carries enough context to
/// emit an envelope error and an exit code at the verb boundary.
struct EmbedderInitError {
    code: &'static str,
    msg: String,
    exit: ExitCode,
}

impl EmbedderInitError {
    fn emit(self, json: bool) -> ExitCode {
        let op_id = new_operation_id();
        if json {
            emit_json(&serde_json::json!({
                "operation_id": op_id.0,
                "verb": "search",
                "status": "error",
                "error": { "code": self.code, "message": self.msg }
            }));
        } else {
            human_error("search", self.code, &self.msg, &op_id);
        }
        self.exit
    }
}

/// Build the embedder selected by `provider`.
///
/// - `Local`: load weights from `.cairn/models/<kind>` via `ModelCache`.
/// - `OpenAi`: construct an `OpenAiEmbedder` from `OPENAI_API_KEY`.
async fn resolve_embedder(
    vault_root: &Path,
    kind: cairn_core::config::EmbeddingModelKind,
    provider: EmbeddingProvider,
) -> Result<Arc<dyn EmbeddingModel>, EmbedderInitError> {
    match provider {
        EmbeddingProvider::Local => resolve_local_embedder(vault_root, kind).await,
        EmbeddingProvider::OpenAi => resolve_openai_embedder(kind),
        // EmbeddingProvider is #[non_exhaustive]; future providers must opt in.
        other => Err(EmbedderInitError {
            code: "Internal",
            msg: format!("unknown embedding provider: {other:?}"),
            exit: ExitCode::FAILURE,
        }),
    }
}

async fn resolve_local_embedder(
    vault_root: &Path,
    kind: cairn_core::config::EmbeddingModelKind,
) -> Result<Arc<dyn EmbeddingModel>, EmbedderInitError> {
    use anyhow::Context as _;

    let models_root = vault_root.join(".cairn").join("models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    tokio::task::spawn_blocking(move || cache.ensure(kind))
        .await
        .context("join error")
        .and_then(|r| r.context("model load failed"))
        .map_err(|e| EmbedderInitError {
            code: "Internal",
            msg: format!("{e:#}"),
            exit: ExitCode::FAILURE,
        })
}

#[cfg(feature = "openai")]
fn resolve_openai_embedder(
    kind: cairn_core::config::EmbeddingModelKind,
) -> Result<Arc<dyn EmbeddingModel>, EmbedderInitError> {
    use cairn_embeddings_openai::OpenAiEmbedder;
    let embedder = OpenAiEmbedder::from_env(kind).map_err(|e| EmbedderInitError {
        code: "CapabilityUnavailable",
        msg: format!("OpenAI embedder init: {e}"),
        exit: ExitCode::from(69),
    })?;
    Ok(Arc::new(embedder))
}

#[cfg(not(feature = "openai"))]
fn resolve_openai_embedder(
    _kind: cairn_core::config::EmbeddingModelKind,
) -> Result<Arc<dyn EmbeddingModel>, EmbedderInitError> {
    // Unreachable in practice: the synchronous `openai_feature_gate` short-
    // circuits before we ever get here. Keep this as a defensive fallback
    // that fails closed instead of panicking.
    Err(EmbedderInitError {
        code: "CapabilityUnavailable",
        msg: "openai feature not compiled in; rebuild with `--features openai`".to_owned(),
        exit: ExitCode::from(69),
    })
}
