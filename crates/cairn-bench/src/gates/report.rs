//! Shared exit-code enum and human-summary writer for all gates.

use std::process::ExitCode;

/// Outcome of a single gate run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// All checks passed.
    Pass,
    /// One or more checks failed.
    Fail,
    /// Required input missing (baseline or manifest).
    MissingInput,
    /// Internal harness error.
    InternalError,
}

impl GateOutcome {
    /// Process exit code consumed by `main`.
    ///
    /// | Outcome        | Code |
    /// |----------------|------|
    /// | `Pass`         | `0`  |
    /// | `Fail`         | `1`  |
    /// | `MissingInput` | `2`  |
    /// | `InternalError`| `3`  |
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::MissingInput => 2,
            Self::InternalError => 3,
        }
    }

    /// Combine two outcomes, returning the one with the highest exit code.
    ///
    /// Used to aggregate results across multiple checks in a single gate run.
    #[must_use]
    pub fn worst_of(self, other: Self) -> Self {
        if self.exit_code() >= other.exit_code() {
            self
        } else {
            other
        }
    }
}

impl From<GateOutcome> for ExitCode {
    fn from(value: GateOutcome) -> Self {
        ExitCode::from(value.exit_code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_of_picks_higher_code() {
        assert_eq!(
            GateOutcome::Pass.worst_of(GateOutcome::Fail),
            GateOutcome::Fail
        );
        assert_eq!(
            GateOutcome::Fail.worst_of(GateOutcome::MissingInput),
            GateOutcome::MissingInput
        );
        assert_eq!(
            GateOutcome::Pass.worst_of(GateOutcome::Pass),
            GateOutcome::Pass
        );
    }

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(GateOutcome::Pass.exit_code(), 0);
        assert_eq!(GateOutcome::Fail.exit_code(), 1);
        assert_eq!(GateOutcome::MissingInput.exit_code(), 2);
        assert_eq!(GateOutcome::InternalError.exit_code(), 3);
    }
}
