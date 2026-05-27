//! Stub-body `HotMemoryAssembler`. Walks `HotMemoryConfig.recipe`,
//! calls a stub `load_step_body` returning `""` for every step, and
//! returns a fully validated `AssembleHotData`. Real source loading is
//! the missing-half of issue #193 — that PR replaces `load_step_body`
//! and changes nothing else.

use super::segments::{
    AssembleHotValidationError, MAX_SEGMENTS, build_segments, validate, validate_with_recipe,
};
use crate::config::{HotMemoryConfig, HotMemoryRecipeStep};
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
    /// `HotMemoryConfig::resolve_recipe` returned `None` for the
    /// configured `default_recipe`. Validation would normally reject
    /// this earlier, but direct in-process mutation of the config can
    /// produce an unresolved default — surface it instead of panicking.
    #[error("unknown default recipe: {name:?}")]
    UnknownRecipe {
        /// The unresolved recipe name.
        name: String,
    },
}

/// Run the hot-memory recipe and return a validated `AssembleHotData`.
///
/// Resolves the default recipe through
/// [`HotMemoryConfig::resolve_recipe`], so the named-recipe table is
/// the single source of truth for every caller — direct in-process
/// mutation of `config.recipes[default_recipe]` takes effect on the
/// very next call.
pub fn assemble_hot(config: &HotMemoryConfig) -> Result<AssembleHotData, AssembleHotError> {
    let resolved = config
        .resolve_recipe(None)
        .ok_or_else(|| AssembleHotError::UnknownRecipe {
            name: config.default_recipe.clone(),
        })?;
    assemble_hot_recipe_with_loader(
        &resolved.name,
        resolved.steps,
        resolved.max_bytes,
        load_step_body,
    )
}

/// Render hot memory using a narrower byte budget than the config default.
/// This keeps `assemble_hot` itself a pure renderer while callers such as
/// pre-compaction can supply a fail-closed budget cap.
pub fn assemble_hot_with_budget(
    config: &HotMemoryConfig,
    budget: u64,
) -> Result<AssembleHotData, AssembleHotError> {
    assemble_hot_with_budget_and_recipe(config, budget, None)
}

