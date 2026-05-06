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
    capability_unavailable_response, emit_json, human_error, internal_error_response,
    invalid_args_response, new_operation_id, not_found_response, unimplemented_response,
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
///
/// `vault_root` is the path resolved by `main` through `vault::resolve_vault`
/// (`--vault NAME_OR_PATH > CAIRN_VAULT > CWD`). Passing it explicitly
/// keeps search in lockstep with `status` and `admin reindex`; an
/// earlier code path re-derived the root from `CAIRN_VAULT > CWD` only
/// and ignored `--vault NAME`, so `cairn --vault prod search foo`
/// could open the CWD vault DB instead of `prod` (round-5 review #1).
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: std::path::PathBuf) -> ExitCode {
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
            if let Some(hint) = cairn_core::status::remediation_for(EXPLAIN_CAPABILITY) {
                eprintln!("  hint: {hint}");
            }
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
            let msg = format!("runtime build: {e}");
            let resp = internal_error_response(ResponseVerb::Search, &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("search", "Internal", &msg, &resp.operation_id);
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

    rt.block_on(async move { run_async(sub, vault_root, json, explain, mode_local).await })
}

#[allow(clippy::too_many_lines)]
// reason: single async verb entry — fans CLI flags + config knobs into the
// dispatcher call; splitting into helpers adds indirection without reducing
// cognitive load here.
async fn run_async(
    sub: &ArgMatches,
    vault_root: std::path::PathBuf,
    json: bool,
    explain: bool,
    mode: SearchMode,
) -> ExitCode {
    let query = sub.get_one::<String>("query").cloned().unwrap_or_default();
    // The generated subcommand registers `limit` as `u32`; map to a usize
    // for downstream args. Floor at 1 to avoid degenerate empty-page calls.
    let limit: usize = sub
        .get_one::<u32>("limit")
        .copied()
        .map_or(10, |l| usize::try_from(l.max(1)).unwrap_or(1));

    // Cheap args-first validation: an empty query is `InvalidArgs`
    // (EX_USAGE 64) and must be rejected before the vault-binding /
    // store-open gates. Otherwise an operator running `cairn search`
    // with no positional arg in an unbound directory would see
    // `EX_CONFIG (78)` instead of the underlying usage error. Use the
    // shared `invalid_args_response` helper so the rejected envelope
    // matches the dispatcher's `InvalidArgs` mapping (round-3 review #4).
    if query.trim().is_empty() {
        let resp = invalid_args_response(ResponseVerb::Search, "query", "must not be empty");
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "search",
                "InvalidArgs",
                "query: must not be empty",
                &resp.operation_id,
            );
        }
        return ExitCode::from(64); // EX_USAGE
    }

    // `vault_root` is the path resolved by `main` through
    // `vault::resolve_vault` (`--vault NAME_OR_PATH > CAIRN_VAULT > CWD
    // > registry default`). Trust it here — the resolver already
    // surfaced explicit/registry errors as EX_CONFIG before dispatch.

    // Vault-binding gate: refuse to open / create `.cairn/cairn.db` in
    // a directory the operator never bootstrapped. status() also
    // refuses to advertise capabilities for an unbound directory; if
    // search proceeded here it would create a side-effect (the SQLite
    // file) in a path advertised as having no backend, and return
    // empty results that read as "this vault has no records" rather
    // than "this isn't a vault."
    match status::probe_vault_binding(&vault_root) {
        status::VaultBinding::Bound => {}
        status::VaultBinding::Unbound => {
            // `NotFound` is the closest IDL error family for "this isn't
            // a vault" — `target = "vault"` carries the structured
            // discriminator so generated clients route it like any
            // other not-found failure (round-6 review #2).
            let msg = format!(
                "no Cairn vault at {} — run `cairn bootstrap` first",
                vault_root.display()
            );
            let resp = not_found_response(ResponseVerb::Search, "vault", &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("search", "NotFound", &msg, &resp.operation_id);
            }
            return ExitCode::from(78); // EX_CONFIG
        }
        status::VaultBinding::Invalid(reason) => {
            // A damaged sentinel is local environment corruption, not
            // a missing target — the IDL `Internal` family is the
            // catch-all here (round-6 review #2).
            let msg = format!("vault binding error — {reason}");
            let resp = internal_error_response(ResponseVerb::Search, &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("search", "Internal", &msg, &resp.operation_id);
            }
            return ExitCode::from(78); // EX_CONFIG
        }
    }

    let db_path = vault_root.join(".cairn").join("cairn.db");

    // Load config. A failure here is local environment corruption
    // (malformed YAML, unresolved env, validation failure) — same
    // category as a damaged vault sentinel, so use the same shared
    // `internal_error_response` helper instead of an ad hoc shape
    // (round-6 review #2).
    let config = match crate::config::load(&vault_root, &crate::config::CliOverrides::default()) {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("{e:#}");
            let resp = internal_error_response(ResponseVerb::Search, &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("search", "Internal", &msg, &resp.operation_id);
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

    // CLI-side capability gate — fires BEFORE embedder resolution so an
    // unadvertised mode (e.g. `--mode semantic` against a vault without
    // an embedding model) maps to an IDL `CapabilityUnavailable`
    // rejected envelope + exit 69 instead of `ModelNotFetched`'s
    // bespoke `status:"error"` JSON. The dispatcher gates the same
    // mode internally (`cairn_core::verbs::search::gate_mode`), but
    // by then the CLI has already opened the embedder and therefore
    // emits a wire-invalid error envelope. Round-9 review #2.
    let mode_capability = match mode {
        SearchMode::Keyword if !caps.keyword_search => Some("cairn.mcp.v1.search.keyword"),
        SearchMode::Semantic if !caps.semantic_search => Some("cairn.mcp.v1.search.semantic"),
        SearchMode::Hybrid if !caps.hybrid_search => Some("cairn.mcp.v1.search.hybrid"),
        _ => None,
    };
    if let Some(capability) = mode_capability {
        let resp = capability_unavailable_response(ResponseVerb::Search, capability);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "search",
                "CapabilityUnavailable",
                &format!("capability unavailable: {capability}"),
                &resp.operation_id,
            );
            if let Some(hint) = cairn_core::status::remediation_for(capability) {
                eprintln!("  hint: {hint}");
            }
        }
        return ExitCode::from(69); // EX_UNAVAILABLE
    }

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
            let msg = format!("store open: {e}");
            let resp = internal_error_response(ResponseVerb::Search, &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("search", "Internal", &msg, &resp.operation_id);
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
        Ok(outcome) => render_outcome(&outcome, json, mode),
        Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
            let resp = capability_unavailable_response(ResponseVerb::Search, capability);
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "search",
                    "CapabilityUnavailable",
                    &format!("capability unavailable: {capability}"),
                    &resp.operation_id,
                );
                if let Some(hint) = cairn_core::status::remediation_for(capability) {
                    eprintln!("  hint: {hint}");
                }
            }
            ExitCode::from(69) // EX_UNAVAILABLE
        }
        Err(cairn_core::verbs::search::SearchError::InvalidArgs { reason }) => {
            let resp = invalid_args_response(ResponseVerb::Search, "args", &reason);
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "search",
                    "InvalidArgs",
                    &format!("invalid args: {reason}"),
                    &resp.operation_id,
                );
            }
            ExitCode::from(64) // EX_USAGE
        }
        Err(e) => {
            let msg = format!("{e}");
            let resp = internal_error_response(ResponseVerb::Search, &msg);
            if json {
                emit_json(&resp);
            } else {
                human_error("search", "Internal", &msg, &resp.operation_id);
            }
            ExitCode::FAILURE
        }
    }
}

