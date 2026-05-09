//! Typed core models and pure budget math for the future pre-compaction hook.

use crate::domain::SessionId;

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

#[cfg(test)]
mod tests {
    use super::compute_budget;

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
}
