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
///
/// Brief §14 (privacy by construction) closure: pre-authentication
/// failures (`InvalidSignature`, `KeyVersionMismatch`) are collapsed
/// to a single generic `Unauthorized` shape so an attacker cannot tell
/// "unknown issuer" from "wrong key version" from "bad signature".
/// `RevokedKey` only originates from the post-authentication lifecycle
/// check, so it remains informative for the (already-authenticated)
/// caller.
#[must_use]
pub fn envelope_error_for(err: &DomainError) -> ErrorBody {
    match err {
        // Pre-auth oracle closure: emit identical wire shape so unknown
        // issuer / wrong key_version / invalid signature are
        // indistinguishable to an unauthenticated probe. Raw detail is
        // preserved server-side via `tracing::debug!` for ops.
        DomainError::InvalidSignature | DomainError::MissingSignature { .. } => {
            tracing::debug!(detail = %err, "pre-auth: signature failure");
            ErrorBody {
                code: ErrorCode::Unauthorized,
                message: "unauthorized".to_owned(),
                data: Some(serde_json::json!({ "required": "authentication" })),
            }
        }

        DomainError::KeyVersionMismatch { .. } => {
            tracing::debug!(detail = %err, "pre-auth: key version mismatch");
            ErrorBody {
                code: ErrorCode::Unauthorized,
                message: "unauthorized".to_owned(),
                data: Some(serde_json::json!({ "required": "authentication" })),
            }
        }

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

        // Post-authentication lifecycle outcome — caller has proven
        // control of the issuer key, so surfacing identity + key_version
        // (both already known to them) is appropriate.
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
        // `Unauthorized` is raised pre-auth by `resolve_issuer` (unknown
        // identity, registry backend error) and by the verifier's
        // issuer-match / key-version checks before signature verification.
        // Collapse to the same body as `InvalidSignature` /
        // `KeyVersionMismatch` so a pre-auth probe cannot tell unknown
        // issuer from wrong key from bad signature.
        DomainError::Unauthorized { message } => {
            tracing::debug!(detail = %message, "pre-auth: unauthorized");
            ErrorBody {
                code: ErrorCode::Unauthorized,
                message: "unauthorized".to_owned(),
                data: Some(serde_json::json!({ "required": "authentication" })),
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
    fn invalid_signature_collapses_to_generic_unauthorized() {
        // Pre-auth oracle closure: bad signature is indistinguishable
        // from unknown issuer / wrong key_version on the wire.
        let body = envelope_error_for(&DomainError::InvalidSignature);
        assert!(matches!(body.code, ErrorCode::Unauthorized));
        assert_eq!(body.message, "unauthorized");
        let data = body.data.as_ref().unwrap();
        assert_eq!(
            data.get("required").unwrap().as_str().unwrap(),
            "authentication"
        );
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
    fn key_version_mismatch_collapses_to_generic_unauthorized() {
        // Pre-auth oracle closure: KeyVersionMismatch (raised by
        // resolve_issuer before any signature is verified) must not
        // leak identity, key_version, or registry's current version.
        let body = envelope_error_for(&DomainError::KeyVersionMismatch {
            id: Identity::parse("hmn:tafeng").unwrap(),
            intent: KeyVersion::FIRST,
            current: Some(KeyVersion::new(std::num::NonZeroU32::new(7).unwrap())),
        });
        assert!(matches!(body.code, ErrorCode::Unauthorized));
        assert_eq!(body.message, "unauthorized");
        let data = body.data.as_ref().unwrap();
        assert_eq!(
            data.get("required").unwrap().as_str().unwrap(),
            "authentication"
        );
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("hmn:tafeng") && !serialized.contains('7'),
            "identity / current key_version leaked into wire body: {serialized}"
        );
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
    fn unauthorized_collapses_to_generic_authentication_no_identity_leak() {
        // `Unauthorized` is pre-auth (raised by resolve_issuer before any
        // signature is verified). Must serialize byte-identically to
        // InvalidSignature / KeyVersionMismatch so the wire body cannot
        // be used as an oracle to enumerate known vs unknown issuers.
        let body = envelope_error_for(&DomainError::Unauthorized {
            message: "envelope issuer hmn:alice does not match resolved hmn:bob".into(),
        });
        assert!(matches!(body.code, ErrorCode::Unauthorized));
        assert_eq!(body.message, "unauthorized");
        let data = body.data.as_ref().unwrap();
        assert_eq!(
            data.get("required").unwrap().as_str().unwrap(),
            "authentication"
        );
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("hmn:alice") && !serialized.contains("hmn:bob"),
            "identity detail leaked into wire body: {serialized}"
        );
    }

    #[test]
    fn all_pre_auth_failures_are_byte_identical_on_the_wire() {
        // Brief §14: an unauthenticated probe must not be able to
        // distinguish unknown issuer / wrong key_version / invalid
        // signature / mismatched issuer from one another. Every pre-auth
        // failure path produces the exact same serialized body — only
        // tracing logs reveal the cause.
        let bad_sig =
            serde_json::to_string(&envelope_error_for(&DomainError::InvalidSignature)).unwrap();
        let wrong_kv =
            serde_json::to_string(&envelope_error_for(&DomainError::KeyVersionMismatch {
                id: Identity::parse("hmn:victim").unwrap(),
                intent: KeyVersion::FIRST,
                current: Some(KeyVersion::new(std::num::NonZeroU32::new(9).unwrap())),
            }))
            .unwrap();
        let unknown_issuer =
            serde_json::to_string(&envelope_error_for(&DomainError::Unauthorized {
                message: "identity hmn:victim not in registry".into(),
            }))
            .unwrap();
        let mismatched_issuer =
            serde_json::to_string(&envelope_error_for(&DomainError::Unauthorized {
                message: "envelope issuer hmn:victim does not match resolved hmn:user".into(),
            }))
            .unwrap();

        assert_eq!(
            bad_sig, wrong_kv,
            "InvalidSignature and KeyVersionMismatch must be wire-identical"
        );
        assert_eq!(
            bad_sig, unknown_issuer,
            "InvalidSignature and Unauthorized(unknown issuer) must be wire-identical"
        );
        assert_eq!(
            bad_sig, mismatched_issuer,
            "InvalidSignature and Unauthorized(mismatched issuer) must be wire-identical"
        );
        // Defense-in-depth: assert no leaky identifier escapes any path.
        for body in [&bad_sig, &wrong_kv, &unknown_issuer, &mismatched_issuer] {
            assert!(!body.contains("hmn:victim"));
            assert!(!body.contains("hmn:user"));
        }
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
