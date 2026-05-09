//! Lock acquisition for record WAL operations.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::domain::{ScopeTuple, TargetId};
use tokio_rusqlite::Connection;

use crate::locks::{LockHandle, LockMode, ResourceKey, acquire};

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
    let (tenant, workspace) = scope_lock_parts(scope);
    let mut handles = Vec::with_capacity(2);
    handles.push(
        acquire(
            conn,
            &ResourceKey::entity(&tenant, &workspace, target.as_str()),
            LockMode::Exclusive,
            &format!("{op_id}:entity"),
            Duration::from_secs(30),
            incarnation,
            operation,
        )
        .await?,
    );
    if let Some(session_id) = scope.session_id.as_deref() {
        handles.push(
            acquire(
                conn,
                &ResourceKey::session(&tenant, &workspace, session_id),
                LockMode::Shared,
                &format!("{op_id}:session"),
                Duration::from_secs(30),
                incarnation,
                operation,
            )
            .await?,
        );
    }
    Ok(RecordLocks::new(handles))
}

fn scope_lock_parts(scope: &ScopeTuple) -> (String, String) {
    (
        scope.tenant.as_deref().unwrap_or("default").to_owned(),
        scope.workspace.as_deref().unwrap_or("default").to_owned(),
    )
}
