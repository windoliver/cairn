//! Three-tier entity resolution for the bitemporal knowledge graph
//! (issue #187, brief §5.2 + §4 `LLMProvider` contract).
//!
//! Pipeline stage between Extract and Store: pure (Tier 1+2) and
//! async (Tier 3) functions that map a candidate entity name plus
//! a caller-provided slice of in-scope existing nodes to a
//! [`Resolution`]. No I/O; no store calls; no harness assumptions.

mod llm;
mod minhash;
mod normalize;

#[cfg(test)]
mod proptests;

/// Maximum number of `MinHash` permutations the resolver will allocate.
/// Signature arrays are sized to this constant; only the first
/// `num_permutations` slots carry meaning at runtime.
pub const MAX_NUM_PERMUTATIONS: usize = 128;

/// Default seed for permutation derivation. Fixed across processes
/// so repeated runs over the same vault produce identical signatures
/// (required for snapshot tests and reproducible merge decisions).
pub const DEFAULT_HASH_SEED: u64 = 0x_CA12_F1A6_5EED_BEEF;

/// Static configuration for [`EntityResolver`]. Defaults match the
/// thresholds pinned in issue #187.
#[derive(Debug, Clone, Copy)]
pub struct ResolverConfig {
    /// Jaccard threshold above which Tier 2 declares a fuzzy merge. `0.85` per issue #187.
    pub fuzzy_threshold: f32,
    /// Lower bound of the Tier-3 LLM trigger band. Tier 3 fires only
    /// when the top in-band candidate has Jaccard ∈ `[llm_low_band, fuzzy_threshold)`.
    pub llm_low_band: f32,
    /// Minimum LLM-reported confidence required to accept a `same: true` verdict as a merge.
    pub llm_min_confidence: f32,
    /// Number of `MinHash` permutations actually used. Must be `> 0` and `<= MAX_NUM_PERMUTATIONS`.
    pub num_permutations: usize,
    /// Seed used to derive per-permutation hash seeds at construction time.
    pub hash_seed: u64,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            fuzzy_threshold: 0.85,
            llm_low_band: 0.5,
            llm_min_confidence: 0.7,
            num_permutations: 128,
            hash_seed: DEFAULT_HASH_SEED,
        }
    }
}

impl ResolverConfig {
    /// Validate the configuration. Called by `EntityResolver::new` (added in Task 6).
    ///
    /// # Errors
    /// Returns the matching [`ResolverConfigError`] variant for any
    /// invariant violation.
    pub fn validate(&self) -> Result<(), ResolverConfigError> {
        if !(0.0..=1.0).contains(&self.fuzzy_threshold) {
            return Err(ResolverConfigError::FuzzyThresholdOutOfRange {
                got: self.fuzzy_threshold,
            });
        }
        if !(0.0..=1.0).contains(&self.llm_min_confidence) {
            return Err(ResolverConfigError::LlmMinConfidenceOutOfRange {
                got: self.llm_min_confidence,
            });
        }
        if self.llm_low_band >= self.fuzzy_threshold {
            return Err(ResolverConfigError::LlmBandInverted {
                low: self.llm_low_band,
                high: self.fuzzy_threshold,
            });
        }
        if self.num_permutations == 0 || self.num_permutations > MAX_NUM_PERMUTATIONS {
            return Err(ResolverConfigError::NumPermutationsOutOfRange {
                got: self.num_permutations,
            });
        }
        Ok(())
    }
}

use crate::contract::llm_provider::LlmError;
use crate::domain::graph::EntityId;

/// Resolution outcome returned by [`EntityResolver::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resolution {
    /// The candidate corresponds to an existing entity. Caller MUST
    /// reuse this `EntityId` rather than create a new node.
    Merge(EntityId),
    /// No tier produced a merge. Caller MUST allocate a fresh
    /// `EntityId` and persist a new node.
    New,
    /// Tier 2 found two or more existing entities with Jaccard
    /// at or above `fuzzy_threshold`. Caller decides:
    /// create a new node + flag for `lint`, invoke LLM disambiguation
    /// across the set, or surface to the user.
    Ambiguous(Vec<EntityId>),
}

use std::sync::Arc;

use crate::contract::llm_provider::LLMProvider;
use crate::domain::graph::EntityNode;

