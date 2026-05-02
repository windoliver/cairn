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
//! - `revoked` (terminal) — author is `Revoked` or `Purged`. Surfaces
//!   as a non-blocking `Warning` because historical signatures may
//!   pre-date the revocation.
//! - `revoked` (in-flight) — author is `RevokePending` or
//!   `PurgePending`. Surfaces as a blocking `Error`: the record landed
//!   while a withdrawal was already in motion, which is the
//!   suspicious-write case (bypassed gate / race / tamper).
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
use crate::domain::{
    ChainRole, Identity, IdentityKind, MemoryKind, MemoryRecord, RecordId, Rfc3339Timestamp,
};

/// Subset of brief §1223 `chain_status` values that this P0 check can
/// emit. Grows as follow-up checks (body integrity, key-version ring,
/// real Ed25519 verify) land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainStatus {
    /// Author identity is in a *terminal* revocation / purge state
    /// (`Revoked` or `Purged`) **and** the chain author timestamp is
    /// at-or-before `revoked_at` (or `revoked_at` is unknown). This is
    /// the legitimate-history case: the record was signed while the
    /// identity was still operational, then the identity was revoked
    /// later. Surfaces at `Severity::Warning` with non-destructive
    /// remediation. Maps to brief §1223 `revoked`.
    Revoked,
    /// Author identity is in a *terminal* revocation / purge state
    /// **and** the chain author timestamp is at-or-after `revoked_at`.
    /// The record was signed under a key whose signing right had
    /// already been withdrawn — a real trust violation. Surfaces at
    /// `Severity::Error`. Maps to brief §1223 `revoked` but split out
    /// from the legitimate-history case so the operator-blocking
    /// signal is not diluted.
    PostRevocationWrite,
    /// Author identity is in an *in-flight* revocation / purge
    /// transition (`RevokePending` or `PurgePending`). A record
    /// surfacing under one of these states means the write happened
    /// while a withdrawal was already in motion — that is **not** the
    /// "old historical record under a now-revoked key" case the
    /// terminal `Revoked` variant covers. The dispatch leaf emits
    /// this finding at `Severity::Error` because the simplest
    /// explanation is a write that should never have landed: a
    /// bypassed trust gate, a race against revocation, or a tampered
    /// chain. Maps to brief §1223 `revoked` but is split out from
    /// terminal revocation so the operator-blocking signal is not
    /// diluted.
    RevocationInFlight,
    /// At-rest verification cannot establish the issuer:
    ///
    /// - the `actor_chain` lacks an `Author` entry, has more than one,
    ///   or violates the `Principal* → Delegator* → Author → Sensor*`
    ///   ordering;
    /// - the author identity is `Pending` (provisioning never
    ///   completed, so the issuer never had authoritative signing
    ///   right);
    /// - the author identity is `Active` but the chain author
    ///   timestamp is *before* `activated_at` — a write that landed
    ///   while the identity had no authoritative signing right, the
    ///   pre-activation tamper case; or
    /// - the author identity is not in the `IdentityRegistry`.
    ///
    /// Maps to brief §1223 `broken`. Failing closed here keeps tampered
    /// chains from sailing through the audit until the full crypto
    /// verifier ships.
    Malformed,
}

/// Pre-fetched lifecycle snapshot of one author identity. Carries the
/// transition timestamps the check needs to compare against the chain
/// author's `at` so it can distinguish:
///
/// - pre-activation writes (chain `at` < `activated_at`) from clean
///   writes;
/// - legitimate pre-revocation history (chain `at` < `revoked_at`)
///   from post-revocation writes (chain `at` >= `revoked_at`).
///
/// Timestamps are passed as `Rfc3339Timestamp` so this stays in
/// `cairn-core` without pulling in `chrono`. The dispatch layer
/// converts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorLifecycle {
    /// Current lifecycle state.
    pub state: ProvisioningState,
    /// When the identity transitioned to `Active`. `None` until the
    /// identity has activated (i.e., `state == Pending`).
    pub activated_at: Option<Rfc3339Timestamp>,
    /// When the identity was revoked. `None` until revoked.
    pub revoked_at: Option<Rfc3339Timestamp>,
}

