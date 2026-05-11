//! Shared helpers for building and emitting response envelopes.

use std::io::Write;

use cairn_core::generated::common::{Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{
    Response, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};

/// `sysexits.h` `EX_UNAVAILABLE`.
pub const EX_UNAVAILABLE: u8 = 69;

/// Generate a fresh Crockford base32 ULID suitable for `operation_id`.
#[must_use]
pub fn new_operation_id() -> Ulid {
    cairn_core::time::new_operation_id()
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

/// Build an `InvalidArgs` rejected response with structured error data.
///
/// Use for fail-fast argument validation that occurs before dispatch
/// (e.g. an empty `query` rejected before the store is opened). The
/// response shape mirrors the dispatcher's `InvalidArgs` mapping so
/// generated clients can parse a single canonical envelope rather than
/// switching on origin. CLI callers should pair this with
/// `ExitCode::from(64)` (`EX_USAGE`).
#[must_use]
pub fn invalid_args_response(verb: ResponseVerb, field: &str, reason: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "InvalidArgs",
            "message": format!("invalid args: {field}: {reason}"),
            "data": { "field": field, "reason": reason },
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb,
    }
}

/// Build an `InvalidFilter` rejected response with structured error data.
///
/// Use for `search.filters` parse/validation failures. CLI callers should
/// pair this with `ExitCode::from(64)` (`EX_USAGE`).
#[must_use]
pub fn invalid_filter_response(verb: ResponseVerb, reason: &str, path: Option<&str>) -> Response {
    let mut data = serde_json::json!({ "reason": reason });
    if let Some(path) = path
        && !path.is_empty()
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert(
            "path".to_owned(),
            serde_json::Value::String(path.to_owned()),
        );
    }
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "InvalidFilter",
            "message": format!("invalid filter: {reason}"),
            "data": data,
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Rejected,
        target: None,
        verb,
    }
}

/// Build a `NotFound` aborted response with structured error data.
///
/// Use for resource-discovery failures that occur before dispatch — the
/// canonical example is the verb's vault target not being a Cairn vault
/// (no `.cairn/vault.id`). `target` becomes the error's `data.target`
/// field; `message` is the human-readable explanation. CLI callers
/// should pair this with `ExitCode::from(78)` (`EX_CONFIG`).
///
/// `status = Aborted` because the IDL `errors/error.json` only admits
/// `NotFound` in the aborted family — generated clients deserialize
/// the envelope through that constraint and reject `Rejected + NotFound`
/// at parse time (round-7 review #1).
#[must_use]
pub fn not_found_response(verb: ResponseVerb, target: &str, message: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "NotFound",
            "message": message,
            "data": { "target": target },
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

/// Build an `Internal` aborted response with a one-line message.
///
/// Use for fail-closed paths whose root cause is local environment
/// corruption (a damaged vault.id sentinel, a config that fails IDL
/// validation, etc.) — the IDL `Internal` family is the catch-all.
/// CLI callers should pair this with `ExitCode::from(78)` (`EX_CONFIG`)
/// when the cause is config-level rather than transient runtime state.
#[must_use]
pub fn internal_error_response(verb: ResponseVerb, message: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message,
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

/// Build a `CapabilityUnavailable` rejected response. `capability` must be
/// a known capability string (validated by the envelope wire schema).
/// CLI callers should pair this with `ExitCode::from(69)` (`EX_UNAVAILABLE`).
///
/// `status = Rejected` because the IDL `errors/error.json` only admits
/// `CapabilityUnavailable` in the rejected family — generated clients
/// deserialize the envelope through that constraint and reject
/// `Aborted + CapabilityUnavailable` at parse time (round-9 review #1).
///
/// The `data.remediation` field is populated automatically from
/// [`cairn_core::status::remediation_for`] when a hint is registered for
/// `capability`. When no hint is registered the field is omitted (the IDL
/// declares it optional with `minLength: 1` so an empty string is invalid).
///
/// Callers that know the specific cause (e.g. `OpenAI` key missing vs model
/// mismatch) should use [`capability_unavailable_response_with_hint`] to
/// provide a cause-specific remediation text instead of the generic table
/// entry.
#[must_use]
pub fn capability_unavailable_response(verb: ResponseVerb, capability: &str) -> Response {
    capability_unavailable_response_with_hint(verb, capability, None)
}

/// Build a `CapabilityUnavailable` rejected response with an optional
/// caller-supplied remediation hint.
///
/// When `override_hint` is `Some`, it is used as `data.remediation` instead
/// of the generic entry in [`cairn_core::status::remediation_for`]. When it
/// is `None`, behaviour is identical to [`capability_unavailable_response`].
///
/// Use this variant when the call site knows the specific cause of the
/// rejection (e.g. `OPENAI_API_KEY` absent vs. unsupported model vs.
/// `openai` feature not compiled in) so operators receive actionable,
/// cause-specific advice rather than generic local-model instructions.
#[must_use]
pub fn capability_unavailable_response_with_hint(
    verb: ResponseVerb,
    capability: &str,
    override_hint: Option<&str>,
) -> Response {
    let mut data = serde_json::json!({ "capability": capability });
    let hint = override_hint.or_else(|| cairn_core::status::remediation_for(capability));
    if let Some(hint) = hint
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert(
            "remediation".to_owned(),
            serde_json::Value::String(hint.to_owned()),
        );
    }
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "CapabilityUnavailable",
            "message": format!(
                "this server does not advertise {capability}; the requested feature requires it"
            ),
            "data": data,
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Rejected,
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
    eprintln!(
        "cairn {verb}: {code} — {message} (operation_id: {})",
        operation_id.0
    );
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
