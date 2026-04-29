//! [`PolicyTraceEntry`] and the [`to_wire`] mapping into the generated
//! [`ResponsePolicyTrace`] shape. Pure functions; no I/O.

use crate::generated::envelope::{ResponsePolicyTrace, ResponsePolicyTraceResult};

use super::{PolicyDetail, PolicyGate, PolicyOutcome};

/// One gate outcome. Verbs build a `Vec<PolicyTraceEntry>` and call
/// [`to_wire`] at the envelope boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTraceEntry {
    /// The gate that ran.
    pub gate: PolicyGate,
    /// `Pass`, `Deny`, or `Error` — the outcome of the gate.
    pub outcome: PolicyOutcome,
    /// Body-free metadata for this outcome.
    pub detail: PolicyDetail,
}

impl PolicyTraceEntry {
    /// Construct an entry with explicit `gate`, `outcome`, and `detail`.
    #[must_use]
    pub const fn new(gate: PolicyGate, outcome: PolicyOutcome, detail: PolicyDetail) -> Self {
        Self {
            gate,
            outcome,
            detail,
        }
    }

    /// `(gate, Pass, None)` — the most common shape.
    #[must_use]
    pub const fn pass(gate: PolicyGate) -> Self {
        Self::new(gate, PolicyOutcome::Pass, PolicyDetail::None)
    }

    /// `(gate, Deny, detail)`.
    #[must_use]
    pub const fn deny(gate: PolicyGate, detail: PolicyDetail) -> Self {
        Self::new(gate, PolicyOutcome::Deny, detail)
    }

    /// `(gate, Error, ErrorCode(code))`.
    #[must_use]
    pub const fn error(gate: PolicyGate, code: &'static str) -> Self {
        Self::new(gate, PolicyOutcome::Error, PolicyDetail::ErrorCode(code))
    }
}

/// Pure mapping from producer-side trace entries to the wire shape.
/// Empty `detail` strings collapse to `None` per the IDL's
/// `skip_serializing_if`.
#[must_use]
pub fn to_wire(entries: &[PolicyTraceEntry]) -> Vec<ResponsePolicyTrace> {
    entries.iter().map(to_wire_one).collect()
}

fn to_wire_one(entry: &PolicyTraceEntry) -> ResponsePolicyTrace {
    let result = match entry.outcome {
        PolicyOutcome::Pass => ResponsePolicyTraceResult::Pass,
        PolicyOutcome::Deny => ResponsePolicyTraceResult::Deny,
        PolicyOutcome::Error => ResponsePolicyTraceResult::Error,
    };
    let detail_str = entry.detail.to_wire_string();
    let detail = if detail_str.is_empty() {
        None
    } else {
        Some(detail_str)
    };
    ResponsePolicyTrace {
        gate: entry.gate.as_str().to_owned(),
        result,
        detail,
    }
}