/// Outcome of looking up the record's author identity in the
/// `IdentityRegistry`. Forces the dispatch layer to handle the missing
/// row case explicitly so it cannot silently collapse with a registry
/// backend error (see module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthorState {
    /// The registry returned a row for the author identity.
    Resolved(AuthorLifecycle),
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
/// given the **pre-fetched** author lifecycle snapshot from
/// `IdentityRegistry`.
///
/// Re-runs `actor_chain` shape validation on every call so persisted
/// records that drifted from domain invariants (direct DB tamper,
/// partial migration, schema-drifted import) cannot pass by relying on
/// ingest-time validation having held. Also compares the chain author
/// entry's `at` against `activated_at` / `revoked_at` so writes that
/// landed *before* the identity had signing right or *after* it was
/// withdrawn surface as blocking findings even though the current
/// lifecycle state alone would let them slip.
///
/// Returns `Some(AuthorLifecycleFinding)` for every non-clean state.
/// Mapping:
///
/// - `actor_chain` shape violation (no `Author`, duplicate `Author`,
///   role-ordering violation per §4.2) — emits `Malformed`.
/// - Author identity is `Pending` — emits `Malformed`.
/// - Author identity is `Active` but chain `at` < `activated_at` —
///   emits `Malformed` (pre-activation tamper).
/// - Author identity in `RevokePending` or `PurgePending` — emits
///   `RevocationInFlight` (blocking error).
/// - Author identity in `Revoked` or `Purged`:
///   - chain `at` >= `revoked_at` — emits `PostRevocationWrite`
///     (blocking error: written under withdrawn signing right);
///   - otherwise — emits `Revoked` (legitimate pre-revocation
///     history; non-blocking warning).
/// - Author identity not present in the registry — emits `Malformed`
///   (fail-closed per brief invariant 6).
///
/// Returns `None` only when the `actor_chain` is well-formed, the
/// author identity is `Active`, *and* (when known) the chain timestamp
/// is at-or-after `activated_at`. **`None` does not certify the
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

    // Sensor-authored records are only legal under tight invariants
    // (`MemoryRecord::validate`, §4.2): `kind == sensor_observation`
    // **and** `author.identity == provenance.source_sensor`. A sensor
    // identity outside that envelope — e.g., a tampered higher-trust
    // record whose author was swapped to `snr:*` — must surface as
    // `Malformed`, not silently skipped. This is exactly the bypass
    // the at-rest audit exists to catch.
    if author.kind() == IdentityKind::Sensor {
        let valid_sensor_authoring = record.kind == MemoryKind::SensorObservation
            && author == record.provenance.source_sensor;
        if valid_sensor_authoring {
            // Legitimately sensor-authored; sensors aren't in the
            // human/agent lifecycle state machine and aren't required
            // to be in `IdentityRegistry`.
            return None;
        }
        return Some(AuthorLifecycleFinding {
            record_id: record.id.clone(),
            author: Some(author),
            status: ChainStatus::Malformed,
            message: format!(
                "sensor identity authored a `{:?}` record (or did not match provenance.source_sensor) — sensor authors are only legal for sensor_observation records bound to provenance.source_sensor",
                record.kind
            ),
        });
    }

    // Chain author entry's `at` — the timestamp captured when the
    // entry was attached to the record. Compared against lifecycle
    // transition timestamps to catch writes that landed before
    // activation or after revocation.
    let chain_author_at = record
        .actor_chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .map(|e| e.at.clone());

    let (status, message) = match author_state {
        AuthorState::Resolved(lc) => classify_resolved(&author, chain_author_at.as_ref(), &lc)?,
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

/// Classify a resolved author against its lifecycle snapshot.
/// Returns `None` when the record is clean (Active and on/after
/// activation), otherwise the `(status, message)` pair the caller
/// wraps into an `AuthorLifecycleFinding`.
fn classify_resolved(
    author: &Identity,
    chain_at: Option<&Rfc3339Timestamp>,
    lc: &AuthorLifecycle,
) -> Option<(ChainStatus, String)> {
    use std::cmp::Ordering;
    match lc.state {
        ProvisioningState::Active => {
            // Active + missing `activated_at` is a corrupted /
            // partially-migrated registry row, not a clean state. The
            // registry's activation path always writes
            // `activated_at` when flipping to Active, so a missing
            // value means the row's invariants are broken; failing
            // open here would silently drop the only ordering check
            // that catches pre-activation tampering. Fail closed
            // (brief invariant 6) — round-10 finding.
            let Some(activated_at) = lc.activated_at.as_ref() else {
                return Some((
                    ChainStatus::Malformed,
                    format!(
                        "author identity `{}` is `Active` but the registry row has no `activated_at` — corrupted lifecycle metadata; fail-closed",
                        author.as_str()
                    ),
                ));
            };
            // Pre-activation tamper: the chain author timestamp is
            // *before* the identity acquired authoritative signing
            // right. Even though current state is Active, the write
            // itself was unsigned-by-design at the moment it landed.
            if let Some(chain_at) = chain_at
                && chain_at.cmp_chronological(activated_at) == Ordering::Less
            {
                return Some((
                    ChainStatus::Malformed,
                    format!(
                        "author identity `{}` is `Active` now but the chain author timestamp ({}) is before `activated_at` ({}) — pre-activation write under an issuer that did not yet have authoritative signing right",
                        author.as_str(),
                        chain_at,
                        activated_at,
                    ),
                ));
            }
            None
        }
        ProvisioningState::Pending => Some((
            ChainStatus::Malformed,
            format!(
                "author identity `{}` is in lifecycle state `Pending` — provisioning never completed, so the issuer never had authoritative signing right",
                author.as_str()
            ),
        )),
        state @ (ProvisioningState::RevokePending | ProvisioningState::PurgePending) => {
            // Round-10 fix: parallel the terminal Revoked branch. Use
            // `revoked_at` as transition evidence — the registry
            // writes `revoked_at` when flipping to RevokePending. If
            // chain `at` < `revoked_at`, the record predates the
            // transition and is legitimate history; only a write at
            // or after the transition is the suspicious-write case.
            if let (Some(chain_at), Some(revoked_at)) = (chain_at, lc.revoked_at.as_ref())
                && chain_at.cmp_chronological(revoked_at) == Ordering::Less
            {
                return Some((
                    ChainStatus::Revoked,
                    format!(
                        "author identity `{}` is `{state:?}` but the chain author timestamp ({}) predates `revoked_at` ({}) — pre-revocation history; non-blocking",
                        author.as_str(),
                        chain_at,
                        revoked_at,
                    ),
                ));
            }
            Some((
                ChainStatus::RevocationInFlight,
                format!(
                    "author identity `{}` is in lifecycle state `{state:?}` — record landed at or after withdrawal began; suspicious write",
                    author.as_str()
                ),
            ))
        }
        state @ (ProvisioningState::Revoked | ProvisioningState::Purged) => {
            // Round-(this loop) fix: terminal revocation/purge with
            // missing `revoked_at` is corrupted/partially-migrated
            // registry metadata, not benign history. The registry's
            // revocation path always writes `revoked_at` when flipping
            // to Revoked/Purged; absence means the only ordering
            // signal that distinguishes pre- from post-revocation
            // writes is gone. Mirror the Active-without-activated_at
            // policy: fail closed (brief invariant 6), do not
            // downgrade to a non-blocking warning.
            let Some(revoked_at) = lc.revoked_at.as_ref() else {
                return Some((
                    ChainStatus::Malformed,
                    format!(
                        "author identity `{}` is `{state:?}` but the registry row has no `revoked_at` — corrupted lifecycle metadata; cannot prove the record predates revocation, fail-closed",
                        author.as_str()
                    ),
                ));
            };
            // Post-revocation write: the chain author timestamp is at
            // or after `revoked_at`. The signing right had already
            // been withdrawn — a real trust violation, not legitimate
            // history.
            if let Some(chain_at) = chain_at
                && chain_at.cmp_chronological(revoked_at) != Ordering::Less
            {
                return Some((
                    ChainStatus::PostRevocationWrite,
                    format!(
                        "author identity `{}` is `{state:?}` and the chain author timestamp ({}) is at-or-after `revoked_at` ({}) — record was signed under withdrawn signing right",
                        author.as_str(),
                        chain_at,
                        revoked_at,
                    ),
                ));
            }
            // chain `at` < `revoked_at` (or chain `at` unknown): the
            // record predates revocation. Legitimate history;
            // non-blocking warning.
            Some((
                ChainStatus::Revoked,
                format!(
                    "author identity `{}` is `{state:?}` (revoked_at {}) — signing right is terminally withdrawn (pre-revocation history; review for audit)",
                    author.as_str(),
                    revoked_at,
                ),
            ))
        }
    }
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

    /// Build an `AuthorState::Resolved` for tests where the lifecycle
    /// *state* drives the verdict and timestamp comparisons are not
    /// the focus. Stamps a far-past `activated_at` and far-future
    /// `revoked_at` so the corruption-detection branches do not fire:
    /// realistic chain `at`s land after activation but before
    /// revocation, putting the record in the legitimate-history case.
    /// Tests that need to exercise the timestamp branches directly
    /// use `resolved_with` instead.
    fn resolved(state: ProvisioningState) -> AuthorState {
        AuthorState::Resolved(AuthorLifecycle {
            state,
            activated_at: Some(Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid")),
            revoked_at: Some(Rfc3339Timestamp::parse("2099-12-31T23:59:59Z").expect("valid")),
        })
    }

    /// Build an `AuthorState::Resolved` with explicit transition
    /// timestamps so tests can pin the timestamp-comparison branches.
    fn resolved_with(
        state: ProvisioningState,
        activated_at: Option<&str>,
        revoked_at: Option<&str>,
    ) -> AuthorState {
        AuthorState::Resolved(AuthorLifecycle {
            state,
            activated_at: activated_at.map(|s| Rfc3339Timestamp::parse(s).expect("valid")),
            revoked_at: revoked_at.map(|s| Rfc3339Timestamp::parse(s).expect("valid")),
        })
    }

    #[test]
    fn active_author_emits_no_finding() {
        let record = record_with_active_author();
        assert_eq!(
            check_author_lifecycle(&record, resolved(ProvisioningState::Active)),
            None
        );
    }

    #[test]
    fn pending_author_is_flagged_as_malformed() {
        let record = record_with_active_author();
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Pending))
            .expect("pending issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert!(finding.author.is_some());
        assert!(finding.message.contains("Pending"));
    }

    #[test]
    fn revoked_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Revoked))
            .expect("revoked issuer must be flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
        assert!(finding.message.contains("Revoked"));
    }

    #[test]
    fn purged_author_is_flagged_as_revoked() {
        let record = record_with_active_author();
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Purged))
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
            check_author_lifecycle(&record, resolved(ProvisioningState::Revoked)).expect("flagged");
        assert_eq!(finding.record_id, expected_id);
        assert_eq!(finding.author, Some(expected_author));
    }

    #[test]
    fn pre_activation_write_under_active_identity_is_flagged_as_malformed() {
        // Round-9 fix: even when current state is Active, a chain
        // author timestamp before `activated_at` proves the write
        // landed while the issuer had no authoritative signing right.
        // This is the "becomes clean once activated" gap.
        let record = record_with_active_author(); // chain `at` = 2026-04-22T14:02:11Z
        let finding = check_author_lifecycle(
            &record,
            resolved_with(
                ProvisioningState::Active,
                Some("2026-05-01T00:00:00Z"), // activated_at AFTER chain.at
                None,
            ),
        )
        .expect("pre-activation write must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert!(finding.message.contains("pre-activation"));
    }

    #[test]
    fn active_without_activated_at_is_flagged_as_malformed() {
        // Round-10 fix: Active + activated_at == None is corrupted
        // registry state, not a clean row. Fail closed.
        let record = record_with_active_author();
        let finding = check_author_lifecycle(
            &record,
            AuthorState::Resolved(AuthorLifecycle {
                state: ProvisioningState::Active,
                activated_at: None,
                revoked_at: None,
            }),
        )
        .expect("corrupted Active row must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert!(finding.message.contains("no `activated_at`"));
    }

    #[test]
    fn revoked_without_revoked_at_is_flagged_as_malformed() {
        // Round-(this loop) fix: terminal Revoked / Purged with
        // `revoked_at == None` is corrupted/partially-migrated
        // registry state, not benign history. Mirrors the Active +
        // missing `activated_at` policy. Fail closed (brief invariant
        // 6); do NOT downgrade post-revocation writes to a warning
        // just because the timestamp is gone.
        let record = record_with_active_author();
        let finding = check_author_lifecycle(
            &record,
            AuthorState::Resolved(AuthorLifecycle {
                state: ProvisioningState::Revoked,
                activated_at: Some(Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid")),
                revoked_at: None,
            }),
        )
        .expect("corrupted Revoked row must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert!(finding.message.contains("no `revoked_at`"));
    }

    #[test]
    fn revoke_pending_with_chain_before_revoked_at_is_warning() {
        // Round-10 fix: a record predating the revoke transition is
        // legitimate history even when the registry is now
        // RevokePending. Falls back to ChainStatus::Revoked so the
        // dispatch leaf maps it to Warning.
        let record = record_with_active_author(); // chain.at = 2026-04-22T14:02:11Z
        let finding = check_author_lifecycle(
            &record,
            resolved_with(
                ProvisioningState::RevokePending,
                Some("2025-01-01T00:00:00Z"),
                Some("2026-12-31T23:59:59Z"), // revoked_at AFTER chain.at
            ),
        )
        .expect("flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
    }

    #[test]
    fn revoke_pending_with_chain_at_or_after_revoked_at_is_error() {
        // Complementary: chain `at` >= revoked_at → suspicious-write
        // case stands.
        let record = record_with_active_author(); // chain.at = 2026-04-22T14:02:11Z
        let finding = check_author_lifecycle(
            &record,
            resolved_with(
                ProvisioningState::RevokePending,
                Some("2025-01-01T00:00:00Z"),
                Some("2026-04-22T14:02:11Z"), // revoked_at == chain.at
            ),
        )
        .expect("flagged");
        assert_eq!(finding.status, ChainStatus::RevocationInFlight);
    }

    #[test]
    fn active_with_chain_at_or_after_activation_is_clean() {
        // The complementary case: chain `at` >= activated_at → clean.
        let record = record_with_active_author();
        assert_eq!(
            check_author_lifecycle(
                &record,
                resolved_with(
                    ProvisioningState::Active,
                    Some("2026-04-01T00:00:00Z"), // activated_at BEFORE chain.at
                    None,
                ),
            ),
            None,
            "chain at-or-after activation must be clean"
        );
    }

    #[test]
    fn post_revocation_write_is_flagged_with_timestamp_evidence() {
        // Round-9 fix: chain `at` >= revoked_at proves the write
        // landed under withdrawn signing right. Surfaces under the
        // dedicated `PostRevocationWrite` variant so the dispatch
        // leaf can route it to Severity::Error rather than collapsing
        // it with legitimate pre-revocation history.
        let record = record_with_active_author(); // chain `at` = 2026-04-22T14:02:11Z
        let finding = check_author_lifecycle(
            &record,
            resolved_with(
                ProvisioningState::Revoked,
                Some("2025-01-01T00:00:00Z"),
                Some("2026-04-22T14:02:11Z"), // revoked_at == chain.at
            ),
        )
        .expect("post-revocation write must surface");
        assert_eq!(finding.status, ChainStatus::PostRevocationWrite);
        assert!(finding.message.contains("at-or-after"));
    }

    #[test]
    fn pre_revocation_history_under_revoked_author_stays_warning_class() {
        // Complementary: chain `at` < revoked_at → legitimate
        // historical record signed before revocation. Stays
        // `ChainStatus::Revoked` (the dispatch leaf maps that to
        // Severity::Warning so routine revocation doesn't poison
        // history).
        let record = record_with_active_author(); // chain `at` = 2026-04-22T14:02:11Z
        let finding = check_author_lifecycle(
            &record,
            resolved_with(
                ProvisioningState::Revoked,
                Some("2025-01-01T00:00:00Z"),
                Some("2026-12-31T23:59:59Z"), // revoked AFTER chain.at
            ),
        )
        .expect("flagged");
        assert_eq!(finding.status, ChainStatus::Revoked);
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
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Active))
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
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Active))
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
            resolved(ProvisioningState::Pending),
            resolved(ProvisioningState::RevokePending),
            resolved(ProvisioningState::Revoked),
            resolved(ProvisioningState::PurgePending),
            resolved(ProvisioningState::Purged),
            AuthorState::MissingFromRegistry,
        ];
        for state in cases {
            let mut record = record_with_active_author();
            record.body = format!("benign prefix {MARKER} benign suffix");
            let label = format!("{state:?}");
            let finding = check_author_lifecycle(&record, state)
                .expect("non-Active state must produce a finding");
            assert!(
                !finding.message.contains(MARKER),
                "finding message leaked record body for state {label}: {}",
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
    fn sensor_authored_record_is_skipped_regardless_of_state() {
        // Sensor identities aren't subject to the human/agent
        // lifecycle state machine. Sensors are commonly machine-derived
        // and may not be provisioned in IdentityRegistry. A
        // sensor-authored sensor_observation record is legitimate
        // captured data, not corruption.
        use crate::domain::{Identity, MemoryClass, MemoryKind};
        let mut record = record_with_active_author();
        // Make this a sensor_observation record authored by the source
        // sensor — the chain validator + record validator both accept
        // this when author == provenance.source_sensor.
        record.kind = MemoryKind::SensorObservation;
        record.class = MemoryClass::Episodic;
        let sensor_id = Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor id");
        let author = record
            .actor_chain
            .iter_mut()
            .find(|e| e.role == ChainRole::Author)
            .expect("sample chain has author");
        author.identity = sensor_id.clone();
        record.provenance.source_sensor = sensor_id.clone();

        // Every state — including MissingFromRegistry which would
        // otherwise emit Malformed — must be a no-op for sensor
        // authors.
        let cases = [
            AuthorState::MissingFromRegistry,
            resolved(ProvisioningState::Active),
            resolved(ProvisioningState::Pending),
            resolved(ProvisioningState::Revoked),
            resolved(ProvisioningState::Purged),
        ];
        for state in cases {
            let label = format!("{state:?}");
            assert_eq!(
                check_author_lifecycle(&record, state),
                None,
                "sensor-authored record must not be flagged for state {label}"
            );
        }
    }

    #[test]
    fn sensor_author_on_non_observation_kind_is_flagged_as_malformed() {
        // A tampered higher-trust record whose author was swapped to a
        // sensor identity is exactly the bypass the at-rest check
        // exists to catch. A blanket "skip all sensor authors" would
        // be a fail-open — the sensor carve-out only applies to
        // sensor_observation records bound to provenance.source_sensor.
        use crate::domain::{Identity, MemoryKind};
        let mut record = record_with_active_author();
        // Leave kind as the sample's default (non-sensor_observation)
        // but swap the author to a sensor identity.
        assert_ne!(record.kind, MemoryKind::SensorObservation);
        let sensor_id = Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor id");
        let author = record
            .actor_chain
            .iter_mut()
            .find(|e| e.role == ChainRole::Author)
            .expect("sample chain has author");
        author.identity = sensor_id.clone();
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Active))
            .expect("sensor author on non-observation record must be flagged");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert_eq!(finding.author, Some(sensor_id));
    }

    #[test]
    fn sensor_author_not_matching_source_sensor_is_flagged_as_malformed() {
        // A sensor_observation record whose author is a *different*
        // sensor than provenance.source_sensor violates the
        // single-source invariant. Must surface as Malformed, not
        // silently skipped.
        use crate::domain::{Identity, MemoryClass, MemoryKind};
        let mut record = record_with_active_author();
        record.kind = MemoryKind::SensorObservation;
        record.class = MemoryClass::Episodic;
        let author_sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor id");
        let other_sensor =
            Identity::parse("snr:local:hook:other-session:v1").expect("valid sensor id");
        let author = record
            .actor_chain
            .iter_mut()
            .find(|e| e.role == ChainRole::Author)
            .expect("sample chain has author");
        author.identity = author_sensor.clone();
        record.provenance.source_sensor = other_sensor;
        let finding = check_author_lifecycle(&record, resolved(ProvisioningState::Active))
            .expect("sensor mismatch must surface");
        assert_eq!(finding.status, ChainStatus::Malformed);
        assert_eq!(finding.author, Some(author_sensor));
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
        let resolved = check_author_lifecycle(&record, resolved(ProvisioningState::Active))
            .expect("active state does not rescue a malformed chain");
        assert_eq!(resolved.status, ChainStatus::Malformed);
    }
}
