//! Typed SDK error.
//!
//! Maps the wire envelope's `error` block (a free-form JSON object on the
//! response) into a closed Rust enum so consumers never parse CLI or MCP
//! text. New variants are additive — the enum is `#[non_exhaustive]`.

use cairn_core::generated::common::Ulid;

/// All errors a [`crate::Sdk`] call can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SdkError {
    /// The request arguments failed validation before any side effect.
    ///
    /// Typically the IDL `oneOf` constraint (e.g. `ingest` requires exactly
    /// one of body/file/url) or a kind enum mismatch.
    #[error("invalid args: {reason}")]
    InvalidArgs {
        /// Human-readable reason. Stable enough for logging, not for
        /// programmatic dispatch — match on the variant instead.
        reason: String,
    },

    /// The verb requested a mode that is not advertised in `status`
    /// (brief §8.0.a fail-closed). The SDK refuses to dispatch.
    #[error("capability unavailable: {capability} ({reason})")]
    CapabilityUnavailable {
        /// The fully-qualified capability identifier (e.g. `cairn.mcp.v1.search.semantic`).
        capability: String,
        /// Why the capability is unavailable in this incarnation.
        reason: String,
        /// Operation correlation ID for log lookup.
        operation_id: Ulid,
    },

    /// An internal error from the verb handler. P0 stubs return this
    /// variant pending the store wiring in #9.
    #[error("internal: {code} — {message}")]
    Internal {
        /// Stable error code from the wire envelope (e.g. `Internal`,
        /// `NotFound`, `Conflict`). Free-form on the wire by §8.0.b.
        code: String,
        /// Human-readable message.
        message: String,
        /// Operation correlation ID for log lookup.
        operation_id: Ulid,
    },
}

impl SdkError {
    /// Operation ID associated with this error, when one was minted.
    /// `InvalidArgs` is rejected before envelope construction so it has none.
    #[must_use]
    pub fn operation_id(&self) -> Option<&Ulid> {
        match self {
            Self::InvalidArgs { .. } => None,
            Self::CapabilityUnavailable { operation_id, .. }
            | Self::Internal { operation_id, .. } => Some(operation_id),
        }
    }
}
