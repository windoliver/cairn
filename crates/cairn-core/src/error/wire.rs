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

        DomainError::RevokedKey {
            id, key_version, ..
        } => ErrorBody {
            code: ErrorCode::RevokedKey,
            message: err.to_string(),
            data: Some(serde_json::json!({
                "issuer": id.as_str(),
                "key_version": i64::from(key_version.as_u32()),
            })),
        },

        DomainError::KeyVersionMismatch { id, intent, .. } => ErrorBody {
            code: ErrorCode::RevokedKey,
            message: err.to_string(),
            data: Some(serde_json::json!({
                "issuer": id.as_str(),
                "key_version": i64::from(intent.as_u32()),
            })),
        },

        // Brief §14 (privacy by construction): the public envelope must
        // not enumerate vault scope policy or registry detail to a
        // (possibly authenticated-but-unauthorized) caller. Emit a stable
        // generic `required` value; raw detail is logged server-side via
        // `tracing::debug!` so operators can still diagnose.
        DomainError::ScopeDenied { message } => {
            tracing::debug!(detail = %message, "scope denied");
            ErrorBody {
                code: ErrorCode::Unauthorized,
                message: "scope denied".to_owned(),
                data: Some(serde_json::json!({ "required": "scope" })),
            }
        }
        DomainError::Unauthorized { message } => {
            tracing::debug!(detail = %message, "unauthorized");
            ErrorBody {
                code: ErrorCode::Unauthorized,
                message: "unauthorized".to_owned(),
                data: Some(serde_json::json!({ "required": "authorization" })),
            }
        }

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
    fn revoked_key_carries_issuer_and_key_version() {
        let body = envelope_error_for(&DomainError::RevokedKey {
            id: Identity::parse("hmn:tafeng").unwrap(),
            key_version: KeyVersion::FIRST,
            state: ProvisioningState::Revoked,
        });
        assert!(matches!(body.code, ErrorCode::RevokedKey));
        let data = body.data.unwrap();
        assert_eq!(data.get("issuer").unwrap().as_str().unwrap(), "hmn:tafeng");
        assert_eq!(data.get("key_version").unwrap().as_i64().unwrap(), 1);
    }

    #[test]
    fn key_version_mismatch_collapses_to_revoked_key_at_p0() {
        let body = envelope_error_for(&DomainError::KeyVersionMismatch {
            id: Identity::parse("hmn:tafeng").unwrap(),
            intent: KeyVersion::FIRST,
            current: None,
        });
        assert!(matches!(body.code, ErrorCode::RevokedKey));
        let data = body.data.unwrap();
        assert_eq!(data.get("issuer").unwrap().as_str().unwrap(), "hmn:tafeng");
        assert_eq!(data.get("key_version").unwrap().as_i64().unwrap(), 1);
    }

    #[test]
    fn scope_denied_returns_generic_required_no_policy_leak() {
        // Brief §14: the public envelope must not embed expected
        // tenant/workspace/tier values from vault policy.
        let body = envelope_error_for(&DomainError::ScopeDenied {
            message: "tenant: expected acme-internal, got other-corp".into(),
        });
        assert!(matches!(body.code, ErrorCode::Unauthorized));
        assert_eq!(body.message, "scope denied");
        let data = body.data.as_ref().unwrap();
        assert_eq!(data.get("required").unwrap().as_str().unwrap(), "scope");
        // Defense-in-depth: assert raw detail is absent from the wire body.
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("acme-internal") && !serialized.contains("other-corp"),
            "policy detail leaked into wire body: {serialized}"
        );
    }

    #[test]
    fn unauthorized_returns_generic_required_no_identity_leak() {
        let body = envelope_error_for(&DomainError::Unauthorized {
            message: "envelope issuer hmn:alice does not match resolved hmn:bob".into(),
        });
        assert!(matches!(body.code, ErrorCode::Unauthorized));
        assert_eq!(body.message, "unauthorized");
        let data = body.data.as_ref().unwrap();
        assert_eq!(
            data.get("required").unwrap().as_str().unwrap(),
            "authorization"
        );
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("hmn:alice") && !serialized.contains("hmn:bob"),
            "identity detail leaked into wire body: {serialized}"
        );
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

    #[test]
    fn round_trips_through_response_validator() {
        // For each variant the verifier can produce, build an ErrorBody,
        // serialize, and embed in a Response that we deserialize back —
        // this exercises validate_error_envelope in the generated module.
        let cases = vec![
            envelope_error_for(&DomainError::InvalidSignature),
            envelope_error_for(&DomainError::ExpiredIntent {
                issued_at: "2026-04-22T14:02:11Z".into(),
                expires_at: "2026-04-22T14:07:11Z".into(),
                now: "2026-04-22T15:00:00Z".into(),
            }),
            envelope_error_for(&DomainError::RevokedKey {
                id: Identity::parse("hmn:tafeng").unwrap(),
                key_version: KeyVersion::FIRST,
                state: ProvisioningState::Revoked,
            }),
            envelope_error_for(&DomainError::KeyVersionMismatch {
                id: Identity::parse("hmn:tafeng").unwrap(),
                intent: KeyVersion::FIRST,
                current: None,
            }),
            envelope_error_for(&DomainError::ScopeDenied {
                message: "tenant: expected acme, got other".into(),
            }),
            envelope_error_for(&DomainError::Unauthorized {
                message: "issuer mismatch".into(),
            }),
        ];

        for body in cases {
            let payload = serde_json::json!({
                "contract": "cairn.mcp.v1",
                "operation_id": "01HQZX9F5N0000000000000000",
                "policy_trace": [],
                "status": "rejected",
                "verb": "ingest",
                "error": serde_json::to_value(&body).unwrap(),
            });
            let result: Result<crate::generated::envelope::Response, _> =
                serde_json::from_value(payload);
            assert!(
                result.is_ok(),
                "ErrorBody {body:?} failed Response deserialization: {:?}",
                result.err()
            );
        }
    }
}
