//! Pure per-record expiration decision (issue #91, brief §10.0).
//!
//! The handler converts each record's
//! [`crate::domain::Rfc3339Timestamp`] to a millisecond integer once
//! and threads `now_ms` through; this function is then a straight
//! table-lookup with no I/O.

use crate::config::ExpirationConfig;
use crate::domain::flush_plan::ExpirationReason;

/// Number of milliseconds in one calendar day. Used by the TTL arm.
pub const MILLIS_PER_DAY: i64 = 86_400_000;

/// Decide whether a record should be soft-retired this sweep.
///
/// Returns `Some(reason)` if the record qualifies for
/// [`MemoryStore::tombstone(_, TombstoneReason::Expire)`](
/// crate::contract::memory_store::MemoryStore::tombstone), or `None` if
/// the record should remain active.
///
/// Decision rules, evaluated in order:
///
/// 1. **TTL** — when `now_ms - updated_at_ms > ttl_days * MILLIS_PER_DAY`
///    the record is `TtlExpired`. The handler always supplies a
///    monotonically non-decreasing `now_ms`; replays remain
///    deterministic because they pin the same `now_ms`.
/// 2. **Salience** — when `salience_floor > 0.0` and the record's
///    salience is strictly below the floor, return
///    `SalienceBelowThreshold`. A floor of `0.0` disables this arm
///    (the operator opted out by leaving the default).
///
/// Records that are not eligible under either rule are kept.
#[must_use]
pub fn decide(
    salience: f32,
    updated_at_ms: i64,
    now_ms: i64,
    config: &ExpirationConfig,
) -> Option<ExpirationReason> {
    if !config.enabled {
        return None;
    }

    // TTL arm. `ttl_days` is validated `>= 1` so the product cannot
    // overflow `i64` for any reasonable wall-clock instant; we still
    // saturate to guard against malformed timestamps.
    let ttl_ms = i64::from(config.ttl_days).saturating_mul(MILLIS_PER_DAY);
    let age_ms = now_ms.saturating_sub(updated_at_ms);
    if age_ms > ttl_ms {
        return Some(ExpirationReason::TtlExpired);
    }

    // Salience arm. Floor of `0.0` disables the arm; otherwise a record
    // whose salience is *strictly* below the floor expires.
    if config.salience_floor > 0.0 && salience < config.salience_floor {
        return Some(ExpirationReason::SalienceBelowThreshold);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config(ttl_days: u32, salience_floor: f32) -> ExpirationConfig {
        ExpirationConfig {
            enabled: true,
            ttl_days,
            salience_floor,
            batch_size: 16,
        }
    }

    #[test]
    fn disabled_config_keeps_everything() {
        let cfg = ExpirationConfig {
            enabled: false,
            ttl_days: 1,
            salience_floor: 0.99,
            batch_size: 16,
        };
        // Even a stale, low-salience record stays alive when the
        // workflow is disabled.
        assert!(decide(0.1, 0, i64::MAX, &cfg).is_none());
    }

    #[test]
    fn ttl_arm_expires_record_past_ttl() {
        let cfg = enabled_config(1, 0.0);
        let now = MILLIS_PER_DAY * 3;
        let updated = 0;
        assert_eq!(
            decide(1.0, updated, now, &cfg),
            Some(ExpirationReason::TtlExpired)
        );
    }

    #[test]
    fn ttl_arm_keeps_record_exactly_at_ttl() {
        // strictly greater-than — the boundary record stays.
        let cfg = enabled_config(1, 0.0);
        let now = MILLIS_PER_DAY;
        let updated = 0;
        assert!(decide(1.0, updated, now, &cfg).is_none());
    }

    #[test]
    fn ttl_arm_keeps_fresh_record() {
        let cfg = enabled_config(30, 0.0);
        let now = MILLIS_PER_DAY * 3;
        let updated = MILLIS_PER_DAY * 2;
        assert!(decide(1.0, updated, now, &cfg).is_none());
    }

    #[test]
    fn salience_arm_disabled_when_floor_is_zero() {
        let cfg = enabled_config(365, 0.0);
        // Salience well below the (disabled) floor: still kept.
        assert!(decide(0.0, 0, 0, &cfg).is_none());
    }

    #[test]
    fn salience_arm_expires_below_floor() {
        let cfg = enabled_config(365, 0.5);
        assert_eq!(
            decide(0.4, 0, 0, &cfg),
            Some(ExpirationReason::SalienceBelowThreshold)
        );
    }

    #[test]
    fn salience_arm_keeps_at_or_above_floor() {
        let cfg = enabled_config(365, 0.5);
        assert!(decide(0.5, 0, 0, &cfg).is_none());
        assert!(decide(0.7, 0, 0, &cfg).is_none());
    }

    #[test]
    fn ttl_takes_priority_over_salience() {
        // If both arms would fire, TTL wins. The metric event records
        // the dominant reason so downstream gauges stay
        // deterministic.
        let cfg = enabled_config(1, 0.99);
        let now = MILLIS_PER_DAY * 5;
        let updated = 0;
        assert_eq!(
            decide(0.0, updated, now, &cfg),
            Some(ExpirationReason::TtlExpired)
        );
    }

    #[test]
    fn negative_age_clock_skew_does_not_expire() {
        // `updated_at > now` (clock skew or a future-dated record)
        // — saturating subtraction floors `age_ms` at 0, so the TTL
        // arm cannot fire.
        let cfg = enabled_config(1, 0.0);
        let now = 0;
        let updated = MILLIS_PER_DAY * 10;
        assert!(decide(1.0, updated, now, &cfg).is_none());
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let cfg = enabled_config(30, 0.4);
        let a = decide(0.5, 1_000, 2_000, &cfg);
        let b = decide(0.5, 1_000, 2_000, &cfg);
        assert_eq!(a, b);
    }
}
