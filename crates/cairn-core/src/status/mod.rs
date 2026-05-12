//! Capability advertisement — the single source of truth.
//!
//! `advertise()` is a pure function from `CapabilityGates` to the wire-format
//! `Vec<Capabilities>`. CLI's `cairn status` handler, `cairn-sdk`'s `Sdk::status`,
//! and `cairn-mcp`'s `get_info` all delegate here; no surface re-derives the
//! per-capability rule.
//!
//! ## Scope of this module
//!
//! Only decisions about which capabilities are *advertised*. **Not** for:
//! - Dispatch gating (use the per-verb error type — `SearchError::CapabilityUnavailable`,
//!   etc. — and reject from there).
//! - Config validation (use `cairn-core::config`).
//! - Runtime feature toggles (use Cargo features at the crate level).
//!
//! ## Mental model
//!
//! Each capability has one row in `advertise()`. A row evaluates to `true` (and
//! pushes the capability into the result Vec) only when *all* of:
//!
//! 1. The vault is bound (`vault_bound: true`).
//! 2. The contract phase is at or beyond the capability's `x-cairn-since` phase.
//! 3. The runtime dispatch path is wired end-to-end (`wiring::*_WIRED`).
//! 4. The local config opted into the feature (`config.semantic_search`, etc.).
//! 5. The wired store advertises the structural backing
//!    (`store_ok(fts)`, `store_ok(vector)`).
//!
//! When no store is wired (CLI `status` does not open `SQLite`), `store_ok`
//! returns `true` so the bound-vault structural backstop drives the decision —
//! every v0.1 bound vault has the FTS5 virtual table. The `Sdk::new()`
//! (no-store-no-vault) path short-circuits at rule 1 and returns `Vec::new()`.

pub mod remediation;
pub mod wiring;

pub use remediation::{REMEDIATION, remediation_for};

use crate::config::CapabilitySet;
use crate::generated::common::Capabilities;

/// Contract-version phase the runtime is operating at. Pins which capabilities
/// can ever appear in `status.capabilities` regardless of runtime wiring —
/// brief §8.0 example pins `forget.session` to v0.2+, `forget.scope` to v0.3+.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// v0.1 — minimum substrate.
    V0_1,
    /// v0.2 — adds `forget.session`, `aggregate` extension.
    V0_2,
    /// v0.3 — adds `forget.scope`, `federation` + `sessiontree` extensions.
    V0_3,
}

/// Snapshot of a wired `MemoryStore`'s structural capabilities, projected to
/// the dimensions `advertise()` cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCaps {
    /// Full-text-search index is queryable.
    pub fts: bool,
    /// Vector / ANN index is queryable.
    pub vector: bool,
}

/// Inputs for the per-capability decision rules in `advertise()`.
// Four boolean fields (vault_bound, model_present, embedding_provider_ready,
// llm_configured) represent orthogonal binary-state runtime probes. Each bool
// corresponds to one distinct environmental check (filesystem, provider,
// LLM config); no enum captures the cross-product cleanly without adding N²
// variants. The struct_excessive_bools lint is suppressed for the same reason
// it is suppressed on CapabilitySet in config/mod.rs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct CapabilityGates {
    /// Config-derived feature flags (already accounts for `local_embeddings`,
    /// `policy_trace`, etc.).
    pub config: CapabilitySet,
    /// Wired `MemoryStore`'s capabilities, when one is in the loop. `None`
    /// means the surface (e.g., the CLI `status` path) chose not to open a
    /// store; structural backing falls back to the vault-bound signal.
    pub store: Option<StoreCaps>,
    /// True when `<vault>/.cairn/vault.id` is present and parses (CLI's
    /// `probe_vault_binding`) or when the surface has a wired store
    /// (`Sdk::with_store`, `CairnMcpHandler::with_store`).
    pub vault_bound: bool,
    /// True when the configured embedding model is materialized on disk
    /// (CLI's `ModelCache::is_present`) or when the wired store advertises
    /// `vector: true`.
    ///
    /// For surfaces that populate [`Self::embedding_provider_ready`], this
    /// field is redundant but kept for backward compat on callers that only
    /// read it for the local-provider path. `advertise()` now gates semantic
    /// and hybrid on `embedding_provider_ready` exclusively.
    pub model_present: bool,
    /// True when the configured embedding *provider* is ready to produce
    /// vectors end-to-end.
    ///
    /// Subsumes `model_present` for local providers: for a `default_provider =
    /// local` config, this is equivalent to `model_present` (the model file
    /// must be on disk). For cloud providers (`default_provider = openai`),
    /// `model_present` is irrelevant — readiness requires the `openai` Cargo
    /// feature to be compiled in AND `OPENAI_API_KEY` to be set in the
    /// environment.
    ///
    /// When `false`, `semantic` and `hybrid` are NOT advertised even if the
    /// wired store has a vector index and `model_present = true`. This
    /// prevents a config with `default_provider = openai` + a stale local
    /// model file from advertising semantic/hybrid and then failing at the
    /// `OpenAI` feature gate or embedder init.
    ///
    /// ## Per-surface population
    ///
    /// - **CLI** (`cairn-cli/src/verbs/status.rs`): for `provider == local`,
    ///   `model_present`; for `provider == openai`, `cfg!(feature = "openai")
    ///   && env::var("OPENAI_API_KEY").is_ok()`.
    /// - **SDK** (`cairn-sdk/src/transport.rs`): `store_caps.vector` (the store
    ///   advertises vector support iff a compatible provider was configured at
    ///   index-time). SDK consumers that construct with a custom embedder must
    ///   set this field manually — the SDK cannot inspect the env.
    /// - **MCP** (`cairn-mcp/src/handler.rs`): mirrors SDK — `store_caps.vector`.
    pub embedding_provider_ready: bool,
    /// True when an `LLMProvider` is configured. P0 default is `false`;
    /// reserved for future `cairn.mcp.v1.llm.*` capabilities.
    pub llm_configured: bool,
    /// Contract-version phase the runtime is operating at.
    pub contract_phase: Phase,
}

