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

/// Errors raised by `EntityResolver::resolve` (added in Task 6).
///
/// `LlmError::NotConfigured` and `LlmError::CapabilityMissing` are
/// silently mapped to `Resolution::New` inside `resolve()` per the
/// P0 offline-graceful contract; only non-skippable LLM failures
/// surface as [`EntityResolutionError::Llm`].
// Task 5/Task 6 wire this into the resolver; suppress dead_code until then.
#[allow(dead_code)]
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
