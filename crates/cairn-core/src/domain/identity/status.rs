//! Reconciliation and status report types per spec §3.5 / §4.5.

use serde::{Deserialize, Serialize};

use crate::domain::identity::{Identity, keys::VaultId};

/// Per-pending-row outcome from a reconciliation sweep.
///
/// Returned when the keystore and the identity store are compared entry-by-entry.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MismatchOutcome {
    /// The pending identity was successfully moved to the active state.
    Activated,
    /// The pending identity has no matching keystore entry and cannot be activated.
    Orphaned,
    /// The active identity's key material does not match the keystore record.
    KeyMaterialMismatch,
}

/// Accumulated result of a single reconciliation sweep over the identity store.
///
/// Build up a report by calling [`record_mismatch`](ReconciliationReport::record_mismatch),
/// [`record_active_mismatch`](ReconciliationReport::record_active_mismatch), and
/// [`record_active_desync`](ReconciliationReport::record_active_desync) as rows are
/// examined.  When `vault_degraded` is `true` the vault requires operator attention
/// before further writes are permitted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconciliationReport {
    /// Identities whose keystore entry does not match the identity-store row.
    pub mismatched_ids: Vec<Identity>,
    /// Active rows whose keystore private key is missing (`NotFound`).
    ///
    /// Recoverable via `rotate <id>`; not vault-degrading on its own.
    pub desynchronized_active_ids: Vec<Identity>,
    /// `true` when at least one mismatch or active-mismatch has been recorded,
    /// indicating the vault is in a degraded state requiring operator attention.
    pub vault_degraded: bool,
}

impl ReconciliationReport {
    /// Record a pending-row mismatch and mark the vault as degraded.
    pub fn record_mismatch(&mut self, id: Identity) {
        self.mismatched_ids.push(id);
        self.vault_degraded = true;
    }

    /// Record an active-row mismatch and mark the vault as degraded.
    ///
    /// Active mismatches are escalated in the same way as pending mismatches
    /// because both signal a keystore / identity-store divergence.
    pub fn record_active_mismatch(&mut self, id: Identity) {
        self.mismatched_ids.push(id);
        self.vault_degraded = true;
    }

    /// Record an active-row desync (private key `NotFound` in keystore).
    ///
    /// Desyncs are recoverable via `rotate <id>` and do **not** flip
    /// `vault_degraded`.
    pub fn record_active_desync(&mut self, id: Identity) {
        self.desynchronized_active_ids.push(id);
    }
}

/// Point-in-time snapshot of the identity subsystem returned by `cairn status`.
///
/// Covers binding state, default identities, any mismatches found during the
/// last reconciliation sweep, and pending eviction/disable counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityStatusReport {
    /// The vault's unique identifier, or `None` if the vault has never been bound.
    pub vault_id: Option<VaultId>,
    /// Whether the vault is bound to a keystore.
    pub binding_state: BindingState,
    /// Whether default human and agent identities have been initialised.
    pub defaults: DefaultsState,
    /// Identities whose keystore entry mismatches the identity-store row.
    pub mismatched_ids: Vec<Identity>,
    /// Active identities whose private key is missing from the keystore.
    pub desynchronized_active_ids: Vec<Identity>,
    /// Number of identity rows awaiting eviction from the store.
    pub pending_evictions: u64,
    /// Number of key rows awaiting disablement in the keystore.
    pub pending_key_disables: u64,
    /// Identities whose purge has been requested but not yet completed.
    pub purge_pending_ids: Vec<Identity>,
    /// `true` when the vault is in a degraded state (mismatches detected).
    pub vault_degraded: bool,
    /// Outcome of the mismatch check performed during status collection.
    pub mismatch_check: MismatchCheckOutcome,
}

/// Whether the vault is bound to a persistent keystore.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingState {
    /// The vault is fully bound: a `VaultId` exists and all key material is present.
    Bound,
    /// A binding has been requested but has not yet been confirmed by the keystore.
    Pending,
    /// No keystore binding exists; the vault operates without persistent key material.
    Unbound,
}

/// Whether the default human and agent identities have been initialised.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DefaultsState {
    /// No defaults have been provisioned yet.
    NotInitialised,
    /// Defaults are active.
    Active {
        /// The default human identity for this vault.
        human: Identity,
        /// The default agent identity for this vault.
        agent: Identity,
    },
}

