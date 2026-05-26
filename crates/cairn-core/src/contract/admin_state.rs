//! Persistence boundary for admin role + connector enable/disable state.

use chrono::{DateTime, Utc};

use crate::contract::memory_store::StoreError;
use crate::domain::admin::AdminRole;
use crate::domain::Identity;

/// A snapshot of a connector's enable/disable state as persisted in the store.
///
/// `#[non_exhaustive]` — additional fields (e.g. metadata, TTL) may be added
/// without breaking existing destructuring.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectorStateRow {
    /// Stable connector name (e.g. `"github"`, `"linear"`).
    pub connector_name: String,
    /// Whether the connector is currently enabled.
    pub enabled: bool,
    /// Wall-clock time when this row was last written.
    pub last_changed_at: DateTime<Utc>,
    /// Identity of the operator who made the last change.
    pub last_changed_by: Identity,
    /// Optional human-readable reason for the change.
    pub reason: Option<String>,
}

impl ConnectorStateRow {
    /// Construct a new [`ConnectorStateRow`].
    ///
    /// Required because `#[non_exhaustive]` prevents struct-literal construction
    /// outside this crate (e.g. in `cairn-store-sqlite`).
    #[must_use]
    pub fn new(
        connector_name: String,
        enabled: bool,
        last_changed_at: DateTime<Utc>,
        last_changed_by: Identity,
        reason: Option<String>,
    ) -> Self {
        Self {
            connector_name,
            enabled,
            last_changed_at,
            last_changed_by,
            reason,
        }
    }
}

/// Persistence contract for admin role assignments and connector enable/disable state.
pub trait AdminStateStore: Send + Sync {
    /// Returns true iff `actor` currently holds `role` (a live, unrevoked assignment).
    fn has_role(&self, actor: &Identity, role: AdminRole) -> Result<bool, StoreError>;

    /// Returns `true` iff at least one identity currently holds the `Operator` role.
    ///
    /// Used by `status::advertise` to decide whether to publish admin capabilities.
    fn has_any_operator(&self) -> Result<bool, StoreError>;

    /// Grant `role` to `actor`. Idempotent: re-granting an active role is a no-op.
    fn grant_role(
        &self,
        actor: &Identity,
        role: AdminRole,
        granted_by: &Identity,
    ) -> Result<(), StoreError>;

    /// Revoke `role` from `actor`. No-op if `actor` does not currently hold the role.
    fn revoke_role(&self, actor: &Identity, role: AdminRole) -> Result<(), StoreError>;

    /// Upsert connector enable/disable row. Returns the row as persisted.
    fn set_connector_enabled(
        &self,
        connector_name: &str,
        enabled: bool,
        changed_by: &Identity,
        reason: Option<&str>,
    ) -> Result<ConnectorStateRow, StoreError>;

    /// Returns `None` if the connector has no state row (treated as "enabled" by default).
    fn get_connector_state(
        &self,
        connector_name: &str,
    ) -> Result<Option<ConnectorStateRow>, StoreError>;

    /// Snapshot of every known connector state row, for `status.connectors[]`.
    fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError>;
}
