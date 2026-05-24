//! `cairn share` subcommand group (brief §12.a, issue #123).
//!
//! Three children:
//!
//! * `cairn share propose` — mint and sign a `SignedShareLink` over a
//!   record set, append a `FederationGrant` consent row, and enqueue an
//!   outbound propagation job.
//! * `cairn share accept` — receive a `FederationEnvelope` from stdin
//!   or a file, verify its signature, and atomically upsert or tombstone
//!   the enclosed records.
//! * `cairn share revoke` — cancel a previously minted share link,
//!   append a `FederationRevoke` consent row, and enqueue an outbound
//!   revoke propagation job.
//!
//! All three verbs dispatch through the `cairn-core::verbs::*` functions
//! (CLI is ground truth per CLAUDE.md §4.3). Dep wiring mirrors the MCP
//! handler's `FederationState` construction in `cairn-mcp`.

use std::io::Read as _;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::domain::identity::keys::SecretHandle;
use cairn_core::domain::time::SystemClock;
use cairn_core::domain::{Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple};
use cairn_core::error::federation::FederationError;
use cairn_core::rebac::RebacContext;
use cairn_store_sqlite::SqliteMemoryStore;

use crate::identity::IdentityService;

use super::envelope::EX_UNAVAILABLE;

/// Exit code for bad configuration (sysexits `EX_CONFIG`).
const EX_CONFIG: u8 = 78;

// ── clap command tree ───────────────────────────────────────────────────

/// Build the `cairn share` command tree.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("share")
        .about("Federation share-link management (brief §12.a)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(propose_subcommand())
        .subcommand(accept_subcommand())
        .subcommand(revoke_subcommand())
}

fn propose_subcommand() -> clap::Command {
    clap::Command::new("propose")
        .about("Mint a signed share link over a record set")
        .arg(
            clap::Arg::new("record-ids")
                .long("record-ids")
                .value_name("ID,...")
                .required(true)
                .value_delimiter(',')
                .help("Comma-separated ULID record ids to bundle in the share link"),
        )
        .arg(
            clap::Arg::new("grant-tier")
                .long("grant-tier")
                .value_name("TIER")
                .required(true)
                .value_parser(["session", "project", "team", "org", "public"])
                .help("Visibility tier the grant authorises"),
        )
        .arg(
            clap::Arg::new("expires-at")
                .long("expires-at")
                .value_name("RFC3339")
                .required(true)
                .help("Hard expiry of the share link (RFC 3339 timestamp)"),
        )
        .arg(
            clap::Arg::new("grantee")
                .long("grantee")
                .value_name("IDENTITY")
                .help("Optional explicit grantee identity (e.g. hmn:bob). Omit for bearer link"),
        )
        .arg(
            clap::Arg::new("peer")
                .long("peer")
                .value_name("ENDPOINT")
                .help("Optional outbound peer endpoint (e.g. loopback://node-b)"),
        )
        .arg(
            clap::Arg::new("scope")
                .long("scope")
                .value_name("KEY=VAL,...")
                .required(true)
                .help(
                    "Scope tuple in canonical wire format (e.g. \
                     agent=claude,project=my-proj,workspace=default). \
                     At least one dimension must be set.",
                ),
        )
        .arg(json_arg())
}

fn accept_subcommand() -> clap::Command {
    clap::Command::new("accept")
        .about("Accept an inbound federation envelope (propose or revoke)")
        .arg(
            clap::Arg::new("envelope")
                .long("envelope")
                .value_name("FILE_OR_DASH")
                .required(true)
                .help("Path to a JSON file containing a FederationEnvelope, or '-' for stdin"),
        )
        .arg(json_arg())
}

fn revoke_subcommand() -> clap::Command {
    clap::Command::new("revoke")
        .about("Revoke a previously minted share link")
        .arg(
            clap::Arg::new("link-id")
                .long("link-id")
                .value_name("ID")
                .required(true)
                .help("Stable share-link id to revoke (e.g. share-01HZZZZZZZZZZZZZZZZZZZZZZZ)"),
        )
        .arg(json_arg())
}