/// Render hot memory with both a narrower byte budget and an optional
/// recipe override. The pre-compaction flow uses this to honor
/// `hot_memory.pre_compact_recipe` instead of falling back to the
/// session-start recipe baked into [`HotMemoryConfig::recipe`].
pub fn assemble_hot_with_budget_and_recipe(
    config: &HotMemoryConfig,
    budget: u64,
    recipe_override: Option<&[HotMemoryRecipeStep]>,
) -> Result<AssembleHotData, AssembleHotError> {
    let mut budgeted = config.clone();
    let budget_u32 = u32::try_from(budget.min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
    budgeted.max_bytes = budget_u32;
    if let Some(recipe) = recipe_override {
        budgeted.recipe = recipe.to_vec();
    }
    // `assemble_hot` reads through `resolve_recipe`, which consults
    // `recipes[default_recipe]` rather than the flat `recipe`/`max_bytes`
    // scalars. Mirror the budget + step override into the default-recipe
    // preset so the override actually reaches the assembler.
    let default_name = budgeted.default_recipe.clone();
    let preset = budgeted.recipes.entry(default_name).or_insert_with(|| {
        crate::config::HotMemoryRecipePreset {
            steps: budgeted.recipe.clone(),
            max_bytes: budget_u32,
        }
    });
    if let Some(recipe) = recipe_override {
        preset.steps = recipe.to_vec();
    }
    preset.max_bytes = budget_u32;
    assemble_hot(&budgeted)
}

/// Variant of [`assemble_hot`] that accepts an explicit fallible loader.
/// Used by tests today; once #193 lands, the real `SQLite` + markdown
/// loader will be threaded in via the same hook so I/O / parse failures
/// surface as [`AssembleHotError::LoadFailed`].
pub fn assemble_hot_with_loader<F>(
    config: &HotMemoryConfig,
    loader: F,
) -> Result<AssembleHotData, AssembleHotError>
where
    F: FnMut(HotRecipeStep) -> Result<String, String>,
{
    let resolved = config
        .resolve_recipe(None)
        .ok_or_else(|| AssembleHotError::UnknownRecipe {
            name: config.default_recipe.clone(),
        })?;
    assemble_hot_recipe_with_loader(&resolved.name, resolved.steps, resolved.max_bytes, loader)
}

/// Run one selected named recipe and return validated `AssembleHotData`.
pub fn assemble_hot_recipe(
    recipe_name: &str,
    steps: &[HotMemoryRecipeStep],
    max_bytes: u32,
) -> Result<AssembleHotData, AssembleHotError> {
    assemble_hot_recipe_with_loader(recipe_name, steps, max_bytes, load_step_body)
}

/// Variant of [`assemble_hot`] that accepts a selected recipe name plus steps.
pub fn assemble_hot_recipe_with_loader<F>(
    recipe_name: &str,
    steps: &[HotMemoryRecipeStep],
    max_bytes: u32,
    mut loader: F,
) -> Result<AssembleHotData, AssembleHotError>
where
    F: FnMut(HotRecipeStep) -> Result<String, String>,
{
    if steps.len() > MAX_SEGMENTS {
        return Err(AssembleHotValidationError::TooManySegments {
            got: steps.len(),
            max: MAX_SEGMENTS,
        }
        .into());
    }
    let recipe: Vec<HotRecipeStep> = steps.iter().copied().map(HotRecipeStep::from).collect();
    let mut bodies: Vec<String> = Vec::with_capacity(recipe.len());
    for step in recipe.iter().copied() {
        let body = loader(step).map_err(|reason| AssembleHotError::LoadFailed { step, reason })?;
        bodies.push(body);
    }
    let bodies_refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies_refs)?;
    let bytes = prefix.len() as u64;
    let max = u64::from(max_bytes);
    if bytes > max {
        return Err(AssembleHotError::BudgetExceeded { got: bytes, max });
    }
    let data = AssembleHotData {
        bytes,
        prefix,
        recipe: Some(recipe_name.to_owned()),
        segments: Some(segments),
        debug: None,
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
    let resolved = config
        .resolve_recipe(None)
        .ok_or_else(|| AssembleHotError::UnknownRecipe {
            name: config.default_recipe.clone(),
        })?;
    validate_recipe_len(resolved.steps)?;
    let recipe: Vec<HotRecipeStep> = resolved
        .steps
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();
    let resolved_max = u64::from(resolved.max_bytes);
    let max = budget_override.unwrap_or(resolved_max).min(resolved_max);
    let trimmed = super::loader::trim_bodies_to_budget(&recipe, bodies, max);
    let refs: Vec<&str> = trimmed.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &refs)?;
    let data = AssembleHotData {
        bytes: prefix.len() as u64,
        prefix,
        recipe: Some(resolved.name.clone()),
        segments: Some(segments),
        debug: None,
    };
    validate_with_recipe(&data, &recipe)?;
    Ok(data)
}

fn validate_recipe_len(steps: &[HotMemoryRecipeStep]) -> Result<(), AssembleHotError> {
    if steps.len() > MAX_SEGMENTS {
        return Err(AssembleHotValidationError::TooManySegments {
            got: steps.len(),
            max: MAX_SEGMENTS,
        }
        .into());
    }
    Ok(())
}