/// Render a successful `cairn search` outcome.
///
/// JSON is the canonical wire envelope (brief §8.0.b): a fully-formed
/// `cairn.mcp.v1` `Response` with `verb=search`, `status=committed`,
/// and `data` carrying the IDL `SearchData` payload (round-8 review #1
/// — the prior bespoke `{ hits, score_explain }` shape diverged from
/// the IDL and broke generated clients negotiating from
/// `status.capabilities`). Human output keeps the operator-friendly
/// per-candidate listing — non-JSON consumers don't go through the
/// envelope validator.
fn render_outcome(
    outcome: &cairn_core::verbs::search::SearchOutcome,
    json: bool,
    mode: SearchMode,
) -> ExitCode {
    if json {
        emit_json(&outcome_envelope(outcome, mode));
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

/// Project a `SearchOutcome` into the IDL response envelope.
///
/// The IDL `Hit.score` is the mode-appropriate ranking score (higher =
/// better). Round-9 review #3: previously every hit's score was set to
/// `bm25`, which is `0.0` for the semantic leg and stale for hybrid
/// (whose true ranking comes from RRF + cosine rerank), so
/// `data.hits[].score` disagreed with `data.score_explain[].final_score`
/// and clients ranking off `score` saw lower-ranked hits as ties.
///
/// Score by mode:
/// - `Keyword` → `bm25` directly (the leg's own ranking signal).
/// - `Semantic` → `1 - cosine_distance` (the leg orders by distance
///   ascending; convert to similarity so higher = better and so the
///   monotonic ordering matches the candidate page order).
/// - `Hybrid` → `score_explain[i].final_score` when present (one-to-one
///   alignment with `candidates`; the dispatcher trims them in
///   lockstep). When `--explain` was off, fall back to a synthetic
///   monotone score derived from the page index so clients ranking
///   purely off `score` still see the leg's ordering.
///
/// `trust = Unknown` because P0 has no provenance ledger yet (#62 /
/// brief §6.4); `citation = None` for the same reason. The CLI and
/// SDK envelopes diverge on `Hit.score` for now — the SDK helper
/// (`cairn-sdk::transport::envelope_from_outcome`) still emits `bm25`
/// and tracks the same fix in #62.
fn outcome_envelope(
    outcome: &cairn_core::verbs::search::SearchOutcome,
    mode: SearchMode,
) -> cairn_core::generated::envelope::Response {
    use cairn_core::generated::common::Ulid;
    use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus};
    use cairn_core::generated::verbs::search::{Hit, HitTrust, ScoreExplain, SearchData};

    let hits: Vec<Hit> = outcome
        .candidates
        .iter()
        .enumerate()
        .map(|(idx, c)| Hit {
            record_id: Ulid(c.record_id.as_str().to_owned()),
            score: hit_score(mode, idx, c, outcome.explain.as_deref()),
            snippet: Some(c.snippet.clone()),
            citation: None,
            trust: HitTrust::Unknown,
        })
        .collect();

    let score_explain = outcome.explain.as_ref().map(|exps| {
        exps.iter()
            .map(|e| ScoreExplain {
                record_id: Ulid(e.record_id.as_str().to_owned()),
                bm25_rank: e.bm25_rank.map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
                semantic_rank: e
                    .semantic_rank
                    .map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
                rrf_score: finite_or_zero(e.rrf_score),
                cosine: finite_option(e.cosine),
                final_score: finite_or_zero(e.final_score),
            })
            .collect()
    });

    let data = SearchData {
        hits,
        next_cursor: None,
        excluded: None,
        score_explain,
    };

    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Search(data)),
        error: None,
        operation_id: new_operation_id(),
        policy_trace: Vec::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Search,
    }
}

