//! Record-at-rest signature lint check (issue #256, brief §1223).
//!
//! P0 scope per `docs/design/2026-04-30-issue-256-signature-lint-design.md`:
//! identity-lifecycle check only. The persisted `MemoryRecord` does not
//! carry the `SignedIntent.target_hash` or the author's `key_version`, so
//! body-integrity and key-version-ring checks need separate schema work.
//! Real Ed25519 verify of `record.signature` lands at P1 alongside the
//! keychain integration (see `crates/cairn-core/src/verifier.rs:8-22`).
//!
//! This check covers exactly one of the four `chain_status` values brief
//! §1223 enumerates: `revoked`. The other three (`expired_key`, `broken`,
//! and the implied `valid`) wait on follow-up issues.

use crate::domain::identity::ProvisioningState;
use crate::domain::{ChainRole, Identity, MemoryRecord, RecordId};

/// Subset of brief §1223 `chain_status` values that this P0 check can
/// emit. Grows as follow-up checks (body integrity, key-version ring,
/// real Ed25519 verify) land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainStatus {
    /// Author identity is in a non-`Active` lifecycle state, or is not
    /// present in the `IdentityRegistry` at all. Maps to brief §1223's
    /// `revoked` status.
    Revoked,
}

/// One record flagged by the at-rest signature check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureFinding {
    /// The record that was flagged.
    pub record_id: RecordId,
    /// The author identity that failed the lifecycle check.
    pub author: Identity,
    /// Which `chain_status` value the finding maps to.
    pub status: ChainStatus,
    /// Human-readable explanation; safe to render in `lint-report.md`.
    /// Carries no body content per brief invariant 9 (privacy-by-construction).
    pub message: String,
}

/// Check a record's at-rest signature state given the **pre-fetched**
/// author lifecycle state from `IdentityRegistry`.
///
/// Caller responsibilities (live in the dispatch layer #96 builds, not
/// here):
///
/// 1. Locate the `Author` entry in `record.actor_chain` (`P0`: exactly
///    one per validated record).
/// 2. Look up that identity in the `IdentityRegistry` with
///    `IdentityVisibility::Audit` so revoked / purged states surface.
/// 3. Pass `Some(state)` if the row exists, `None` if no row matches —
///    `None` is treated as "fail closed" per brief invariant 6.
///
/// Returns `Some(SignatureFinding)` for every non-`Active` state and for
/// the missing-row case; `None` when the author is `Active`. A record
/// that lacks an `Author` chain entry returns `None`: chain-shape
/// validation already lives in `validate_chain` (§4.2) and surfacing it
/// twice would double-report — leave it to the schema-shape lint check.
#[must_use]
pub fn check_signature(
    record: &MemoryRecord,
    author_state: Option<ProvisioningState>,
) -> Option<SignatureFinding> {
    let author = record
        .actor_chain
        .iter()
        .find(|e| e.role == ChainRole::Author)?
        .identity
        .clone();

    let message = match author_state {
        Some(ProvisioningState::Active) => return None,
        Some(state) => format!(
            "author identity `{}` is in lifecycle state `{state:?}` — record was signed under a non-`Active` key",
            author.as_str()
        ),
        None => format!(
            "author identity `{}` has no row in IdentityRegistry — record was signed under an unknown identity",
            author.as_str()
        ),
    };

    Some(SignatureFinding {
        record_id: record.id.clone(),
        author,
        status: ChainStatus::Revoked,
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
            check_signature(&record, Some(ProvisioningState::Active)),
            None
        );
    }

    #[test]
    fn pending_author_is_flagged() {
        let record = record_with_active_author();
        let finding = check_signature(&record, Some(ProvisioningState::Pending))
            .expect("pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Pending"));
    }

    #[test]
    fn revoke_pending_author_is_flagged() {
        let record = record_with_active_author();
        let finding = check_signature(&record, Some(ProvisioningState::RevokePending))
            .expect("revoke-pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("RevokePending"));
    }

    #[test]
    fn revoked_author_is_flagged() {
        let record = record_with_active_author();
        let finding = check_signature(&record, Some(ProvisioningState::Revoked))
            .expect("revoked issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Revoked"));
    }

    #[test]
    fn purge_pending_author_is_flagged() {
        let record = record_with_active_author();
        let finding = check_signature(&record, Some(ProvisioningState::PurgePending))
            .expect("purge-pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("PurgePending"));
    }

    #[test]
    fn purged_author_is_flagged() {
        let record = record_with_active_author();
        let finding = check_signature(&record, Some(ProvisioningState::Purged))
            .expect("purged issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Purged"));
    }

    #[test]
    fn missing_registry_row_fails_closed() {
        let record = record_with_active_author();
        let finding = check_signature(&record, None).expect("missing row must fail closed");
        assert_eq!(finding.status, ChainStatus::Revoked);
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
        let finding = check_signature(&record, Some(ProvisioningState::Revoked)).expect("flagged");
        assert_eq!(finding.record_id, expected_id);
        assert_eq!(finding.author, expected_author);
    }

    #[test]
    fn record_without_author_entry_returns_none() {
        // Chain-shape violation is reported by validate_chain / a separate
        // lint check; signature-state lint should not double-report.
        let mut record = record_with_active_author();
        record.actor_chain.retain(|e| e.role != ChainRole::Author);
        // Replace with a sensor entry to keep the chain non-empty (a
        // real chain-shape lint would still flag this; that is not our
        // job here).
        record.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: record.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        assert_eq!(check_signature(&record, None), None);
    }
}
