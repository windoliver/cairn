//! Shared envelope helpers and P0 stub responses.
//!
//! These mirror the CLI's `verbs::envelope` helpers byte-for-byte so the
//! SDK and CLI emit identical envelopes for the same operation. When verb
//! handlers move into `cairn-core::verbs::*`, both the CLI and SDK switch
//! to that single source.

use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::generated::common::{Nonce16Base64, Ulid};

use crate::SdkError;

/// Mint a fresh ULID for use as an `operation_id`.
pub(crate) fn new_operation_id() -> Ulid {
    Ulid(ulid::Ulid::new().to_string())
}

/// Mint a fresh 16-byte nonce as standard base64 (24 chars with `==` padding).
pub(crate) fn new_nonce() -> Nonce16Base64 {
    use base64::Engine as _;
    let raw = ulid::Ulid::new().0.to_be_bytes();
    Nonce16Base64(base64::engine::general_purpose::STANDARD.encode(raw))
}

/// Current epoch milliseconds, saturating to `0` if the host clock is set
/// before 1970. Returning a sentinel beats panicking the SDK at the
/// `status`/`handshake` boundary on a misconfigured clock.
pub(crate) fn now_ms() -> u64 {
    #[allow(clippy::cast_possible_truncation)]
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    ms
}

/// Current UTC time as RFC-3339 with second precision (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Implemented locally (no `chrono` dep) to match the CLI's `status` output
/// format byte-for-byte. Saturates to the Unix epoch if the host clock is
/// pre-1970 — see [`now_ms`].
pub(crate) fn now_rfc3339_seconds() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn secs_to_ymdhms(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    let mut days = s;
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days + 1, hour, min, sec)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// Build the canonical "store not wired in this P0 build" stub error.
///
/// Mirrors `cairn-cli::verbs::envelope::unimplemented_response` so SDK and
/// CLI emit identical error payloads. The verb handler is tracked under #9.
pub(crate) fn store_not_wired() -> SdkError {
    SdkError::Internal {
        code: "Internal".to_owned(),
        message: "store not wired in this P0 build — verb dispatch lands in #9".to_owned(),
        operation_id: new_operation_id(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_24_chars_base64() {
        let n = new_nonce();
        assert_eq!(n.0.len(), 24);
        assert!(n.0.ends_with("=="));
    }

    #[test]
    fn operation_id_is_26_char_ulid() {
        let id = new_operation_id();
        assert_eq!(id.0.len(), 26);
    }

    #[test]
    fn rfc3339_format_is_20_chars() {
        let now = now_rfc3339_seconds();
        assert_eq!(now.len(), 20);
        assert!(now.ends_with('Z'));
        assert!(now.contains('T'));
    }

    #[test]
    fn rfc3339_epoch() {
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn rfc3339_y2k_leap() {
        let (y, mo, d, _, _, _) = secs_to_ymdhms(951_782_400);
        assert_eq!((y, mo, d), (2000, 2, 29));
    }

    #[test]
    fn store_not_wired_carries_operation_id() {
        let err = store_not_wired();
        match &err {
            SdkError::Internal {
                code, operation_id, ..
            } => {
                assert_eq!(code, "Internal");
                assert_eq!(operation_id.0.len(), 26);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