/// Outcome of the mismatch-check step performed during `cairn status`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MismatchCheckOutcome {
    /// The check ran to completion without errors.
    Completed,
    /// The keystore was locked; the check was skipped.
    KeystoreLocked,
    /// A `VaultId` conflict was detected; the check was aborted.
    VaultIdConflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(s: &str) -> Identity {
        Identity::parse(s).expect("valid test identity")
    }

    // ── ReconciliationReport ──────────────────────────────────────────────────

    #[test]
    fn record_mismatch_sets_vault_degraded() {
        let mut report = ReconciliationReport::default();
        assert!(!report.vault_degraded);
        report.record_mismatch(make_id("agt:model:opus:reviewer:v1"));
        assert!(report.vault_degraded);
        assert_eq!(report.mismatched_ids.len(), 1);
        assert!(report.desynchronized_active_ids.is_empty());
    }

    #[test]
    fn record_active_mismatch_sets_vault_degraded() {
        let mut report = ReconciliationReport::default();
        report.record_active_mismatch(make_id("hmn:alice"));
        assert!(report.vault_degraded);
        assert_eq!(report.mismatched_ids.len(), 1);
    }

    #[test]
    fn record_active_desync_does_not_set_vault_degraded() {
        let mut report = ReconciliationReport::default();
        report.record_active_desync(make_id("agt:model:sonnet:writer:v1"));
        assert!(!report.vault_degraded);
        assert_eq!(report.desynchronized_active_ids.len(), 1);
        assert!(report.mismatched_ids.is_empty());
    }

    #[test]
    fn multiple_records_accumulate() {
        let mut report = ReconciliationReport::default();
        report.record_mismatch(make_id("hmn:bob"));
        report.record_active_mismatch(make_id("hmn:carol"));
        report.record_active_desync(make_id("agt:model:haiku:monitor:v1"));
        assert_eq!(report.mismatched_ids.len(), 2);
        assert_eq!(report.desynchronized_active_ids.len(), 1);
        assert!(report.vault_degraded);
    }

    // ── IdentityStatusReport round-trip ──────────────────────────────────────

    #[test]
    fn identity_status_report_round_trips_json() {
        let report = IdentityStatusReport {
            vault_id: None,
            binding_state: BindingState::Unbound,
            defaults: DefaultsState::NotInitialised,
            mismatched_ids: vec![],
            desynchronized_active_ids: vec![],
            pending_evictions: 0,
            pending_key_disables: 0,
            purge_pending_ids: vec![],
            vault_degraded: false,
            mismatch_check: MismatchCheckOutcome::Completed,
        };

        let json = serde_json::to_string(&report).expect("serialization must succeed");
        let back: IdentityStatusReport =
            serde_json::from_str(&json).expect("deserialization must succeed");

        assert_eq!(back.pending_evictions, report.pending_evictions);
        assert_eq!(back.vault_degraded, report.vault_degraded);
        assert!(matches!(back.binding_state, BindingState::Unbound));
        assert!(matches!(back.defaults, DefaultsState::NotInitialised));
        assert!(matches!(
            back.mismatch_check,
            MismatchCheckOutcome::Completed
        ));
    }

    #[test]
    fn identity_status_report_with_defaults_round_trips_json() {
        let human = make_id("hmn:tafeng");
        let agent = make_id("agt:claude-code:opus-4-7:reviewer:v1");
        let report = IdentityStatusReport {
            vault_id: None,
            binding_state: BindingState::Bound,
            defaults: DefaultsState::Active {
                human: human.clone(),
                agent: agent.clone(),
            },
            mismatched_ids: vec![make_id("hmn:bob")],
            desynchronized_active_ids: vec![],
            pending_evictions: 3,
            pending_key_disables: 1,
            purge_pending_ids: vec![],
            vault_degraded: true,
            mismatch_check: MismatchCheckOutcome::KeystoreLocked,
        };

        let json = serde_json::to_string(&report).expect("serialization must succeed");
        let back: IdentityStatusReport =
            serde_json::from_str(&json).expect("deserialization must succeed");

        assert_eq!(back.pending_evictions, 3);
        assert!(back.vault_degraded);
        assert_eq!(back.mismatched_ids.len(), 1);
        assert!(matches!(back.binding_state, BindingState::Bound));
        assert!(matches!(
            back.mismatch_check,
            MismatchCheckOutcome::KeystoreLocked
        ));
        if let DefaultsState::Active { human: h, agent: a } = back.defaults {
            assert_eq!(h, human);
            assert_eq!(a, agent);
        } else {
            panic!("expected DefaultsState::Active");
        }
    }

    // ── Enum serde snake_case ─────────────────────────────────────────────────

    #[test]
    fn binding_state_serializes_snake_case() {
        let json = serde_json::to_string(&BindingState::Pending).expect("ser");
        assert_eq!(json, r#""pending""#);
    }

    #[test]
    fn mismatch_check_outcome_serializes_snake_case() {
        let json = serde_json::to_string(&MismatchCheckOutcome::VaultIdConflict).expect("ser");
        assert_eq!(json, r#""vault_id_conflict""#);
    }
}
