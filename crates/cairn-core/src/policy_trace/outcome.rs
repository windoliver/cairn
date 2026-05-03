//! Outcome of a single gate evaluation. Mirrors
//! `ResponsePolicyTraceResult` on the wire.

use std::fmt;

/// Three-valued outcome: `Pass` (gate ran, allowed), `Deny` (gate ran,
/// blocked), `Error` (gate evaluation itself failed mid-flight). Maps
/// 1:1 to `cairn_core::generated::envelope::ResponsePolicyTraceResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PolicyOutcome {
    /// Gate ran and allowed the request through.
    Pass,
    /// Gate ran and blocked the request.
    Deny,
    /// Gate evaluation itself failed mid-flight.
    Error,
}

impl PolicyOutcome {
    /// Returns the canonical lowercase wire string for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Deny => "deny",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for PolicyOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
