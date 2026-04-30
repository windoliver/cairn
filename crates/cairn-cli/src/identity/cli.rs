//! `cairn identity` subcommand surface (issue #50, D10).
//!
//! Wires the 12 identity subcommands to the [`IdentityService`] methods.
//! Each subcommand dispatches to its service method via a short inline
//! wrapper that maps typed errors to sysexits-style exit codes per CLAUDE.md
//! §6.5.
//!
//! # Exit code mapping
//!
//! | Condition                                              | Code |
//! |--------------------------------------------------------|------|
//! | Success                                               | 0    |
//! | Generic / unclassified                                | 1    |
//! | `CapabilityUnavailable`, `Keystore::Locked`           | 69   |
//! | `PurgeAckMissing`                                     | 65   |
//! | `VaultDegraded`, `FirstBindInProgress`                | 75   |
//! | `VaultIdMissing`, `VaultIdConflict`                   | 78   |

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::{
    contract::identity_registry::{IdentityVisibility, MaintenanceMode, PurgeReason},
    domain::identity::{
        Identity, IdentityKind,
        keys::{IdentityRevision, VaultId},
        provision::{ProvisionInput, mint_agent_id, mint_human_id, mint_sensor_id},
    },
    error::identity::IdentityServiceError,
};
use clap::{Arg, ArgAction, ArgMatches, Command};
use rand_core::OsRng;

use super::IdentityService;

// ── Exit code constants (sysexits.h) ─────────────────────────────────────────

/// `EX_DATAERR` (65) — input data was incorrect.
const EX_DATAERR: u8 = 65;
/// `EX_UNAVAILABLE` (69) — capability unavailable / keystore locked.
const EX_UNAVAILABLE: u8 = 69;
/// `EX_TEMPFAIL` (75) — temporary failure (vault degraded, first-bind in
/// progress).
const EX_TEMPFAIL: u8 = 75;
/// `EX_CONFIG` (78) — configuration error (vault id missing or conflict).
const EX_CONFIG: u8 = 78;

// ── Subcommand builder ────────────────────────────────────────────────────────

