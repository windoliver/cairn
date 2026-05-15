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
