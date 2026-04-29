//! Policy trace — typed gate vocabulary and outcome shapes for the
//! `policy_trace` field on every verb response (brief §8.0.b, §14, §5.2).
//!
//! The module is producer-side only: closed enums for gates and outcomes,
//! body-free metadata for `detail`, and a pure `to_wire` mapping into the
//! generated `ResponsePolicyTrace` shape. Verbs compose gates as today,
//! push `PolicyTraceEntry` values onto a local vector, and call `to_wire`
//! at the envelope boundary.
//!
//! No I/O. No store dependency. No allocations beyond what `Vec<…>` and
//! `BTreeMap<…>` require for the trace itself.

mod gate;

pub use gate::PolicyGate;