/// Build the `cairn identity` clap subcommand tree.
///
/// Returns a fully constructed [`Command`] whose subcommands mirror the
/// 12 identity verbs defined in the issue #50 plan.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "12 subcommands; splitting adds indirection for no gain"
)]
pub fn identity_subcommand() -> Command {
    Command::new("identity")
        .about("Manage vault identities (§3.5 / §4.1)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        // ── read/display ────────────────────────────────────────────────────
        .subcommand(
            Command::new("status")
                .about("Show vault identity health report")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("list")
                .about("List operational identities")
                .arg(
                    Arg::new("kind")
                        .long("kind")
                        .value_name("KIND")
                        .help("Filter by kind: human, agent, sensor"),
                )
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("show")
                .about("Show a single identity record")
                .arg(
                    Arg::new("id")
                        .value_name("IDENTITY")
                        .required(true)
                        .help("Identity wire form (e.g. hmn:alice:v1)"),
                )
                .arg(json_flag()),
        )
        // ── lifecycle mutations ──────────────────────────────────────────────
        .subcommand(
            Command::new("provision")
                .about("Provision a new identity")
                .arg(
                    Arg::new("kind")
                        .value_name("KIND")
                        .required(true)
                        .help("Identity kind: human, agent, sensor"),
                )
                .arg(
                    Arg::new("slug")
                        .value_name("SLUG")
                        .required(true)
                        .help("Slug / triple for the identity (format depends on kind)"),
                )
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("init-defaults")
                .about("Provision the default human and agent identities")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("rotate")
                .about("Rotate the signing key for an identity")
                .arg(
                    Arg::new("id")
                        .value_name("IDENTITY")
                        .required(true)
                        .help("Identity wire form"),
                )
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("revoke")
                .about("Revoke an identity (two-phase WAL tombstone)")
                .arg(
                    Arg::new("id")
                        .value_name("IDENTITY")
                        .required(true)
                        .help("Target identity wire form"),
                )
                .arg(
                    Arg::new("signer")
                        .long("signer")
                        .value_name("IDENTITY")
                        .help("Signer identity wire form (default: first active agent)"),
                )
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("reconcile")
                .about("Bulk repair all pending identity rows")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("repair")
                .about("Repair state-machine for a single identity")
                .arg(
                    Arg::new("id")
                        .value_name("IDENTITY")
                        .required(true)
                        .help("Identity wire form to repair"),
                )
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("purge")
                .about("Permanently purge an identity (requires .cairn/maintenance/purge-ack file)")
                .arg(
                    Arg::new("id")
                        .value_name("IDENTITY")
                        .required(true)
                        .help("Target identity wire form"),
                )
                .arg(
                    Arg::new("resume")
                        .long("resume")
                        .action(ArgAction::SetTrue)
                        .help("Resume a previously interrupted purge"),
                )
                .arg(json_flag()),
        )
        // ── recovery / administrative ────────────────────────────────────────
        .subcommand(
            Command::new("vault-id-recover")
                .about("Recover or reconstruct the .cairn/vault.id file")
                .arg(
                    Arg::new("probe-keychain")
                        .long("probe-keychain")
                        .action(ArgAction::SetTrue)
                        .help("Probe OS keychain for vault namespaces"),
                )
                .arg(
                    Arg::new("vault-id")
                        .long("vault-id")
                        .value_name("UUID")
                        .help("Explicit vault UUID override"),
                )
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("finalise-binding")
                .about("Resume or abandon a partial first-bind after a crash")
                .arg(
                    Arg::new("abandon")
                        .long("abandon")
                        .action(ArgAction::SetTrue)
                        .help("Abandon the partial bind (deletes .cairn/vault.binding.pending)"),
                )
                .arg(
                    Arg::new("vault-id")
                        .long("vault-id")
                        .value_name("UUID")
                        .help("Explicit vault UUID override"),
                )
                .arg(json_flag()),
        )
}

/// Shared `--json` flag definition.
fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Emit JSON output instead of human-readable text")
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

/// Dispatch `cairn identity <subcommand>` to the appropriate [`IdentityService`]
/// method and return the appropriate exit code.
///
/// `matches` is the [`ArgMatches`] for the `identity` subcommand.
/// `explicit_vault` is the global `--vault` flag merged with `CAIRN_VAULT` env
/// (resolved by `main`); it takes precedence over the subcommand's own
/// `--vault-path` if both are set, matching the §3.3 precedence rule.
#[must_use]
pub fn run_identity(matches: &ArgMatches, explicit_vault: Option<String>) -> ExitCode {
    // Resolve the vault path with full §3.3 precedence:
    //   1. global `--vault` flag / `CAIRN_VAULT` env (passed in as `explicit_vault`)
    //   2. subcommand-local `--vault-path` (legacy, kept for back-compat)
    //   3. cwd walk-up via the registry resolver
    //   4. registry default
    let vault_path = resolve_vault_path(matches, explicit_vault);

    match matches.subcommand() {
        Some(("status", sub)) => run_status(sub, vault_path),
        Some(("list", sub)) => run_list(sub, vault_path),
        Some(("show", sub)) => run_show(sub, vault_path),
        Some(("provision", sub)) => run_provision(sub, vault_path),
        Some(("init-defaults", sub)) => run_init_defaults(sub, vault_path),
        Some(("rotate", sub)) => run_rotate(sub, vault_path),
        Some(("revoke", sub)) => run_revoke(sub, vault_path),
        Some(("reconcile", sub)) => run_reconcile(sub, vault_path),
        Some(("repair", sub)) => run_repair(sub, vault_path),
        Some(("purge", sub)) => run_purge(sub, vault_path),
        Some(("vault-id-recover", sub)) => run_vault_id_recover(sub, vault_path),
        Some(("finalise-binding", sub)) => run_finalise_binding(sub, vault_path),
        _ => unreachable!(
            "clap subcommand_required(true) on identity ensures a subcommand is always present"
        ),
    }
}

// ── vault path resolution ─────────────────────────────────────────────────────

/// Resolve the vault root path with full §3.3 precedence.
///
/// Priority:
/// 1. Global `--vault` flag or `CAIRN_VAULT` env (passed via `explicit_vault`).
/// 2. Subcommand-local `--vault-path` (legacy).
/// 3. cwd walk-up + registry default (via [`crate::vault::resolve_vault`]).
/// 4. cwd as last-ditch fallback.
fn resolve_vault_path(matches: &ArgMatches, explicit_vault: Option<String>) -> PathBuf {
    // 1. Subcommand-local --vault-path always wins if explicitly given.
    //    (Kept for back-compat; future cleanups may remove the per-subcommand flag.)
    if let Some(p) = matches.get_one::<String>("vault-path") {
        return PathBuf::from(p);
    }

    // 2. Use the global resolver when an explicit vault selector is present
    //    OR fall back to walk-up / registry default.
    let store_path = std::env::var("CAIRN_REGISTRY")
        .ok()
        .map(PathBuf::from)
        .or_else(|| crate::vault::VaultRegistryStore::default_path().ok());
    if let Some(store_path) = store_path {
        let store = crate::vault::VaultRegistryStore::new(store_path);
        let opts = crate::vault::ResolveOpts {
            explicit: explicit_vault,
            cwd: std::env::current_dir().ok(),
            store: &store,
        };
        if let Ok(p) = crate::vault::resolve_vault(opts) {
            return p;
        }
    }

    // 3. Last resort.
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// ── error → exit code ─────────────────────────────────────────────────────────

/// Map an [`IdentityServiceError`] to a sysexits-style exit code.
fn identity_exit_code(err: &IdentityServiceError) -> u8 {
    match err {
        IdentityServiceError::PurgeAckMissing => EX_DATAERR,
        IdentityServiceError::Keystore(cairn_core::contract::keystore::KeystoreError::Locked) => {
            EX_UNAVAILABLE
        }
        IdentityServiceError::VaultDegraded { .. }
        | IdentityServiceError::FirstBindInProgress
        | IdentityServiceError::PurgeResumeRequired { .. }
        | IdentityServiceError::VaultReconciliationIncomplete
        | IdentityServiceError::AbandonAfterCommit => EX_TEMPFAIL,
        IdentityServiceError::VaultIdMissing | IdentityServiceError::VaultIdConflict { .. } => {
            EX_CONFIG
        }
        _ => 1,
    }
}

/// Print the error and return the appropriate exit code.
fn identity_err(context: &str, err: &IdentityServiceError) -> ExitCode {
    eprintln!("cairn identity {context}: {err:#}");
    ExitCode::from(identity_exit_code(err))
}

// ── tokio runtime helper ──────────────────────────────────────────────────────

/// Run an async block on a current-thread tokio runtime.
///
/// Short-lived CLI verbs use `current_thread` to avoid spinning a threadpool
/// (CLAUDE.md §6.3).
macro_rules! run_async {
    ($expr:expr) => {{
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("invariant: tokio runtime construction should not fail")
            .block_on($expr)
    }};
}

// ── subcommand handlers ───────────────────────────────────────────────────────

fn run_status(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    run_async!(async {
        let svc = match IdentityService::open_for_maintenance(vault_path, MaintenanceMode::ReadOnly)
            .await
        {
            Ok(s) => s,
            Err(e) => return identity_err("status", &e),
        };
        match svc.status_report().await {
            Ok(report) => {
                if json {
                    match serde_json::to_string_pretty(&report) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("cairn identity status: json serialization failed: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!("{report:?}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("status", &e),
        }
    })
}

fn run_list(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let kind_filter = sub.get_one::<String>("kind").and_then(|k| parse_kind(k));
    run_async!(async {
        let svc = match IdentityService::open_for_maintenance(vault_path, MaintenanceMode::ReadOnly)
            .await
        {
            Ok(s) => s,
            Err(e) => return identity_err("list", &e),
        };
        match svc
            .registry
            .list_identities(kind_filter, IdentityVisibility::Operational)
            .await
        {
            Ok(records) => {
                if json {
                    match serde_json::to_string_pretty(&records) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("cairn identity list: json serialization failed: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else if records.is_empty() {
                    println!("cairn identity list: no identities provisioned");
                } else {
                    for r in &records {
                        println!(
                            "{:<40}  {:?}  {:?}",
                            r.id.as_str(),
                            r.kind,
                            r.provisioning_state
                        );
                    }
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("list", &IdentityServiceError::Registry(e)),
        }
    })
}

fn run_show(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let id_str = sub
        .get_one::<String>("id")
        .expect("invariant: id is required");
    let id = match Identity::parse(id_str.clone()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cairn identity show: invalid identity '{id_str}': {e}");
            return ExitCode::from(EX_DATAERR);
        }
    };
    run_async!(async {
        let svc = match IdentityService::open_for_maintenance(vault_path, MaintenanceMode::ReadOnly)
            .await
        {
            Ok(s) => s,
            Err(e) => return identity_err("show", &e),
        };
        match svc
            .registry
            .get_identity(&id, IdentityVisibility::Operational)
            .await
        {
            Ok(Some(record)) => {
                if json {
                    match serde_json::to_string_pretty(&record) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("cairn identity show: json serialization failed: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!(
                        "{}\n  kind: {:?}\n  state: {:?}\n  key_version: {}",
                        record.id.as_str(),
                        record.kind,
                        record.provisioning_state,
                        record.current_key_version.as_u32()
                    );
                }
                ExitCode::SUCCESS
            }
            Ok(None) => {
                eprintln!("cairn identity show: identity '{id_str}' not found");
                ExitCode::from(EX_DATAERR)
            }
            Err(e) => identity_err("show", &IdentityServiceError::Registry(e)),
        }
    })
}

fn run_provision(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let kind_str = sub
        .get_one::<String>("kind")
        .expect("invariant: kind is required");
    let slug = sub
        .get_one::<String>("slug")
        .expect("invariant: slug is required")
        .clone();

    let Some(kind) = parse_kind(kind_str) else {
        eprintln!(
            "cairn identity provision: unknown kind '{kind_str}' — use human, agent, or sensor"
        );
        return ExitCode::from(EX_DATAERR);
    };

    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("provision", &e),
        };
        let mut rng = OsRng;

        let id = match build_provision_input(&svc, kind, &slug, IdentityRevision::FIRST) {
            Ok(input) => match svc.provision(kind, input, &mut rng).await {
                Ok(i) => i,
                Err(e) => return identity_err("provision", &e),
            },
            Err(e) => return identity_err("provision", &e),
        };

        if json {
            println!("{}", serde_json::json!({ "provisioned": id.as_str() }));
        } else {
            println!("cairn identity provision: provisioned {}", id.as_str());
        }
        ExitCode::SUCCESS
    })
}

fn run_init_defaults(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("init-defaults", &e),
        };
        let mut rng = OsRng;
        match svc.init_defaults(&mut rng).await {
            Ok(state) => {
                if json {
                    match serde_json::to_string_pretty(&state) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!(
                                "cairn identity init-defaults: json serialization failed: {e}"
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!("cairn identity init-defaults: {state:?}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("init-defaults", &e),
        }
    })
}

fn run_rotate(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let id_str = sub
        .get_one::<String>("id")
        .expect("invariant: id is required");
    let id = match Identity::parse(id_str.clone()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cairn identity rotate: invalid identity '{id_str}': {e}");
            return ExitCode::from(EX_DATAERR);
        }
    };
    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("rotate", &e),
        };
        match svc.rotate(&id).await {
            Ok(receipt) => {
                if json {
                    match serde_json::to_string_pretty(&receipt) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("cairn identity rotate: json serialization failed: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!(
                        "cairn identity rotate: rotated {} (receipt id={})",
                        id.as_str(),
                        receipt.id.0
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("rotate", &e),
        }
    })
}

fn run_revoke(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let target_str = sub
        .get_one::<String>("id")
        .expect("invariant: id is required");
    let target = match Identity::parse(target_str.clone()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cairn identity revoke: invalid identity '{target_str}': {e}");
            return ExitCode::from(EX_DATAERR);
        }
    };
    let signer_str = sub.get_one::<String>("signer").cloned();

    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("revoke", &e),
        };

        // Resolve signer: explicit arg, else first independent active agent.
        // We deliberately do NOT fall back to self-revocation: a destructive
        // trust-state mutation must be authorized by an independent signer
        // (or be an explicit operator opt-in via `--signer <target>`).
        // Self-revocation by a compromised identity would otherwise produce a
        // valid-looking receipt and weaken the trust boundary on exactly the
        // failure path we care about.
        let signer = if let Some(s) = signer_str {
            match Identity::parse(s.clone()) {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("cairn identity revoke: invalid signer '{s}': {e}");
                    return ExitCode::from(EX_DATAERR);
                }
            }
        } else {
            match resolve_first_active_agent(&svc).await {
                Ok(Some(id)) if id != target => id,
                Ok(_) => {
                    // No independent signer available. Fail closed; operator
                    // must pass `--signer <id>` explicitly (including the
                    // self-revocation case `--signer <target>`).
                    return identity_err("revoke", &IdentityServiceError::NoLiveAttributableSigner);
                }
                Err(e) => return identity_err("revoke", &e),
            }
        };

        match svc.revoke(&target, &signer).await {
            Ok(receipt) => {
                if json {
                    match serde_json::to_string_pretty(&receipt) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("cairn identity revoke: json serialization failed: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                } else {
                    println!(
                        "cairn identity revoke: revoked {} by {} (receipt id={})",
                        target.as_str(),
                        signer.as_str(),
                        receipt.id.0
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("revoke", &e),
        }
    })
}

fn run_reconcile(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("reconcile", &e),
        };
        match svc.reconcile().await {
            Ok(()) => {
                if json {
                    println!("{}", serde_json::json!({ "reconciled": true }));
                } else {
                    println!("cairn identity reconcile: ok");
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("reconcile", &e),
        }
    })
}

fn run_repair(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let id_str = sub
        .get_one::<String>("id")
        .expect("invariant: id is required");
    let id = match Identity::parse(id_str.clone()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cairn identity repair: invalid identity '{id_str}': {e}");
            return ExitCode::from(EX_DATAERR);
        }
    };
    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("repair", &e),
        };
        match svc.repair(&id).await {
            Ok(()) => {
                if json {
                    println!("{}", serde_json::json!({ "repaired": id.as_str() }));
                } else {
                    println!("cairn identity repair: repaired {}", id.as_str());
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("repair", &e),
        }
    })
}

