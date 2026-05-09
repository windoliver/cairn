//! Stub-body `HotMemoryAssembler`. Walks `HotMemoryConfig.recipe`,
//! calls a stub `load_step_body` returning `""` for every step, and
//! returns a fully validated `AssembleHotData`. Real source loading is
//! the missing-half of issue #193 — that PR replaces `load_step_body`
//! and changes nothing else.

use super::segments::{
    AssembleHotValidationError, MAX_SEGMENTS, build_segments, validate, validate_with_recipe,
};
use crate::config::HotMemoryConfig;
use crate::generated::verbs::assemble_hot::{AssembleHotData, HotRecipeStep};

/// Errors returned by [`assemble_hot`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleHotError {
    /// Segment construction or validation failed.
    #[error("segment construction: {0}")]
    Segments(#[from] AssembleHotValidationError),
    /// The assembled prefix exceeds the vault's configured `max_bytes`
    /// hot-memory budget.
    #[error("hot memory exceeded budget: {got} > {max} bytes")]
    BudgetExceeded {
        /// Actual prefix length.
        got: u64,
        /// Configured `HotMemoryConfig.max_bytes`.
        max: u64,
    },
    /// A recipe-step body loader returned an error. Real loaders (#193)
    /// surface I/O, parse, or partial-read failures here so degraded
    /// dependencies fail closed instead of silently emitting empty hot
    /// memory.
    #[error("load_step_body({step:?}): {reason}")]
    LoadFailed {
        /// The step that failed to load.
        step: HotRecipeStep,
        /// Loader-supplied reason string.
        reason: String,
    },
}

/// Run the hot-memory recipe and return a validated `AssembleHotData`.
pub fn assemble_hot(config: &HotMemoryConfig) -> Result<AssembleHotData, AssembleHotError> {
    assemble_hot_with_loader(config, load_step_body)
}

/// Variant of [`assemble_hot`] that accepts an explicit fallible loader.
/// Used by tests today; once #193 lands, the real `SQLite` + markdown
/// loader will be threaded in via the same hook so I/O / parse failures
/// surface as [`AssembleHotError::LoadFailed`].
pub fn assemble_hot_with_loader<F>(
    config: &HotMemoryConfig,
    mut loader: F,
) -> Result<AssembleHotData, AssembleHotError>
where
    F: FnMut(HotRecipeStep) -> Result<String, String>,
{
    validate_config_recipe_len(config)?;
    let recipe: Vec<HotRecipeStep> = config
        .recipe
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();
    let mut bodies: Vec<String> = Vec::with_capacity(recipe.len());
    for step in recipe.iter().copied() {
        let body = loader(step).map_err(|reason| AssembleHotError::LoadFailed { step, reason })?;
        bodies.push(body);
    }
    let bodies_refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies_refs)?;
    let bytes = prefix.len() as u64;
    let max = u64::from(config.max_bytes);
    if bytes > max {
        return Err(AssembleHotError::BudgetExceeded { got: bytes, max });
    }
    let data = AssembleHotData {
        bytes,
        prefix,
        segments: Some(segments),
    };
    // Run the same trust-boundary validator a deserializer would apply.
    // Catches contract-violating outputs (e.g. recipe.len() > MAX_SEGMENTS)
    // before they reach the wire.
    validate(&data)?;
    Ok(data)
}

/// Assemble hot memory from preloaded recipe bodies, trimming to the configured budget.
pub fn assemble_hot_from_bodies(
    config: &HotMemoryConfig,
    bodies: Vec<String>,
    budget_override: Option<u64>,
) -> Result<AssembleHotData, AssembleHotError> {
    validate_config_recipe_len(config)?;
    let recipe: Vec<HotRecipeStep> = config
        .recipe
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();
    let max = budget_override
        .unwrap_or_else(|| u64::from(config.max_bytes))
        .min(u64::from(config.max_bytes));
    let trimmed = super::loader::trim_bodies_to_budget(&recipe, bodies, max);
    let refs: Vec<&str> = trimmed.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &refs)?;
    let data = AssembleHotData {
        bytes: prefix.len() as u64,
        prefix,
        segments: Some(segments),
    };
    validate_with_recipe(&data, &recipe)?;
    Ok(data)
}

fn validate_config_recipe_len(config: &HotMemoryConfig) -> Result<(), AssembleHotError> {
    if config.recipe.len() > MAX_SEGMENTS {
        return Err(AssembleHotValidationError::TooManySegments {
            got: config.recipe.len(),
            max: MAX_SEGMENTS,
        }
        .into());
    }
    Ok(())
}

