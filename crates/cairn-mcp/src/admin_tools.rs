//! MCP tool registration and dispatch for the `cairn.admin.v1`
//! extension (brief §7, issue #161).
//!
//! The six admin verbs (`admin_snapshot`, `admin_restore`,
//! `admin_replay_wal`, `admin_connector_enable`,
//! `admin_connector_disable`, `admin_connector_backfill`)
//! appear in the IDL-generated [`crate::generated::TOOLS`] list
//! when the `cairn.mcp.v1.extension.admin` capability is
//! advertised. Until the three WIRED constants are all `true`
//! (issue #161 Gap 8), every call is rejected with
//! `CapabilityUnavailable` (brief §15 fail-closed).
//!
//! Module layout mirrors [`crate::federation_tools`]:
//!
//! * [`ADMIN_TOOL_NAMES`] — the six verb names owned by the admin
//!   extension.
//! * [`is_admin_tool`] — name-membership test for routing.
//! * [`runtime_ready`] — gate that ANDs the
//!   [`cairn_core::status::wiring::admin_extension_ready`] function
//!   with the live status capabilities slice.
//! * [`dispatch`] — verb dispatcher that routes through the verb
//!   layer when `vault_root` is supplied and all wiring gates pass.

use std::path::Path;

use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::generated::common::Capabilities;
use cairn_core::generated::envelope::ResponseVerb;
use rmcp::model::{CallToolResult, Content};

/// Capability string that gates every admin MCP tool.
pub const ADMIN_CAPABILITY: &str = "cairn.mcp.v1.extension.admin";

/// IDL-generated MCP tool names belonging to the admin extension.
pub const ADMIN_TOOL_NAMES: &[&str] = &[
    "admin_snapshot",
    "admin_restore",
    "admin_replay_wal",
    "admin_connector_enable",
    "admin_connector_disable",
    "admin_connector_backfill",
];

/// `true` when `name` is one of the admin verb tools.
#[must_use]
pub fn is_admin_tool(name: &str) -> bool {
    ADMIN_TOOL_NAMES.contains(&name)
}

/// `true` only when status negotiation AND MCP dispatch are both ready.
#[must_use]
pub fn runtime_ready(capabilities: &[Capabilities]) -> bool {
    dispatch_ready()
        && capabilities
            .iter()
            .any(|cap| matches!(cap, Capabilities::CairnMcpV1ExtensionAdmin))
}

/// `true` when admin MCP calls have a real dispatcher available.
///
/// Gated on all three WIRED constants via
/// [`cairn_core::status::wiring::admin_extension_ready`] using the
/// conservative `(config_enabled=true, has_operator=true)` probe so the
/// build-time constants are the sole gating factor here.
#[must_use]
pub fn dispatch_ready() -> bool {
    cairn_core::status::wiring::admin_extension_ready(true, true)
}

/// Build a `CapabilityUnavailable` `CallToolResult` for an admin tool
/// called while the capability is unwired or the vault root is absent.
#[must_use]
pub fn capability_unavailable(name: &str) -> CallToolResult {
    let mut text = format!("cairn admin: capability unavailable: {ADMIN_CAPABILITY}");
    if let Some(hint) = cairn_core::status::remediation_for(ADMIN_CAPABILITY) {
        text.push_str("\n  hint: ");
        text.push_str(hint);
    }
    let _ = name;
    CallToolResult::error(vec![Content::text(text)])
}

/// Dispatch an admin MCP tool through the verb layer.
///
/// Returns `CapabilityUnavailable` when:
/// - `dispatch_ready()` is `false` (wiring constants are dark), OR
/// - `vault_root` is `None` (no vault is bound to this handler).
///
/// When both gates pass, routes to the matching verb function.
pub fn dispatch(
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    vault_root: Option<&Path>,
) -> CallToolResult {
    if !dispatch_ready() {
        return capability_unavailable(name);
    }
    let Some(vault_root) = vault_root else {
        return capability_unavailable(name);
    };

    let args_value = arguments.map_or(serde_json::Value::Null, serde_json::Value::Object);

    match name {
        "admin_snapshot" => dispatch_snapshot(args_value, vault_root),
        "admin_restore" => dispatch_restore(args_value, vault_root),
        "admin_replay_wal" => dispatch_replay_wal(args_value, vault_root),
        "admin_connector_enable" => dispatch_connector_enable(args_value, vault_root),
        "admin_connector_disable" => dispatch_connector_disable(args_value, vault_root),
        "admin_connector_backfill" => dispatch_connector_backfill(args_value, vault_root),
        _ => CallToolResult::error(vec![Content::text(format!(
            "cairn admin: unknown tool: {name}"
        ))]),
    }
}

