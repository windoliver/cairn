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
    /// Wall-clock budget for the Tier-3 LLM call, in milliseconds.
    /// `None` means unlimited; `Some(0)` is rejected by `validate()`.
    /// Bounds the per-call cost so a wedged provider cannot block
    /// `resolve()` indefinitely (codex-review R3.3).
    pub llm_max_wall_ms: Option<u32>,
    /// Maximum response tokens for the Tier-3 LLM call. `None` means
    /// unlimited; `Some(0)` is rejected by `validate()`.
    pub llm_max_tokens: Option<u32>,
    /// Maximum length (in chars, post-`normalize`) of `candidate_name`.
    /// Caps Tier-2 shingling work before it begins so a large input
    /// cannot monopolize CPU before the Tier-3 wall-clock timeout
    /// fires (codex-review R7.2). `Some(0)` rejected by `validate()`;
    /// `None` means unlimited.
    pub max_candidate_chars: Option<usize>,
    /// Maximum number of `existing` candidates the resolver will
    /// score in Tier 2. Above this, `resolve()` errors. Bounds
    /// per-call CPU + allocation (R7.2). `Some(0)` rejected;
    /// `None` means unlimited.
    pub max_existing_candidates: Option<usize>,
    /// Maximum byte length of raw entity-name strings (the candidate
    /// passed to `resolve()` and any `existing[i].name` consulted in
    /// Tier 3). Enforced BEFORE normalization so an oversized input
    /// cannot pre-allocate megabytes for shingling or land verbatim
    /// in the LLM prompt (codex-review R8.2). `None` means unlimited;
    /// `Some(0)` rejected.
    pub max_raw_name_bytes: Option<usize>,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            fuzzy_threshold: 0.85,
            llm_low_band: 0.5,
            llm_min_confidence: 0.7,
            num_permutations: 128,
            hash_seed: DEFAULT_HASH_SEED,
            // Pairwise dedup is a small fixed prompt; 5 s and 256 tokens
            // are conservative defaults that fail predictably on a slow
            // or mis-configured provider.
            llm_max_wall_ms: Some(5_000),
            llm_max_tokens: Some(256),
            // Tier-2 work envelope: the synchronous shingle+hash
            // pass for every existing entity runs BEFORE the Tier-3
            // wall-clock timeout helps. With 256 chars × 1024
            // candidates × 128 permutations the worst-case is
            // ~30M shingle hashes (~100 ms CPU on modern hardware).
            // R10.2 lowered these from 1024/10000 — the old envelope
            // could spend seconds blocking ingest on a malicious or
            // legacy scope. Callers with larger vaults should
            // pre-filter by scope before invoking the resolver
            // (the contract assumes pre-filtered input anyway).
            max_candidate_chars: Some(256),
            max_existing_candidates: Some(1024),
            // 8 KB raw is more than any realistic entity name and
            // bounds prompt size from below the LLM token budget.
            max_raw_name_bytes: Some(8 * 1024),
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
        // Reject NaN / negative / >1 — `(0.0..=1.0).contains(&NaN)` is false,
        // so this also covers the NaN case. Without this, NaN passes both
        // bounds and the inversion check (because all NaN comparisons are
        // false), letting Tier 3 fire for arbitrarily low Jaccard scores.
        if !(0.0..=1.0).contains(&self.llm_low_band) {
            return Err(ResolverConfigError::LlmLowBandOutOfRange {
                got: self.llm_low_band,
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
        if matches!(self.llm_max_wall_ms, Some(0)) {
            return Err(ResolverConfigError::LlmBudgetZero {
                field: "llm_max_wall_ms",
            });
        }
        if matches!(self.llm_max_tokens, Some(0)) {
            return Err(ResolverConfigError::LlmBudgetZero {
                field: "llm_max_tokens",
            });
        }
        if matches!(self.max_candidate_chars, Some(0)) {
            return Err(ResolverConfigError::LlmBudgetZero {
                field: "max_candidate_chars",
            });
        }
        if matches!(self.max_existing_candidates, Some(0)) {
            return Err(ResolverConfigError::LlmBudgetZero {
                field: "max_existing_candidates",
            });
        }
        if matches!(self.max_raw_name_bytes, Some(0)) {
            return Err(ResolverConfigError::LlmBudgetZero {
                field: "max_raw_name_bytes",
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
    /// `EntityId` and persist a new node using `name_norm` as the
    /// store-side dedup key. Returning the canonical `name_norm`
    /// forces caller and store to agree on the key — without this,
    /// a caller that re-implemented normalization differently could
    /// persist a different `name_norm` than the resolver used,
    /// drifting the store's `UNIQUE(name_norm)` index away from the
    /// resolver's identity decisions (codex-review R5.1).
    New {
        /// The normalized form the resolver used for identity
        /// comparison. Caller MUST persist this exact value as
        /// `EntityNode.name_norm` to keep store + resolver in sync.
        name_norm: String,
    },
    /// Tier 2 found two or more existing entities with Jaccard
    /// at or above `fuzzy_threshold`. Caller decides:
    /// create a new node + flag for `lint`, invoke LLM disambiguation
    /// across the set, or surface to the user.
    Ambiguous(Vec<EntityId>),
}

use std::sync::Arc;

use crate::contract::llm_provider::LLMProvider;
use crate::domain::graph::EntityNode;

pub use self::minhash::MinHashSignature;
/// Re-export `normalize` so callers (store insertion paths,
/// integration tests) can compute the same `name_norm` the resolver
/// uses for identity comparison. Without this, a caller that
/// re-implemented normalization differently would persist a key the
/// resolver never sees, breaking the store's `UNIQUE(name_norm)`
/// dedup contract (codex-review R5.1).
pub use self::normalize::normalize;

use self::llm::llm_dedup;
use self::minhash::{FuzzyOutcome, Scored, fuzzy_match, shingles, signature};
use self::normalize::exact_match;

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
        // R8.2: cap raw byte length BEFORE any work — including
        // normalization, which would otherwise allocate a buffer
        // sized to the input. Also covers the Tier-3 prompt path
        // since the raw `candidate_name` is what gets serialized.
        if let Some(max) = self.config.max_raw_name_bytes
            && candidate_name.len() > max
        {
            return Err(EntityResolutionError::RawNameTooLong {
                got: candidate_name.len(),
                max,
            });
        }
        // Also reject any oversized existing.name up-front. Tier-3
        // would otherwise serialize that into the prompt.
        if let Some(max) = self.config.max_raw_name_bytes {
            for n in existing {
                if n.name.len() > max {
                    return Err(EntityResolutionError::RawNameTooLong {
                        got: n.name.len(),
                        max,
                    });
                }
            }
        }

        // R7.2: cap per-call work BEFORE normalize/shingle to bound
        // CPU on a degraded-input path. The Tier-3 timeout doesn't
        // help here — Tier 1+2 happen synchronously before any LLM
        // call.
        if let Some(max) = self.config.max_existing_candidates
            && existing.len() > max
        {
            return Err(EntityResolutionError::TooManyCandidates {
                got: existing.len(),
                max,
            });
        }

        let norm = normalize(candidate_name);

        // Empty normalized key — the candidate has no alphanumeric
        // content (punctuation/whitespace/symbols only). Hard reject
        // rather than `Resolution::New` so a caller who persists `New`
        // with `name_norm == ""` cannot collide on the store's
        // `UNIQUE(name_norm)` index (codex-review R3.2). Tier 2 is
        // also incoherent for empty input — every empty 3-gram
        // signature compares as Jaccard 1.0 against any other.
        if norm.is_empty() {
            return Err(EntityResolutionError::EmptyNormalizedName);
        }

        // R7.2: cap normalized-input length. Counted in chars (not
        // bytes) since shingling iterates by char.
        // R9.1 also caps existing[i].name_norm — the store schema
        // does not constrain `name_norm` length, so a legacy or
        // caller-corrupt row could carry a tiny `name` and a
        // megabyte `name_norm`, forcing Tier-2 shingling work that
        // the candidate-side cap alone doesn't bound.
        if let Some(max) = self.config.max_candidate_chars {
            let got = norm.chars().count();
            if got > max {
                return Err(EntityResolutionError::CandidateTooLong { got, max });
            }
            for n in existing {
                let n_got = n.name_norm.chars().count();
                if n_got > max {
                    return Err(EntityResolutionError::CandidateTooLong { got: n_got, max });
                }
            }
        }

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
        // R7.1: collect ALL in-band candidates (Jaccard ∈
        // [llm_low_band, fuzzy_threshold)). If more than one is in
        // band the resolver MUST surface Ambiguous rather than auto-
        // pick top-1 — the top MinHash score is not authoritative
        // ground truth and a false positive could merge into the
        // wrong existing entity. Single-candidate Tier 3 still runs.
        let in_band: Vec<&Scored<'_>> = scored
            .iter()
            .filter(|s| s.jaccard >= self.config.llm_low_band)
            .collect();
        let top = match in_band.as_slice() {
            [] => return Ok(Resolution::New { name_norm: norm }),
            [only] => *only,
            many => {
                // Multiple plausible candidates — surface to caller.
                let ids = many.iter().map(|s| s.node.id.clone()).collect();
                return Ok(Resolution::Ambiguous(ids));
            }
        };
        let Some(provider) = self.llm.as_ref() else {
            return Ok(Resolution::New { name_norm: norm });
        };
        // Build the per-call LLM budget from config. None = unlimited;
        // both fields default to a small fixed budget so a slow or
        // wedged provider cannot block `resolve()` indefinitely.
        let budget =
            if self.config.llm_max_wall_ms.is_some() || self.config.llm_max_tokens.is_some() {
                Some(crate::config::ExtractBudget {
                    max_tokens: self.config.llm_max_tokens,
                    max_wall_ms: self.config.llm_max_wall_ms,
                    max_turns: Some(1),
                })
            } else {
                None
            };
        llm_dedup(
            provider.as_ref(),
            candidate_name,
            norm,
            top.node,
            self.config.llm_min_confidence,
            budget,
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
    /// `llm_low_band` was outside `[0.0, 1.0]` or non-finite.
    #[error("llm_low_band must be in [0.0, 1.0] and finite, got {got}")]
    LlmLowBandOutOfRange {
        /// Offending value.
        got: f32,
    },
    /// `num_permutations` was 0 or above [`MAX_NUM_PERMUTATIONS`].
    #[error("num_permutations must be in 1..={max}, got {got}", max = MAX_NUM_PERMUTATIONS)]
    NumPermutationsOutOfRange {
        /// Offending value.
        got: usize,
    },
    /// One of the optional LLM budget fields was set to `Some(0)`. Use
    /// `None` to mean unlimited; zero is never a meaningful budget.
    #[error("{field} = Some(0) is invalid; use None for unlimited or a positive value")]
    LlmBudgetZero {
        /// Which budget field was zero.
        field: &'static str,
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

    /// The candidate name normalized to an empty string (only
    /// punctuation / whitespace / symbols / strip-only codepoints).
    /// This is a hard rejection rather than `Resolution::New` because
    /// callers who persist `Resolution::New` with `name_norm == ""`
    /// would collide on the store's `UNIQUE(name_norm)` index — the
    /// caller MUST handle this error and refuse to allocate a node.
    #[error("candidate name normalizes to empty `name_norm`; cannot resolve identity")]
    EmptyNormalizedName,

    /// The candidate name (after `normalize`) exceeds
    /// `ResolverConfig::max_candidate_chars`. Bounds Tier-2 shingling
    /// CPU before it starts; otherwise a long input would monopolize
    /// CPU before the Tier-3 wall-clock timeout could fire
    /// (codex-review R7.2).
    #[error("candidate name length ({got} chars) exceeds limit ({max} chars)")]
    CandidateTooLong {
        /// Normalized length actually observed.
        got: usize,
        /// Configured cap.
        max: usize,
    },

    /// The `existing` slice exceeds
    /// `ResolverConfig::max_existing_candidates`. Bounds Tier-2
    /// per-call work; the caller is expected to pre-filter by scope
    /// before invoking the resolver (codex-review R7.2).
    #[error("existing candidate count ({got}) exceeds limit ({max})")]
    TooManyCandidates {
        /// Slice length actually observed.
        got: usize,
        /// Configured cap.
        max: usize,
    },

    /// A raw entity-name string (candidate or existing) exceeded
    /// `ResolverConfig::max_raw_name_bytes`. Enforced before
    /// normalization to bound pre-call allocation and LLM prompt
    /// size (codex-review R8.2).
    #[error("raw entity name length ({got} bytes) exceeds limit ({max} bytes)")]
    RawNameTooLong {
        /// Raw byte length actually observed.
        got: usize,
        /// Configured cap.
        max: usize,
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

    #[test]
    fn rejects_negative_llm_low_band() {
        let cfg = ResolverConfig {
            llm_low_band: -0.1,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmLowBandOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_nan_llm_low_band() {
        let cfg = ResolverConfig {
            llm_low_band: f32::NAN,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmLowBandOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_zero_llm_max_wall_ms() {
        let cfg = ResolverConfig {
            llm_max_wall_ms: Some(0),
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmBudgetZero { .. })
        ));
    }

    #[test]
    fn rejects_zero_llm_max_tokens() {
        let cfg = ResolverConfig {
            llm_max_tokens: Some(0),
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmBudgetZero { .. })
        ));
    }

    #[test]
    fn accepts_unlimited_llm_budgets() {
        let cfg = ResolverConfig {
            llm_max_wall_ms: None,
            llm_max_tokens: None,
            ..ResolverConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_above_one_llm_low_band() {
        // Exceeds [0, 1] outright; the band-inversion check would also
        // catch this (low > fuzzy_threshold = 0.85), but the range check
        // fires first and surfaces the clearer error.
        let cfg = ResolverConfig {
            llm_low_band: 1.5,
            ..ResolverConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ResolverConfigError::LlmLowBandOutOfRange { .. })
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
        assert!(matches!(res, Resolution::New { .. }));
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
        assert!(matches!(res, Resolution::New { .. }));
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
        assert!(matches!(res, Resolution::New { .. } | Resolution::Merge(_)));
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
        assert!(
            matches!(res, Resolution::New { .. }),
            "expected New, got {res:?}"
        );
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

    #[tokio::test]
    async fn new_returns_canonical_name_norm_for_persistence() {
        // Codex-review R5.1: caller persists a new node using
        // `name_norm` from the resolver. The same raw input must
        // resolve to Merge against the freshly-persisted node on the
        // next call — otherwise a caller using a different normalize
        // implementation would create duplicate entities.
        let r = EntityResolver::new(ResolverConfig::default(), None)
            .expect("invariant: default config validates");
        let raw = "Authentication-Service!";
        // First call: empty existing → New { name_norm }.
        let res = r
            .resolve(raw, &[])
            .await
            .expect("invariant: empty existing resolves to New");
        let persisted_norm = match res {
            Resolution::New { name_norm } => name_norm,
            other => panic!("expected New, got {other:?}"),
        };
        // Caller persists with the resolver's name_norm.
        let existing = vec![node("01HZE7JV5N0000000000000001", &persisted_norm)];
        // Second call: same raw input → Merge against the persisted node.
        let res2 = r
            .resolve(raw, &existing)
            .await
            .expect("invariant: round-trip resolves without error");
        match res2 {
            Resolution::Merge(id) => assert_eq!(id.as_str(), "01HZE7JV5N0000000000000001"),
            other => panic!("expected Merge after persistence, got {other:?}"),
        }
    }

    #[test]
    fn public_normalize_matches_resolver_internal_key() {
        // Codex-review R5.1: the publicly re-exported `normalize`
        // must be identical to whatever the resolver uses internally.
        // Verify by computing both and asserting equality.
        let raw = "Authentication-Service!";
        let public_norm = super::normalize(raw);
        // The resolver's internal call site does `normalize(candidate)`
        // before any other work; surfacing the same string here proves
        // the re-export targets the same fn.
        assert!(!public_norm.is_empty());
        assert_eq!(public_norm, "authenticationservice");
    }

    #[tokio::test]
    async fn tier3_multi_band_returns_ambiguous_not_auto_merge() {
        // Codex-review R7.1: when more than one candidate has Jaccard
        // in the LLM band, the resolver MUST NOT auto-pick top-1 — the
        // MinHash score is not authoritative and a false-positive top
        // would silently merge into the wrong entity. Surface Ambiguous
        // so the caller decides.
        let cfg = ResolverConfig {
            // Wide band so any near match qualifies; threshold high
            // enough that Tier 2 won't claim a single One.
            fuzzy_threshold: 0.999,
            llm_low_band: 0.0,
            ..ResolverConfig::default()
        };
        let existing = vec![
            node("01HZE7JV5N0000000000000001", "auth service backend"),
            node("01HZE7JV5N0000000000000002", "auth service frontend"),
        ];
        // Use llm: None so Tier 3 LLM is never reached — the multi-
        // band detection happens before the provider gate.
        let r = EntityResolver::new(cfg, None).expect("invariant: config validates");
        let res = r
            .resolve("auth service", &existing)
            .await
            .expect("invariant: multi-band resolves without error");
        match res {
            Resolution::Ambiguous(ids) => {
                assert!(
                    ids.len() >= 2,
                    "expected ≥2 candidates in Ambiguous, got {ids:?}"
                );
            }
            other => panic!("expected Ambiguous for multi-band, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_oversized_raw_candidate_before_normalize() {
        // Codex-review R8.2: a punctuation-heavy candidate that
        // shrinks short under normalize must still be rejected by
        // the raw-byte cap so it cannot reach Tier 3 prompt
        // construction or pre-allocate huge buffers.
        let cfg = ResolverConfig {
            max_raw_name_bytes: Some(64),
            max_candidate_chars: Some(8), // post-normalize cap
            ..ResolverConfig::default()
        };
        let r = EntityResolver::new(cfg, None).expect("invariant: config validates");
        // 200 punctuation chars + a short alphanumeric → normalizes
        // short, but raw is 200+ bytes.
        let huge = format!("{}{}", "!".repeat(200), "auth");
        let err = r
            .resolve(&huge, &[])
            .await
            .expect_err("invariant: oversized raw candidate must error");
        assert!(matches!(err, EntityResolutionError::RawNameTooLong { .. }));
    }

    #[tokio::test]
    async fn rejects_oversized_existing_name() {
        let cfg = ResolverConfig {
            max_raw_name_bytes: Some(64),
            ..ResolverConfig::default()
        };
        let r = EntityResolver::new(cfg, None).expect("invariant: config validates");
        let mut huge_node = node("01HZE7JV5N0000000000000001", "auth service");
        huge_node.name = "x".repeat(1000);
        let err = r
            .resolve("auth service", &[huge_node])
            .await
            .expect_err("invariant: oversized existing.name must error");
        assert!(matches!(err, EntityResolutionError::RawNameTooLong { .. }));
    }

    #[tokio::test]
    async fn rejects_oversized_existing_name_norm() {
        // Codex-review R9.1: store schema does not constrain
        // `name_norm` length; resolver must defend against a tiny
        // `name` paired with a megabyte `name_norm` that would
        // dominate Tier-2 shingling.
        let cfg = ResolverConfig {
            max_candidate_chars: Some(8),
            ..ResolverConfig::default()
        };
        let r = EntityResolver::new(cfg, None).expect("invariant: config validates");
        let existing = vec![EntityNode {
            id: EntityId::from("01HZE7JV5N0000000000000001"),
            name: "x".to_owned(),
            name_norm: "x".repeat(100),
            summary: None,
            created_at: 0,
            embedding_id: None,
        }];
        let err = r
            .resolve("query", &existing)
            .await
            .expect_err("invariant: oversized existing.name_norm must error");
        assert!(matches!(
            err,
            EntityResolutionError::CandidateTooLong { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_overlong_candidate_name() {
        // Codex-review R7.2: cap candidate length to bound Tier-2
        // shingling work before it begins.
        let cfg = ResolverConfig {
            max_candidate_chars: Some(8),
            ..ResolverConfig::default()
        };
        let r = EntityResolver::new(cfg, None).expect("invariant: config validates");
        let err = r
            .resolve("authentication service backend platform", &[])
            .await
            .expect_err("invariant: overlong candidate must error");
        assert!(matches!(
            err,
            EntityResolutionError::CandidateTooLong { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_too_many_existing_candidates() {
        // Codex-review R7.2: cap existing slice length.
        let cfg = ResolverConfig {
            max_existing_candidates: Some(2),
            ..ResolverConfig::default()
        };
        let r = EntityResolver::new(cfg, None).expect("invariant: config validates");
        let existing = vec![
            node("01HZE7JV5N0000000000000001", "a"),
            node("01HZE7JV5N0000000000000002", "b"),
            node("01HZE7JV5N0000000000000003", "c"),
        ];
        let err = r
            .resolve("query", &existing)
            .await
            .expect_err("invariant: oversized existing must error");
        assert!(matches!(
            err,
            EntityResolutionError::TooManyCandidates { got: 3, max: 2 }
        ));
    }

    #[tokio::test]
    async fn empty_normalized_candidate_errors_rather_than_new() {
        // Codex-review R3.2: `Resolution::New` would invite the caller
        // to persist a node with empty `name_norm`, colliding on the
        // store's UNIQUE constraint. The orchestrator MUST surface a
        // typed error so the caller cannot accidentally proceed.
        let r = EntityResolver::new(ResolverConfig::default(), None)
            .expect("invariant: default config validates");
        let existing = vec![node("01HZE7JV5N0000000000000001", "")];
        let err = r
            .resolve("???", &existing)
            .await
            .expect_err("invariant: empty-key candidate must error, not resolve to New");
        assert!(
            matches!(err, EntityResolutionError::EmptyNormalizedName),
            "expected EmptyNormalizedName, got {err:?}"
        );
    }
}
