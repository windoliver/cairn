//! Deterministic `ConnectorEventId` minting for web clips.
//!
//! Identical re-deliveries of the same clip must collapse to one record at the
//! substrate's event-id dedup gate. We hash the clip identity tuple
//! `(url, captured_at, payload_hash)` into the 16 bytes of a ULID, so the same
//! clip always yields the same id while two distinct clips of the same URL at
//! the same second differ via `payload_hash`.

use cairn_connectors_core::ConnectorEventId;
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Mint a deterministic ULID from the clip identity components.
///
/// All components are NUL-separated in the hash input so they cannot collide
/// across boundaries (e.g. `["ab","c"]` != `["abc"]`).
pub(crate) fn from_parts(kind: &str, url: &str, components: &[&str]) -> ConnectorEventId {
    let mut hasher = Sha256::new();
    hasher.update(b"cairn-connectors-webclip/v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(url.as_bytes());
    for c in components {
        hasher.update(b"\0");
        hasher.update(c.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ConnectorEventId::new(Ulid::from_bytes(bytes).to_string())
}

/// Hash arbitrary wire bytes into a short hex revision component.
pub(crate) fn payload_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(&hasher.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_connectors_core::ConnectorEventId;
    use proptest::prelude::*;

    #[test]
    fn same_inputs_yield_same_id() {
        let a = from_parts("clip", "https://e.com/a", &["100", "deadbeef"]);
        let b = from_parts("clip", "https://e.com/a", &["100", "deadbeef"]);
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn different_payload_hash_yields_different_id() {
        let a = from_parts("clip", "https://e.com/a", &["100", "aaaa"]);
        let b = from_parts("clip", "https://e.com/a", &["100", "bbbb"]);
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn output_is_valid_ulid_for_substrate_parse() {
        let id = from_parts("clip", "https://e.com/a", &["100", "rev"]);
        let parsed = ConnectorEventId::parse(id.as_str()).expect("substrate accepts ULID");
        assert_eq!(parsed.as_str(), id.as_str());
    }

    proptest! {
        #[test]
        fn from_parts_is_deterministic(url in "[a-z]{1,12}", ts in any::<i64>(), body in ".{0,40}") {
            let h = payload_hash(body.as_bytes());
            let ts_s = ts.to_string();
            let a = from_parts("clip", &url, &[&ts_s, &h]);
            let b = from_parts("clip", &url, &[&ts_s, &h]);
            prop_assert_eq!(a.as_str(), b.as_str());
        }
    }
}