use self::llm::llm_dedup;
use self::minhash::{FuzzyOutcome, fuzzy_match, shingles, signature};
use self::normalize::{exact_match, normalize};

/// Three-tier entity resolver. Pure pipeline stage in `cairn-core`:
/// no I/O, no store calls. The caller pre-fetches in-scope candidate
/// nodes and supplies them as `existing`.
pub struct EntityResolver {
    config: ResolverConfig,
    seeds: Vec<u64>,
    llm: Option<Arc<dyn LLMProvider>>,
}

impl EntityResolver {
    /// Construct a new resolver. Validates `config` up-front so that
    /// [`EntityResolver::resolve`] never errors on configuration.
    ///
    /// `llm = None` disables Tier 3 entirely (P0 offline path).
    ///
    /// # Errors
    /// Returns [`ResolverConfigError`] if `config.validate()` rejects.
    pub fn new(
        config: ResolverConfig,
        llm: Option<Arc<dyn LLMProvider>>,
    ) -> Result<Self, ResolverConfigError> {
        config.validate()?;
        let seeds = derive_seeds(config.hash_seed, config.num_permutations);
        Ok(Self { config, seeds, llm })
    }

    /// Resolve `candidate_name` against the supplied `existing`
    /// entities. See [`Resolution`] for outcome semantics.
    ///
    /// # Errors
    /// Returns [`EntityResolutionError::Llm`] when Tier 3 surfaces a
    /// non-skippable `LlmError`. `LlmError::NotConfigured` and
    /// `LlmError::CapabilityMissing` are silently mapped to
    /// `Resolution::New` per the offline-graceful contract.
    pub async fn resolve(
        &self,
        candidate_name: &str,
        existing: &[EntityNode],
    ) -> Result<Resolution, EntityResolutionError> {
        let norm = normalize(candidate_name);

        // Tier 1.
        if let Some(id) = exact_match(&norm, existing) {
            return Ok(Resolution::Merge(id.clone()));
        }

        // Tier 2.
        let cand_ranges = shingles(&norm);
        let cand_sig = signature(&norm, &cand_ranges, &self.seeds);
        let n = self.config.num_permutations;
        let (outcome, scored) = fuzzy_match(
            &cand_sig,
            existing,
            &self.seeds,
            self.config.fuzzy_threshold,
            n,
        );
        match outcome {
            FuzzyOutcome::One(id) => return Ok(Resolution::Merge(id)),
            FuzzyOutcome::Many(ids) => return Ok(Resolution::Ambiguous(ids)),
            FuzzyOutcome::None => {}
        }

        // Tier 3.
        let Some(top) = scored.first() else {
            return Ok(Resolution::New);
        };
        if top.jaccard < self.config.llm_low_band {
            return Ok(Resolution::New);
        }
        let Some(provider) = self.llm.as_ref() else {
            return Ok(Resolution::New);
        };
        llm_dedup(
            provider.as_ref(),
            candidate_name,
            top.node,
            self.config.llm_min_confidence,
        )
        .await
    }
}

/// Derive `n` permutation seeds from `seed` via splitmix64. Stable
/// across processes; required for reproducible signatures.
fn derive_seeds(seed: u64, n: usize) -> Vec<u64> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        })
        .collect()
}

/// Errors raised when [`ResolverConfig::validate`] rejects a configuration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolverConfigError {
    /// `fuzzy_threshold` was outside `[0.0, 1.0]`.
    #[error("fuzzy_threshold must be in [0.0, 1.0], got {got}")]
    FuzzyThresholdOutOfRange {
        /// Offending value.
        got: f32,
    },
    /// `llm_low_band` was not strictly less than `fuzzy_threshold`.
    #[error("llm_low_band ({low}) must be < fuzzy_threshold ({high})")]
    LlmBandInverted {
        /// Configured low band.
        low: f32,
        /// Configured fuzzy threshold.
        high: f32,
    },
    /// `llm_min_confidence` was outside `[0.0, 1.0]`.
    #[error("llm_min_confidence must be in [0.0, 1.0], got {got}")]
    LlmMinConfidenceOutOfRange {
        /// Offending value.
        got: f32,
    },
    /// `num_permutations` was 0 or above [`MAX_NUM_PERMUTATIONS`].
    #[error("num_permutations must be in 1..={max}, got {got}", max = MAX_NUM_PERMUTATIONS)]
    NumPermutationsOutOfRange {
        /// Offending value.
        got: usize,
    },
}

