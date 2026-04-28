//! Typed SDK error.
//!
//! Maps the wire envelope's `error` block (a free-form JSON object on the
//! response) into a closed Rust enum so consumers never parse CLI or MCP
//! text. Callers branch on [`SdkError`] variants and, for wire-level
//! protocol failures, on the typed [`ErrorCode`] inside the [`SdkError::Protocol`]
//! variant — the same closed enum the IDL generates from `errors/error.json`.

use cairn_core::generated::common::Ulid;
pub use cairn_core::generated::errors::ErrorCode;

/// All errors a [`crate::Sdk`] call can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SdkError {
    /// The request arguments failed validation before any side effect.
    ///
    /// Typically the IDL `oneOf` constraint (e.g. `ingest` requires exactly
    /// one of body/file/url) or a closed-grammar wire constraint surfaced
    /// by the generated `TryFrom<Raw...>` deserializer (non-empty strings,
    /// numeric ranges, etc.). No `operation_id` is minted because no
    /// envelope was produced.
    #[error("invalid args: {reason}")]
    InvalidArgs {
        /// Human-readable reason. Stable enough for logging, not for
        /// programmatic dispatch — match on the variant instead.
        reason: String,
    },

    /// The verb requested a mode that is not advertised in `status`
    /// (brief §8.0.a fail-closed). The SDK refuses to dispatch.
    ///
    /// This is a structured shorthand for
    /// [`SdkError::Protocol`] with `code = ErrorCode::CapabilityUnavailable`,
    /// kept as a top-level variant so the common fail-closed branch is a
    /// one-arm match.
    #[error("capability unavailable: {capability} ({reason})")]
    CapabilityUnavailable {
        /// The fully-qualified capability identifier (e.g. `cairn.mcp.v1.search.semantic`).
        capability: String,
        /// Why the capability is unavailable in this incarnation.
        reason: String,
        /// Operation correlation ID for log lookup.
        operation_id: Ulid,
    },

    /// Wire-level protocol error from the verb handler.
    ///
    /// The `code` is the closed [`ErrorCode`] enum lowered from the IDL,
    /// so callers can branch deterministically on `Unauthorized`,
    /// `ReplayDetected`, `NotFound`, `ConflictVersion`, etc. without
    /// parsing the human-readable message. New codes are additive — both
    /// `SdkError` and `ErrorCode` are `#[non_exhaustive]`.
    #[error("{code:?}: {message}")]
    Protocol {
        /// Typed wire error code.
        code: ErrorCode,
        /// Human-readable message; intended for logs, not dispatch.
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
            | Self::Protocol { operation_id, .. } => Some(operation_id),
        }
    }

    /// Typed wire code, when this error originated from a protocol response.
    /// `InvalidArgs` is pre-envelope and returns `None`.
    #[must_use]
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::InvalidArgs { .. } => None,
            Self::CapabilityUnavailable { .. } => Some(ErrorCode::CapabilityUnavailable),
            Self::Protocol { code, .. } => Some(*code),
        }
    }
}
