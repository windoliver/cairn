//! Wire-error mapping: [`crate::domain::DomainError`] →
//! [`ErrorBody`] (the typed Rust counterpart of the JSON object that
//! the generated `Response.error` deserializer validates structurally
//! via `validate_error_envelope`).
//!
//! Single source of truth for error envelopes across CLI / MCP / SDK.

use serde::Serialize;

use crate::domain::DomainError;
use crate::generated::errors::ErrorCode;

/// Typed wire error envelope.
///
/// Serialises into `{"code": "...", "message": "...", "data": {...}}` —
/// the same shape `validate_error_envelope` enforces. `data` is omitted
/// when absent rather than serialised as `null`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorBody {
    /// Closed-enum string from [`ErrorCode`].
    #[serde(serialize_with = "serialize_error_code")]
    pub code: ErrorCode,
    /// Human-readable summary; never empty.
    pub message: String,
    /// Per-code structured payload. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// `serde(serialize_with = ...)` requires the `&T` signature; can't take by value here.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn serialize_error_code<S: serde::Serializer>(code: &ErrorCode, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(code.as_str())
}

/// Map a [`DomainError`] from the verifier (or a deserializer reject) to
/// a typed [`ErrorBody`]. Variants the verifier itself does not produce
/// fall through to `InvalidArgs` with the error's `Display` as `reason`.
#[must_use]
pub fn envelope_error_for(err: &DomainError) -> ErrorBody {
    match err {
        DomainError::InvalidSignature | DomainError::MissingSignature { .. } => ErrorBody {
            code: ErrorCode::MissingSignature,
            message: err.to_string(),
            data: None,
        },

        DomainError::ExpiredIntent {
            issued_at,
            expires_at,
            now,
        } => ErrorBody {
            code: ErrorCode::ExpiredIntent,
            message: err.to_string(),
            data: Some(serde_json::json!({
                "issued_at": issued_at,
                "expires_at": expires_at,
                "now": now,
            })),
        },

        DomainError::RevokedKey { .. } | DomainError::KeyVersionMismatch { .. } => ErrorBody {
            code: ErrorCode::RevokedKey,
            message: err.to_string(),
            data: None,
        },

        DomainError::ScopeDenied { .. } | DomainError::Unauthorized { .. } => ErrorBody {
            code: ErrorCode::Unauthorized,
            message: err.to_string(),
            data: None,
        },

        // Fall-through: every remaining DomainError variant maps to
        // InvalidArgs with the error's Display in the reason field.
        other => ErrorBody {
            code: ErrorCode::InvalidArgs,
            message: other.to_string(),
            data: Some(serde_json::json!({
                "field": "envelope",
                "reason": other.to_string(),
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Identity;
    use crate::domain::identity::keys::KeyVersion;
    use crate::domain::identity::records::ProvisioningState;

    #[test]
    fn invalid_signature_maps_to_missing_signature() {
        let body = envelope_error_for(&DomainError::InvalidSignature);
        assert!(matches!(body.code, ErrorCode::MissingSignature));
        assert!(body.data.is_none());
        assert!(!body.message.is_empty());
    }

    #[test]
    fn expired_intent_carries_iso_data() {
        let err = DomainError::ExpiredIntent {
            issued_at: "2026-04-22T14:02:11Z".into(),
            expires_at: "2026-04-22T14:07:11Z".into(),
            now: "2026-04-22T15:00:00Z".into(),
        };
        let body = envelope_error_for(&err);
        let data = body.data.unwrap();
        assert_eq!(
            data.get("now").unwrap().as_str().unwrap(),
            "2026-04-22T15:00:00Z"
        );
    }

    #[test]
    fn revoked_key_has_no_data() {
        let body = envelope_error_for(&DomainError::RevokedKey {
            id: Identity::parse("hmn:tafeng").unwrap(),
            state: ProvisioningState::Revoked,
        });
        assert!(matches!(body.code, ErrorCode::RevokedKey));
        assert!(body.data.is_none());
    }

    #[test]
    fn key_version_mismatch_collapses_to_revoked_key_at_p0() {
        let body = envelope_error_for(&DomainError::KeyVersionMismatch {
            intent: KeyVersion::FIRST,
            current: None,
        });
        assert!(matches!(body.code, ErrorCode::RevokedKey));
    }

    #[test]
    fn scope_denied_maps_to_unauthorized() {
        let body = envelope_error_for(&DomainError::ScopeDenied {
            message: "tenant".into(),
        });
        assert!(matches!(body.code, ErrorCode::Unauthorized));
    }

    #[test]
    fn unknown_variant_falls_through_to_invalid_args() {
        let body = envelope_error_for(&DomainError::InvalidIdentity {
            message: "bad".into(),
        });
        assert!(matches!(body.code, ErrorCode::InvalidArgs));
        let data = body.data.unwrap();
        assert!(data.get("field").is_some());
        assert!(data.get("reason").is_some());
    }
}
