//! `OpenAI` embedding adapter for Cairn.
//!
//! Opt-in: this crate is only compiled when `cairn-cli` is built with
//! `--features openai`. Reads `OPENAI_API_KEY` from env or config; talks to
//! `https://api.openai.com/v1/embeddings` via `reqwest` + `rustls`.
//!
//! See `docs/superpowers/plans/2026-04-30-hybrid-rerank-brainbench.md`
//! (Task 9).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod client;
mod error;
mod types;

pub use client::OpenAiEmbedder;
pub use error::OpenAiEmbeddingError;

/// Return `true` when the supplied model kind and API key are both ready
/// for `OpenAI` embedding calls.
///
/// The two conditions this predicate enforces:
/// 1. `key` is non-empty after trimming (whitespace-only keys are rejected
///    by the `OpenAI` endpoint and are treated as absent here).
/// 2. `model` is a native `OpenAI` text-embedding variant
///    (`OpenAiTextEmbedding3Large` or `OpenAiTextEmbedding3Small`). Local
///    candle models (BGE, `MiniLM`) cannot be served by the `OpenAI` HTTP
///    endpoint; advertising `semantic_search` for them with an `OpenAI`
///    provider config would produce a runtime failure rather than a clean
///    capability rejection.
///
/// Neither network I/O nor filesystem access is performed. This function is
/// safe to call from a capability-check hot path (e.g.
/// `embedding_provider_ready` in `cairn-cli`).
///
/// Both `embedding_provider_ready` (status advertisement) and
/// `resolve_openai_embedder` (actual embedder construction) call this
/// predicate so the two sites cannot diverge.
#[must_use]
pub fn config_ready(model: cairn_core::config::EmbeddingModelKind, key: &str) -> bool {
    use cairn_core::config::EmbeddingModelKind;
    if key.trim().is_empty() {
        return false;
    }
    matches!(
        model,
        EmbeddingModelKind::OpenAiTextEmbedding3Large
            | EmbeddingModelKind::OpenAiTextEmbedding3Small
    )
}
