//! Lock acquisition for record WAL operations.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::domain::{ScopeTuple, TargetId};
use tokio_rusqlite::Connection;

use crate::locks::{LockHandle, LockMode, LockScope, ResourceKey, acquire};

pub(crate) struct RecordLocks {
    handles: Vec<LockHandle>,
}

impl RecordLocks {
    #[must_use]
    pub(crate) fn new(handles: Vec<LockHandle>) -> Self {
        Self { handles }
    }

    pub(crate) fn assert_live_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<(), crate::locks::LockError> {
        for handle in &self.handles {
            handle.assert_live_in_tx(tx)?;
        }
        Ok(())
    }
}

pub(crate) async fn acquire_for_record(
    conn: &Arc<Connection>,
    scope: &ScopeTuple,
    target: &TargetId,
    incarnation: &Arc<str>,
    op_id: &str,
    operation: &'static str,
) -> Result<RecordLocks, crate::locks::LockError> {
    let resources = record_lock_resources(scope, target);
    let mut handles = Vec::with_capacity(resources.len());
    for resource in resources {
        let (mode, holder_suffix) = match resource.scope() {
            LockScope::Vault => (LockMode::Exclusive, "lineage"),
            LockScope::Entity => (LockMode::Exclusive, "entity"),
            LockScope::Session => (LockMode::Shared, "session"),
        };
        handles.push(
            acquire(
                conn,
                &resource,
                mode,
                &format!("{op_id}:{holder_suffix}"),
                Duration::from_secs(30),
                incarnation,
                operation,
            )
            .await?,
        );
    }
    Ok(RecordLocks::new(handles))
}

fn record_lock_resources(scope: &ScopeTuple, target: &TargetId) -> Vec<ResourceKey> {
    let (tenant, workspace) = scope_lock_parts(scope);
    let mut resources = Vec::with_capacity(if scope.session_id.is_some() { 3 } else { 2 });
    resources.push(target_lineage_resource(target));
    resources.push(ResourceKey::entity(&tenant, &workspace, target.as_str()));
    if let Some(session_id) = scope.session_id.as_deref() {
        resources.push(ResourceKey::session(&tenant, &workspace, session_id));
    }
    resources
}

fn target_lineage_resource(target: &TargetId) -> ResourceKey {
    ResourceKey::vault(&format!("record-target-lineage:{}", target.as_str()))
}

fn scope_lock_parts(scope: &ScopeTuple) -> (String, String) {
    (
        scope.tenant.as_deref().unwrap_or("default").to_owned(),
        scope.workspace.as_deref().unwrap_or("default").to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_lineage_resource_is_stable_and_scope_independent() {
        let target = TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id");
        let mut scope_a = ScopeTuple::default();
        scope_a.tenant = Some("tenant-a".to_owned());
        scope_a.workspace = Some("workspace-a".to_owned());
        let mut scope_b = ScopeTuple::default();
        scope_b.tenant = Some("tenant-b".to_owned());
        scope_b.workspace = Some("workspace-b".to_owned());

        let resource_a = record_lock_resources(&scope_a, &target)
            .into_iter()
            .next()
            .expect("lineage resource");
        let resource_b = record_lock_resources(&scope_b, &target)
            .into_iter()
            .next()
            .expect("lineage resource");

        assert_eq!(resource_a, resource_b);
        assert_eq!(
            resource_a.as_resource_str(),
            "vault:record-target-lineage:01HQZX9F5N0000000000000000"
        );
    }
}
