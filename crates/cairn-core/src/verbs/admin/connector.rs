//! `cairn.admin.v1` connector enable/disable verbs.
//!
//! v0.2 cut: verbs flip the `connector_state` row via `AdminStateStore`.
//! Runtime cancellation of in-flight polls is owned by the scheduler
//! tick (which reads the same row at the top of each cycle — that's
//! Task 4.2). This keeps cairn-core dependency-free of
//! `cairn-connectors-core` and matches the outer-layer pattern used by
//! snapshot/restore.

use crate::contract::admin_state::{AdminStateStore, ConnectorStateRow};
use crate::domain::admin::{AdminContext, AdminError, AdminRole};

/// Request to enable a connector.
#[derive(Debug, Clone)]
pub struct ConnectorEnableRequest {
    /// Stable connector name (e.g. `"github"`, `"linear"`).
    pub name: String,
}

/// Request to disable a connector.
#[derive(Debug, Clone)]
pub struct ConnectorDisableRequest {
    /// Stable connector name (e.g. `"github"`, `"linear"`).
    pub name: String,
    /// Optional human-readable reason for the change.
    pub reason: Option<String>,
}

/// Response returned by both enable and disable verbs.
#[derive(Debug, Clone)]
pub struct ConnectorStateResponse {
    /// The connector state row as persisted in the store.
    pub row: ConnectorStateRow,
}

/// Mark `name` as enabled in the connector state table.
///
/// # Errors
/// - `AdminError::NotAuthorized` — caller lacks operator role.
/// - `AdminError::Store(_)` — admin store write failure.
pub fn enable(
    ctx: &AdminContext,
    req: &ConnectorEnableRequest,
    admin: &dyn AdminStateStore,
) -> Result<ConnectorStateResponse, AdminError> {
    super::guard::require_role(ctx, admin, AdminRole::Operator)?;
    let row = admin
        .set_connector_enabled(&req.name, true, &ctx.actor, None)
        .map_err(AdminError::Store)?;
    Ok(ConnectorStateResponse { row })
}

