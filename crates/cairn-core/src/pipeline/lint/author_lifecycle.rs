//! Author-lifecycle slice of record-at-rest verification (issue #256,
//! brief §1223).
//!
//! # What this is *not*
//!
//! **This is not a cryptographic signature verifier.** Returning `None`
//! from [`check_author_lifecycle`] does **not** imply that the record's
//! `record.signature` is valid, that the body has not been tampered
//! with, or that the issuer's key version is current. It only means
//! that the author identity is `Active` in the registry and the
//! `actor_chain` shape passes domain validation.
//!
//! Real Ed25519 verify of `record.signature` and body-integrity
//! checking (recomputing `target_hash` from the canonical record
//! payload) land at P1 alongside the keychain integration — see
//! `crates/cairn-core/src/verifier.rs:8-22` and the design doc at
//! `docs/design/2026-04-30-issue-256-signature-lint-design.md`.
//!
//! # What this *is*
//!
//! The cheapest defensive slice of brief §1223 we can ship at P0
//! without schema changes: surface records whose author identity has
//! lost (or never had) authoritative signing right, and records whose
//! persisted chain has lost shape. Two `chain_status` values from
//! §1223 are emitted today:
//!
//! - `revoked` — author is `Revoked` / `RevokePending` / `Purged` /
//!   `PurgePending`.
//! - `broken` — chain itself is malformed (no `Author`, duplicate
//!   `Author`, role-ordering violation), the issuer is not in the
//!   registry, or the issuer's lifecycle state is `Pending` (issuer
//!   never had authoritative signing right).
//!
//! `expired_key` and the affirmative `valid` from §1223 wait on
//! follow-up issues (`key_version` persistence + Ed25519 verify).
//!
//! # Boundary with the dispatch layer (#96)
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

use crate::domain::actor_chain::validate_chain;
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
    /// At-rest verification cannot establish the issuer:
    ///
    /// - the `actor_chain` lacks an `Author` entry, has more than one,
    ///   or violates the `Principal* → Delegator* → Author → Sensor*`
    ///   ordering;
    /// - the author identity is `Pending` (provisioning never
    ///   completed, so the issuer never had authoritative signing
    ///   right); or
    /// - the author identity is not in the `IdentityRegistry`.
    ///
    /// Maps to brief §1223 `broken`. Failing closed here keeps tampered
    /// chains from sailing through the audit until the full crypto
    /// verifier ships.
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

/// One record flagged by the at-rest author-lifecycle check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorLifecycleFinding {
    /// The record that was flagged.
    pub record_id: RecordId,
    /// The author identity that failed the lifecycle check, when one
    /// could be extracted from the chain. `None` when the chain itself
    /// is malformed (no `Author`, duplicate `Author`, or role-order
    /// violation) so an unambiguous author cannot be named.
    pub author: Option<Identity>,
    /// Which `chain_status` value the finding maps to.
    pub status: ChainStatus,
    /// Human-readable explanation; safe to render in `lint-report.md`.
    /// Carries no body content per brief invariant 9 (privacy-by-construction).
    pub message: String,
}