/// Mode-appropriate ranking score for a single search hit.
///
/// `idx` is the candidate's 0-based position in the page (only
/// consulted on the hybrid-without-explain fallback path, where it
/// produces a monotone score that matches the leg's existing order).
/// `explain` is the lockstep `ScoreExplain` page when `--explain`
/// populated the outcome — see `outcome_envelope` for the per-mode
/// rationale.
///
/// Always returns a finite `f64`. Inputs from the store/embedder
/// (BM25, cosine distance, RRF final score) are unconstrained and can
/// in principle be `NaN` or `±Infinity`; JSON Schema's `number` type
/// admits neither. Non-finite values are rewritten to `0.0` so a
/// committed envelope cannot serialize as wire-invalid traffic
/// (round-10 review #4). The same guard runs over `ScoreExplain`
/// fields below.
fn hit_score(
    mode: SearchMode,
    idx: usize,
    c: &cairn_core::contract::memory_store::SearchCandidate,
    explain: Option<&[cairn_core::search::ScoreExplain]>,
) -> f64 {
    let raw = match mode {
        SearchMode::Keyword => c.bm25,
        SearchMode::Semantic => {
            // semantic_distance is cosine distance ([0, 2]); similarity
            // = 1 - distance has the right monotonicity for a "higher is
            // better" score. Rows missing a distance reading rank below
            // any real similarity (score = 0).
            c.semantic_distance.map_or(0.0, |d| 1.0 - f64::from(d))
        }
        SearchMode::Hybrid => {
            // Prefer the dispatcher's authoritative `final_score` when
            // explain was requested. The dispatcher trims candidates
            // and explain in lockstep (`token_budget_trim`), so
            // index-aligned access is safe and preserves the canonical
            // hybrid ranking signal.
            if let Some(exps) = explain
                && let Some(e) = exps.get(idx)
            {
                e.final_score
            } else {
                // No explain block — synthesize a monotonically
                // decreasing score from the candidate's page position
                // so clients ranking purely off `Hit.score` still see
                // the leg's RRF/cosine ordering. The exact magnitude
                // is unstable (`--explain` exposes the real number);
                // operators reading the field for ordering only need
                // the sign and rank. Tracked for full-fidelity scores
                // in #62 (carry `final_score` through `SearchOutcome`
                // independent of the explain block).
                #[allow(clippy::cast_precision_loss)] // monotone synthetic score
                let rank_score = 1.0 / (1.0 + idx as f64);
                rank_score
            }
        }
    };
    finite_or_zero(raw)
}

/// Replace `NaN` / `±Infinity` with `0.0` so JSON serialization stays
/// schema-conformant. JSON Schema's `number` type does not admit
/// non-finite floats; smuggling them into a committed envelope would
/// produce traffic that the generated `Response` deserializer (and any
/// schema-validating client) rejects (round-10 review #4).
#[inline]
#[must_use]
fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

