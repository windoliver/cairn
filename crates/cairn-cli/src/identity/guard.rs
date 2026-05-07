//! Issuer-dependent verb guard per spec §3.5.
//!
//! Every verb that signs or mutates identity-bearing records must call
//! [`refuse_if_degraded`] before doing any work.  When the vault's
//! reconciliation report carries `vault_degraded = true` the guard returns
//! [`IdentityServiceError::VaultDegraded`], which the caller maps to
//! `EX_TEMPFAIL` (75) and rejects the operation.
//!
//! # Full async wiring (deferred to issue #9)
//!
//! [`open_for_signed_verb`] is the production entry point.  It calls
//! [`IdentityService::open`], runs the full reconciliation sweep, and
//! refuses if the vault is degraded.  Today's verb handlers (`ingest`,
//! `forget`) are synchronous stubs; once they are promoted to async in
//! issue #9 they should call [`open_for_signed_verb`] directly.

use std::path::PathBuf;

use cairn_core::{domain::identity::Identity, error::identity::IdentityServiceError};

use super::{IdentityService, status::ReconciliationReport};

/// Open the identity service and refuse if any reconciliation mismatch
/// flagged the vault as degraded.
///
/// This is the production entry point for signed verbs.  It:
/// 1. Calls [`IdentityService::open`] which runs the full reconciliation sweep.
/// 2. Inspects the returned [`ReconciliationReport`].
/// 3. Returns `Err(IdentityServiceError::VaultDegraded { … })` when
///    `report.vault_degraded` is `true`.
///
/// # Errors
///
/// - Propagates any [`IdentityServiceError`] returned by `IdentityService::open`.
/// - Returns [`IdentityServiceError::VaultDegraded`] when the reconciliation
///   report indicates key-material mismatches.
pub async fn open_for_signed_verb(
    vault_path: PathBuf,
) -> Result<IdentityService, IdentityServiceError> {
    let (svc, report) = IdentityService::open(vault_path).await?;
    refuse_if_degraded(&report, report.mismatched_ids.clone())?;
    Ok(svc)
}

/// Pure guard: return `Err(VaultDegraded)` when `report.vault_degraded` is set.
///
/// Extracted from [`open_for_signed_verb`] so that unit tests can exercise both
/// branches (clean vault → `Ok(())`; degraded vault → `Err(VaultDegraded)`)
/// without touching the OS keychain or any filesystem state.
///
/// # Errors
///
/// Returns [`IdentityServiceError::VaultDegraded`] when `report.vault_degraded`
/// is `true`, carrying a clone of `mismatched_ids` as context for the caller.
#[cfg_attr(test, allow(dead_code))]
pub fn refuse_if_degraded(
    report: &ReconciliationReport,
    mismatched_ids: Vec<Identity>,
) -> Result<(), IdentityServiceError> {
    if report.keystore_locked {
        // Incomplete sweep: cannot prove vault is healthy. Hard stop for
        // any caller that intends to mutate state.
        return Err(IdentityServiceError::VaultReconciliationIncomplete);
    }
    if report.vault_degraded {
        return Err(IdentityServiceError::VaultDegraded { mismatched_ids });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cairn_core::domain::identity::Identity;

    use super::*;

    fn id(s: &str) -> Identity {
        Identity::parse(s).expect("valid test identity")
    }

    // ── refuse_if_degraded — clean report ────────────────────────────────────

    #[test]
    fn clean_report_passes_guard() {
        let report = ReconciliationReport::default();
        let result = refuse_if_degraded(&report, vec![]);
        assert!(result.is_ok(), "clean vault must pass the guard");
    }

    // ── refuse_if_degraded — degraded via pending mismatch ───────────────────

    #[test]
    fn degraded_report_from_mismatch_refuses() {
        let mut report = ReconciliationReport::default();
        let alice = id("hmn:alice:v1");
        report.record_mismatch(alice.clone());
        let result = refuse_if_degraded(&report, report.mismatched_ids.clone());
        match result {
            Err(IdentityServiceError::VaultDegraded { mismatched_ids }) => {
                assert_eq!(mismatched_ids, vec![alice]);
            }
            other => panic!("expected VaultDegraded, got {other:?}"),
        }
    }

    // ── refuse_if_degraded — degraded via active mismatch ───────────────────

    #[test]
    fn degraded_report_from_active_mismatch_refuses() {
        let mut report = ReconciliationReport::default();
        let bot = id("agt:claude-code:opus-4-7:main:v1");
        report.record_active_mismatch(bot.clone());
        let result = refuse_if_degraded(&report, report.mismatched_ids.clone());
        assert!(
            matches!(result, Err(IdentityServiceError::VaultDegraded { .. })),
            "active mismatch must flip vault_degraded and trigger refusal"
        );
    }

    // ── refuse_if_degraded — desync alone does NOT degrade ──────────────────

    #[test]
    fn desync_only_does_not_refuse() {
        let mut report = ReconciliationReport::default();
        report.record_active_desync(id("agt:claude-code:sonnet:writer:v1"));
        // vault_degraded is false for desyncs; guard must pass.
        let result = refuse_if_degraded(&report, report.mismatched_ids.clone());
        assert!(
            result.is_ok(),
            "desync-only vault must not be refused by the guard"
        );
    }

    // ── refuse_if_degraded — multiple mismatched_ids carried through ─────────

    #[test]
    fn multiple_mismatched_ids_carried_in_error() {
        let mut report = ReconciliationReport::default();
        let a = id("hmn:alice:v1");
        let b = id("hmn:bob:v2");
        report.record_mismatch(a.clone());
        report.record_mismatch(b.clone());
        let ids = report.mismatched_ids.clone();
        let result = refuse_if_degraded(&report, ids);
        match result {
            Err(IdentityServiceError::VaultDegraded { mismatched_ids }) => {
                assert_eq!(mismatched_ids.len(), 2);
                assert!(mismatched_ids.contains(&a));
                assert!(mismatched_ids.contains(&b));
            }
            other => panic!("expected VaultDegraded, got {other:?}"),
        }
    }
}