/// Check a record's at-rest author-lifecycle and chain-shape state
/// given the **pre-fetched** author lifecycle state from
/// `IdentityRegistry`.
///
/// Re-runs `actor_chain` shape validation on every call so persisted
/// records that drifted from domain invariants (direct DB tamper,
/// partial migration, schema-drifted import) cannot pass by relying on
/// ingest-time validation having held.
///
/// Returns `Some(AuthorLifecycleFinding)` for every non-clean state.
/// Mapping:
///
/// - `actor_chain` shape violation (no `Author`, duplicate `Author`,
///   role-ordering violation per §4.2) — emits `Malformed`.
/// - Author identity is `Pending` — emits `Malformed` (provisioning
///   never completed, so the issuer never had authoritative signing
///   right).
/// - Author identity in `Revoked`, `RevokePending`, `Purged`, or
///   `PurgePending` — emits `Revoked`.
/// - Author identity not present in the registry — emits `Malformed`
///   (fail-closed per brief invariant 6).
///
/// Returns `None` only when the `actor_chain` is well-formed *and* the
/// author identity is `Active`. **`None` does not certify the
/// signature**; see module docs.
#[must_use]
pub fn check_author_lifecycle(
    record: &MemoryRecord,
    author_state: AuthorState,
) -> Option<AuthorLifecycleFinding> {
    if let Err(e) = validate_chain(&record.actor_chain) {
        return Some(AuthorLifecycleFinding {
            record_id: record.id.clone(),
            author: None,
            status: ChainStatus::Malformed,
            message: format!(
                "actor_chain failed shape validation: {e} — at-rest signature cannot be attributed to a single issuer"
            ),
        });
    }

    // After validate_chain succeeds we know there is exactly one Author
    // entry; .find returns Some.
    let author = record
        .actor_chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .map(|e| e.identity.clone())?;

    let (status, message) = match author_state {
        AuthorState::Resolved(ProvisioningState::Active) => return None,
        AuthorState::Resolved(ProvisioningState::Pending) => (
            ChainStatus::Malformed,
            format!(
                "author identity `{}` is in lifecycle state `Pending` — provisioning never completed, so the issuer never had authoritative signing right",
                author.as_str()
            ),
        ),
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

    Some(AuthorLifecycleFinding {
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
    use crate::domain::{ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp};

    fn record_with_active_author() -> MemoryRecord {
        sample_record()
    }

    fn entry(role: ChainRole, identity: &str) -> ActorChainEntry {
        ActorChainEntry {
            role,
            identity: Identity::parse(identity).expect("valid identity"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }
    }

    #[test]
    fn active_author_emits_no_finding() {
        let record = record_with_active_author();
        assert_eq!(
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Active)),
            None
        );
    }

    #[test]
    fn pending_author_is_flagged_as_malformed() {
        let record = record_with_active_author();
        let finding =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Pending))
                .expect("pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert!(finding.author.is_some());
        assert!(finding.message.contains("Pending"));
    }

    #[test]
    fn revoke_pending_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_author_lifecycle(
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
        let finding =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Revoked))
                .expect("revoked issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Revoked"));
    }

    #[test]
    fn purge_pending_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_author_lifecycle(
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
        let finding =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Purged))
                .expect("purged issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Purged"));
    }

    #[test]
    fn missing_registry_row_is_flagged_as_malformed() {
        let record = record_with_active_author();
        let finding = check_author_lifecycle(&record, AuthorState::MissingFromRegistry)
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
        let finding =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Revoked))
                .expect("flagged");
        assert_eq!(finding.record_id, expected_id);
        assert_eq!(finding.author, Some(expected_author));
    }

    #[test]
    fn record_without_author_entry_is_flagged_as_malformed() {
        let mut record = record_with_active_author();
        record.actor_chain.retain(|e| e.role != ChainRole::Author);
        record
            .actor_chain
            .push(entry(ChainRole::Sensor, "snr:local:hook:cc-session:v1"));
        let finding = check_author_lifecycle(&record, AuthorState::MissingFromRegistry)
            .expect("malformed chain must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert_eq!(finding.author, None);
        assert!(finding.message.contains("shape validation"));
    }

    #[test]
    fn record_with_duplicate_author_is_flagged_as_malformed() {
        // §4.2 mandates exactly one Author. A persisted record drifted
        // to two Authors is exactly the corruption an at-rest audit
        // should surface — relying on ingest-time validation is unsafe
        // under direct DB tamper or partial migration.
        let mut record = record_with_active_author();
        record
            .actor_chain
            .push(entry(ChainRole::Author, "hmn:other"));
        let finding =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Active))
                .expect("duplicate author must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert_eq!(finding.author, None);
    }

    #[test]
    fn record_with_misordered_chain_is_flagged_as_malformed() {
        // §4.2 mandates Principal* → Delegator* → Author → Sensor*.
        // A Sensor entry before the Author violates that order.
        let mut record = record_with_active_author();
        record
            .actor_chain
            .insert(0, entry(ChainRole::Sensor, "snr:local:hook:cc-session:v1"));
        let finding =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Active))
                .expect("misordered chain must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert_eq!(finding.author, None);
    }

    #[test]
    fn finding_message_never_leaks_record_body() {
        // Brief invariant 9 (privacy-by-construction): findings render
        // into lint-report.md and must never contain raw record body
        // content. Seed every flag-eligible state with a body marker
        // and assert the marker does not appear in the message.
        const MARKER: &str = "SECRET-MARKER-PRIVACY-9";
        let cases = [
            AuthorState::Resolved(ProvisioningState::Pending),
            AuthorState::Resolved(ProvisioningState::RevokePending),
            AuthorState::Resolved(ProvisioningState::Revoked),
            AuthorState::Resolved(ProvisioningState::PurgePending),
            AuthorState::Resolved(ProvisioningState::Purged),
            AuthorState::MissingFromRegistry,
        ];
        for state in cases {
            let mut record = record_with_active_author();
            record.body = format!("benign prefix {MARKER} benign suffix");
            let finding = check_author_lifecycle(&record, state)
                .expect("non-Active state must produce a finding");
            assert!(
                !finding.message.contains(MARKER),
                "finding message leaked record body for state {state:?}: {}",
                finding.message
            );
        }
        // Same for the malformed-chain path.
        let mut malformed = record_with_active_author();
        malformed.body = format!("benign prefix {MARKER} benign suffix");
        malformed
            .actor_chain
            .retain(|e| e.role != ChainRole::Author);
        malformed
            .actor_chain
            .push(entry(ChainRole::Sensor, "snr:local:hook:cc-session:v1"));
        let finding = check_author_lifecycle(&malformed, AuthorState::MissingFromRegistry)
            .expect("malformed-chain finding");
        assert!(!finding.message.contains(MARKER));
    }

    #[test]
    fn malformed_chain_is_independent_of_author_state() {
        // The chain-shape branch fires regardless of what state the
        // dispatch layer prefetched — there is no unambiguous author to
        // apply that state to.
        let mut record = record_with_active_author();
        record.actor_chain.retain(|e| e.role != ChainRole::Author);
        record
            .actor_chain
            .push(entry(ChainRole::Sensor, "snr:local:hook:cc-session:v1"));
        let resolved =
            check_author_lifecycle(&record, AuthorState::Resolved(ProvisioningState::Active))
                .expect("active state does not rescue a malformed chain");
        assert_eq!(resolved.status, ChainStatus::Malformed);
    }
}
