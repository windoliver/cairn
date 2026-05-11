//! Typed core models and pure budget math for the future pre-compaction hook.

use crate::config::{HotMemoryConfig, HotMemoryRecipeStep};
use crate::domain::SessionId;
use crate::generated::verbs::assemble_hot::AssembleHotData;
use crate::verbs::assemble_hot::assembler::{
    AssembleHotError, assemble_hot_with_budget_and_recipe,
};

/// Recipe name reserved for the pre-compaction handoff preset.
///
/// Defined locally until the recipe-preset registry from #293 lands. The
/// handoff preset is intentionally a minimal stable prefix — purpose plus
/// pinned feedback — so the post-compaction window keeps the priors most
/// likely to survive a context squeeze without burning budget on volatile
/// signal.
pub const HANDOFF_RECIPE_NAME: &str = "handoff";

/// Resolve a `pre_compact_recipe` config value to a concrete recipe step
/// list. Unknown names fall back to the caller's session-start recipe so
/// misconfiguration cannot silently downgrade to the empty render.
#[must_use]
pub fn resolve_pre_compact_recipe(
    name: &str,
    fallback: &[HotMemoryRecipeStep],
) -> Vec<HotMemoryRecipeStep> {
    match name {
        HANDOFF_RECIPE_NAME => vec![
            HotMemoryRecipeStep::Purpose,
            HotMemoryRecipeStep::PinnedFeedback,
        ],
        _ => fallback.to_vec(),
    }
}

/// Input snapshot for a pre-compaction render attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactEvent {
    /// Session being compacted.
    pub session_id: SessionId,
    /// Token count before compaction starts.
    pub token_count_before: u32,
    /// Token target the runtime intends to compact away.
    pub compaction_target: u32,
    /// Last user-visible turn index at the time the hook fires.
    pub last_user_turn_index: u64,
}

/// Output of a pre-compaction render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactOutput {
    /// Rendered reinjection payload text.
    pub reinjection_text: String,
    /// Byte length of `reinjection_text`.
    pub output_bytes: u64,
    /// Maximum bytes budgeted for reinjection.
    pub budget_bytes: u64,
    /// Recipe identifier used to render the output.
    pub recipe: String,
}

/// Failure modes for pre-compaction orchestration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PreCompactError {
    /// Hot-memory assembly failed before a snapshot could be persisted.
    #[error("assemble_hot: {0}")]
    AssembleHot(#[from] AssembleHotError),
    /// Persisting the pre-compaction snapshot failed, so the hook rejects.
    #[error("snapshot: {reason}")]
    Snapshot {
        /// Store or persistence layer failure detail.
        reason: String,
    },
}

/// Compute the reinjection budget from the compaction target and safety ratio.
#[must_use]
pub fn compute_budget(compaction_target: u32, max_bytes: u32, ratio: f64) -> u64 {
    if compaction_target == 0 || !ratio.is_finite() || ratio <= 0.0 {
        return 0;
    }

    let hinted = floor_decimal_product(compaction_target, ratio);
    hinted.min(u64::from(max_bytes))
}

fn floor_decimal_product(compaction_target: u32, ratio: f64) -> u64 {
    let rendered = ratio.to_string();
    let (significand, exponent) = rendered
        .split_once(['e', 'E'])
        .map_or((rendered.as_str(), 0_i32), |(sig, exp)| {
            (sig, exp.parse::<i32>().unwrap_or(0))
        });

    let (whole, fractional) = significand
        .split_once('.')
        .map_or((significand, ""), |(whole, fractional)| (whole, fractional));
    let digits = format!("{whole}{fractional}");
    let numerator = digits.parse::<u128>().unwrap_or(0);
    let scale = exponent - i32::try_from(fractional.len()).unwrap_or(i32::MAX);
    let target = u128::from(compaction_target);

    if scale >= 0 {
        let shift = u32::try_from(scale).unwrap_or(u32::MAX);
        match 10_u128.checked_pow(shift) {
            Some(multiplier) => target
                .saturating_mul(numerator)
                .saturating_mul(multiplier)
                .try_into()
                .unwrap_or(u64::MAX),
            None => u64::MAX,
        }
    } else {
        let shift = scale.unsigned_abs();
        match 10_u128.checked_pow(shift) {
            Some(divisor) => (target.saturating_mul(numerator) / divisor)
                .try_into()
                .unwrap_or(u64::MAX),
            None => 0,
        }
    }
}