/// `Option` variant of [`finite_or_zero`] that preserves `None` so
/// optional `ScoreExplain` floats (`cosine`) can be sanitized without
/// losing the absent/missing distinction.
#[inline]
#[must_use]
fn finite_option(value: Option<f64>) -> Option<f64> {
    value.map(finite_or_zero)
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
        // Round all error JSON through the IDL envelope helpers so an
        // OpenAI-without-feature configuration still produces a wire-
        // valid `Response` (round-10 review #1). The advertised
        // capability identifier is the same as the dispatcher's gate
        // for `openai` so generated clients can route the failure off
        // a single capability id.
        const OPENAI_CAP: &str = "cairn.mcp.v1.search.semantic";
        let resp = capability_unavailable_response(ResponseVerb::Search, OPENAI_CAP);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "search",
                "CapabilityUnavailable",
                "search.default_provider = openai requires the `openai` cargo feature; \
                 rebuild cairn-cli with `--features openai`",
                &resp.operation_id,
            );
            if let Some(hint) = cairn_core::status::remediation_for(OPENAI_CAP) {
                eprintln!("  hint: {hint}");
            }
        }
        Some(ExitCode::from(69)) // EX_UNAVAILABLE
    }
}

/// Reason an embedder could not be constructed. Carries enough context to
/// emit an envelope error and an exit code at the verb boundary.
///
/// `kind` selects which IDL envelope family we project the failure
/// into so generated clients can deserialize it without a special case
/// per call site (round-10 review #1):
/// - `CapabilityUnavailable` → rejected envelope, code
///   `CapabilityUnavailable`, paired with the named capability so
///   clients can route off `error.data.capability`.
/// - `Internal` → aborted envelope, code `Internal`, used for local
///   environment failures (model load error, unknown provider).
enum EmbedderInitError {
    CapabilityUnavailable {
        capability: &'static str,
        msg: String,
    },
    Internal {
        msg: String,
    },
}

impl EmbedderInitError {
    fn emit(self, json: bool) -> ExitCode {
        match self {
            EmbedderInitError::CapabilityUnavailable { capability, msg } => {
                let resp = capability_unavailable_response(ResponseVerb::Search, capability);
                if json {
                    emit_json(&resp);
                } else {
                    human_error("search", "CapabilityUnavailable", &msg, &resp.operation_id);
                    if let Some(hint) = cairn_core::status::remediation_for(capability) {
                        eprintln!("  hint: {hint}");
                    }
                }
                ExitCode::from(69)
            }
            EmbedderInitError::Internal { msg } => {
                let resp = internal_error_response(ResponseVerb::Search, &msg);
                if json {
                    emit_json(&resp);
                } else {
                    human_error("search", "Internal", &msg, &resp.operation_id);
                }
                ExitCode::FAILURE
            }
        }
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
        other => Err(EmbedderInitError::Internal {
            msg: format!("unknown embedding provider: {other:?}"),
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
        .map_err(|e| EmbedderInitError::Internal {
            msg: format!("{e:#}"),
        })
}

#[cfg(feature = "openai")]
fn resolve_openai_embedder(
    kind: cairn_core::config::EmbeddingModelKind,
) -> Result<Arc<dyn EmbeddingModel>, EmbedderInitError> {
    use cairn_embeddings_openai::OpenAiEmbedder;
    let embedder =
        OpenAiEmbedder::from_env(kind).map_err(|e| EmbedderInitError::CapabilityUnavailable {
            capability: "cairn.mcp.v1.search.semantic",
            msg: format!("OpenAI embedder init: {e}"),
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
    Err(EmbedderInitError::CapabilityUnavailable {
        capability: "cairn.mcp.v1.search.semantic",
        msg: "openai feature not compiled in; rebuild with `--features openai`".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_or_zero_passes_normal_values() {
        assert!((finite_or_zero(1.5) - 1.5).abs() < f64::EPSILON);
        assert!((finite_or_zero(-0.5) - -0.5).abs() < f64::EPSILON);
        assert!((finite_or_zero(0.0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn finite_or_zero_zeroes_non_finite() {
        // JSON Schema number does not admit NaN / Infinity; the guard
        // rewrites them to 0.0 so committed envelopes stay
        // schema-valid (round-10 review #4).
        assert!((finite_or_zero(f64::NAN) - 0.0).abs() < f64::EPSILON);
        assert!((finite_or_zero(f64::INFINITY) - 0.0).abs() < f64::EPSILON);
        assert!((finite_or_zero(f64::NEG_INFINITY) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn finite_option_preserves_none_and_sanitizes_some() {
        assert_eq!(finite_option(None), None);
        assert_eq!(finite_option(Some(0.5)), Some(0.5));
        assert_eq!(finite_option(Some(f64::NAN)), Some(0.0));
        assert_eq!(finite_option(Some(f64::INFINITY)), Some(0.0));
    }
}
