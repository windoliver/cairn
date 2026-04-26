//! Shared helpers for building and emitting response envelopes.

use std::io::Write;

use cairn_core::generated::common::{Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{
    Response, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};

/// Generate a fresh Crockford base32 ULID suitable for `operation_id`.
#[must_use]
pub fn new_operation_id() -> Ulid {
    Ulid(ulid::Ulid::new().to_string())
}

/// Generate a fresh 16-byte nonce as standard base64 (24 chars with `==` padding).
///
/// Uses ULID bytes: 48-bit timestamp + 80-bit random component.
/// Adequate for P0 challenge correlation; replace with `rand::OsRng` when
/// the handshake challenge-response is wired to a real signature scheme.
#[must_use]
pub fn new_nonce() -> Nonce16Base64 {
    use base64::Engine as _;
    let raw = ulid::Ulid::new().0.to_be_bytes();
    Nonce16Base64(base64::engine::general_purpose::STANDARD.encode(raw))
}

/// Build an `Internal` aborted response for a verb that has no store wired yet.
#[must_use]
pub fn unimplemented_response(verb: ResponseVerb) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": "store not wired in this P0 build — verb dispatch lands in #9",
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

/// Serialize a value as compact JSON + newline to stdout.
pub fn emit_json<T: serde::Serialize>(value: &T) {
    let mut out = std::io::stdout().lock();
    let json = serde_json::to_string(value)
        .expect("invariant: generated types are always JSON-serializable");
    let _ = writeln!(out, "{json}");
}

/// Print a human-readable one-line error to stderr.
pub fn human_error(verb: &str, code: &str, message: &str, operation_id: &Ulid) {
    eprintln!("cairn {verb}: {code} — {message} (operation_id: {})", operation_id.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_operation_id_is_valid_ulid_shape() {
        let id = new_operation_id();
        assert_eq!(id.0.len(), 26, "ULID must be 26 chars: {:?}", id.0);
        assert!(
            id.0
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')),
            "ULID must be Crockford base32: {:?}",
            id.0
        );
    }

    #[test]
    fn new_nonce_is_valid_nonce16_shape() {
        let n = new_nonce();
        // Standard base64 for 16 bytes → 24 chars (22 significant + "==")
        assert_eq!(n.0.len(), 24, "nonce must be 24 chars: {:?}", n.0);
        assert!(
            n.0.ends_with("=="),
            "standard base64 for 16 bytes ends with ==: {:?}",
            n.0
        );
        // 22nd char (index 21) must be in [AQgw] — canonical 16-byte base64 last char
        let tail = n.0.as_bytes()[21];
        assert!(
            matches!(tail, b'A' | b'Q' | b'g' | b'w'),
            "nonce[21] must be in [AQgw], got: {:?}",
            tail as char
        );
    }

    #[test]
    fn two_operation_ids_differ() {
        let a = new_operation_id();
        let b = new_operation_id();
        assert_ne!(a.0, b.0, "consecutive operation_ids must differ");
    }

    #[test]
    fn two_nonces_differ() {
        let a = new_nonce();
        let b = new_nonce();
        assert_ne!(a.0, b.0, "consecutive nonces must differ");
    }

    #[test]
    fn unimplemented_response_fields_are_correct() {
        let resp = unimplemented_response(ResponseVerb::Ingest);
        assert_eq!(resp.contract, "cairn.mcp.v1");
        assert!(matches!(resp.status, ResponseStatus::Aborted));
        assert!(matches!(resp.verb, ResponseVerb::Ingest));
        assert!(resp.data.is_none());
        assert!(resp.target.is_none());
        let err = resp.error.expect("aborted response must have error");
        assert_eq!(err["code"], "Internal");
        assert!(!err["message"].as_str().unwrap_or("").is_empty());
    }
}
