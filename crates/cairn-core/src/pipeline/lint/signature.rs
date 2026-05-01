//! Record-at-rest signature lint check (issue #256, brief §1223).
//!
//! P0 scope per `docs/design/2026-04-30-issue-256-signature-lint-design.md`:
//! identity-lifecycle check only. The persisted `MemoryRecord` does not
//! carry the `SignedIntent.target_hash` or the author's `key_version`, so
//! body-integrity and key-version-ring checks need separate schema work.
//! Real Ed25519 verify of `record.signature` lands at P1 alongside the
//! keychain integration (see `crates/cairn-core/src/verifier.rs:8-22`).
//!
//! Two `chain_status` values from brief §1223 are emitted today:
//! `revoked` (issuer's signing right has been withdrawn) and `broken`
//! (the chain itself is malformed, or the issuer cannot be located).
//! `expired_key` and the affirmative `valid` wait on follow-up issues
//! (`key_version` persistence + Ed25519 verify).
//!
//! ## Boundary with the dispatch layer (#96)
//!
//! The check is a pure function. The dispatch layer is responsible for:
//!
//! 1. Calling `IdentityRegistry::get_identity(author, IdentityVisibility::Audit)`
//!    so revoked / purged states surface.
//! 2. Mapping the `Result`:
//!   - `Ok(Some(record))` → [`AuthorState::Resolved`] with `record.provisioning_state`.
//!   - `Ok(None)` → [`AuthorState::MissingFromRegistry`].
//!   - `Err(_)` → **do not call this function**. A registry backend error
//!     is an infrastructure fault, not a per-record finding; surface it
//!     via the dispatch layer's own `CapabilityUnavailable` /
//!     capability-error path so it is not silently mis-reported as a
//!     missing identity (brief invariant 6, fail-closed).

use crate::domain::identity::ProvisioningState;
use crate::domain::{ChainRole, Identity, MemoryRecord, RecordId};

/// Subset of brief §1223 `chain_status` values that this P0 check can
/// emit. Grows as follow-up checks (body integrity, key-version ring,
/// real Ed25519 verify) land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainStatus {
    /// Author identity has been revoked or purged (or one of those
    /// transitions is in flight). Historical signatures still verify
    /// for audit, but the record should not be treated as
    /// authoritatively-signed going forward. Maps to brief §1223
    /// `revoked`.
    Revoked,
    /// At-rest verification cannot establish the issuer: either the
    /// `actor_chain` lacks an `Author` entry, or the author identity
    /// is not in the `IdentityRegistry`. Maps to brief §1223 `broken`.
    /// Failing closed here keeps tampered chains from sailing through
    /// the audit until a dedicated chain-shape lint lands.
    Malformed,
}

/// Outcome of looking up the record's author identity in the
/// `IdentityRegistry`. Forces the dispatch layer to handle the missing
/// row case explicitly so it cannot silently collapse with a registry
/// backend error (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthorState {
    /// The registry returned a row for the author identity.
    Resolved(ProvisioningState),
    /// The registry returned `Ok(None)` — the author identity has no
    /// row at any visibility level. Treat as a `Malformed` finding.
    MissingFromRegistry,
}

/// One record flagged by the at-rest signature check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureFinding {
    /// The record that was flagged.
    pub record_id: RecordId,
    /// The author identity that failed the lifecycle check, when one
    /// could be extracted from the chain. `None` when the chain itself
    /// has no `Author` entry.
    pub author: Option<Identity>,
    /// Which `chain_status` value the finding maps to.
    pub status: ChainStatus,
    /// Human-readable explanation; safe to render in `lint-report.md`.
    /// Carries no body content per brief invariant 9 (privacy-by-construction).
    pub message: String,
}