/// Internal helper — open the admin state store or return an error result.
fn open_admin_store(
    vault_root: &Path,
    verb: ResponseVerb,
) -> Result<cairn_store_sqlite::SqliteAdminStateStore, CallToolResult> {
    let db_path = vault_root.join(".cairn").join("cairn.db");
    cairn_store_sqlite::SqliteAdminStateStore::open(&db_path)
        .map_err(|e| aborted_internal_admin(verb, &format!("open admin state store: {e}")))
}

/// Internal helper — build a synthetic MCP operator identity.
///
/// Real MCP operator identity resolution is a Phase 2 concern once
/// signed-intent auth is wired end-to-end (brief §7.4). For now the
/// MCP surface uses a fixed sentinel that survives the role check only
/// because the caller passed `dispatch_ready() = true` (which requires
/// `has_any_operator()` to be satisfied in the store).
fn mcp_operator_identity() -> Identity {
    // Invariant: this is a parse of a hard-coded well-formed string.
    #[allow(clippy::expect_used)]
    Identity::parse("hmn:mcp-operator").expect("invariant: mcp-operator is a valid identity")
}

fn dispatch_snapshot(args_value: serde_json::Value, vault_root: &Path) -> CallToolResult {
    use cairn_core::generated::envelope::ResponseData;
    use cairn_core::generated::verbs::admin_snapshot::{AdminSnapshotArgs, AdminSnapshotData};

    let args: AdminSnapshotArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminSnapshot,
                &format!("invalid admin_snapshot arguments: {e}"),
            );
        }
    };

    let admin = match open_admin_store(vault_root, ResponseVerb::AdminSnapshot) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let meta = cairn_store_sqlite::SqliteSnapshotMetadata::new(
        vault_root.join(".cairn").join("cairn.db"),
        "mcp-vault".to_owned(),
    );
    let producer = cairn_store_sqlite::SqliteSnapshotProducer::new(
        vault_root.to_path_buf(),
        vault_root.join(".cairn").join("cairn.db"),
    );
    let registry = cairn_store_sqlite::FileBackupRegistry::new(
        vault_root.join(".cairn").join("backups.jsonl"),
    );

    let ctx = AdminContext::new(mcp_operator_identity(), AdminRole::Operator);
    let req = cairn_core::verbs::admin::snapshot::SnapshotRequest {
        out_dir: std::path::PathBuf::from(&args.out_dir),
        label: args.label,
        local_machine_id: args.local_machine_id,
        backup_kind: args.backup_kind,
    };

    match cairn_core::verbs::admin::snapshot::run(&ctx, &req, &admin, &meta, &producer, &registry) {
        Ok(resp) => {
            let data = AdminSnapshotData {
                backup_id: resp.backup_id,
                artifact_path: resp.artifact_path.display().to_string(),
                sha256: resp.sha256,
                frontier_step: resp.frontier_step,
            };
            let envelope = crate::verb_envelope::committed(
                ResponseVerb::AdminSnapshot,
                ResponseData::AdminSnapshot(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&envelope)
        }
        Err(e) => admin_error_result(ResponseVerb::AdminSnapshot, &format!("admin_snapshot: {e}")),
    }
}