/// Load the body for one recipe step. Stub: always `Ok("")`. The
/// missing-half of issue #193 replaces this single function with the real
/// `SQLite` + markdown loader; nothing else here changes. The signature
/// matches `assemble_hot_with_loader`'s `FnMut(HotRecipeStep) -> Result<String, String>`
/// so the real loader can surface I/O / parse errors via the same hook.
#[allow(clippy::unnecessary_wraps)]
fn load_step_body(_step: HotRecipeStep) -> Result<String, String> {
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotMemoryConfig;

    #[test]
    fn assemble_hot_default_config_returns_six_zero_length_segments() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.bytes, 0);
        let segments = data.segments.expect("segments emitted");
        assert_eq!(segments.len(), cfg.recipe.len());
        for s in &segments {
            assert_eq!(s.byte_start, 0);
            assert_eq!(s.byte_end, 0);
        }
    }

    #[test]
    fn assemble_hot_empty_recipe() {
        let mut cfg = HotMemoryConfig::default();
        cfg.recipe.clear();
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.segments, Some(vec![]));
    }

    #[test]
    fn assemble_hot_rejects_over_max_segments_recipe() {
        // A recipe with 65 steps would emit a `segments` array the wire
        // contract rejects (MAX_SEGMENTS = 64). The assembler must catch
        // this before serialization.
        use crate::config::HotMemoryRecipeStep;
        let cfg = HotMemoryConfig {
            max_bytes: 4_194_304,
            recipe: vec![HotMemoryRecipeStep::Purpose; 65],
        };
        let err = assemble_hot(&cfg).unwrap_err();
        match err {
            AssembleHotError::Segments(
                super::super::segments::AssembleHotValidationError::TooManySegments { got, max },
            ) => {
                assert_eq!(got, 65);
                assert_eq!(max, 64);
            }
            other => panic!("expected TooManySegments, got {other:?}"),
        }
    }

    #[test]
    fn assemble_hot_rejects_over_max_segments_before_loading() {
        use crate::config::HotMemoryRecipeStep;
        let cfg = HotMemoryConfig {
            max_bytes: 4_194_304,
            recipe: vec![HotMemoryRecipeStep::Purpose; 65],
        };
        let mut calls = 0_u32;
        let err = assemble_hot_with_loader(&cfg, |_| {
            calls += 1;
            Ok(String::new())
        })
        .unwrap_err();

        assert_eq!(calls, 0, "loader must not run for oversized recipes");
        match err {
            AssembleHotError::Segments(
                super::super::segments::AssembleHotValidationError::TooManySegments { got, max },
            ) => {
                assert_eq!(got, 65);
                assert_eq!(max, 64);
            }
            other => panic!("expected TooManySegments, got {other:?}"),
        }
    }

    #[test]
    fn assemble_hot_from_bodies_rejects_over_max_segments_before_trimming() {
        use crate::config::HotMemoryRecipeStep;
        let cfg = HotMemoryConfig {
            max_bytes: 4_194_304,
            recipe: vec![HotMemoryRecipeStep::Purpose; 65],
        };
        let err = assemble_hot_from_bodies(&cfg, Vec::new(), Some(1)).unwrap_err();

        match err {
            AssembleHotError::Segments(
                super::super::segments::AssembleHotValidationError::TooManySegments { got, max },
            ) => {
                assert_eq!(got, 65);
                assert_eq!(max, 64);
            }
            other => panic!("expected TooManySegments, got {other:?}"),
        }
    }

    #[test]
    fn assemble_hot_rejects_over_budget_recipe() {
        // Use a non-stub loader to simulate #193's real loading path.
        // max_bytes is 8, but each body is 4 bytes × 6 steps = 24 bytes.
        let cfg = HotMemoryConfig {
            max_bytes: 8,
            ..HotMemoryConfig::default()
        };
        let err = assemble_hot_with_loader(&cfg, |_| Ok("AAAA".to_owned())).unwrap_err();
        match err {
            AssembleHotError::BudgetExceeded { got, max } => {
                assert_eq!(got, 24);
                assert_eq!(max, 8);
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }
    }

    #[test]
    fn assemble_hot_accepts_within_budget_recipe() {
        let cfg = HotMemoryConfig {
            max_bytes: 64,
            ..HotMemoryConfig::default()
        };
        let data = assemble_hot_with_loader(&cfg, |_| Ok("AA".to_owned())).unwrap();
        assert_eq!(data.bytes, 12);
    }

    #[test]
    fn assemble_hot_propagates_loader_failure() {
        // Real loaders must be able to surface I/O / parse / partial-read
        // errors as a typed AssembleHotError variant rather than degrading
        // silently to empty content.
        let cfg = HotMemoryConfig::default();
        let err = assemble_hot_with_loader(&cfg, |_| {
            Err::<String, String>("simulated I/O failure".to_owned())
        })
        .unwrap_err();
        match err {
            AssembleHotError::LoadFailed { step: _, reason } => {
                assert!(reason.contains("simulated I/O failure"), "reason: {reason}");
            }
            other => panic!("expected LoadFailed, got {other:?}"),
        }
    }

    #[test]
    fn assemble_hot_output_round_trips_through_deserialize() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&cfg).unwrap();
        let json = serde_json::to_string(&data).unwrap();
        let back: crate::generated::verbs::assemble_hot::AssembleHotData =
            serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
    }
}
