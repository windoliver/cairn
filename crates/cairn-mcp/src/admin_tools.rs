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
/// - `runtime_ready(capabilities)` is `false` — i.e. either the build-time
///   wiring constants are dark OR the admin extension capability is absent
///   from the negotiated `status` advertisement, OR
/// - `vault_root` is `None` (no vault is bound to this handler).
///
/// The capability slice is the authoritative admission gate: the handler
/// advertises `cairn.mcp.v1.extension.admin` **only** when
/// `config.admin.enabled` is true AND at least one operator row exists
/// (see `handler::build_status_response`). Passing that live slice in here
/// means a direct `call_tool` against an admin tool fails closed on a
/// config-disabled or no-operator server — not just in `tools/list`
/// filtering (brief §15 fail-closed; round-3 adversarial review #1).
///
/// When both gates pass, routes to the matching verb function.
pub fn dispatch(
    name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    vault_root: Option<&Path>,
    capabilities: &[Capabilities],
) -> CallToolResult {
    if !runtime_ready(capabilities) {
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

/// Internal helper — derive the **local-operator bootstrap identity** and
/// vault id from `.cairn/vault.id`.
///
/// Returns `(actor_identity, vault_id_string)`.
///
/// ## This is a local bootstrap principal, NOT signed-chain auth
///
/// The IDL annotates every admin verb `x-cairn-auth: signed_chain`, but the
/// stdio MCP transport is single-tenant — there is no per-request signed
/// intent to verify yet (`McpAuthContext` carries only the fixed server
/// principal; per-request signature extraction from `RequestContext` is
/// deferred to Phase 2, brief §7.4). Rather than collapse callers into a
/// universal sentinel, this derives a **machine-and-vault-specific** actor
/// `hmn:local-vault:<vault_id>` from the on-disk vault id.
///
/// Three locks must ALL be satisfied before any admin verb runs through
/// this path, so it is a deliberate, opt-in, local-trust bootstrap — not a
/// silent bypass:
/// 1. `config.admin.enabled = true` (operator opt-in; default false), AND
/// 2. an operator row exists AND has explicitly granted
///    `hmn:local-vault:<vault_id>` the `Operator` role — `guard::require_role`
///    rejects with `NotAuthorized` otherwise, AND
/// 3. the build-time wiring constants are flipped on.
///
/// Locks (1)+(2) are also what gate advertisement of the
/// `cairn.mcp.v1.extension.admin` capability, so the whole surface is dark
/// until the operator deliberately enables it. Until Phase 2 lands signed
/// intent, operators exposing the MCP server over a network transport should
/// keep `config.admin.enabled = false` and drive admin verbs from the CLI
/// (which has the real signed-verb path). See round-1/round-3 adversarial
/// review.
///
/// # Errors
/// Returns a `CallToolResult::error` when `.cairn/vault.id` is absent,
/// unreadable, or produces an identity that fails to parse.
fn local_vault_identity(
    vault_root: &Path,
    verb: ResponseVerb,
) -> Result<(Identity, String), CallToolResult> {
    let id_path = vault_root.join(".cairn").join("vault.id");
    let vault_id = std::fs::read_to_string(&id_path)
        .map(|s| s.trim().to_owned())
        .map_err(|e| {
            aborted_internal_admin(
                verb,
                &format!("read .cairn/vault.id (run `cairn bootstrap` to initialise): {e}"),
            )
        })?;
    if vault_id.is_empty() {
        return Err(aborted_internal_admin(verb, ".cairn/vault.id is empty"));
    }
    // Construct a machine-specific identity tied to this vault.
    let raw = format!("hmn:local-vault:{vault_id}");
    #[allow(clippy::expect_used)]
    let identity = Identity::parse(&raw)
        .map_err(|e| aborted_internal_admin(verb, &format!("parse vault identity: {e}")))?;
    Ok((identity, vault_id))
}

/// Derive the local **machine** fingerprint for the cross-machine restore
/// guard — a stable, host-local secret, NOT the vault id and NOT the hostname.
///
/// A snapshot records this as its `source_machine_id`; restore refuses when it
/// differs from the local value, so a vault relocated to a different host
/// cannot restore a snapshot made elsewhere — preserving the per-machine
/// salt/integrity assumptions the guard protects. Fails closed when the
/// fingerprint cannot be derived.
fn local_machine_id(verb: ResponseVerb) -> Result<String, CallToolResult> {
    host_fingerprint()
        .map_err(|e| aborted_internal_admin(verb, &format!("derive host fingerprint: {e}")))
}

/// Stable, host-local machine fingerprint.
///
/// Reads (or generates on first use) a random secret at
/// `$XDG_CONFIG_HOME/cairn/host-id` (else `$HOME/.config/cairn/host-id`) — kept
/// OUTSIDE any vault so it does NOT travel with a copied vault — and returns
/// its sha256 so the raw secret never lands in a manifest. Unlike a hostname
/// this is stable across machine renames and unique per host even when
/// hostnames collide across cloned VMs (round-6 review #4).
fn host_fingerprint() -> std::io::Result<String> {
    host_fingerprint_at(&host_id_path()?)
}

/// Read-or-generate the host secret at `path` and return its sha256. Split out
/// from [`host_fingerprint`] so tests can drive it with an explicit path
/// instead of mutating process-global `XDG_CONFIG_HOME`/`HOME`.
fn host_fingerprint_at(path: &std::path::Path) -> std::io::Result<String> {
    // Generate a fresh secret ONLY on first use (NotFound). An empty, corrupt,
    // or unreadable host-id must NOT be silently overwritten — that would
    // change the machine identity and break cross-machine restore of older
    // same-host snapshots. Fail closed so the operator repairs it explicitly
    // (round-7 review #6).
    let secret = match std::fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "host-id at {} is empty; refusing to regenerate — remove it \
                         explicitly to mint a new machine identity",
                        path.display()
                    ),
                ));
            }
            trimmed.to_owned()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let secret = uuid::Uuid::new_v4().to_string();
            write_host_id(path, &secret)?;
            secret
        }
        // Permission denied, invalid UTF-8, etc. — do not regenerate; surface it.
        Err(e) => return Err(e),
    };
    Ok(cairn_core::verbs::admin::manifest::sha256_hex(
        secret.as_bytes(),
    ))
}

