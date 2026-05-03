//! `cairn search` handler.
//!
//! Dispatches to keyword (`--mode keyword`), semantic (`--mode semantic`), or
//! hybrid (`--mode hybrid`) search depending on the parsed mode flag.
//!
//! The three former per-mode runners (`run_keyword`, `run_semantic`,
//! `run_hybrid` and their `_async` siblings) have been collapsed into a
//! single `run_async` that delegates to `cairn_core::verbs::search::run`.

use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::config::EmbeddingProvider;
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

/// Local enum mirroring the IDL `SearchArgsMode` to avoid leaking the
/// generated type into the dispatcher signature.
#[derive(Debug, Clone, Copy)]
enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}

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

    let mode_local = match mode {
        SearchArgsMode::Keyword => SearchMode::Keyword,
        SearchArgsMode::Semantic => SearchMode::Semantic,
        SearchArgsMode::Hybrid => SearchMode::Hybrid,
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
            return ExitCode::FAILURE;
        }
    };

    rt.block_on(async move { run_async(sub, json, explain, mode_local).await })
}

#[allow(clippy::too_many_lines)]
// reason: single async verb entry — fans CLI flags + config knobs into the
// dispatcher call; splitting into helpers adds indirection without reducing
// cognitive load here.
async fn run_async(sub: &ArgMatches, json: bool, explain: bool, mode: SearchMode) -> ExitCode {
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
    // CAIRN_MOCK_EMBEDDER=1 is a test-only escape hatch: treat the model as
    // present so semantic/hybrid capabilities are advertised without requiring
    // real model weights on disk. See `resolve_local_embedder` for the
    // corresponding embedder substitution.
    let models_root = vault_root.join(".cairn").join("models");
    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let kind = config.search.embedding_model;
    let mock_embedder = std::env::var("CAIRN_MOCK_EMBEDDER").as_deref() == Ok("1");
    let model_present = mock_embedder || cache.is_present(kind);
    let caps = config.capabilities(model_present);
    let provider = config.search.default_provider;

    // Fail-closed if OpenAi is requested but the `openai` feature is not
    // compiled in. CLAUDE.md §4 invariant 6: never silently downgrade.
    if let Some(rc) = openai_feature_gate(provider, json) {
        return rc;
    }

    // Embedder is required for semantic + hybrid; for keyword it's optional
    // (FTS5 does not need a vector model).
    let embedder = if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
        match resolve_embedder(&vault_root, kind, provider).await {
            Ok(e) => Some(e),
            Err(rc) => return rc.emit(json),
        }
    } else {
        None
    };

    // Open store with embedder + configured FTS column weights.
    let store = match cairn_store_sqlite::open_with_embedder_and_config(
        &db_path,
        embedder,
        config.search.fts_column_weights,
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

    let request = cairn_core::verbs::search::SearchRequest {
        query,
        mode: match mode {
            SearchMode::Keyword => cairn_core::verbs::search::SearchMode::Keyword,
            SearchMode::Semantic => cairn_core::verbs::search::SearchMode::Semantic,
            SearchMode::Hybrid => cairn_core::verbs::search::SearchMode::Hybrid,
        },
        limit,
        visibility_allowlist: vec![],
        model_label: kind.as_str().to_owned(),
        explain,
    };

    match cairn_core::verbs::search::run(&store, &config, &caps, request).await {
        Ok(outcome) => render_outcome(&outcome, json),
        Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
            let op_id = new_operation_id();
            let msg = format!("capability unavailable: {capability}");
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
        Err(cairn_core::verbs::search::SearchError::InvalidArgs { reason }) => {
            let op_id = new_operation_id();
            let msg = format!("invalid args: {reason}");
            if json {
                emit_json(&serde_json::json!({
                    "operation_id": op_id.0,
                    "verb": "search",
                    "status": "error",
                    "error": { "code": "InvalidArgs", "message": msg }
                }));
            } else {
                human_error("search", "InvalidArgs", &msg, &op_id);
            }
            ExitCode::from(64) // EX_USAGE
        }
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

fn render_outcome(outcome: &cairn_core::verbs::search::SearchOutcome, json: bool) -> ExitCode {
    if json {
        let hits: Vec<serde_json::Value> = outcome
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
        let mut out = serde_json::json!({ "hits": hits });
        if let Some(exps) = outcome.explain.as_ref() {
            let arr: Vec<serde_json::Value> = exps
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "record_id": e.record_id.as_str(),
                        "bm25_rank": e.bm25_rank,
                        "semantic_rank": e.semantic_rank,
                        "rrf_score": e.rrf_score,
                        "cosine": e.cosine,
                        "final_score": e.final_score,
                    })
                })
                .collect();
            out["score_explain"] = serde_json::Value::Array(arr);
        }
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else if outcome.candidates.is_empty() {
        println!("search: no results");
    } else {
        for (i, c) in outcome.candidates.iter().enumerate() {
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
        if let Some(exps) = outcome.explain.as_ref() {
            println!("\n--- score explain ---");
            for e in exps {
                println!(
                    "  [{}] bm25_rank={:?} sem_rank={:?} rrf={:.4} cos={:?} final={:.4}",
                    e.record_id.as_str(),
                    e.bm25_rank,
                    e.semantic_rank,
                    e.rrf_score,
                    e.cosine,
                    e.final_score
                );
            }
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
    vault_root: &std::path::Path,
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
    vault_root: &std::path::Path,
    kind: cairn_core::config::EmbeddingModelKind,
) -> Result<Arc<dyn EmbeddingModel>, EmbedderInitError> {
    use anyhow::Context as _;

    // Test escape hatch: CAIRN_MOCK_EMBEDDER=1 bypasses disk model loading.
    // Used by CLI integration tests that seed the DB with MockEmbedder vectors
    // and need deterministic, offline query embedding.
    if std::env::var("CAIRN_MOCK_EMBEDDER").as_deref() == Ok("1") {
        let embedder: Arc<dyn EmbeddingModel> =
            Arc::new(cairn_embeddings_local::MockEmbedder::new(kind));
        return Ok(embedder);
    }

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