/// Check a record's at-rest signature state given the **pre-fetched**
/// author lifecycle state from `IdentityRegistry`.
///
/// Returns `Some(SignatureFinding)` for:
///
/// - `actor_chain` without an `Author` entry — emits `Malformed`.
/// - Author identity in `Revoked`, `RevokePending`, `Purged`, or
///   `PurgePending` — emits `Revoked`.
/// - Author identity not present in the registry — emits `Malformed`
///   (fail-closed per brief invariant 6).
///
/// Returns `None` for:
///
/// - Author identity is `Active`.
/// - Author identity is `Pending`. A pending identity should never be
///   able to sign at the trust boundary, so a stored record carrying a
///   pending author is a provisioning anomaly distinct from revocation;
///   surfacing it as `KeyRevoked` would mis-direct operators. A
///   dedicated provisioning-anomaly lint check is tracked as a
///   follow-up to #256 and #96.
#[must_use]
pub fn check_signature(
    record: &MemoryRecord,
    author_state: AuthorState,
) -> Option<SignatureFinding> {
    let author = record
        .actor_chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .map(|e| e.identity.clone());

    let Some(author) = author else {
        return Some(SignatureFinding {
            record_id: record.id.clone(),
            author: None,
            status: ChainStatus::Malformed,
            message: "actor_chain has no `Author` entry — at-rest signature cannot be attributed to an issuer".to_owned(),
        });
    };

    let (status, message) = match author_state {
        // Active → ok. Pending → also no finding here: a stored record
        // signed by a non-`Active` identity is a provisioning anomaly,
        // not a revocation. Surfacing it as `KeyRevoked` would
        // mis-direct operators; a dedicated provisioning-anomaly lint
        // check is tracked as a follow-up to #256.
        AuthorState::Resolved(ProvisioningState::Active | ProvisioningState::Pending) => {
            return None;
        }
        AuthorState::Resolved(state) => (
            ChainStatus::Revoked,
            format!(
                "author identity `{}` is in lifecycle state `{state:?}` — signing right is withdrawn or pending withdrawal",
                author.as_str()
            ),
        ),
        AuthorState::MissingFromRegistry => (
            ChainStatus::Malformed,
            format!(
                "author identity `{}` has no row in IdentityRegistry — record was signed under an unknown issuer",
                author.as_str()
            ),
        ),
    };

    Some(SignatureFinding {
        record_id: record.id.clone(),
        author: Some(author),
        status,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::{ActorChainEntry, ChainRole, Rfc3339Timestamp};

    fn record_with_active_author() -> MemoryRecord {
        sample_record()
    }

    #[test]
    fn active_author_emits_no_finding() {
        let record = record_with_active_author();
        assert_eq!(
            check_signature(&record, AuthorState::Resolved(ProvisioningState::Active)),
            None
        );
    }

    #[test]
    fn pending_author_emits_no_finding_at_p0() {
        // Pending is a provisioning anomaly, not a revocation. Returning
        // None keeps the operator-facing finding free of misdirection;
        // a dedicated provisioning-anomaly lint check is the follow-up.
        let record = record_with_active_author();
        assert_eq!(
            check_signature(&record, AuthorState::Resolved(ProvisioningState::Pending)),
            None
        );
    }

    #[test]
    fn revoke_pending_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_signature(
            &record,
            AuthorState::Resolved(ProvisioningState::RevokePending),
        )
        .expect("revoke-pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.author.is_some());
        assert!(finding.message.contains("RevokePending"));
    }

    #[test]
    fn revoked_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_signature(&record, AuthorState::Resolved(ProvisioningState::Revoked))
            .expect("revoked issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Revoked"));
    }

    #[test]
    fn purge_pending_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_signature(
            &record,
            AuthorState::Resolved(ProvisioningState::PurgePending),
        )
        .expect("purge-pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("PurgePending"));
    }

    #[test]
    fn purged_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_signature(&record, AuthorState::Resolved(ProvisioningState::Purged))
            .expect("purged issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Purged"));
    }

    #[test]
    fn missing_registry_row_is_flagged_as_malformed() {
        let record = record_with_active_author();
        let finding = check_signature(&record, AuthorState::MissingFromRegistry)
            .expect("missing row must fail closed");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert!(finding.author.is_some());
        assert!(finding.message.contains("no row in IdentityRegistry"));
    }

    #[test]
    fn finding_carries_record_id_and_author() {
        let record = record_with_active_author();
        let expected_id = record.id.clone();
        let expected_author = record
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            .map(|e| e.identity.clone())
            .expect("sample record has author");
        let finding = check_signature(&record, AuthorState::Resolved(ProvisioningState::Revoked))
            .expect("flagged");
        assert_eq!(finding.record_id, expected_id);
        assert_eq!(finding.author, Some(expected_author));
    }

    #[test]
    fn record_without_author_entry_is_flagged_as_malformed() {
        // A tampered or otherwise corrupted chain that is missing its
        // Author entry must surface — silently dropping it would let
        // exactly the records this audit is meant to find sail through.
        let mut record = record_with_active_author();
        record.actor_chain.retain(|e| e.role != ChainRole::Author);
        record.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: record.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let finding = check_signature(&record, AuthorState::MissingFromRegistry)
            .expect("malformed chain must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert_eq!(finding.author, None);
        assert!(finding.message.contains("no `Author` entry"));
    }

    #[test]
    fn missing_author_finding_is_independent_of_author_state() {
        // The malformed-chain branch should fire regardless of what
        // state the dispatch layer has prefetched — there is no author
        // identity to apply that state to.
        let mut record = record_with_active_author();
        record.actor_chain.retain(|e| e.role != ChainRole::Author);
        record.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: record.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let resolved = check_signature(&record, AuthorState::Resolved(ProvisioningState::Active))
            .expect("active state does not rescue a malformed chain");
        assert_eq!(resolved.status, ChainStatus::Malformed);
    }
}