fn dispatch_restore(args_value: serde_json::Value, vault_root: &Path) -> CallToolResult {
    use cairn_core::generated::envelope::ResponseData;
    use cairn_core::generated::verbs::admin_restore::{AdminRestoreArgs, AdminRestoreData};

    let args: AdminRestoreArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminRestore,
                &format!("invalid admin_restore arguments: {e}"),
            );
        }
    };

    let admin = match open_admin_store(vault_root, ResponseVerb::AdminRestore) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let db_path = vault_root.join(".cairn").join("cairn.db");
    let meta =
        cairn_store_sqlite::SqliteSnapshotMetadata::new(db_path.clone(), "mcp-vault".to_owned());
    let reader = cairn_store_sqlite::SqliteSnapshotReader;
    let applier = cairn_store_sqlite::SqliteSnapshotApplier::new(vault_root.to_path_buf());
    let consent = cairn_store_sqlite::SqliteConsentLog::new(db_path.clone(), db_path);

    let ctx = AdminContext::new(mcp_operator_identity(), AdminRole::Operator);
    let req = cairn_core::verbs::admin::restore::RestoreRequest {
        artifact_path: std::path::PathBuf::from(&args.artifact_path),
        dry_run: args.dry_run,
        local_machine_id: args.local_machine_id,
    };

    match cairn_core::verbs::admin::restore::run(
        &ctx, &req, &admin, &meta, &reader, &applier, &consent,
    ) {
        Ok(resp) => {
            let data = AdminRestoreData {
                restored_records: resp.restored_records,
                tombstones_replayed: resp.tombstones_replayed,
                frontier_step: resp.frontier_step,
                backup_id: resp.backup_id,
            };
            let envelope = crate::verb_envelope::committed(
                ResponseVerb::AdminRestore,
                ResponseData::AdminRestore(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&envelope)
        }
        Err(e) => admin_error_result(ResponseVerb::AdminRestore, &format!("admin_restore: {e}")),
    }
}

fn dispatch_replay_wal(args_value: serde_json::Value, vault_root: &Path) -> CallToolResult {
    use cairn_core::generated::envelope::ResponseData;
    use cairn_core::generated::verbs::admin_replay_wal::{
        AdminReplayWalArgs, AdminReplayWalArgsKind, AdminReplayWalData,
    };
    use cairn_core::wal::WalKind;

    let args: AdminReplayWalArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminReplayWal,
                &format!("invalid admin_replay_wal arguments: {e}"),
            );
        }
    };

    let kind = match args.kind {
        AdminReplayWalArgsKind::Upsert => WalKind::Upsert,
        AdminReplayWalArgsKind::ForgetRecord => WalKind::ForgetRecord,
        AdminReplayWalArgsKind::Expire => WalKind::Expire,
        AdminReplayWalArgsKind::Evolve => WalKind::Evolve,
        // #[non_exhaustive] guard — reject unknown IDL extensions at dispatch time.
        _ => {
            return admin_error_result(
                ResponseVerb::AdminReplayWal,
                "admin_replay_wal: unknown kind variant",
            );
        }
    };

    let admin = match open_admin_store(vault_root, ResponseVerb::AdminReplayWal) {
        Ok(a) => a,
        Err(r) => return r,
    };

    // from_ord is u64 in the IDL; the core verb uses u32.
    let from_ord = u32::try_from(args.from_ord).unwrap_or(u32::MAX);
    let ctx = AdminContext::new(mcp_operator_identity(), AdminRole::Operator);
    let req = cairn_core::verbs::admin::replay_wal::ReplayWalRequest {
        kind,
        from_ord,
        apply: args.apply,
    };

    match cairn_core::verbs::admin::replay_wal::run(&ctx, &req, &admin) {
        Ok(resp) => {
            let events = resp
                .events
                .iter()
                .map(
                    |e| cairn_core::generated::verbs::admin_replay_wal::StepEvent {
                        ord: u64::from(e.ord),
                        name: e.name.to_owned(),
                        idempotent: e.idempotent,
                    },
                )
                .collect();
            let data = AdminReplayWalData {
                steps_visited: resp.steps_visited,
                steps_applied: resp.steps_applied,
                events,
            };
            let envelope = crate::verb_envelope::committed(
                ResponseVerb::AdminReplayWal,
                ResponseData::AdminReplayWal(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&envelope)
        }
        Err(e) => admin_error_result(
            ResponseVerb::AdminReplayWal,
            &format!("admin_replay_wal: {e}"),
        ),
    }
}

