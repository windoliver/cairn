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
//! [`SdkError::Unimplemented`] with the `store not wired in this P0 build`
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
use cairn_core::generated::envelope::{
    ResponsePolicyTrace, ResponseStatus, ResponseTarget, ResponseVerb,
};

/// Successful verb response.
///
/// Serializes to the canonical wire envelope (brief §8.0.b): the JSON
/// shape includes `contract` (`cairn.mcp.v1`), `status` (`committed`),
/// `verb`, `operation_id`, `policy_trace`, and `data`. CLI / MCP / SDK
/// therefore emit byte-compatible success envelopes for the same
/// operation.
#[derive(Debug, Clone, PartialEq)]
pub struct VerbResponse<D> {
    /// Correlation ID for tracing this call across surfaces.
    pub operation_id: Ulid,
    /// Privacy and policy decisions emitted while processing this call
    /// (brief §14). Empty in P0 stubs.
    pub policy_trace: Vec<ResponsePolicyTrace>,
    /// Verb the response is for. Always matches the SDK fn called.
    pub verb: ResponseVerb,
    /// Retrieve-only discriminator echoing the requested target (record,
    /// session, turn, folder, scope, profile). The wire envelope requires
    /// this field on every committed `verb=retrieve` response and forbids
    /// it elsewhere; SDK retrieve responses populate it from the
    /// [`generated::verbs::retrieve::RetrieveArgs`] variant.
    pub target: Option<ResponseTarget>,
    /// Typed verb-specific data payload.
    pub data: D,
}

impl<D: serde::Serialize> serde::Serialize for VerbResponse<D> {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error as _, SerializeStruct as _};
        // Wire envelope invariant (cairn_core::generated::envelope::Response
        // RawResponse::TryFrom): `target` is required when verb=retrieve
        // with status=committed, and forbidden otherwise. Reject the
        // malformed permutations here so SDK consumers cannot emit
        // envelope-invalid traffic, even when constructing
        // `VerbResponse` by hand.
        // `unknown` is reserved for the rejected/UnknownVerb arm; a
        // committed success envelope must name a real verb.
        if matches!(self.verb, ResponseVerb::Unknown) {
            return Err(S::Error::custom(
                "verb=unknown is only valid on rejected responses",
            ));
        }
        let is_retrieve = matches!(self.verb, ResponseVerb::Retrieve);
        match (is_retrieve, self.target.is_some()) {
            (true, false) => {
                return Err(S::Error::custom(
                    "verb=retrieve requires `target` on committed responses",
                ));
            }
            (false, true) => {
                return Err(S::Error::custom(
                    "`target` is forbidden when verb is not retrieve",
                ));
            }
            _ => {}
        }
        let len = 6 + usize::from(self.target.is_some());
        let mut s = ser.serialize_struct("Response", len)?;
        s.serialize_field("contract", CONTRACT)?;
        s.serialize_field("status", &ResponseStatus::Committed)?;
        s.serialize_field("verb", &self.verb)?;
        s.serialize_field("operation_id", &self.operation_id)?;
        s.serialize_field("policy_trace", &self.policy_trace)?;
        if let Some(target) = &self.target {
            s.serialize_field("target", target)?;
        }
        s.serialize_field("data", &self.data)?;
        s.end()
    }
}

/// Crate version as reported by `Sdk::version`. Matches the `server_info.version`
/// field of [`status`](Sdk::status) so consumers can verify protocol parity.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Contract identifier this SDK speaks. Always matches the response envelope.
pub const CONTRACT: &str = "cairn.mcp.v1";
