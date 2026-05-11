//! Conformance fixtures shared by `crates/cairn-mcp/tests/mcp_conformance.rs`.
//!
//! Each fixture is a `(request, response)` envelope pair embedded from
//! `fixtures/v0/mcp/conformance/` at compile time via `include_dir!`. Each
//! verb-group directory carries a `_meta.json` that names per-case kind and
//! config overrides; the loader pairs files and meta entries strictly, panicking
//! on orphans or missing entries (brief §8.0.a fail-closed invariant projected
//! into the test infra layer).

use serde::Deserialize;

/// One fixture entry, ready for replay.
#[derive(Debug, Clone)]
pub struct ConformanceCase {
    /// `"<verb_dir>/<case_id>"` — e.g., `"search/err_semantic_disabled"`.
    pub id: String,
    /// Cairn verb name as it appears in the envelope, e.g., `"search"`. For
    /// cross-verb directories (`_envelope`, `_extension`) this is the directory
    /// name (callers reading `verb` should treat values starting with `_` as
    /// synthetic groupings, not real verbs).
    pub verb: String,
    /// What the runner should expect when dispatching the request.
    pub kind: CaseKind,
    /// Per-case capability gates fed into `build_handler_for`.
    pub config: ConfigOverrides,
    /// Canonical envelope per brief §8.0.b.
    pub request: serde_json::Value,
    /// Expected canonical envelope after replay.
    pub response: serde_json::Value,
}

/// What outcome a case asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// `status = "committed"` (or `"aborted"` for valid-but-rejected
    /// non-capability cases — none expected at v0.1).
    Ok,
    /// `status = "rejected"`, `error.code = "InvalidArgs"` or
    /// `"InvalidFilter"` or `"UnknownVerb"`.
    InvalidArgs,
    /// `status = "rejected"`, `error.code = "CapabilityUnavailable"`,
    /// `error.data.capability` matches a known capability id.
    CapabilityRejected,
    /// Same as `CapabilityRejected` but for verbs from an extension namespace
    /// that the runtime does not advertise (brief §8.0.a, extensions table).
    ExtensionRejected,
}

/// Per-case capability gates. Mirrors the subset of
/// `cairn_core::status::CapabilityGates` and `wiring::*_WIRED` that conformance
/// cases need to switch on. Typed booleans, not strings — drift between this
/// struct and `cairn-core::status::advertise` is caught by
/// `config_overrides_match_advertised_capabilities` in the runner self-tests.
// Six booleans are intentional here: each maps 1-to-1 to a distinct
// capability gate in `cairn-core::status::advertise`. Refactoring into a
// state machine would obscure that direct correspondence.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ConfigOverrides {
    /// Whether keyword search (`fts5`) is advertised by the test handler.
    pub keyword_search: bool,
    /// Whether semantic (vector) search is advertised; requires an embedding provider.
    pub semantic_search: bool,
    /// Whether hybrid search (keyword + semantic) is advertised.
    pub hybrid_search: bool,
    /// Whether policy-trace capture is advertised.
    pub policy_trace: bool,
    /// Whether the aggregate extension namespace is advertised.
    pub aggregate_extension_enabled: bool,
    /// Whether the admin extension namespace is advertised.
    pub admin_extension_enabled: bool,
}

impl Default for ConfigOverrides {
    /// P0 baseline — keyword on, semantic + hybrid require an embedding
    /// provider to be ready (off by default in tests), extensions disabled.
    fn default() -> Self {
        Self {
            keyword_search: true,
            semantic_search: false,
            hybrid_search: false,
            policy_trace: false,
            aggregate_extension_enabled: false,
            admin_extension_enabled: false,
        }
    }
}

impl ConfigOverrides {
    /// Convenience: every search mode on (requires the test handler to be
    /// constructed with `embedding_provider_ready = true`).
    #[must_use]
    pub fn search_all_on() -> Self {
        Self {
            keyword_search: true,
            semantic_search: true,
            hybrid_search: true,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_kind_deserializes_snake_case() {
        let v: CaseKind = serde_json::from_str("\"ok\"").unwrap();
        assert_eq!(v, CaseKind::Ok);
        let v: CaseKind = serde_json::from_str("\"invalid_args\"").unwrap();
        assert_eq!(v, CaseKind::InvalidArgs);
        let v: CaseKind = serde_json::from_str("\"capability_rejected\"").unwrap();
        assert_eq!(v, CaseKind::CapabilityRejected);
        let v: CaseKind = serde_json::from_str("\"extension_rejected\"").unwrap();
        assert_eq!(v, CaseKind::ExtensionRejected);
    }

    #[test]
    fn config_overrides_default_is_p0_baseline() {
        let c = ConfigOverrides::default();
        assert!(c.keyword_search);
        assert!(!c.semantic_search);
        assert!(!c.hybrid_search);
        assert!(!c.aggregate_extension_enabled);
    }

    #[test]
    fn config_overrides_deserialize_partial() {
        let v: ConfigOverrides = serde_json::from_str(r#"{"semantic_search": true}"#).unwrap();
        assert!(v.semantic_search);
        assert!(v.keyword_search); // serde(default) applied → default = true
    }
}