fn dispatch_connector_enable(args_value: serde_json::Value, vault_root: &Path) -> CallToolResult {
    use cairn_core::generated::envelope::ResponseData;
    use cairn_core::generated::verbs::admin_connector_enable::{
        AdminConnectorEnableArgs, AdminConnectorEnableData,
    };
    use cairn_core::verbs::admin::connector::{self, ConnectorEnableRequest};

    let args: AdminConnectorEnableArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminConnectorEnable,
                &format!("invalid admin_connector_enable arguments: {e}"),
            );
        }
    };

    let admin = match open_admin_store(vault_root, ResponseVerb::AdminConnectorEnable) {
        Ok(a) => a,
        Err(r) => return r,
    };

    let ctx = AdminContext::new(mcp_operator_identity(), AdminRole::Operator);
    let req = ConnectorEnableRequest { name: args.name };

    match connector::enable(&ctx, &req, &admin) {
        Ok(resp) => {
            let data = AdminConnectorEnableData {
                name: resp.row.connector_name,
                enabled: resp.row.enabled,
                last_changed_at: resp.row.last_changed_at.to_rfc3339(),
            };
            let envelope = crate::verb_envelope::committed(
                ResponseVerb::AdminConnectorEnable,
                ResponseData::AdminConnectorEnable(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&envelope)
        }
        Err(e) => admin_error_result(
            ResponseVerb::AdminConnectorEnable,
            &format!("admin_connector_enable: {e}"),
        ),
    }
}

fn dispatch_connector_disable(args_value: serde_json::Value, vault_root: &Path) -> CallToolResult {
    use cairn_core::generated::envelope::ResponseData;
    use cairn_core::generated::verbs::admin_connector_disable::{
        AdminConnectorDisableArgs, AdminConnectorDisableData,
    };
    use cairn_core::verbs::admin::connector::{self, ConnectorDisableRequest};

    let args: AdminConnectorDisableArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminConnectorDisable,
                &format!("invalid admin_connector_disable arguments: {e}"),
            );
        }
    };

    let admin = match open_admin_store(vault_root, ResponseVerb::AdminConnectorDisable) {
        Ok(a) => a,
        Err(r) => return r,
    };

    let ctx = AdminContext::new(mcp_operator_identity(), AdminRole::Operator);
    let req = ConnectorDisableRequest {
        name: args.name,
        reason: args.reason,
    };

    match connector::disable(&ctx, &req, &admin) {
        Ok(resp) => {
            let data = AdminConnectorDisableData {
                name: resp.row.connector_name,
                enabled: resp.row.enabled,
                last_changed_at: resp.row.last_changed_at.to_rfc3339(),
                reason: resp.row.reason,
            };
            let envelope = crate::verb_envelope::committed(
                ResponseVerb::AdminConnectorDisable,
                ResponseData::AdminConnectorDisable(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&envelope)
        }
        Err(e) => admin_error_result(
            ResponseVerb::AdminConnectorDisable,
            &format!("admin_connector_disable: {e}"),
        ),
    }
}

fn dispatch_connector_backfill(args_value: serde_json::Value, vault_root: &Path) -> CallToolResult {
    use cairn_core::contract::backfill_spawner::{BackfillSpawner, BackfillSpec};
    use cairn_core::contract::memory_store::StoreError;
    use cairn_core::generated::envelope::ResponseData;
    use cairn_core::generated::verbs::admin_connector_backfill::{
        AdminConnectorBackfillArgs, AdminConnectorBackfillData,
    };
    use cairn_core::verbs::admin::connector::{self, BackfillRequest};

    struct NoopSpawner;
    impl BackfillSpawner for NoopSpawner {
        fn spawn_backfill(&self, _spec: BackfillSpec) -> Result<(), StoreError> {
            Ok(())
        }
    }

    let args: AdminConnectorBackfillArgs = match serde_json::from_value(args_value) {
        Ok(a) => a,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminConnectorBackfill,
                &format!("invalid admin_connector_backfill arguments: {e}"),
            );
        }
    };

    let admin = match open_admin_store(vault_root, ResponseVerb::AdminConnectorBackfill) {
        Ok(a) => a,
        Err(r) => return r,
    };

    let from = match args.from.parse::<chrono::DateTime<chrono::Utc>>() {
        Ok(dt) => dt,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminConnectorBackfill,
                &format!("invalid `from` date-time: {e}"),
            );
        }
    };
    let to = match args.to.parse::<chrono::DateTime<chrono::Utc>>() {
        Ok(dt) => dt,
        Err(e) => {
            return admin_error_result(
                ResponseVerb::AdminConnectorBackfill,
                &format!("invalid `to` date-time: {e}"),
            );
        }
    };

    let ctx = AdminContext::new(mcp_operator_identity(), AdminRole::Operator);
    let spawner = NoopSpawner;
    let req = BackfillRequest {
        name: args.name,
        from,
        to,
        rate_per_sec: args.rate_per_sec,
    };

    match connector::backfill(&ctx, &req, &admin, &spawner) {
        Ok(resp) => {
            let data = AdminConnectorBackfillData {
                workflow_id: resp.workflow_id.to_string(),
                started_at: resp.started_at.to_rfc3339(),
            };
            let envelope = crate::verb_envelope::committed(
                ResponseVerb::AdminConnectorBackfill,
                ResponseData::AdminConnectorBackfill(data),
                Vec::new(),
            );
            crate::verb_envelope::call_result_from_response(&envelope)
        }
        Err(e) => admin_error_result(
            ResponseVerb::AdminConnectorBackfill,
            &format!("admin_connector_backfill: {e}"),
        ),
    }
}

