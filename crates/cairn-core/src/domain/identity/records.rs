//! Public identity records and supporting entry types per spec §4.1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, WitnessHash},
};

/// Persisted public row for a provisioned identity (§4.1).
///
/// All timestamps are UTC. Fields that are not yet set (e.g., `activated_at`
/// for a still-`Pending` identity) are `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicIdentityRecord {
    /// The identity this row describes.
    pub id: Identity,
    /// Which of the three identity kinds (`hmn`, `agt`, `snr`).
    pub kind: IdentityKind,
    /// Current active key version for this identity.
    pub current_key_version: KeyVersion,
    /// Monotonically increasing profile revision.
    pub revision: IdentityRevision,
    /// Current lifecycle state of the identity.
    pub provisioning_state: ProvisioningState,
    /// When the identity row was first written.
    pub created_at: DateTime<Utc>,
    /// When the identity transitioned to `Active`. `None` until then.
    pub activated_at: Option<DateTime<Utc>>,
    /// When the identity was revoked. `None` until revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    /// When a purge was requested. `None` until purge is requested.
    pub purge_requested_at: Option<DateTime<Utc>>,
    /// When the identity was fully purged. `None` until purge completes.
    pub purged_at: Option<DateTime<Utc>>,
}

/// Lifecycle states of an identity row (§4.1 state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningState {
    /// Identity created but not yet confirmed / first-bound.
    Pending,
    /// Identity is fully operational: can sign, appears in operational reads.
    Active,
    /// A revocation request is in-flight; signing is blocked.
    RevokePending,
    /// Identity has been revoked; historical signatures remain verifiable.
    Revoked,
    /// A purge request is in-flight; the row will be deleted shortly.
    PurgePending,
    /// Identity data has been purged from the store.
    Purged,
}

impl ProvisioningState {
    /// True if signing under this identity is permitted by the trust gate
    /// (`require_attributable_signer`). Per spec §3.10 / §3.5: only `Active`
    /// can sign new envelopes; `Revoked` keys verify history but do not
    /// sign new ones; transitional states are non-signing.
    #[must_use]
    pub fn can_sign(self) -> bool {
        matches!(self, Self::Active)
    }

    /// True if the row should appear in `IdentityVisibility::Operational`
    /// reads (per §4.1).
    #[must_use]
    pub fn is_operational(self) -> bool {
        matches!(self, Self::Active | Self::Revoked)
    }
}

/// One entry in the `identity_keys` table — a single version of a signing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityKeyEntry {
    /// The identity that owns this key slot.
    pub identity_id: Identity,
    /// Version number of this key.
    pub key_version: KeyVersion,
    /// Ed25519 verifying key bytes (32 bytes).
    pub public_key: [u8; 32],
    /// Optional Ed25519 signature over the predecessor's public key, proving
    /// continuity during rotation.
    pub signed_predecessor: Option<Vec<u8>>,
    /// When this key version was created.
    pub created_at: DateTime<Utc>,
    /// When this key version was superseded (rotated or revoked). `None` if
    /// this is still the current version.
    pub superseded_at: Option<DateTime<Utc>>,
}

/// Pending-first-bind row written before the WAL commit is confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingIdentityEntry {
    /// The identity being provisioned.
    pub identity: Identity,
    /// Key version minted for this bind attempt.
    pub key_version: KeyVersion,
    /// Ed25519 verifying key bytes for the tentative key.
    pub public_key: [u8; 32],
    /// When this pending entry was created.
    pub created_at: DateTime<Utc>,
}

/// WAL row tracking an in-flight revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokePendingEntry {
    /// The identity being revoked.
    pub identity: Identity,
    /// When the revocation was initiated.
    pub revoked_at: DateTime<Utc>,
    /// Opaque receipt identifying this WAL operation.
    pub receipt_id: ReceiptId,
}

/// WAL row tracking an old key version that must be evicted from the keystore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEvictionEntry {
    /// Opaque receipt identifying this WAL operation.
    pub receipt_id: ReceiptId,
    /// The identity whose key is being evicted.
    pub identity: Identity,
    /// The key version to evict.
    pub evict_version: KeyVersion,
    /// When the rotation that triggered eviction occurred.
    pub rotated_at: DateTime<Utc>,
}

/// WAL row tracking keys that must be disabled in the keystore after revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingKeyDisableEntry {
    /// Opaque receipt identifying this WAL operation.
    pub receipt_id: ReceiptId,
    /// The identity whose keys are being disabled.
    pub identity: Identity,
    /// When the revocation was recorded.
    pub revoked_at: DateTime<Utc>,
    /// Key versions that should remain enabled (e.g., for historical verification).
    pub retained_versions: Vec<KeyVersion>,
}

/// WAL row tracking an in-flight purge request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgePendingEntry {
    /// The identity being purged.
    pub identity: Identity,
    /// When the purge was requested.
    pub purge_requested_at: DateTime<Utc>,
    /// Human-readable reason for the purge (e.g., GDPR erasure).
    pub purge_reason: String,
}