/// Assemble hot memory from a typed `HotMemoryInputs` projection
/// (issue #82). Walks `config.recipe`, dispatches each step to its
/// source-specific `select`, glues the bodies via [`build_segments`],
/// validates the wire shape, and (when `inputs.include_debug` is true)
/// emits a per-step inclusion / exclusion trace on
/// `AssembleHotData.debug`.
///
/// Distinct from [`assemble_hot_from_bodies`]: this entry point lets
/// callers express the per-step ranking + admissibility logic via the
/// `sources::*` modules instead of pre-loading bodies, which is the
/// path the CLI uses when `--explain` is requested.
///
/// # Errors
///
/// Returns [`AssembleHotError::Segments`] if the assembled segments
/// violate the wire contract; [`AssembleHotError::BudgetExceeded`] if
/// the byte total exceeds `config.max_bytes`.
pub fn assemble_hot_with_inputs(
    inputs: &super::inputs::HotMemoryInputs<'_>,
    config: &HotMemoryConfig,
) -> Result<AssembleHotData, AssembleHotError> {
    use crate::generated::verbs::assemble_hot::{HotMemoryDebug, HotStepTrace};

    let resolved = config
        .resolve_recipe(None)
        .ok_or_else(|| AssembleHotError::UnknownRecipe {
            name: config.default_recipe.clone(),
        })?;
    validate_recipe_len(resolved.steps)?;
    let recipe: Vec<HotRecipeStep> = resolved
        .steps
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();

    let mut bodies: Vec<String> = Vec::with_capacity(recipe.len());
    let mut traces: Vec<HotStepTrace> = Vec::with_capacity(recipe.len());
    let mut emitted_bytes = 0_u64;
    let max_bytes = u64::from(resolved.max_bytes);

    for step in recipe.iter().copied() {
        let remaining_budget = Some(max_bytes.saturating_sub(emitted_bytes));
        let segment = inputs_run_step(step, inputs, remaining_budget);
        if inputs.include_debug {
            traces.push(inputs_to_step_trace(step, &segment));
        }
        emitted_bytes = emitted_bytes.saturating_add(segment.body.len() as u64);
        bodies.push(segment.body);
    }

    let bodies_refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies_refs)?;

    let bytes = prefix.len() as u64;
    let max = u64::from(resolved.max_bytes);
    if bytes > max {
        return Err(AssembleHotError::BudgetExceeded { got: bytes, max });
    }

    let debug = if inputs.include_debug {
        Some(HotMemoryDebug { steps: traces })
    } else {
        None
    };

    let data = AssembleHotData {
        bytes,
        prefix,
        recipe: Some(resolved.name.clone()),
        segments: Some(segments),
        debug,
    };
    validate(&data)?;
    Ok(data)
}

fn inputs_run_step(
    step: HotRecipeStep,
    inputs: &super::inputs::HotMemoryInputs<'_>,
    remaining_budget: Option<u64>,
) -> super::inclusion::LoadedSegment {
    match step {
        HotRecipeStep::Purpose => super::sources::purpose::select(inputs),
        HotRecipeStep::Index => super::sources::index::select(inputs),
        HotRecipeStep::PinnedFeedback => super::sources::pinned::select(inputs),
        HotRecipeStep::TopSalienceProject => super::sources::project::select(inputs),
        HotRecipeStep::ActivePlaybook => {
            super::sources::playbook::select_with_budget(inputs, remaining_budget)
        }
        HotRecipeStep::RecentUserSignal => super::sources::user_signal::select(inputs),
    }
}

fn inputs_to_step_trace(
    step: HotRecipeStep,
    segment: &super::inclusion::LoadedSegment,
) -> crate::generated::verbs::assemble_hot::HotStepTrace {
    use crate::generated::verbs::assemble_hot::{HotExclusion, HotStepTrace};

    // Authorization-related exclusions (`OutOfScope`, `VisibilityDenied`,
    // `ForgottenScope`) MUST NOT appear on the wire trace. The
    // candidate slot can legitimately contain records the caller is not
    // allowed to see — the assembler's `admit()` predicate is the trust
    // boundary, and emitting their record_id on `data.debug` would turn
    // `--explain` into an enumeration channel.
    let safe_excluded: Vec<HotExclusion> = segment
        .excluded
        .iter()
        .filter(|t| !inputs_is_auth_redacted(t.reason))
        .map(inputs_to_exclusion)
        .collect();
    HotStepTrace {
        step,
        included: segment.included.iter().map(inputs_to_inclusion).collect(),
        excluded: safe_excluded,
    }
}

const fn inputs_is_auth_redacted(reason: super::inclusion::ExclusionReason) -> bool {
    use super::inclusion::ExclusionReason;
    matches!(
        reason,
        ExclusionReason::OutOfScope
            | ExclusionReason::VisibilityDenied
            | ExclusionReason::ForgottenScope
    )
}

fn inputs_to_inclusion(
    trace: &super::inclusion::InclusionTrace,
) -> crate::generated::verbs::assemble_hot::HotInclusion {
    crate::generated::verbs::assemble_hot::HotInclusion {
        record_id: trace.record_id.as_str().to_owned(),
        score: trace.score,
        note: trace.note.to_owned(),
    }
}