/// Errors raised by [`EntityResolver::resolve`].
///
/// `LlmError::NotConfigured` and `LlmError::CapabilityMissing` are
/// silently mapped to `Resolution::New` inside `resolve()` per the
/// P0 offline-graceful contract; only non-skippable LLM failures
/// surface as [`EntityResolutionError::Llm`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EntityResolutionError {
    /// Tier 3 LLM call returned a non-skippable error
    /// (transport / auth / parse / budget).
    #[error("llm tier-3 failed: {source}")]
    Llm {
        /// Underlying LLM error.
        #[source]
        source: LlmError,
    },

    /// Tier 3 returned a payload the resolver could not interpret
    /// even though no `LlmError` was raised. Defence-in-depth — when
    /// `LLMProvider::complete` honours the schema arg, this is unreachable.
    #[error("llm tier-3 returned malformed payload: {detail}")]
    LlmInvalidResponse {
        /// Reason the payload could not be interpreted.
        detail: String,
    },
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // exact bit-for-bit round-trip: literals assigned in Default, no arithmetic
    fn default_config_passes_validation() {
        let cfg = ResolverConfig::default();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.fuzzy_threshold, 0.85);
        assert_eq!(cfg.llm_low_band, 0.5);
        assert_eq!(cfg.llm_min_confidence, 0.7);
        assert_eq!(cfg.num_permutations, 128);
    }

    #[test]
    fn rejects_threshold_above_one() {
        let cfg = ResolverConfig {
            fuzzy_threshold: 1.1,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::FuzzyThresholdOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_threshold_below_zero() {
        let cfg = ResolverConfig {
            fuzzy_threshold: -0.1,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::FuzzyThresholdOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_inverted_band() {
        let cfg = ResolverConfig {
            llm_low_band: 0.9,
            fuzzy_threshold: 0.85,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmBandInverted { .. })
        ));
    }

    #[test]
    fn rejects_min_confidence_out_of_range() {
        let cfg = ResolverConfig {
            llm_min_confidence: 1.5,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmMinConfidenceOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_zero_permutations() {
        let cfg = ResolverConfig {
            num_permutations: 0,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::NumPermutationsOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_too_many_permutations() {
        let cfg = ResolverConfig {
            num_permutations: MAX_NUM_PERMUTATIONS + 1,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::NumPermutationsOutOfRange { .. })
        ));
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use crate::contract::llm_provider::LlmError;

    #[test]
    fn llm_error_display_includes_source() {
        let inner = LlmError::ProviderUnreachable {
            detail: "connect refused".into(),
        };
        let e = EntityResolutionError::Llm { source: inner };
        let s = e.to_string();
        assert!(s.contains("llm tier-3 failed"), "got: {s}");
    }

    #[test]
    fn invalid_response_display() {
        let e = EntityResolutionError::LlmInvalidResponse {
            detail: "missing field `same`".into(),
        };
        assert!(e.to_string().contains("malformed payload"));
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use crate::contract::llm_provider::{
        CompletionOutput, CompletionRequest, LLMProviderCapabilities,
    };
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::domain::graph::EntityId;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn caps() -> &'static LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }

    fn versions() -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    struct CannedJsonLlm(Value);

    #[async_trait]
    #[allow(clippy::unnecessary_literal_bound)] // Stub impl returns a literal; trait defines &str.
    impl LLMProvider for CannedJsonLlm {
        fn name(&self) -> &str {
            "canned"
        }
        fn capabilities(&self) -> &LLMProviderCapabilities {
            caps()
        }
        fn supported_contract_versions(&self) -> VersionRange {
            versions()
        }
        async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
            Ok(CompletionOutput::Json(self.0.clone()))
        }
    }

    fn node(id: &str, name_norm: &str) -> EntityNode {
        EntityNode {
            id: EntityId::from(id),
            name: name_norm.to_owned(),
            name_norm: name_norm.to_owned(),
            summary: None,
            created_at: 0,
            embedding_id: None,
        }
    }

    #[tokio::test]
    async fn empty_existing_returns_new() {
        let r = EntityResolver::new(ResolverConfig::default(), None)
            .expect("invariant: default config validates");
        let res = r
            .resolve("AuthService", &[])
            .await
            .expect("invariant: resolve with empty existing never errors");
        assert!(matches!(res, Resolution::New));
    }

    #[tokio::test]
    async fn tier1_exact_match_preempts_tier2() {
        let existing = vec![node("01HZE7JV5N0000000000000001", "authservice")];
        let r = EntityResolver::new(ResolverConfig::default(), None)
            .expect("invariant: default config validates");
        let res = r
            .resolve("AuthService", &existing)
            .await
            .expect("invariant: tier-1 exact match never errors");
        assert!(
            matches!(res, Resolution::Merge(id) if id.as_str() == "01HZE7JV5N0000000000000001")
        );
    }

    #[tokio::test]
    async fn no_match_returns_new_without_llm() {
        let existing = vec![node("01HZE7JV5N0000000000000001", "billing service")];
        let r = EntityResolver::new(ResolverConfig::default(), None)
            .expect("invariant: default config validates");
        let res = r
            .resolve("payments gateway xyz", &existing)
            .await
            .expect("invariant: low-similarity resolve never errors");
        assert!(matches!(res, Resolution::New));
    }

    #[tokio::test]
    async fn tier3_skipped_when_llm_none_yields_new_or_merge() {
        // Construct an existing node whose Jaccard against the candidate
        // could be in the LLM band (depends on hash output); without an
        // LLM provider, the only valid outcomes are New or Merge (Tier 2).
        let existing = vec![node("01HZE7JV5N0000000000000001", "auth service backend")];
        let r = EntityResolver::new(ResolverConfig::default(), None)
            .expect("invariant: default config validates");
        let res = r
            .resolve("auth service frontend", &existing)
            .await
            .expect("invariant: resolve without LLM never errors");
        assert!(matches!(res, Resolution::New | Resolution::Merge(_)));
    }

    #[tokio::test]
    async fn tier3_merges_when_llm_says_same_above_threshold() {
        // Tune config so Tier 2 cannot match (threshold near 1.0) and
        // Tier 3 always fires (low_band = 0.0). The candidate need only
        // be similar enough to produce some non-zero Jaccard, which 3-gram
        // shingles over related strings reliably do.
        let cfg = ResolverConfig {
            fuzzy_threshold: 0.999,
            llm_low_band: 0.0,
            llm_min_confidence: 0.7,
            ..ResolverConfig::default()
        };
        let existing = vec![node("01HZE7JV5N0000000000000001", "auth service")];
        let llm = Arc::new(CannedJsonLlm(json!({
            "same": true,
            "confidence": 0.95,
            "reasoning": "stub"
        })));
        let r = EntityResolver::new(cfg, Some(llm)).expect("invariant: tuned config validates");
        let res = r
            .resolve("authentication service", &existing)
            .await
            .expect("invariant: canned LLM never errors");
        assert!(
            matches!(res, Resolution::Merge(ref id) if id.as_str() == "01HZE7JV5N0000000000000001"),
            "expected Merge, got {res:?}"
        );
    }

    #[tokio::test]
    async fn tier3_returns_new_when_llm_says_different() {
        // Same tuning as above; canned response now declines.
        let cfg = ResolverConfig {
            fuzzy_threshold: 0.999,
            llm_low_band: 0.0,
            llm_min_confidence: 0.7,
            ..ResolverConfig::default()
        };
        let existing = vec![node("01HZE7JV5N0000000000000001", "auth service")];
        let llm = Arc::new(CannedJsonLlm(json!({
            "same": false,
            "confidence": 0.99,
            "reasoning": "stub"
        })));
        let r = EntityResolver::new(cfg, Some(llm)).expect("invariant: tuned config validates");
        let res = r
            .resolve("authentication service", &existing)
            .await
            .expect("invariant: canned LLM never errors");
        assert!(matches!(res, Resolution::New), "expected New, got {res:?}");
    }

    #[tokio::test]
    async fn invalid_config_rejected_at_construction() {
        let bad = ResolverConfig {
            fuzzy_threshold: 1.5,
            ..ResolverConfig::default()
        };
        let r = EntityResolver::new(bad, None);
        assert!(matches!(
            r,
            Err(ResolverConfigError::FuzzyThresholdOutOfRange { .. })
        ));
    }
}