/// Path to the host-local secret: `$XDG_CONFIG_HOME/cairn/host-id`, else
/// `$HOME/.config/cairn/host-id`.
fn host_id_path() -> std::io::Result<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither XDG_CONFIG_HOME nor HOME is set; cannot locate host-id",
            )
        })?;
    Ok(base.join("cairn").join("host-id"))
}

/// Write the host secret, restricting it to the owner (0600) on Unix.
#[cfg(unix)]
fn write_host_id(path: &std::path::Path, secret: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(secret.as_bytes())
}

#[cfg(not(unix))]
fn write_host_id(path: &std::path::Path, secret: &str) -> std::io::Result<()> {
    std::fs::write(path, secret.as_bytes())
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
    let (actor, vault_id) = match local_vault_identity(vault_root, ResponseVerb::AdminSnapshot) {
        Ok(pair) => pair,
        Err(r) => return r,
    };
    let machine_id = match local_machine_id(ResponseVerb::AdminSnapshot) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let meta = cairn_store_sqlite::SqliteSnapshotMetadata::new(
        vault_root.join(".cairn").join("cairn.db"),
        vault_id.clone(),
    );
    let producer = cairn_store_sqlite::SqliteSnapshotProducer::new(
        vault_root.to_path_buf(),
        vault_root.join(".cairn").join("cairn.db"),
    );
    // The registry roots at the vault dir and writes
    // `<vault>/.cairn/backups/<backup_id>.json`. Pass the vault root itself —
    // NOT a `.cairn/backups.jsonl` sub-path — so restore-time integrity
    // verification (which reads the same registry) agrees on the location.
    let registry = cairn_store_sqlite::FileBackupRegistry::new(vault_root.to_path_buf());

    let ctx = AdminContext::new(actor, AdminRole::Operator);
    let req = cairn_core::verbs::admin::snapshot::SnapshotRequest {
        out_dir: std::path::PathBuf::from(&args.out_dir),
        label: args.label,
        // Machine fingerprint (host), recorded as the manifest's
        // source_machine_id — NOT the vault id.
        local_machine_id: machine_id,
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
    // Derive the actor (bootstrap identity) from the vault — never trust
    // caller-supplied identity.
    let (actor, vault_id) = match local_vault_identity(vault_root, ResponseVerb::AdminRestore) {
        Ok(pair) => pair,
        Err(r) => return r,
    };
    // The cross-machine guard compares against the host fingerprint, not the
    // vault id (a copied vault carries its id but lands on a different host).
    let machine_id = match local_machine_id(ResponseVerb::AdminRestore) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let db_path = vault_root.join(".cairn").join("cairn.db");
    let meta = cairn_store_sqlite::SqliteSnapshotMetadata::new(db_path.clone(), vault_id.clone());
    let reader = cairn_store_sqlite::SqliteSnapshotReader;
    let applier = cairn_store_sqlite::SqliteSnapshotApplier::new(vault_root.to_path_buf());
    let consent = cairn_store_sqlite::SqliteConsentLog::new(db_path);
    // Trusted integrity anchor: the backup registry stores the artifact's
    // full three-part digest at snapshot time, in `.cairn/backups/` —
    // separate from `cairn.db`, so it survives DB loss. Restore verifies the
    // artifact against it before swapping (round-3 adversarial review #4).
    let registry = cairn_store_sqlite::FileBackupRegistry::new(vault_root.to_path_buf());

    let ctx = AdminContext::new(actor, AdminRole::Operator);
    let req = cairn_core::verbs::admin::restore::RestoreRequest {
        artifact_path: std::path::PathBuf::from(&args.artifact_path),
        dry_run: args.dry_run,
        // Host fingerprint; ignore any caller-supplied value so a remote caller
        // cannot bypass the cross-machine guard.
        local_machine_id: machine_id,
    };

    // Hold the vault write gate EXCLUSIVE across the entire restore (frontier
    // capture → purge → swap). Concurrent forgets take the gate SHARED, so this
    // quiesces them for the duration — no forget can commit into the DB that
    // swap_in is about to discard (round-7 review #2). Held until end of fn.
    let _write_gate =
        match cairn_store_sqlite::lock_exclusive(&cairn_store_sqlite::gate_path(vault_root)) {
            Ok(g) => g,
            Err(e) => {
                return aborted_internal_admin(
                    ResponseVerb::AdminRestore,
                    &format!("acquire vault write gate: {e}"),
                );
            }
        };

    match cairn_core::verbs::admin::restore::run(
        &ctx, &req, &admin, &meta, &reader, &applier, &consent, &registry,
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
    let (actor, _) = match local_vault_identity(vault_root, ResponseVerb::AdminReplayWal) {
        Ok(pair) => pair,
        Err(r) => return r,
    };

    // from_ord is u64 in the IDL; the core verb uses u32.
    let from_ord = u32::try_from(args.from_ord).unwrap_or(u32::MAX);
    let ctx = AdminContext::new(actor, AdminRole::Operator);
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
    let (actor, _) = match local_vault_identity(vault_root, ResponseVerb::AdminConnectorEnable) {
        Ok(pair) => pair,
        Err(r) => return r,
    };

    let ctx = AdminContext::new(actor, AdminRole::Operator);
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
    let (actor, _) = match local_vault_identity(vault_root, ResponseVerb::AdminConnectorDisable) {
        Ok(pair) => pair,
        Err(r) => return r,
    };

    let ctx = AdminContext::new(actor, AdminRole::Operator);
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

fn dispatch_connector_backfill(
    args_value: serde_json::Value,
    _vault_root: &Path,
) -> CallToolResult {
    use cairn_core::generated::verbs::admin_connector_backfill::AdminConnectorBackfillArgs;

    // Validate the argument shape so malformed calls still get a clean
    // InvalidArgs-style error rather than the generic "not wired" message.
    if let Err(e) = serde_json::from_value::<AdminConnectorBackfillArgs>(args_value) {
        return admin_error_result(
            ResponseVerb::AdminConnectorBackfill,
            &format!("invalid admin_connector_backfill arguments: {e}"),
        );
    }

    // Fail closed instead of faking success. No `BackfillSpawner` is wired
    // into the MCP adapter, so executing the verb here would persist no job
    // and start no scheduler work — yet the previous code returned a
    // `committed` envelope with a fabricated workflow id, giving operators a
    // false "backfill running" signal (round-3 adversarial review #5).
    //
    // Until a real spawner is wired (scheduler integration follow-up), this
    // verb is advertised for API/wire compatibility under the admin umbrella
    // but cannot execute over MCP. Return an explicit aborted error so the
    // caller never mistakes a no-op for a started backfill. Operators who
    // need a backfill today must drive it from the CLI, which surfaces the
    // same "spawner not yet wired" caveat.
    admin_error_result(
        ResponseVerb::AdminConnectorBackfill,
        "admin_connector_backfill is not executable over MCP in this build: \
         no BackfillSpawner is wired, so no job would be enqueued. The verb is \
         advertised for compatibility but refuses rather than reporting a \
         false success. Run the backfill from the CLI or wait for scheduler \
         integration.",
    )
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
    fn dispatch_returns_capability_unavailable_when_capability_absent() {
        // An empty capability slice models a server that did not advertise the
        // admin extension (config disabled / no operator) — `dispatch` must
        // fail closed regardless of vault_root or wiring state.
        let result = dispatch(
            "admin_snapshot",
            None,
            Some(std::path::Path::new("/tmp")),
            &[],
        );
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
        // With the admin capability advertised AND wiring on, the runtime gate
        // passes — but a missing vault_root still yields capability_unavailable.
        if !dispatch_ready() {
            return; // skip — the runtime gate can't pass when wiring is dark
        }
        let result = dispatch(
            "admin_snapshot",
            None,
            None,
            &[Capabilities::CairnMcpV1ExtensionAdmin],
        );
        assert!(result.is_error.unwrap_or(false));
    }

    /// Round-6 review #4: the host fingerprint is a STABLE sha256 of a persisted
    /// secret (not the raw secret), and distinct host secrets hash differently.
    #[test]
    fn host_fingerprint_is_stable_and_hashed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("cairn").join("host-id");

        let fp1 = host_fingerprint_at(&path).expect("first fingerprint");
        let fp2 = host_fingerprint_at(&path).expect("second fingerprint");
        assert_eq!(fp1, fp2, "fingerprint must be stable across calls");
        assert_eq!(fp1.len(), 64, "sha256 hex is 64 chars");
        assert!(
            fp1.chars().all(|c| c.is_ascii_hexdigit()),
            "fingerprint must be lowercase hex"
        );

        // The persisted secret is NOT the fingerprint (it is hashed).
        let raw_secret = std::fs::read_to_string(&path).expect("read host-id");
        assert_ne!(fp1, raw_secret.trim(), "the raw secret must be hashed");

        // A different host secret yields a different fingerprint.
        let dir2 = tempfile::tempdir().expect("tmpdir2");
        let fp3 = host_fingerprint_at(&dir2.path().join("host-id")).expect("third fingerprint");
        assert_ne!(fp1, fp3, "distinct host secrets must hash differently");
    }
}
