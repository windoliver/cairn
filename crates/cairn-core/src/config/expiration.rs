//! `ExpirationWorkflow` configuration (issue #91, brief §10.0, §6).
//!
//! Drives the minimum-path soft-retirement sweep:
//!
//! * A record is *expired* when its age exceeds `ttl_days` or its
//!   `salience` falls below `salience_floor`.
//! * Expiration calls
//!   [`MemoryStore::tombstone(id, TombstoneReason::Expire)`](
//!   crate::contract::memory_store::MemoryStore::tombstone) — the
//!   store filters tombstoned rows out of default reads (brief §10
//!   "removes from default reads" acceptance criterion).
//! * Hard delete remains the responsibility of the `forget` verb.

use serde::{Deserialize, Serialize};

/// Typed configuration for the minimum-path `ExpirationWorkflow`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpirationConfig {
    /// Master switch. When `false` the workflow refuses to enqueue
    /// and `status` does not advertise
    /// `cairn.workflows.v1.expiration`.
    #[serde(default = "defaults::enabled")]
    pub enabled: bool,

    /// Default per-record TTL in days. Records whose age exceeds this
    /// value are eligible for expiration with reason `TtlExpired`.
    /// A future revision may grow per-kind overrides; the minimum
    /// path uses a single global TTL per brief §10.0 "tiered decay".
    #[serde(default = "defaults::ttl_days")]
    pub ttl_days: u32,

    /// Records whose `salience` falls strictly below this floor are
    /// eligible for expiration with reason `SalienceBelowThreshold`.
    /// Range `[0.0, 1.0]`. A value of `0.0` disables the salience
    /// arm and leaves only TTL-driven sweeps.
    #[serde(default = "defaults::salience_floor")]
    pub salience_floor: f32,

    /// Soft cap on the number of records the handler tombstones per
    /// sweep. Keeps a single job from monopolising the worker pool.
    /// Subsequent sweeps drain the rest.
    #[serde(default = "defaults::batch_size")]
    pub batch_size: u32,
}

impl Default for ExpirationConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::enabled(),
            ttl_days: defaults::ttl_days(),
            salience_floor: defaults::salience_floor(),
            batch_size: defaults::batch_size(),
        }
    }
}

mod defaults {
    pub const fn enabled() -> bool {
        // Off by P0 default — the issue ships the workflow surface
        // but soft-retirement is opt-in until the brief §5.6 WAL
        // `expire` op lands and an operator confirms semantics.
        false
    }
    pub const fn ttl_days() -> u32 {
        // Brief §10 "Idle >30 days + recall_count = 0 → cold".
        30
    }
    pub const fn salience_floor() -> f32 {
        // Disables the salience arm by default — TTL alone drives
        // expiration unless an operator opts in.
        0.0
    }
    pub const fn batch_size() -> u32 {
        128
    }
}

/// Validation errors raised by [`ExpirationConfig::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExpirationConfigError {
    /// `ttl_days` was zero — an expiry of "now" would purge the
    /// freshest writes the moment a sweep runs.
    #[error("expiration.ttl_days must be \u{2265} 1")]
    ZeroTtl,
    /// `salience_floor` outside the `[0.0, 1.0]` band.
    #[error("expiration.salience_floor {actual} outside [0.0, 1.0]")]
    SalienceOutOfRange {
        /// Provided value.
        actual: f32,
    },
    /// `batch_size` was zero — the handler would loop with no
    /// useful work.
    #[error("expiration.batch_size must be \u{2265} 1")]
    ZeroBatch,
}

impl ExpirationConfig {
    /// Validate semantic invariants the serde layer cannot express.
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), ExpirationConfigError> {
        if self.ttl_days == 0 {
            return Err(ExpirationConfigError::ZeroTtl);
        }
        if !(0.0..=1.0).contains(&self.salience_floor) || self.salience_floor.is_nan() {
            return Err(ExpirationConfigError::SalienceOutOfRange {
                actual: self.salience_floor,
            });
        }
        if self.batch_size == 0 {
            return Err(ExpirationConfigError::ZeroBatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_brief_p0() {
        let cfg = ExpirationConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.ttl_days, 30);
        assert!(cfg.salience_floor.abs() < f32::EPSILON);
        assert_eq!(cfg.batch_size, 128);
    }

    #[test]
    fn rejects_zero_ttl() {
        let cfg = ExpirationConfig {
            ttl_days: 0,
            ..ExpirationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ExpirationConfigError::ZeroTtl)
        ));
    }

    #[test]
    fn rejects_salience_out_of_range() {
        let cfg = ExpirationConfig {
            salience_floor: 1.5,
            ..ExpirationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ExpirationConfigError::SalienceOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_negative_salience() {
        let cfg = ExpirationConfig {
            salience_floor: -0.01,
            ..ExpirationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ExpirationConfigError::SalienceOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_nan_salience() {
        let cfg = ExpirationConfig {
            salience_floor: f32::NAN,
            ..ExpirationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ExpirationConfigError::SalienceOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_zero_batch() {
        let cfg = ExpirationConfig {
            batch_size: 0,
            ..ExpirationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ExpirationConfigError::ZeroBatch)
        ));
    }
}
