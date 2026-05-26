//! Deterministic `ConnectorEventId` minting from upstream identity.
//!
//! Substrate's `ConnectorEventId::parse` requires a valid 26-char Crockford
//! base32 ULID; we satisfy that by computing a SHA-256 over the identity
//! tuple and feeding the first 16 bytes to `Ulid::from_bytes`. The same
//! upstream object + revision always yields the same ULID, making poll +
//! webhook retries idempotent at the substrate's event-id dedup gate.

use cairn_connectors_core::ConnectorEventId;
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Mint a deterministic ULID from `(kind, system_id, revision)`.
///
/// `revision` should be a value that changes when the upstream object is
/// updated (e.g. `updated_at.timestamp()` for issues/PRs, the SHA for
/// commits). The same `(kind, system_id, revision)` tuple always produces
/// the same `ConnectorEventId`, making retried polls idempotent at the
/// substrate's event-id dedup gate.
pub(crate) fn deterministic(kind: &str, system_id: &str, revision: &str) -> ConnectorEventId {
    let mut hasher = Sha256::new();
    hasher.update(b"cairn-connectors-github/v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(system_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(revision.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ConnectorEventId::new(Ulid::from_bytes(bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_yield_same_id() {
        let a = deterministic("issue", "gh:o/r#42", "1700000000");
        let b = deterministic("issue", "gh:o/r#42", "1700000000");
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn different_revisions_yield_different_ids() {
        let a = deterministic("issue", "gh:o/r#42", "1700000000");
        let b = deterministic("issue", "gh:o/r#42", "1700000001");
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn different_kinds_yield_different_ids() {
        let a = deterministic("issue", "gh:o/r#42", "x");
        let b = deterministic("pr", "gh:o/r#42", "x");
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn output_is_valid_ulid_for_substrate_parse() {
        let id = deterministic("issue", "gh:o/r#42", "rev");
        // Round-trip through substrate's parse (which enforces ULID validity).
        let parsed = ConnectorEventId::parse(id.as_str()).expect("substrate accepts");
        assert_eq!(parsed.as_str(), id.as_str());
    }
}
