//! Typed core models and pure budget math for the future pre-compaction hook.

use crate::config::HotMemoryConfig;
use crate::domain::SessionId;
use crate::generated::verbs::assemble_hot::AssembleHotData;
use crate::verbs::assemble_hot::assembler::{AssembleHotError, assemble_hot_with_budget};

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
pub fn run_pre_compact<SNAP>(
    event: PreCompactEvent,
    cfg: &HotMemoryConfig,
    mut snapshot: SNAP,
) -> Result<PreCompactOutput, PreCompactError>
where
    SNAP: FnMut(&PreCompactEvent, &AssembleHotData) -> Result<(), String>,
{
    let budget = compute_budget(
        event.compaction_target,
        cfg.max_bytes,
        cfg.pre_compact_safety_ratio,
    );
    let data = assemble_hot_with_budget(cfg, budget)?;
    snapshot(&event, &data).map_err(|reason| PreCompactError::Snapshot { reason })?;

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

    use super::{PreCompactError, PreCompactEvent, compute_budget, run_pre_compact};
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
        let out = run_pre_compact(event.clone(), &sample_cfg(), |snap_event, assembled| {
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
        cfg.recipe = vec![HotMemoryRecipeStep::Purpose; 65];

        let err = run_pre_compact(sample_event(), &cfg, |_, _| {
            snapshot_called.set(true);
            Ok(())
        })
        .unwrap_err();

        assert!(!snapshot_called.get());
        assert!(matches!(err, PreCompactError::AssembleHot(_)));
    }

    #[test]
    fn pre_compact_snapshot_failure_rejects_hook() {
        let err = run_pre_compact(sample_event(), &sample_cfg(), |_, assembled| {
            assert_eq!(assembled.bytes, 0);
            assert_eq!(assembled.prefix, "");
            Err("disk full".to_owned())
        })
        .unwrap_err();

        assert!(err.to_string().contains("snapshot"));
    }
}