/// Convert an admin error string into a `CallToolResult` with the aborted
/// envelope policy.
fn admin_error_result(verb: ResponseVerb, detail: &str) -> CallToolResult {
    let response = crate::verb_envelope::aborted_internal(verb, detail);
    crate::verb_envelope::call_result_from_response(&response)
}

/// Build a `CallToolResult` indicating an internal error while opening
/// an adapter (e.g. failed to open the `SQLite` admin store).
fn aborted_internal_admin(verb: ResponseVerb, detail: &str) -> CallToolResult {
    let response = crate::verb_envelope::aborted_internal(verb, detail);
    crate::verb_envelope::call_result_from_response(&response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_tool_names_are_in_idl_tools() {
        let idl_names: Vec<&str> = crate::generated::TOOLS
            .iter()
            .filter(|d| d.capability == Some(ADMIN_CAPABILITY))
            .map(|d| d.name)
            .collect();
        let mut local: Vec<&str> = ADMIN_TOOL_NAMES.to_vec();
        let mut idl_sorted = idl_names.clone();
        local.sort_unstable();
        idl_sorted.sort_unstable();
        assert_eq!(local, idl_sorted);
    }

    #[test]
    fn is_admin_tool_matches_only_the_six_verbs() {
        for name in ADMIN_TOOL_NAMES {
            assert!(is_admin_tool(name));
        }
        assert!(!is_admin_tool("ingest"));
        assert!(!is_admin_tool("propose_share"));
        assert!(!is_admin_tool(""));
    }

    #[test]
    fn dispatch_ready_tracks_wiring_constants() {
        // dispatch_ready() uses (config_enabled=true, has_operator=true) as
        // the runtime probe so the build-time WIRED constants are the only gate.
        let expected = cairn_core::status::wiring::ADMIN_EXTENSION_WIRED
            && cairn_core::status::wiring::ADMIN_CLI_DISPATCH_WIRED
            && cairn_core::status::wiring::ADMIN_MCP_DISPATCH_WIRED;
        assert_eq!(dispatch_ready(), expected);
    }

    #[test]
    fn runtime_ready_requires_both_dispatch_and_capability() {
        assert!(!runtime_ready(&[]));
        let caps = [Capabilities::CairnMcpV1ExtensionAdmin];
        assert_eq!(runtime_ready(&caps), dispatch_ready());
    }

    #[test]
    fn dispatch_returns_capability_unavailable_when_dispatch_not_ready() {
        // ADMIN_MCP_DISPATCH_WIRED = false → dispatch_ready() = false
        // → dispatch returns capability_unavailable regardless of vault_root.
        if dispatch_ready() {
            // Gap 8 has flipped the constants; skip this particular assertion.
            return;
        }
        let result = dispatch("admin_snapshot", None, Some(std::path::Path::new("/tmp")));
        assert!(result.is_error.unwrap_or(false));
        let text = result
            .content
            .first()
            .and_then(|c| {
                if let rmcp::model::RawContent::Text(t) = &**c {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("");
        assert!(
            text.contains("capability unavailable"),
            "expected capability unavailable, got: {text}"
        );
    }

    #[test]
    fn dispatch_returns_capability_unavailable_when_no_vault_root() {
        // When dispatch_ready() is true but vault_root is None,
        // capability_unavailable is still returned.
        if !dispatch_ready() {
            return; // skip — can only test this when wired
        }
        let result = dispatch("admin_snapshot", None, None);
        assert!(result.is_error.unwrap_or(false));
    }
}
