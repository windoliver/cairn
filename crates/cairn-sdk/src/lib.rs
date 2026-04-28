//! `cairn-sdk` — typed in-process SDK surface for the cairn.mcp.v1 contract
//! (brief §8 four surfaces, §8.0.b envelope).
//!
//! The SDK is a thin wrapper over the same generated request/response types
//! consumed by the CLI and MCP adapter. It provides:
//!
//! - A typed function per verb (the eight P0 verbs) plus `status` and
//!   `handshake`.
//! - A typed [`SdkError`] enum so consumers never parse CLI text.
//! - Local in-process transport (no subprocess, no network) so the SDK can
//!   be embedded in the same binary as the verbs.
//!
//! `bootstrap` is intentionally **not** part of the SDK: its I/O lives in
//! `cairn-cli::vault` and is not yet exposed through `cairn-core`. Adding
//! it is tracked as a follow-up to #60.
//!
//! ## Status
//!
//! P0 wires the canonical `status` and `handshake` responses (matching the
//! CLI byte-for-byte). Capability-gated verbs (`search`, `retrieve`,
//! `forget`) reject with [`SdkError::CapabilityUnavailable`] when the
//! requested mode/variant is not advertised in [`Sdk::status`] — fail-closed
//! per CLAUDE.md §4.6. The remaining verbs return
//! [`SdkError::Internal`] with the `store not wired in this P0 build`
//! message so failures are distinguishable from capability skew. The verb
//! dispatch itself is tracked under parent epic #9. When verb handlers move
//! into `cairn-core::verbs::*`, every SDK fn becomes a one-line dispatch
//! into the same handler the CLI uses, preserving the "CLI is ground truth"
//! invariant.

#![warn(missing_docs)]

pub mod error;
pub mod transport;

mod stub;

pub use cairn_core::generated;
pub use error::SdkError;
pub use transport::{Sdk, Transport};

use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{ResponsePolicyTrace, ResponseVerb};

/// Successful verb response.
///
/// Carries the operation correlation ID, policy trace, and typed verb data.
/// The wire envelope's `contract`, `verb`, and `status` fields are implicit
/// (`cairn.mcp.v1`, the verb the SDK fn was called on, and `Ok` for the
/// `Ok(_)` arm).
#[derive(Debug, Clone, PartialEq)]
pub struct VerbResponse<D> {
    /// Correlation ID for tracing this call across surfaces.
    pub operation_id: Ulid,
    /// Privacy and policy decisions emitted while processing this call
    /// (brief §14). Empty in P0 stubs.
    pub policy_trace: Vec<ResponsePolicyTrace>,
    /// Verb the response is for. Always matches the SDK fn called.
    pub verb: ResponseVerb,
    /// Typed verb-specific data payload.
    pub data: D,
}

/// Crate version as reported by `Sdk::version`. Matches the `server_info.version`
/// field of [`status`](Sdk::status) so consumers can verify protocol parity.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Contract identifier this SDK speaks. Always matches the response envelope.
pub const CONTRACT: &str = "cairn.mcp.v1";
