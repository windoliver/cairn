//! Pure salience scoring helpers for access strengthening and decay.

/// Strengthen salience after a successful read hit.
///
/// Uses diminishing returns so highly-salient records approach `1.0`
/// asymptotically instead of saturating from a small number of reads.
#[must_use]
pub fn apply_access(salience: f32) -> f32 {
    let s = finite_unit(salience);
    (s + 0.05 * (1.0 - s)).clamp(0.0, 1.0)
}

/// Decay salience according to an exponential forgetting curve.
#[must_use]
pub fn decay_salience(salience: f32, decay_rate: f32, days_since_last_access: u32) -> f32 {
    let s = finite_unit(salience);
    let rate = if decay_rate.is_finite() {
        decay_rate.max(0.0)
    } else {
        0.0
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "salience is stored as f32, and the forgetting curve intentionally operates in that domain"
    )]
    let exponent = -rate * days_since_last_access as f32;
    (s * exponent.exp()).clamp(0.0, 1.0)
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn access_never_decreases_and_stays_bounded(s in 0.0f32..=1.0) {
            let next = apply_access(s);
            prop_assert!(next >= s);
            prop_assert!((0.0..=1.0).contains(&next));
        }

        #[test]
        fn decay_never_increases_and_stays_bounded(s in 0.0f32..=1.0, days in 0u32..365) {
            let next = decay_salience(s, 0.05, days);
            prop_assert!(next <= s);
            prop_assert!((0.0..=1.0).contains(&next));
        }
    }

    #[test]
    fn access_uses_diminishing_returns() {
        assert!((apply_access(0.5) - 0.525).abs() < 0.000_001);
        assert!((apply_access(0.9) - 0.905).abs() < 0.000_001);
    }

    #[test]
    fn decay_matches_forgetting_curve() {
        let next = decay_salience(1.0, 0.05, 14);
        assert!((next - 0.496_585).abs() < 0.000_01);
    }
}