/// Mark `name` as disabled in the connector state table. Scheduler
/// honors the flag at its next tick (see Task 4.2 for the runtime gate).
///
/// # Errors
/// Same as [`enable`].
pub fn disable(
    ctx: &AdminContext,
    req: &ConnectorDisableRequest,
    admin: &dyn AdminStateStore,
) -> Result<ConnectorStateResponse, AdminError> {
    super::guard::require_role(ctx, admin, AdminRole::Operator)?;
    let row = admin
        .set_connector_enabled(&req.name, false, &ctx.actor, req.reason.as_deref())
        .map_err(AdminError::Store)?;
    Ok(ConnectorStateResponse { row })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::memory_store::StoreError;
    use crate::domain::Identity;
    use std::sync::Mutex;

    struct StubAdmin {
        is_op: bool,
        last_set: Mutex<Option<(String, bool, Option<String>)>>,
    }

    impl StubAdmin {
        fn new(is_op: bool) -> Self {
            Self {
                is_op,
                last_set: Mutex::new(None),
            }
        }
    }

    impl AdminStateStore for StubAdmin {
        fn has_role(&self, _: &Identity, _: AdminRole) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn has_any_operator(&self) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn grant_role(&self, _: &Identity, _: AdminRole, _: &Identity) -> Result<(), StoreError> {
            Ok(())
        }

        fn revoke_role(&self, _: &Identity, _: AdminRole) -> Result<(), StoreError> {
            Ok(())
        }

        fn set_connector_enabled(
            &self,
            name: &str,
            enabled: bool,
            by: &Identity,
            reason: Option<&str>,
        ) -> Result<ConnectorStateRow, StoreError> {
            *self.last_set.lock().expect("test mutex") =
                Some((name.into(), enabled, reason.map(str::to_string)));
            Ok(ConnectorStateRow::new(
                name.into(),
                enabled,
                chrono::Utc::now(),
                by.clone(),
                reason.map(str::to_string),
            ))
        }

        fn get_connector_state(&self, _: &str) -> Result<Option<ConnectorStateRow>, StoreError> {
            Ok(None)
        }

        fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    fn op_ctx() -> AdminContext {
        AdminContext::new(
            Identity::parse("hmn:op").expect("test fixture: known-valid identity"),
            AdminRole::Operator,
        )
    }

    #[test]
    fn enable_writes_row_when_authorized() {
        let admin = StubAdmin::new(true);
        let resp = enable(
            &op_ctx(),
            &ConnectorEnableRequest {
                name: "github".into(),
            },
            &admin,
        )
        .expect("enable");
        assert!(resp.row.enabled);
        assert_eq!(resp.row.connector_name, "github");
        let last = admin.last_set.lock().expect("test mutex");
        assert_eq!(
            last.as_ref().map(|t| (t.0.as_str(), t.1)),
            Some(("github", true))
        );
    }

    #[test]
    fn disable_writes_row_with_reason() {
        let admin = StubAdmin::new(true);
        let resp = disable(
            &op_ctx(),
            &ConnectorDisableRequest {
                name: "github".into(),
                reason: Some("rate-limit".into()),
            },
            &admin,
        )
        .expect("disable");
        assert!(!resp.row.enabled);
        assert_eq!(resp.row.reason.as_deref(), Some("rate-limit"));
    }

    #[test]
    fn enable_rejects_non_operator() {
        let admin = StubAdmin::new(false);
        let err = enable(
            &op_ctx(),
            &ConnectorEnableRequest {
                name: "github".into(),
            },
            &admin,
        )
        .unwrap_err();
        assert!(matches!(err, AdminError::NotAuthorized { .. }));
        assert!(
            admin.last_set.lock().expect("test mutex").is_none(),
            "no admin store write on auth failure"
        );
    }

    #[test]
    fn disable_rejects_non_operator() {
        let admin = StubAdmin::new(false);
        let err = disable(
            &op_ctx(),
            &ConnectorDisableRequest {
                name: "github".into(),
                reason: None,
            },
            &admin,
        )
        .unwrap_err();
        assert!(matches!(err, AdminError::NotAuthorized { .. }));
    }
}

// ── backfill verb ─────────────────────────────────────────────────────────────

use crate::contract::backfill_spawner::{BackfillSpec, BackfillSpawner};
use crate::domain::workflow::WorkflowId;
use chrono::{DateTime, Utc};

/// Request to spawn a bounded backfill of a named connector.
#[derive(Debug, Clone)]
pub struct BackfillRequest {
    /// Stable connector name (e.g. `"github"`, `"linear"`).
    pub name: String,
    /// Inclusive start of the backfill time window (UTC).
    pub from: DateTime<Utc>,
    /// Inclusive end of the backfill time window (UTC).
    pub to: DateTime<Utc>,
    /// Maximum ingest rate the scheduler should honour (items per second).
    pub rate_per_sec: f64,
}

/// Response returned immediately by the backfill verb.
///
/// Progress is emitted asynchronously by the adapter through a
/// `ProgressEvent` stream; callers use `workflow_id` to correlate events.
#[derive(Debug, Clone)]
pub struct BackfillResponse {
    /// Unique identifier for the spawned workflow.
    pub workflow_id: WorkflowId,
    /// Wall-clock time the verb accepted and enqueued the request.
    pub started_at: DateTime<Utc>,
}

/// Spawn a bounded backfill of `name`. Returns immediately with the
/// workflow id; progress is emitted asynchronously by the adapter.
///
/// # Errors
/// - `AdminError::NotAuthorized` — caller lacks operator role.
/// - `AdminError::UnknownConnector { name }` — the connector has a
///   `connector_state` row marked `enabled = false`. Backfill is refused
///   on disabled connectors; the operator must re-enable first.
/// - `AdminError::Store(_)` — spawner persistence failure.
pub fn backfill(
    ctx: &AdminContext,
    req: &BackfillRequest,
    admin: &dyn AdminStateStore,
    spawner: &dyn BackfillSpawner,
) -> Result<BackfillResponse, AdminError> {
    super::guard::require_role(ctx, admin, AdminRole::Operator)?;

    // Refuse backfill on a connector that is explicitly disabled.
    if let Some(row) = admin
        .get_connector_state(&req.name)
        .map_err(AdminError::Store)?
        && !row.enabled
    {
        return Err(AdminError::UnknownConnector {
            name: format!("{} (currently disabled)", req.name),
        });
    }

    let workflow_id = WorkflowId::new();
    let started_at = Utc::now();
    let spec = BackfillSpec {
        workflow_id: workflow_id.clone(),
        connector_name: req.name.clone(),
        from: req.from,
        to: req.to,
        rate_per_sec: req.rate_per_sec,
        started_at,
        actor: ctx.actor.clone(),
    };
    spawner.spawn_backfill(spec).map_err(AdminError::Store)?;
    Ok(BackfillResponse {
        workflow_id,
        started_at,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod backfill_tests {
    use super::*;
    use crate::contract::backfill_spawner::BackfillSpec;
    use crate::contract::memory_store::StoreError;
    use crate::domain::Identity;
    use std::sync::Mutex;

    // ── local stubs ───────────────────────────────────────────────────────────

    struct StubAdminB {
        is_op: bool,
        connector_enabled: Option<bool>,
    }

    impl StubAdminB {
        fn new(is_op: bool) -> Self {
            Self {
                is_op,
                connector_enabled: None,
            }
        }

        /// Simulate a known connector that is either enabled or disabled.
        fn with_connector(mut self, enabled: bool) -> Self {
            self.connector_enabled = Some(enabled);
            self
        }
    }

    impl AdminStateStore for StubAdminB {
        fn has_role(&self, _: &Identity, _: AdminRole) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn has_any_operator(&self) -> Result<bool, StoreError> {
            Ok(self.is_op)
        }

        fn grant_role(&self, _: &Identity, _: AdminRole, _: &Identity) -> Result<(), StoreError> {
            Ok(())
        }

        fn revoke_role(&self, _: &Identity, _: AdminRole) -> Result<(), StoreError> {
            Ok(())
        }

        fn set_connector_enabled(
            &self,
            name: &str,
            enabled: bool,
            by: &Identity,
            reason: Option<&str>,
        ) -> Result<ConnectorStateRow, StoreError> {
            Ok(ConnectorStateRow::new(
                name.into(),
                enabled,
                chrono::Utc::now(),
                by.clone(),
                reason.map(str::to_string),
            ))
        }

        fn get_connector_state(&self, name: &str) -> Result<Option<ConnectorStateRow>, StoreError> {
            Ok(self.connector_enabled.map(|enabled| {
                ConnectorStateRow::new(
                    name.into(),
                    enabled,
                    chrono::Utc::now(),
                    Identity::parse("hmn:op").expect("test fixture: known-valid identity"),
                    None,
                )
            }))
        }

        fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    struct StubSpawner {
        spawned: Mutex<Vec<BackfillSpec>>,
    }

    impl StubSpawner {
        fn new() -> Self {
            Self {
                spawned: Mutex::new(Vec::new()),
            }
        }
    }

    impl BackfillSpawner for StubSpawner {
        fn spawn_backfill(&self, spec: BackfillSpec) -> Result<(), StoreError> {
            self.spawned.lock().expect("test mutex").push(spec);
            Ok(())
        }
    }

    fn op_ctx() -> AdminContext {
        AdminContext::new(
            Identity::parse("hmn:op").expect("test fixture: known-valid identity"),
            AdminRole::Operator,
        )
    }

    fn sample_req() -> BackfillRequest {
        BackfillRequest {
            name: "github".into(),
            from: chrono::Utc::now() - chrono::Duration::hours(1),
            to: chrono::Utc::now(),
            rate_per_sec: 10.0,
        }
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn backfill_spawns_when_authorized_and_no_state_row() {
        // No connector_state row → treat as "enabled by default" → spawn.
        let admin = StubAdminB::new(true);
        let sink = StubSpawner::new();
        let resp = backfill(&op_ctx(), &sample_req(), &admin, &sink).expect("backfill");
        let jobs = sink.spawned.lock().expect("test mutex");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].connector_name, "github");
        assert_eq!(resp.workflow_id, jobs[0].workflow_id);
    }

    #[test]
    fn backfill_spawns_when_authorized_and_connector_enabled() {
        let admin = StubAdminB::new(true).with_connector(true);
        let sink = StubSpawner::new();
        let resp = backfill(&op_ctx(), &sample_req(), &admin, &sink).expect("backfill");
        let jobs = sink.spawned.lock().expect("test mutex");
        assert_eq!(jobs.len(), 1);
        assert_eq!(resp.workflow_id, jobs[0].workflow_id);
    }

    #[test]
    fn backfill_rejects_disabled_connector() {
        let admin = StubAdminB::new(true).with_connector(false);
        let sink = StubSpawner::new();
        let err = backfill(&op_ctx(), &sample_req(), &admin, &sink).unwrap_err();
        assert!(
            matches!(err, AdminError::UnknownConnector { .. }),
            "expected UnknownConnector, got {err:?}"
        );
        assert!(
            sink.spawned.lock().expect("test mutex").is_empty(),
            "sink must not be called on disabled connector"
        );
    }

    #[test]
    fn backfill_rejects_non_operator() {
        let admin = StubAdminB::new(false);
        let sink = StubSpawner::new();
        let err = backfill(&op_ctx(), &sample_req(), &admin, &sink).unwrap_err();
        assert!(
            matches!(err, AdminError::NotAuthorized { .. }),
            "expected NotAuthorized, got {err:?}"
        );
        assert!(sink.spawned.lock().expect("test mutex").is_empty());
    }
}
