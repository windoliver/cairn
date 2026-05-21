//! `FailureClass` — typed taxonomy for workflow failure reasons.
//!
//! See spec §4.2. Handlers may return `Transient | Validation | Poison`;
//! the scheduler stamps `Timeout | LeaseLost`. `Validation` and `Poison`
//! always force terminal disposition regardless of handler-supplied
//! `FailDisposition`.

use serde::{Deserialize, Serialize};

/// Classification of why a workflow job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureClass {
    /// Network blip, lock contention, transient I/O — retry helps.
    Transient,
    /// Bad payload, schema mismatch — retry will not help. Terminal.
    Validation,
    /// Repeated same error across attempts — terminal.
    Poison,
    /// Handler exceeded lease deadline — scheduler-stamped.
    Timeout,
    /// Heartbeat lost, watchdog fired — scheduler-stamped.
    LeaseLost,
}

impl FailureClass {
    /// `true` iff the scheduler must force `FailDisposition::Permanent`
    /// regardless of handler-supplied disposition.
    #[must_use]
    pub const fn forces_permanent(self) -> bool {
        matches!(self, Self::Validation | Self::Poison)
    }

    /// `true` iff this class is only legal from the scheduler (handlers
    /// must not return it).
    #[must_use]
    pub const fn is_scheduler_only(self) -> bool {
        matches!(self, Self::Timeout | Self::LeaseLost)
    }

    /// Snake-case wire string used by the `SQLite` adapter for the
    /// `failure_class` column and by `MetricEvent` serialization. Kept
    /// in lockstep with the `serde(rename_all = "snake_case")` derive
    /// so the two surfaces never drift.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Validation => "validation",
            Self::Poison => "poison",
            Self::Timeout => "timeout",
            Self::LeaseLost => "lease_lost",
        }
    }
}

impl std::str::FromStr for FailureClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "transient" => Self::Transient,
            "validation" => Self::Validation,
            "poison" => Self::Poison,
            "timeout" => Self::Timeout,
            "lease_lost" => Self::LeaseLost,
            other => return Err(format!("unknown failure_class: {other}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_permanent_matrix() {
        assert!(!FailureClass::Transient.forces_permanent());
        assert!(FailureClass::Validation.forces_permanent());
        assert!(FailureClass::Poison.forces_permanent());
        assert!(!FailureClass::Timeout.forces_permanent());
        assert!(!FailureClass::LeaseLost.forces_permanent());
    }

    #[test]
    fn is_scheduler_only_matrix() {
        assert!(!FailureClass::Transient.is_scheduler_only());
        assert!(!FailureClass::Validation.is_scheduler_only());
        assert!(!FailureClass::Poison.is_scheduler_only());
        assert!(FailureClass::Timeout.is_scheduler_only());
        assert!(FailureClass::LeaseLost.is_scheduler_only());
    }

    #[test]
    fn as_str_from_str_round_trip() {
        use std::str::FromStr as _;
        for c in [
            FailureClass::Transient,
            FailureClass::Validation,
            FailureClass::Poison,
            FailureClass::Timeout,
            FailureClass::LeaseLost,
        ] {
            let s = c.as_str();
            let back = FailureClass::from_str(s).expect("round trip");
            assert_eq!(c, back, "as_str/from_str disagree for {c:?}");
        }
        assert_eq!(FailureClass::LeaseLost.as_str(), "lease_lost");
        assert!(FailureClass::from_str("unknown").is_err());
    }

    #[test]
    fn as_str_matches_serde_snake_case() {
        // The wire form used by the `SQLite` adapter must match the
        // serde-encoded form so a row written via `as_str()` round-trips
        // back through `serde_json::from_str` without churn.
        for c in [
            FailureClass::Transient,
            FailureClass::Validation,
            FailureClass::Poison,
            FailureClass::Timeout,
            FailureClass::LeaseLost,
        ] {
            let json = serde_json::to_string(&c).expect("serialize");
            // serde wraps in quotes — strip them for byte-equality.
            let stripped = json.trim_matches('"');
            assert_eq!(stripped, c.as_str(), "drift for {c:?}");
        }
    }

    #[test]
    fn json_round_trip_snake_case() {
        for c in [
            FailureClass::Transient,
            FailureClass::Validation,
            FailureClass::Poison,
            FailureClass::Timeout,
            FailureClass::LeaseLost,
        ] {
            let j = serde_json::to_string(&c).expect("serialize");
            let back: FailureClass = serde_json::from_str(&j).expect("deserialize");
            assert_eq!(c, back, "round trip for {c:?}");
        }
        assert_eq!(
            serde_json::to_string(&FailureClass::LeaseLost).expect("serialize"),
            "\"lease_lost\""
        );
    }
}