impl CapabilityGates {
    /// `true` when either no store is wired (use the structural backstop) or
    /// the wired store advertises `field`. Used internally by `advertise()`.
    fn store_ok(&self, field: fn(&StoreCaps) -> bool) -> bool {
        self.store.as_ref().is_none_or(field)
    }
}

/// The decision table — single source of truth for capability advertisement.
///
/// Returns the wire-stable order: search → `policy_trace` → forget → retrieve
/// → replay. `vault_bound: false` short-circuits to `Vec::new()`.
#[must_use]
pub fn advertise(gates: &CapabilityGates) -> Vec<Capabilities> {
    if !gates.vault_bound {
        return Vec::new();
    }

    let phase = gates.contract_phase;
    let cfg = &gates.config;
    let mut out = Vec::with_capacity(9);

    // ── search ────────────────────────────────────────────────────────────
    if cfg.keyword_search && gates.store_ok(|s| s.fts) {
        out.push(Capabilities::CairnMcpV1SearchKeyword);
    }
    // Gate semantic and hybrid on `embedding_provider_ready` rather than
    // the legacy `model_present`. For local providers the two are equivalent;
    // for cloud providers `embedding_provider_ready` additionally requires the
    // feature flag + API key, preventing over-advertising when a stale local
    // model file happens to be on disk but the configured provider is OpenAI.
    if cfg.semantic_search && gates.embedding_provider_ready && gates.store_ok(|s| s.vector) {
        out.push(Capabilities::CairnMcpV1SearchSemantic);
    }
    if cfg.hybrid_search
        && gates.embedding_provider_ready
        && gates.store_ok(|s| s.fts)
        && gates.store_ok(|s| s.vector)
    {
        out.push(Capabilities::CairnMcpV1SearchHybrid);
    }

    // ── policy_trace ──────────────────────────────────────────────────────
    if cfg.policy_trace {
        out.push(Capabilities::CairnMcpV1PolicyTrace);
    }

    // ── forget (capability surfaces; runtime wiring still all-false) ──────
    if phase >= Phase::V0_1 && wiring::FORGET_RECORD_WIRED {
        out.push(Capabilities::CairnMcpV1ForgetRecord);
    }
    if phase >= Phase::V0_2 && wiring::FORGET_SESSION_WIRED {
        out.push(Capabilities::CairnMcpV1ForgetSession);
    }
    if phase >= Phase::V0_3 && wiring::FORGET_SCOPE_WIRED {
        out.push(Capabilities::CairnMcpV1ForgetScope);
    }

    // ── retrieve (all v0.1 per brief §8.0.a; held behind wiring flags) ────
    if wiring::RETRIEVE_RECORD_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveRecord);
    }
    if wiring::RETRIEVE_SESSION_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveSession);
    }
    if wiring::RETRIEVE_TURN_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveTurn);
    }
    if wiring::RETRIEVE_TOOL_CALL_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveToolCall);
    }
    if wiring::RETRIEVE_FOLDER_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveFolder);
    }
    if wiring::RETRIEVE_SCOPE_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveScope);
    }
    if wiring::RETRIEVE_PROFILE_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveProfile);
    }

    // ── replay (held back per brief §15 fail-closed) ─────────────────────
    if wiring::REPLAY_SEQUENCE_WIRED {
        out.push(Capabilities::CairnMcpV1ReplaySequence);
    }
    if wiring::REPLAY_CHALLENGE_WIRED {
        out.push(Capabilities::CairnMcpV1ReplayChallenge);
    }

    out
}

#[cfg(test)]
mod tests;