/// Run the fail-closed pre-compaction flow: budgeted assembly first, then
/// snapshot persistence, returning reinjection metadata only on success.
///
/// Emits a `sensor.pre_compact` span carrying `session_id`, `recipe`,
/// `budget`, and `output_bytes` so harnesses can audit each fire-and-splice.
pub fn run_pre_compact<SNAP>(
    event: &PreCompactEvent,
    cfg: &HotMemoryConfig,
    mut snapshot: SNAP,
) -> Result<PreCompactOutput, PreCompactError>
where
    SNAP: FnMut(&PreCompactEvent, &AssembleHotData) -> Result<(), String>,
{
    let span = tracing::info_span!(
        "sensor.pre_compact",
        session_id = %event.session_id,
        recipe = %cfg.pre_compact_recipe,
        budget = tracing::field::Empty,
        output_bytes = tracing::field::Empty,
    );
    let _enter = span.enter();
    let budget = compute_budget(
        event.compaction_target,
        cfg.max_bytes,
        cfg.pre_compact_safety_ratio,
    );
    span.record("budget", budget);
    let recipe_steps = resolve_pre_compact_recipe(&cfg.pre_compact_recipe, &cfg.recipe);
    let data = assemble_hot_with_budget_and_recipe(cfg, budget, Some(&recipe_steps))?;
    span.record("output_bytes", data.bytes);
    snapshot(event, &data).map_err(|reason| PreCompactError::Snapshot { reason })?;

    tracing::info!(
        session_id = %event.session_id,
        recipe = %cfg.pre_compact_recipe,
        budget,
        output_bytes = data.bytes,
        "sensor.pre_compact: reinjection rendered",
    );

    Ok(PreCompactOutput {
        reinjection_text: data.prefix,
        output_bytes: data.bytes,
        budget_bytes: budget,
        recipe: cfg.pre_compact_recipe.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::{
        HANDOFF_RECIPE_NAME, PreCompactError, PreCompactEvent, compute_budget, run_pre_compact,
    };
    use crate::config::{HotMemoryConfig, HotMemoryRecipeStep};
    use crate::domain::SessionId;

    fn sample_event() -> PreCompactEvent {
        PreCompactEvent {
            session_id: SessionId::parse("sess_01jv3e0h5n7d9c1m2p4q6r8s0t")
                .expect("valid session id"),
            token_count_before: 12_000,
            compaction_target: 8_000,
            last_user_turn_index: 42,
        }
    }

    fn sample_cfg() -> HotMemoryConfig {
        HotMemoryConfig::default()
    }

    #[test]
    fn computes_budget_from_target_and_ratio() {
        let budget = compute_budget(8_000, 25_600, 0.30);
        assert_eq!(budget, 2_400);
    }

    #[test]
    fn caps_budget_at_hot_memory_max_bytes() {
        let budget = compute_budget(8_000, 1_000, 0.30);
        assert_eq!(budget, 1_000);
    }

    #[test]
    fn zero_target_yields_zero_budget() {
        let budget = compute_budget(0, 25_600, 0.30);
        assert_eq!(budget, 0);
    }

    #[test]
    fn avoids_undercount_from_valid_floating_point_ratio() {
        let budget = compute_budget(50, 1_000, 0.58);
        assert_eq!(budget, 29);
    }

    #[test]
    fn does_not_overcount_ratio_just_below_integer_boundary() {
        let budget = compute_budget(1, 1_000, 0.999_999_999_999_999_9);
        assert_eq!(budget, 0);
    }

    #[test]
    fn pre_compact_runs_snapshot_after_real_assembly_and_returns_metadata() {
        let calls = RefCell::new(Vec::new());

        let event = sample_event();
        let out = run_pre_compact(&event, &sample_cfg(), |snap_event, assembled| {
            calls.borrow_mut().push(format!(
                "snapshot:{}:{}:{}",
                snap_event.last_user_turn_index, assembled.bytes, assembled.prefix
            ));
            Ok(())
        })
        .unwrap();

        assert_eq!(*calls.borrow(), vec!["snapshot:42:0:"]);
        assert_eq!(out.reinjection_text, "");
        assert_eq!(out.output_bytes, 0);
        assert_eq!(out.budget_bytes, 2_400);
        assert_eq!(out.recipe, "handoff");
    }

    #[test]
    fn pre_compact_assemble_failure_prevents_snapshot() {
        let snapshot_called = Cell::new(false);
        let mut cfg = sample_cfg();
        // Force the fallback path so the recipe override doesn't shrink
        // the assembled steps under the budget before assembly fails.
        cfg.pre_compact_recipe = "force-fallback".to_owned();
        cfg.recipe = vec![HotMemoryRecipeStep::Purpose; 65];

        let err = run_pre_compact(&sample_event(), &cfg, |_, _| {
            snapshot_called.set(true);
            Ok(())
        })
        .unwrap_err();

        assert!(!snapshot_called.get());
        assert!(matches!(err, PreCompactError::AssembleHot(_)));
    }

    #[test]
    fn pre_compact_output_bytes_stay_within_safety_ratio_for_8000_target() {
        // Issue #310 acceptance: a pre_compact event whose harness target is
        // 8 000 tokens must yield reinjection bytes inside the 0.30-ratio
        // budget (≤ 2 400 bytes), regardless of recipe content.
        let out = run_pre_compact(&sample_event(), &sample_cfg(), |_, _| Ok(())).unwrap();

        assert_eq!(out.budget_bytes, 2_400);
        assert!(
            out.output_bytes <= 2_400,
            "output {} bytes exceeded the 2_400-byte safety budget",
            out.output_bytes,
        );
        assert!(out.reinjection_text.len() as u64 <= out.budget_bytes);
    }

    #[tracing_test::traced_test]
    #[test]
    fn pre_compact_emits_sensor_span_with_required_fields() {
        // Issue #310 acceptance: a `sensor.pre_compact` span must carry
        // session_id, recipe, budget, and output_bytes for harness audit.
        let out = run_pre_compact(&sample_event(), &sample_cfg(), |_, _| Ok(())).unwrap();

        assert!(logs_contain("sensor.pre_compact"));
        assert!(logs_contain("session_id=sess_01jv3e0h5n7d9c1m2p4q6r8s0t"));
        assert!(logs_contain("recipe=handoff"));
        assert!(logs_contain(&format!("budget={}", out.budget_bytes)));
        assert!(logs_contain(&format!("output_bytes={}", out.output_bytes)));
    }

    #[test]
    fn pre_compact_handoff_recipe_overrides_session_start_steps() {
        // The handoff preset must be the minimal stable prefix — purpose
        // plus pinned feedback — not the full session-start recipe baked
        // into HotMemoryConfig.recipe. Inspect the assembled segments to
        // prove the override actually reached the assembler.
        let mut cfg = sample_cfg();
        cfg.recipe = vec![
            HotMemoryRecipeStep::Purpose,
            HotMemoryRecipeStep::Index,
            HotMemoryRecipeStep::PinnedFeedback,
            HotMemoryRecipeStep::TopSalienceProject,
            HotMemoryRecipeStep::ActivePlaybook,
            HotMemoryRecipeStep::RecentUserSignal,
        ];
        cfg.pre_compact_recipe = HANDOFF_RECIPE_NAME.to_owned();

        let captured = RefCell::new(None);
        let _ = run_pre_compact(&sample_event(), &cfg, |_, assembled| {
            captured.replace(assembled.segments.clone());
            Ok(())
        })
        .unwrap();

        let segments = captured.borrow().clone().expect("segments emitted");
        let kinds: Vec<String> = segments
            .into_iter()
            .map(|seg| {
                serde_json::to_value(seg.step)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(kinds, vec!["purpose".to_string(), "pinned_feedback".into()]);
    }

    #[test]
    fn pre_compact_unknown_recipe_falls_back_to_session_start() {
        // Misconfiguration must not silently downgrade to the empty
        // render: an unknown preset reuses the operator's session-start
        // recipe rather than producing zero segments.
        let mut cfg = sample_cfg();
        cfg.recipe = vec![HotMemoryRecipeStep::Purpose, HotMemoryRecipeStep::Index];
        cfg.pre_compact_recipe = "not-a-real-preset".to_owned();

        let captured = RefCell::new(None);
        let _ = run_pre_compact(&sample_event(), &cfg, |_, assembled| {
            captured.replace(assembled.segments.clone());
            Ok(())
        })
        .unwrap();

        let segments = captured.borrow().clone().expect("segments emitted");
        let kinds: Vec<String> = segments
            .into_iter()
            .map(|seg| {
                serde_json::to_value(seg.step)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(kinds, vec!["purpose".to_string(), "index".into()]);
    }

    #[test]
    fn pre_compact_snapshot_failure_rejects_hook() {
        let err = run_pre_compact(&sample_event(), &sample_cfg(), |_, assembled| {
            assert_eq!(assembled.bytes, 0);
            assert_eq!(assembled.prefix, "");
            Err("disk full".to_owned())
        })
        .unwrap_err();

        assert!(err.to_string().contains("snapshot"));
    }
}