/// Opaque receipt identifier — the `SQLite` rowid of the WAL operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReceiptId(
    /// `SQLite` rowid.
    pub i64,
);

/// Result of a first-bind lookup (§4.1): encodes the three states the
/// store can be in before an identity is fully operational.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirstBindState {
    /// No record exists yet for this identity.
    Absent,
    /// A pending-bind row exists but the WAL commit has not been applied.
    Reserved {
        /// The tentative public identity record.
        record: PublicIdentityRecord,
        /// The tentative key entry.
        key: IdentityKeyEntry,
        /// Witness hash covering the bind payload (used to detect replay).
        witness_hash: WitnessHash,
    },
    /// The identity is fully active and the first-bind is complete.
    Activated {
        /// The confirmed public identity record.
        record: PublicIdentityRecord,
        /// The confirmed key entry.
        key: IdentityKeyEntry,
    },
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use chrono::Utc;

    use super::*;
    use crate::domain::identity::{Identity, IdentityKind};

    fn sample_identity() -> Identity {
        Identity::parse("hmn:alice").expect("valid identity")
    }

    fn sample_key_version() -> KeyVersion {
        KeyVersion::FIRST
    }

    fn sample_revision() -> IdentityRevision {
        IdentityRevision::new(NonZeroU32::MIN)
    }

    fn sample_record(state: ProvisioningState) -> PublicIdentityRecord {
        PublicIdentityRecord {
            id: sample_identity(),
            kind: IdentityKind::Human,
            current_key_version: sample_key_version(),
            revision: sample_revision(),
            provisioning_state: state,
            created_at: Utc::now(),
            activated_at: None,
            revoked_at: None,
            purge_requested_at: None,
            purged_at: None,
        }
    }

    fn sample_key_entry() -> IdentityKeyEntry {
        IdentityKeyEntry {
            identity_id: sample_identity(),
            key_version: sample_key_version(),
            public_key: [0u8; 32],
            signed_predecessor: None,
            created_at: Utc::now(),
            superseded_at: None,
        }
    }

    // ── ProvisioningState predicate tests ────────────────────────────────────

    #[test]
    fn active_can_sign() {
        assert!(ProvisioningState::Active.can_sign());
    }

    #[test]
    fn pending_cannot_sign() {
        assert!(!ProvisioningState::Pending.can_sign());
    }

    #[test]
    fn revoke_pending_cannot_sign() {
        assert!(!ProvisioningState::RevokePending.can_sign());
    }

    #[test]
    fn revoked_cannot_sign() {
        assert!(!ProvisioningState::Revoked.can_sign());
    }

    #[test]
    fn active_is_operational() {
        assert!(ProvisioningState::Active.is_operational());
    }

    #[test]
    fn revoked_is_operational() {
        assert!(ProvisioningState::Revoked.is_operational());
    }

    #[test]
    fn pending_not_operational() {
        assert!(!ProvisioningState::Pending.is_operational());
    }

    #[test]
    fn revoke_pending_not_operational() {
        assert!(!ProvisioningState::RevokePending.is_operational());
    }

    #[test]
    fn purge_pending_not_operational() {
        assert!(!ProvisioningState::PurgePending.is_operational());
    }

    #[test]
    fn purged_not_operational() {
        assert!(!ProvisioningState::Purged.is_operational());
    }

    // ── Serde round-trip tests ────────────────────────────────────────────────

    #[test]
    fn public_identity_record_round_trip() {
        let record = sample_record(ProvisioningState::Active);
        let json = serde_json::to_string(&record).expect("serialize");
        let back: PublicIdentityRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.provisioning_state, ProvisioningState::Active);
        assert_eq!(back.id, record.id);
    }

    #[test]
    fn first_bind_state_reserved_round_trip() {
        let state = FirstBindState::Reserved {
            record: sample_record(ProvisioningState::Pending),
            key: sample_key_entry(),
            witness_hash: WitnessHash::from_bytes([0u8; 32]),
        };
        let json = serde_json::to_string(&state).expect("serialize");
        let back: FirstBindState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, state);
    }

    #[test]
    fn first_bind_state_absent_round_trip() {
        let state = FirstBindState::Absent;
        let json = serde_json::to_string(&state).expect("serialize");
        let back: FirstBindState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, state);
    }

    #[test]
    fn receipt_id_round_trip() {
        let r = ReceiptId(42);
        let json = serde_json::to_string(&r).expect("serialize");
        let back: ReceiptId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, r);
    }

    #[test]
    fn provisioning_state_serde_snake_case() {
        let json = serde_json::to_string(&ProvisioningState::RevokePending).expect("serialize");
        assert_eq!(json, "\"revoke_pending\"");
        let back: ProvisioningState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ProvisioningState::RevokePending);
    }
}
