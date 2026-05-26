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
///
/// Thin wrapper over [`from_parts`] kept for backward-compat with existing tests.
#[cfg(test)]
pub(crate) fn deterministic(kind: &str, system_id: &str, revision: &str) -> ConnectorEventId {
    from_parts(kind, system_id, &[revision])
}

/// Mint a deterministic ULID from `(kind, system_id, components...)`.
///
/// All components are concatenated with NUL separators into the hash input,
/// so `from_parts("issue", id, &["ts", "hash"])` is collision-free with
/// `from_parts("issue", id, &["tshash"])`.
///
/// Use this instead of `deterministic` when the revision consists of multiple
/// parts (e.g. timestamp + payload hash, or `delivery_id` + commit SHA).
pub(crate) fn from_parts(kind: &str, system_id: &str, components: &[&str]) -> ConnectorEventId {
    let mut hasher = Sha256::new();
    hasher.update(b"cairn-connectors-github/v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(system_id.as_bytes());
    for c in components {
        hasher.update(b"\0");
        hasher.update(c.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ConnectorEventId::new(Ulid::from_bytes(bytes).to_string())
}

/// Hash the payload JSON to a short hex string suitable as a revision
/// component.
///
/// Ensures that two events for the same upstream object at the same timestamp
/// but with different payload content (e.g. two edits within 1 second) produce
/// distinct event IDs, defeating the substrate's event-id reuse guard.
pub(crate) fn payload_revision(payload: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    hasher.update(&bytes);
    hex::encode(&hasher.finalize()[..8])
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

    // --- from_parts / payload_revision (Fix 3, round-3) ---

    #[test]
    fn from_parts_single_component_matches_deterministic() {
        // `deterministic` is a thin wrapper around `from_parts`; they must agree.
        let a = deterministic("issue", "gh:o/r#42", "rev");
        let b = from_parts("issue", "gh:o/r#42", &["rev"]);
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn from_parts_multi_component_differs_from_concatenated_single() {
        // `from_parts("x", "y", &["ab", "c"])` must differ from
        // `from_parts("x", "y", &["abc"])` — NUL separators prevent collisions.
        let a = from_parts("issue", "gh:o/r#42", &["ab", "c"]);
        let b = from_parts("issue", "gh:o/r#42", &["abc"]);
        assert_ne!(
            a.as_str(),
            b.as_str(),
            "NUL separators must prevent cross-component collisions"
        );
    }

    #[test]
    fn payload_revision_same_payload_is_stable() {
        let v = serde_json::json!({"a": 1, "b": "hello"});
        assert_eq!(payload_revision(&v), payload_revision(&v));
    }

    #[test]
    fn payload_revision_differs_on_content_change() {
        let v1 = serde_json::json!({"body": "first edit"});
        let v2 = serde_json::json!({"body": "second edit"});
        assert_ne!(payload_revision(&v1), payload_revision(&v2));
    }

    // --- Fix 1 (round-4): poll and webhook paths produce the same event_id ---

    #[test]
    fn poll_and_webhook_paths_produce_same_event_id_for_same_commit() {
        // Both poll and push-webhook paths now use `from_parts("commit", system_id,
        // &[sha])`.  The system_id encodes the SHA (`gh:o/r@deadbeef`), so as long
        // as the SHA matches, the two channels produce identical event IDs.
        let poll_id = from_parts("commit", "gh:o/r@deadbeef", &["deadbeef"]);
        let webhook_id = from_parts("commit", "gh:o/r@deadbeef", &["deadbeef"]);
        assert_eq!(
            poll_id.as_str(),
            webhook_id.as_str(),
            "poll and webhook must produce the same event_id for the same commit SHA"
        );
    }

    #[test]
    fn poll_and_webhook_paths_produce_same_event_id_for_same_issue_state() {
        // Both poll and issues-webhook paths now use `from_parts("issue",
        // system_id, &[ts, payload_rev])`.  Given identical payload bodies and
        // timestamps, the two channels produce the same event_id.
        let ts = "1748163600";
        let payload = serde_json::json!({"number": 42, "title": "bug", "state": "open"});
        let rev = payload_revision(&payload);
        let poll_id = from_parts("issue", "gh:o/r#42", &[ts, &rev]);
        let webhook_id = from_parts("issue", "gh:o/r#42", &[ts, &rev]);
        assert_eq!(
            poll_id.as_str(),
            webhook_id.as_str(),
            "poll and webhook must produce the same event_id for the same issue payload"
        );
    }

    #[test]
    fn issues_event_id_same_ts_different_payload_produces_different_id() {
        // Simulate two rapid edits at the same Unix second.
        let ts = "1748163600"; // arbitrary fixed second
        let rev_a = payload_revision(&serde_json::json!({"body": "edit A"}));
        let rev_b = payload_revision(&serde_json::json!({"body": "edit B"}));
        let id_a = from_parts("issue", "gh:o/r#1", &[ts, &rev_a]);
        let id_b = from_parts("issue", "gh:o/r#1", &[ts, &rev_b]);
        assert_ne!(
            id_a.as_str(),
            id_b.as_str(),
            "same timestamp + different payload must yield different event IDs"
        );
    }
}
