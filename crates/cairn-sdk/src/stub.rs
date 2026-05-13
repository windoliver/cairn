//! Shared envelope helpers and P0 stub responses.
//!
//! These mirror the CLI's `verbs::envelope` helpers byte-for-byte so the
//! SDK and CLI emit identical envelopes for the same operation. When verb
//! handlers move into `cairn-core::verbs::*`, both the CLI and SDK switch
//! to that single source.

use cairn_core::generated::common::{Nonce16Base64, Ulid};

use crate::SdkError;

/// Mint a fresh ULID for use as an `operation_id`.
pub(crate) fn new_operation_id() -> Ulid {
    cairn_core::time::new_operation_id()
}

/// Mint a fresh 16-byte nonce as standard base64 (24 chars with `==` padding).
pub(crate) fn new_nonce() -> Nonce16Base64 {
    use base64::Engine as _;
    let raw = ulid::Ulid::new().0.to_be_bytes();
    Nonce16Base64(base64::engine::general_purpose::STANDARD.encode(raw))
}

/// Current UTC time as RFC-3339 with second precision (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Delegates to [`cairn_core::time::now_rfc3339_seconds`] to match the CLI's
/// `status` output format byte-for-byte.
pub(crate) fn now_rfc3339_seconds() -> String {
    cairn_core::time::now_rfc3339_seconds()
}

/// Build the canonical "store not wired in this P0 build" stub error.
///
/// Returns the dedicated [`SdkError::Unimplemented`] variant rather than a
/// generic `Internal`, so callers can fail-fast against verbs whose happy
/// path has not yet shipped. The verb handler is tracked under epic #9.
pub(crate) fn store_not_wired(verb: &'static str) -> SdkError {
    SdkError::Unimplemented {
        verb,
        tracking: "verb dispatch lands in epic #9 (store wiring)",
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
        let (y, mo, d, h, mi, s) = cairn_core::time::secs_to_ymdhms(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn rfc3339_y2k_leap() {
        let (y, mo, d, _, _, _) = cairn_core::time::secs_to_ymdhms(951_782_400);
        assert_eq!((y, mo, d), (2000, 2, 29));
    }

    #[test]
    fn store_not_wired_carries_unimplemented_variant() {
        let err = store_not_wired("ingest");
        match &err {
            SdkError::Unimplemented {
                verb,
                tracking,
                operation_id,
            } => {
                assert_eq!(*verb, "ingest");
                assert!(tracking.contains("#9"));
                assert_eq!(operation_id.0.len(), 26);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