fn json_arg() -> clap::Arg {
    clap::Arg::new("json")
        .long("json")
        .action(clap::ArgAction::SetTrue)
        .help("Emit machine-readable JSON response envelope to stdout")
}

// ── dispatch ────────────────────────────────────────────────────────────

/// Run the `cairn share` subcommand.
#[must_use]
pub fn run(sub: &clap::ArgMatches, vault_root: &Path, config: &CairnConfig) -> ExitCode {
    match sub.subcommand() {
        Some(("propose", args)) => run_propose(args, vault_root, config),
        Some(("accept", args)) => run_accept(args, vault_root, config),
        Some(("revoke", args)) => run_revoke(args, vault_root, config),
        _ => {
            // clap's subcommand_required(true) prevents this.
            eprintln!("cairn share: unknown subcommand");
            ExitCode::from(64)
        }
    }
}

// ── propose ─────────────────────────────────────────────────────────────

/// Parsed arguments for `cairn share propose`.
struct ProposeArgs {
    record_ids: Vec<String>,
    grant_tier: MemoryVisibility,
    expires_at: Rfc3339Timestamp,
    grantee: Option<Identity>,
    peer: Option<cairn_core::domain::federation::PeerEndpoint>,
    scope: ScopeTuple,
}

/// Parse all CLI arguments for `cairn share propose`, returning early via
/// exit code on any validation failure.
fn parse_propose_args(sub: &clap::ArgMatches) -> Result<ProposeArgs, ExitCode> {
    let record_ids: Vec<String> = sub
        .get_many::<String>("record-ids")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let grant_tier = match sub.get_one::<String>("grant-tier").map(String::as_str) {
        Some("session") => MemoryVisibility::Session,
        Some("project") => MemoryVisibility::Project,
        Some("team") => MemoryVisibility::Team,
        Some("org") => MemoryVisibility::Org,
        Some("public") => MemoryVisibility::Public,
        _ => {
            eprintln!("cairn share propose: invalid --grant-tier");
            return Err(ExitCode::from(64));
        }
    };

    let Some(expires_at_str) = sub.get_one::<String>("expires-at") else {
        eprintln!("cairn share propose: --expires-at is required");
        return Err(ExitCode::from(64));
    };
    let expires_at = Rfc3339Timestamp::parse(expires_at_str.clone()).map_err(|_| {
        eprintln!("cairn share propose: --expires-at must be a valid RFC 3339 timestamp");
        ExitCode::from(64)
    })?;

    let grantee = if let Some(g) = sub.get_one::<String>("grantee") {
        let id = Identity::parse(g.clone()).map_err(|_| {
            eprintln!("cairn share propose: --grantee must be a valid identity (e.g. hmn:bob)");
            ExitCode::from(64)
        })?;
        Some(id)
    } else {
        None
    };

    let peer = sub
        .get_one::<String>("peer")
        .map(|p| cairn_core::domain::federation::PeerEndpoint(p.clone()));

    let scope = if let Some(s) = sub.get_one::<String>("scope") {
        parse_scope_wire(s).map_err(|msg| {
            eprintln!("cairn share propose: {msg}");
            ExitCode::from(64)
        })?
    } else {
        eprintln!("cairn share propose: --scope is required");
        return Err(ExitCode::from(64));
    };

    Ok(ProposeArgs {
        record_ids,
        grant_tier,
        expires_at,
        grantee,
        peer,
        scope,
    })
}

