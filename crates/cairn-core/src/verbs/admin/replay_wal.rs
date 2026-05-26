//! `cairn.admin.v1` `replay_wal` verb (outer-layer orchestrator).
//!
//! v0.2 ships dry-run mode end-to-end. Apply mode is gated behind a
//! `WalApplyAdapter` trait that isn't wired in v0.2 — calls with
//! `apply: true` short-circuit to `AdminError::ReplayEscalated` after
//! the operator-role check.
//!
//! TODO(#161 follow-up): wire `WalApplyAdapter` + cairn-store-sqlite impl
//! so apply mode re-invokes idempotent steps and escalates non-idempotent
//! ones to `PURGE_PENDING` per brief §5.6.

use crate::contract::admin_state::AdminStateStore;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};
use crate::wal::{StepEvent, WalKind, graph_for, iter_from};

/// Request parameters for the `replay_wal` verb.
#[derive(Debug, Clone)]
pub struct ReplayWalRequest {
    /// Which WAL operation kind to replay.
    pub kind: WalKind,
    /// Starting ordinal (inclusive). Steps at indices `[from_ord, len)` are visited.
    pub from_ord: u32,
    /// When `false` (dry-run): collect and return the step sequence without
    /// applying any side-effects. When `true` (apply mode): requires
    /// operator role; currently deferred — returns
    /// [`AdminError::ReplayEscalated`] immediately.
    pub apply: bool,
}

/// Response produced by a successful `replay_wal` run.
#[derive(Debug, Clone)]
pub struct ReplayWalResponse {
    /// Number of steps visited in the graph (counting from `from_ord`).
    pub steps_visited: u64,
    /// Number of steps applied (always `0` in dry-run; deferred in apply mode).
    pub steps_applied: u64,
    /// Ordered sequence of [`StepEvent`]s that were visited.
    pub events: Vec<StepEvent>,
}

/// Run the `replay_wal` verb.
///
/// # Dry-run mode (`apply: false`)
///
/// No role check is required (read-only, safe per spec §7.1). The verb calls
/// [`iter_from`] over the static step graph for `kind` and collects every
/// [`StepEvent`] at or after `from_ord` into [`ReplayWalResponse::events`].
///
/// # Apply mode (`apply: true`) — deferred
///
/// Requires [`AdminRole::Operator`]; after that check passes the verb
/// immediately returns [`AdminError::ReplayEscalated`] with
/// `step = "apply-mode-not-yet-wired"`. A follow-up issue will wire the
/// `WalApplyAdapter` trait and the `cairn-store-sqlite` implementation
/// (brief §5.6 `PURGE_PENDING` escalation path).
///
/// # Errors
/// - [`AdminError::NotAuthorized`] — apply mode requested without operator role.
/// - [`AdminError::Store`] — wraps [`crate::wal::ReplayError::OrdOutOfRange`]
///   when `from_ord` is past the end of the step graph.
/// - [`AdminError::ReplayEscalated`] — apply mode is requested; deferred in v0.2.
pub fn run(
    ctx: &AdminContext,
    req: &ReplayWalRequest,
    admin: &dyn AdminStateStore,
) -> Result<ReplayWalResponse, AdminError> {
    if req.apply {
        super::guard::require_role(ctx, admin, AdminRole::Operator)?;
        return Err(AdminError::ReplayEscalated {
            step: "apply-mode-not-yet-wired".into(),
        });
    }

    // Dry-run path: no role check (read-only-safe per spec §7.1).
    let graph = graph_for(req.kind);
    let events: Vec<StepEvent> = iter_from(graph, req.from_ord)
        .map_err(|e| AdminError::Store(Box::new(e)))?
        .collect();

    let visited = u64::try_from(events.len()).unwrap_or(u64::MAX);
    Ok(ReplayWalResponse {
        steps_visited: visited,
        steps_applied: 0,
        events,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::admin_state::{AdminStateStore, ConnectorStateRow};
    use crate::contract::memory_store::StoreError;
    use crate::domain::Identity;

    struct StubAdmin {
        is_op: bool,
    }

    impl AdminStateStore for StubAdmin {
        fn has_role(&self, _: &Identity, _: AdminRole) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn has_any_operator(&self) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn grant_role(
            &self,
            _: &Identity,
            _: AdminRole,
            _: &Identity,
        ) -> Result<(), StoreError> {
            Ok(())
        }

        fn revoke_role(&self, _: &Identity, _: AdminRole) -> Result<(), StoreError> {
            Ok(())
        }

        fn set_connector_enabled(
            &self,
            name: &str,
            en: bool,
            by: &Identity,
            reason: Option<&str>,
        ) -> Result<ConnectorStateRow, StoreError> {
            Ok(ConnectorStateRow::new(
                name.into(),
                en,
                chrono::Utc::now(),
                by.clone(),
                reason.map(str::to_string),
            ))
        }

        fn get_connector_state(
            &self,
            _: &str,
        ) -> Result<Option<ConnectorStateRow>, StoreError> {
            Ok(None)
        }

        fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    fn ctx() -> AdminContext {
        AdminContext::new(
            Identity::parse("hmn:op").expect("test fixture"),
            AdminRole::Operator,
        )
    }

    #[test]
    fn dry_run_no_role_needed() {
        let admin = StubAdmin { is_op: false };
        let req = ReplayWalRequest {
            kind: WalKind::Upsert,
            from_ord: 0,
            apply: false,
        };
        let resp = run(&ctx(), &req, &admin).expect("dry-run");
        assert!(resp.steps_visited > 0);
        assert_eq!(resp.steps_applied, 0);
        assert_eq!(resp.events.len() as u64, resp.steps_visited);
    }

    #[test]
    fn dry_run_from_middle() {
        let admin = StubAdmin { is_op: false };
        let g = graph_for(WalKind::Upsert);
        let from = u32::try_from(g.len() / 2).unwrap();
        let req = ReplayWalRequest {
            kind: WalKind::Upsert,
            from_ord: from,
            apply: false,
        };
        let resp = run(&ctx(), &req, &admin).expect("dry-run");
        assert_eq!(resp.events[0].ord, from);
    }

    #[test]
    fn dry_run_past_end_errors_as_store() {
        let admin = StubAdmin { is_op: false };
        let g = graph_for(WalKind::Upsert);
        let past = u32::try_from(g.len()).unwrap();
        let req = ReplayWalRequest {
            kind: WalKind::Upsert,
            from_ord: past,
            apply: false,
        };
        let err = run(&ctx(), &req, &admin).unwrap_err();
        assert!(matches!(err, AdminError::Store(_)));
    }

    #[test]
    fn apply_without_role_rejected() {
        let admin = StubAdmin { is_op: false };
        let req = ReplayWalRequest {
            kind: WalKind::Upsert,
            from_ord: 0,
            apply: true,
        };
        let err = run(&ctx(), &req, &admin).unwrap_err();
        assert!(matches!(err, AdminError::NotAuthorized { .. }));
    }

    #[test]
    fn apply_with_role_escalates_not_yet_wired() {
        let admin = StubAdmin { is_op: true };
        let req = ReplayWalRequest {
            kind: WalKind::Upsert,
            from_ord: 0,
            apply: true,
        };
        let err = run(&ctx(), &req, &admin).unwrap_err();
        match err {
            AdminError::ReplayEscalated { step } => {
                assert_eq!(step, "apply-mode-not-yet-wired");
            }
            other => panic!("expected ReplayEscalated, got {other:?}"),
        }
    }
}