fn inputs_to_exclusion(
    trace: &super::inclusion::ExclusionTrace,
) -> crate::generated::verbs::assemble_hot::HotExclusion {
    crate::generated::verbs::assemble_hot::HotExclusion {
        record_id: trace.record_id.as_str().to_owned(),
        reason: inputs_to_wire_reason(trace.reason),
    }
}

fn inputs_to_wire_reason(
    reason: super::inclusion::ExclusionReason,
) -> crate::generated::verbs::assemble_hot::HotExclusionReason {
    use super::inclusion::ExclusionReason;
    use crate::generated::verbs::assemble_hot::HotExclusionReason;
    match reason {
        ExclusionReason::Tombstoned => HotExclusionReason::Tombstoned,
        ExclusionReason::ForgottenScope => HotExclusionReason::ForgottenScope,
        ExclusionReason::BelowConfidenceFloor => HotExclusionReason::BelowConfidenceFloor,
        ExclusionReason::OutOfScope => HotExclusionReason::OutOfScope,
        ExclusionReason::VisibilityDenied => HotExclusionReason::VisibilityDenied,
        ExclusionReason::OutsideRecencyWindow => HotExclusionReason::OutsideRecencyWindow,
        ExclusionReason::BeyondTopK => HotExclusionReason::BeyondTopK,
        ExclusionReason::NotPinned => HotExclusionReason::NotPinned,
        ExclusionReason::EmptyBody => HotExclusionReason::EmptyBody,
    }
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
        assert_eq!(data.recipe.as_deref(), Some("chat"));
        let segments = data.segments.expect("segments emitted");
        assert_eq!(segments.len(), cfg.recipe.len());
        for s in &segments {
            assert_eq!(s.byte_start, 0);
            assert_eq!(s.byte_end, 0);
        }
    }

    #[test]
    fn assemble_hot_empty_recipe() {
        use crate::config::HotMemoryRecipePreset;
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes.insert(
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![],
                max_bytes: cfg.max_bytes,
            },
        );
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.recipe.as_deref(), Some("chat"));
        assert_eq!(data.segments, Some(vec![]));
    }

    #[test]
    fn assemble_hot_resolves_through_recipes_table() {
        // Regression: in-process mutation of `recipes[default_recipe]`
        // must take effect on the very next call. Public APIs route
        // through `HotMemoryConfig::resolve_recipe` rather than the
        // legacy flat fields.
        use crate::config::{HotMemoryRecipePreset, HotMemoryRecipeStep};
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes.insert(
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose],
                max_bytes: 1024,
            },
        );
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(
            data.segments.as_ref().map_or(0, Vec::len),
            1,
            "mutated recipes[chat] must drive output, not flat cfg.recipe"
        );
    }

    #[test]
    fn assemble_hot_rejects_over_max_segments_recipe() {
        // A recipe with 65 steps would emit a `segments` array the wire
        // contract rejects (MAX_SEGMENTS = 64). The assembler must catch
        // this before serialization.
        use crate::config::{HotMemoryRecipePreset, HotMemoryRecipeStep};
        let mut cfg = HotMemoryConfig {
            max_bytes: 4_194_304,
            recipe: vec![HotMemoryRecipeStep::Purpose; 65],
            ..HotMemoryConfig::default()
        };
        cfg.recipes.insert(
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose; 65],
                max_bytes: 4_194_304,
            },
        );
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
        use crate::config::{HotMemoryRecipePreset, HotMemoryRecipeStep};
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes.insert(
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose; 65],
                max_bytes: 4_194_304,
            },
        );
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
        use crate::config::{HotMemoryRecipePreset, HotMemoryRecipeStep};
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes.insert(
            "chat".into(),
            HotMemoryRecipePreset {
                steps: vec![HotMemoryRecipeStep::Purpose; 65],
                max_bytes: 4_194_304,
            },
        );
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
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes
            .get_mut("chat")
            .expect("chat preset exists in defaults")
            .max_bytes = 8;
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
        let mut cfg = HotMemoryConfig::default();
        cfg.recipes
            .get_mut("chat")
            .expect("chat preset exists in defaults")
            .max_bytes = 64;
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