fn run_propose(sub: &clap::ArgMatches, vault_root: &Path, _config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");

    let args = match parse_propose_args(sub) {
        Ok(a) => a,
        Err(code) => return code,
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn share propose: failed to build runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    rt.block_on(async {
        let deps = match open_federation_deps(vault_root).await {
            Ok(d) => d,
            Err(code) => return code,
        };

        let rebac = RebacContext::for_scope(
            deps.identity.clone(),
            &args.scope,
            cairn_core::rebac::RebacAction::Write,
            args.grant_tier,
        );
        let clock = SystemClock;

        let request = cairn_core::verbs::propose_share::ProposeShareRequest {
            record_ids: args.record_ids,
            grantee: args.grantee,
            scope: args.scope,
            grant_tier: args.grant_tier,
            expires_at: args.expires_at,
            peer: args.peer,
        };

        let verb_deps = cairn_core::verbs::propose_share::ProposeShareDeps {
            store: deps.store.as_ref(),
            outbox: deps.store.as_ref(),
            signing_key: &deps.signing_key,
            signer_identity: &deps.identity,
            signer_key_version: deps.key_version,
            rebac: &rebac,
            clock: &clock,
            federation_ready: cairn_core::status::wiring::federation_extension_ready(),
        };

        match cairn_core::verbs::propose_share::propose_share(request, &verb_deps).await {
            Ok(response) => {
                if json {
                    let wire_link =
                        cairn_core::domain::federation::signed_share_link_to_wire(&response.link);
                    let envelope = match wire_link {
                        Ok(link) => serde_json::json!({
                            "status": "committed",
                            "verb": "propose_share",
                            "operation_id": response.operation_id,
                            "link": link,
                        }),
                        Err(_) => serde_json::json!({
                            "status": "committed",
                            "verb": "propose_share",
                            "operation_id": response.operation_id,
                        }),
                    };
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&envelope).unwrap_or_default()
                    );
                } else {
                    println!(
                        "share link minted: link_id={} operation_id={}",
                        response.link.link_id, response.operation_id,
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => federation_error_exit(&e, "propose_share", json),
        }
    })
}

// ── accept ──────────────────────────────────────────────────────────────

fn run_accept(sub: &clap::ArgMatches, vault_root: &Path, _config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");

    let Some(envelope_arg) = sub.get_one::<String>("envelope").cloned() else {
        eprintln!("cairn share accept: --envelope is required");
        return ExitCode::from(64);
    };

    // Read the envelope JSON from file or stdin.
    let envelope_bytes = if envelope_arg == "-" {
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
            eprintln!("cairn share accept: failed to read stdin: {e}");
            return ExitCode::FAILURE;
        }
        buf
    } else {
        match std::fs::read(&envelope_arg) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("cairn share accept: failed to read {envelope_arg}: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    let envelope: cairn_core::generated::common::FederationEnvelope =
        match serde_json::from_slice(&envelope_bytes) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("cairn share accept: invalid envelope JSON: {e}");
                return ExitCode::from(64);
            }
        };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn share accept: failed to build runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    rt.block_on(async {
        let deps = match open_federation_deps(vault_root).await {
            Ok(d) => d,
            Err(code) => return code,
        };

        let clock = SystemClock;
        // TODO(federation-P2): `issuer_verifying_key` below uses the
        // local signing key's verifying half.  Correct for single-vault /
        // loopback testing; real cross-party federation requires resolving
        // `issuer_key_id` via a trust store (P2 follow-up).
        let rebac = RebacContext::for_principal(deps.identity.clone());
        let verifying_key = deps.signing_key.verifying_key();
        let inbound_sensor = deps.identity.clone();

        let request = cairn_core::verbs::accept_share::AcceptShareRequest { envelope };

        let verb_deps = cairn_core::verbs::accept_share::AcceptShareDeps {
            store: deps.store.as_ref(),
            outbox: deps.store.as_ref(),
            consent_lookup: deps.store.as_ref(),
            local_signing_key: &deps.signing_key,
            receiver_identity: &deps.identity,
            issuer_verifying_key: &verifying_key,
            rebac: &rebac,
            clock: &clock,
            inbound_sensor: &inbound_sensor,
            federation_ready: cairn_core::status::wiring::federation_extension_ready(),
        };

        match cairn_core::verbs::accept_share::accept_share(request, &verb_deps).await {
            Ok(response) => {
                if json {
                    let envelope = serde_json::json!({
                        "status": "committed",
                        "verb": "accept_share",
                        "outcome": format!("{:?}", response.outcome),
                        "applied_records": response.applied_records,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&envelope).unwrap_or_default()
                    );
                } else {
                    println!(
                        "accept_share: outcome={:?} applied_records={}",
                        response.outcome,
                        response.applied_records.len(),
                    );
                    for id in &response.applied_records {
                        println!("  {id}");
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => federation_error_exit(&e, "accept_share", json),
        }
    })
}

// ── revoke ──────────────────────────────────────────────────────────────

fn run_revoke(sub: &clap::ArgMatches, vault_root: &Path, _config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");

    let Some(link_id) = sub.get_one::<String>("link-id").cloned() else {
        eprintln!("cairn share revoke: --link-id is required");
        return ExitCode::from(64);
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn share revoke: failed to build runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    rt.block_on(async {
        let deps = match open_federation_deps(vault_root).await {
            Ok(d) => d,
            Err(code) => return code,
        };

        let clock = SystemClock;
        let request = cairn_core::verbs::revoke_share::RevokeShareRequest { link_id };

        let verb_deps = cairn_core::verbs::revoke_share::RevokeShareDeps {
            outbox: deps.store.as_ref(),
            consent_lookup: deps.store.as_ref(),
            signing_key: &deps.signing_key,
            signer_identity: &deps.identity,
            signer_key_version: deps.key_version,
            clock: &clock,
            federation_ready: cairn_core::status::wiring::federation_extension_ready(),
        };

        match cairn_core::verbs::revoke_share::revoke_share(request, &verb_deps).await {
            Ok(response) => {
                if json {
                    let envelope = serde_json::json!({
                        "status": "committed",
                        "verb": "revoke_share",
                        "operation_id": response.operation_id,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&envelope).unwrap_or_default()
                    );
                } else {
                    println!("share link revoked: operation_id={}", response.operation_id);
                }
                ExitCode::SUCCESS
            }
            Err(e) => federation_error_exit(&e, "revoke_share", json),
        }
    })
}

// ── federation deps ─────────────────────────────────────────────────────

/// Resolved federation runtime dependencies for CLI verb dispatch.
///
/// Mirrors `cairn_mcp::handler::FederationState` but adapted for the
/// short-lived CLI context: the identity service + keystore are opened
/// per invocation rather than held for the server lifetime.
struct FederationDeps {
    store: Arc<SqliteMemoryStore>,
    signing_key: cairn_core::domain::identity::keys::SigningKey,
    identity: Identity,
    key_version: u32,
}

/// Open the vault's identity service + store and load the active signing
/// key. Returns `Err(ExitCode)` on any infrastructure failure so the
/// caller can short-circuit without further logic.
async fn open_federation_deps(vault_root: &Path) -> Result<FederationDeps, ExitCode> {
    // 1. Open the identity service (reconciliation sweep + vault-id check).
    let (identity_svc, report) = IdentityService::open(vault_root.to_path_buf())
        .await
        .map_err(|e| {
            eprintln!("cairn share: identity open failed: {e}");
            ExitCode::from(EX_CONFIG)
        })?;

    crate::identity::guard::refuse_if_degraded(&report, report.mismatched_ids.clone()).map_err(
        |e| {
            eprintln!("cairn share: vault degraded: {e}");
            ExitCode::from(EX_CONFIG)
        },
    )?;

    // 2. Resolve the active human identity + key version.
    let active = resolve_active_identity(&identity_svc).await.map_err(|e| {
        eprintln!("cairn share: {e}");
        ExitCode::from(EX_CONFIG)
    })?;

    // 3. Load the signing key from the keystore.
    let handle = SecretHandle::for_identity(
        identity_svc.vault_id.clone(),
        active.identity.clone(),
        active.key_version,
    );
    let signing_key = identity_svc
        .keystore
        .load_signing_key(&handle)
        .await
        .map_err(|e| {
            eprintln!("cairn share: signing key load failed: {e}");
            ExitCode::from(EX_CONFIG)
        })?;

    // 4. Open the SQLite store.
    let db_path = vault_root.join(".cairn/cairn.db");
    let store = cairn_store_sqlite::open(&db_path).await.map_err(|e| {
        eprintln!("cairn share: store open failed: {e}");
        ExitCode::from(EX_CONFIG)
    })?;

    Ok(FederationDeps {
        store: Arc::new(store),
        signing_key,
        identity: active.identity,
        key_version: active.key_version.as_u32(),
    })
}

/// Resolved active identity for federation verb dispatch.
struct ActiveIdentity {
    identity: Identity,
    key_version: cairn_core::domain::identity::keys::KeyVersion,
}

/// Find the first operational human identity in the vault's registry.
///
/// Federation verbs require a human issuer (brief §12.a); agent identities
/// are filtered out. If no human identity is found, returns an error
/// message suitable for display.
async fn resolve_active_identity(svc: &IdentityService) -> Result<ActiveIdentity, String> {
    // List operational human identities.
    let identities = svc
        .registry
        .list_identities(
            Some(cairn_core::domain::identity::IdentityKind::Human),
            IdentityVisibility::Operational,
        )
        .await
        .map_err(|e| format!("identity registry list failed: {e}"))?;

    if let Some(record) = identities.first() {
        return Ok(ActiveIdentity {
            identity: record.id.clone(),
            key_version: record.current_key_version,
        });
    }

    Err("no operational human identity found in this vault. \
         Run `cairn identity provision` to create one."
        .to_owned())
}

// ── error helpers ───────────────────────────────────────────────────────

/// Map a `FederationError` to an appropriate exit code + output.
fn federation_error_exit(err: &FederationError, verb: &str, json: bool) -> ExitCode {
    let code = match err {
        FederationError::CapabilityDisabled => EX_UNAVAILABLE,
        _ => 1,
    };

    if json {
        let envelope = serde_json::json!({
            "status": "rejected",
            "verb": verb,
            "error": {
                "code": format!("{err:?}"),
                "message": format!("{err}"),
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).unwrap_or_default()
        );
    } else {
        eprintln!("cairn share {verb}: {err}");
    }

    ExitCode::from(code)
}

// ── scope parser ───────────────────────────────────────────────────────

/// Parse a canonical-wire scope string (`key=val,key=val,...`) into a
/// [`ScopeTuple`].  Returns an error message suitable for display when
/// the input is empty, contains unknown keys, or has malformed pairs.
fn parse_scope_wire(input: &str) -> Result<ScopeTuple, String> {
    let mut scope = ScopeTuple::default();
    let mut count = 0usize;
    for pair in input.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("--scope: malformed pair (missing '='): {pair}"))?;
        if value.is_empty() {
            return Err(format!("--scope: empty value for key '{key}'"));
        }
        match key {
            "agent" => scope.agent = Some(value.to_owned()),
            "entity" => scope.entity = Some(value.to_owned()),
            "project" => scope.project = Some(value.to_owned()),
            "session_id" => scope.session_id = Some(value.to_owned()),
            "tenant" => scope.tenant = Some(value.to_owned()),
            "user" => scope.user = Some(value.to_owned()),
            "workspace" => scope.workspace = Some(value.to_owned()),
            _ => return Err(format!("--scope: unknown dimension '{key}'")),
        }
        count += 1;
    }
    if count == 0 {
        return Err("--scope: at least one dimension must be set".to_owned());
    }
    Ok(scope)
}