fn run_purge(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let resume = sub.get_flag("resume");
    let id_str = sub
        .get_one::<String>("id")
        .expect("invariant: id is required");
    let id = match Identity::parse(id_str.clone()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cairn identity purge: invalid identity '{id_str}': {e}");
            return ExitCode::from(EX_DATAERR);
        }
    };
    run_async!(async {
        let (svc, _recon) = match IdentityService::open(vault_path).await {
            Ok(s) => s,
            Err(e) => return identity_err("purge", &e),
        };
        let reason = PurgeReason("cli".to_owned());
        match svc.purge(&id, reason, resume).await {
            Ok(()) => {
                if json {
                    println!("{}", serde_json::json!({ "purged": id.as_str() }));
                } else {
                    println!("cairn identity purge: purged {}", id.as_str());
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("purge", &e),
        }
    })
}

fn run_vault_id_recover(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let probe = sub.get_flag("probe-keychain");
    let override_str = sub.get_one::<String>("vault-id").cloned();
    let override_id = match override_str.as_deref().map(VaultId::parse) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            eprintln!("cairn identity vault-id-recover: invalid vault-id: {e}");
            return ExitCode::from(EX_DATAERR);
        }
        None => None,
    };
    run_async!(async {
        match super::vault_id_recover(vault_path, probe, override_id).await {
            Ok(vault_id) => {
                if json {
                    println!("{}", serde_json::json!({ "vault_id": vault_id.as_str() }));
                } else {
                    println!(
                        "cairn identity vault-id-recover: recovered {}",
                        vault_id.as_str()
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("vault-id-recover", &e),
        }
    })
}

fn run_finalise_binding(sub: &ArgMatches, vault_path: PathBuf) -> ExitCode {
    let json = sub.get_flag("json");
    let abandon = sub.get_flag("abandon");
    let override_str = sub.get_one::<String>("vault-id").cloned();
    let override_id = match override_str.as_deref().map(VaultId::parse) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            eprintln!("cairn identity finalise-binding: invalid vault-id: {e}");
            return ExitCode::from(EX_DATAERR);
        }
        None => None,
    };
    run_async!(async {
        match super::finalise_binding(vault_path, abandon, override_id).await {
            Ok(()) => {
                if json {
                    println!("{}", serde_json::json!({ "finalised": true }));
                } else {
                    println!("cairn identity finalise-binding: ok");
                }
                ExitCode::SUCCESS
            }
            Err(e) => identity_err("finalise-binding", &e),
        }
    })
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Parse the `kind` string to [`IdentityKind`].
fn parse_kind(kind: &str) -> Option<IdentityKind> {
    match kind {
        "human" => Some(IdentityKind::Human),
        "agent" => Some(IdentityKind::Agent),
        "sensor" => Some(IdentityKind::Sensor),
        _ => None,
    }
}

/// Build a [`ProvisionInput`] for the given `kind` and `slug`.
///
/// - `human`: slug is treated as the OS username slug.
/// - `agent`: slug must be `harness:model:role`.
/// - `sensor`: slug must be `family:name:host`.
fn build_provision_input(
    svc: &IdentityService,
    kind: IdentityKind,
    slug: &str,
    rev: IdentityRevision,
) -> Result<ProvisionInput, IdentityServiceError> {
    use cairn_core::contract::identity_registry::RegistryError;

    let id = match kind {
        IdentityKind::Human => mint_human_id(slug, rev)
            .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?,
        IdentityKind::Agent => {
            let parts: Vec<&str> = slug.splitn(3, ':').collect();
            if parts.len() != 3 {
                return Err(IdentityServiceError::Registry(RegistryError::Backend(
                    "agent slug must be harness:model:role".into(),
                )));
            }
            mint_agent_id(parts[0], parts[1], parts[2], rev)
                .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?
        }
        IdentityKind::Sensor => {
            let parts: Vec<&str> = slug.splitn(3, ':').collect();
            if parts.len() != 3 {
                return Err(IdentityServiceError::Registry(RegistryError::Backend(
                    "sensor slug must be family:name:host".into(),
                )));
            }
            mint_sensor_id(parts[0], parts[1], parts[2], rev)
                .map_err(|e| IdentityServiceError::Registry(RegistryError::Backend(Box::new(e))))?
        }
    };

    Ok(ProvisionInput {
        vault_id: svc.vault_id.clone(),
        id,
        kind,
        revision: rev,
    })
}

/// Return the first Active agent identity from the registry, if any.
async fn resolve_first_active_agent(
    svc: &IdentityService,
) -> Result<Option<Identity>, IdentityServiceError> {
    use cairn_core::domain::identity::records::ProvisioningState;

    let agents = svc
        .registry
        .list_identities(Some(IdentityKind::Agent), IdentityVisibility::Operational)
        .await
        .map_err(IdentityServiceError::Registry)?;

    Ok(agents
        .into_iter()
        .find(|r| r.provisioning_state == ProvisioningState::Active)
        .map(|r| r.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_subcommand_help_renders() {
        let mut cmd = identity_subcommand();
        let mut help = Vec::new();
        cmd.write_long_help(&mut help).unwrap();
        let s = String::from_utf8(help).unwrap();
        assert!(s.contains("provision"), "help missing 'provision': {s}");
        assert!(s.contains("rotate"), "help missing 'rotate': {s}");
        assert!(s.contains("status"), "help missing 'status': {s}");
    }

    #[test]
    fn parse_kind_variants() {
        assert_eq!(parse_kind("human"), Some(IdentityKind::Human));
        assert_eq!(parse_kind("agent"), Some(IdentityKind::Agent));
        assert_eq!(parse_kind("sensor"), Some(IdentityKind::Sensor));
        assert_eq!(parse_kind("other"), None);
    }

    #[test]
    fn identity_subcommand_list_with_kind_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "list", "--kind", "human"])
            .expect("should parse");
        let (sub_name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(sub_name, "list");
        assert_eq!(
            sub_m.get_one::<String>("kind").map(String::as_str),
            Some("human")
        );
    }

    #[test]
    fn identity_subcommand_list_with_json_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "list", "--json"])
            .expect("should parse");
        let (_name, sub_m) = m.subcommand().expect("subcommand present");
        assert!(sub_m.get_flag("json"));
    }

    #[test]
    fn identity_subcommand_provision_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "provision", "human", "alice"])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "provision");
        assert_eq!(
            sub_m.get_one::<String>("kind").map(String::as_str),
            Some("human")
        );
        assert_eq!(
            sub_m.get_one::<String>("slug").map(String::as_str),
            Some("alice")
        );
    }

    #[test]
    fn identity_subcommand_rotate_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "rotate", "hmn:alice:v1"])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "rotate");
        assert_eq!(
            sub_m.get_one::<String>("id").map(String::as_str),
            Some("hmn:alice:v1")
        );
    }

    #[test]
    fn identity_subcommand_revoke_with_signer_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from([
                "identity",
                "revoke",
                "hmn:alice:v1",
                "--signer",
                "agt:claude-code:opus-4-7:main:v1",
            ])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "revoke");
        assert_eq!(
            sub_m.get_one::<String>("signer").map(String::as_str),
            Some("agt:claude-code:opus-4-7:main:v1")
        );
    }

    #[test]
    fn identity_subcommand_purge_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "purge", "hmn:alice:v1"])
            .expect("should parse");
        let (name, _sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "purge");
    }

    #[test]
    fn identity_subcommand_vault_id_recover_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "vault-id-recover", "--probe-keychain"])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "vault-id-recover");
        assert!(sub_m.get_flag("probe-keychain"));
    }

    #[test]
    fn identity_subcommand_finalise_binding_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "finalise-binding", "--abandon"])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "finalise-binding");
        assert!(sub_m.get_flag("abandon"));
    }

    #[test]
    fn identity_subcommand_status_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "status"])
            .expect("should parse");
        let (name, _) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "status");
    }

    #[test]
    fn identity_subcommand_reconcile_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "reconcile"])
            .expect("should parse");
        let (name, _) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "reconcile");
    }

    #[test]
    fn identity_subcommand_repair_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "repair", "hmn:alice:v1"])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "repair");
        assert_eq!(
            sub_m.get_one::<String>("id").map(String::as_str),
            Some("hmn:alice:v1")
        );
    }

    #[test]
    fn identity_subcommand_show_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "show", "hmn:alice:v1"])
            .expect("should parse");
        let (name, sub_m) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "show");
        assert_eq!(
            sub_m.get_one::<String>("id").map(String::as_str),
            Some("hmn:alice:v1")
        );
    }

    #[test]
    fn identity_subcommand_init_defaults_parses() {
        let cmd = identity_subcommand();
        let m = cmd
            .try_get_matches_from(["identity", "init-defaults"])
            .expect("should parse");
        let (name, _) = m.subcommand().expect("subcommand present");
        assert_eq!(name, "init-defaults");
    }
}
