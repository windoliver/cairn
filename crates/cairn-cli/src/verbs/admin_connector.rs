//! `cairn admin connector {enable, disable, backfill}` — connector lifecycle verbs.
//!
//! Thin CLI wrapper over [`cairn_core::verbs::admin::connector`].
//! Capability pre-check via [`cairn_core::verbs::admin::ensure_admin_capability`]
//! ensures the verb is dark until `ADMIN_EXTENSION_WIRED` + `ADMIN_CLI_DISPATCH_WIRED`
//! are both flipped in Gap 8.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Arg, ArgMatches, Command};

use cairn_core::contract::AdminStateStore as _;
use cairn_core::contract::backfill_spawner::{BackfillSpawner, BackfillSpec};
use cairn_core::contract::memory_store::StoreError;
use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminError, AdminRole};
use cairn_core::verbs::admin::{
    CAP_CONNECTOR_BACKFILL, CAP_CONNECTOR_DISABLE, CAP_CONNECTOR_ENABLE,
    connector::{self, BackfillRequest, ConnectorDisableRequest, ConnectorEnableRequest},
    ensure_admin_capability,
};
use cairn_store_sqlite::SqliteAdminStateStore;

/// Build the `connector` subcommand group.
#[must_use]
pub fn build_subcommand() -> Command {
    Command::new("connector")
        .about("cairn.admin.v1 connector lifecycle verbs")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("enable")
                .about("Mark a connector enabled (gates the poll loop)")
                .arg(Arg::new("name").required(true).help("Connector name"))
                .arg(
                    Arg::new("actor")
                        .long("actor")
                        .required(true)
                        .help("Operator identity wire form (e.g. hmn:alice)"),
                ),
        )
        .subcommand(
            Command::new("disable")
                .about("Mark a connector disabled (poll loop honors at next tick)")
                .arg(Arg::new("name").required(true).help("Connector name"))
                .arg(
                    Arg::new("actor")
                        .long("actor")
                        .required(true)
                        .help("Operator identity wire form"),
                )
                .arg(
                    Arg::new("reason")
                        .long("reason")
                        .help("Optional human-readable reason for the change"),
                ),
        )
        .subcommand(
            Command::new("backfill")
                .about("Spawn a bounded backfill (spawner adapter not yet wired)")
                .arg(Arg::new("name").required(true).help("Connector name"))
                .arg(
                    Arg::new("from")
                        .long("from")
                        .required(true)
                        .help("Backfill window start (RFC3339, e.g. 2026-01-01T00:00:00Z)"),
                )
                .arg(
                    Arg::new("to")
                        .long("to")
                        .required(true)
                        .help("Backfill window end (RFC3339)"),
                )
                .arg(
                    Arg::new("rate")
                        .long("rate-per-sec")
                        .default_value("10")
                        .help("Maximum ingest rate in items per second"),
                )
                .arg(
                    Arg::new("actor")
                        .long("actor")
                        .required(true)
                        .help("Operator identity wire form"),
                ),
        )
}

/// Run `cairn admin connector`.
#[must_use]
pub fn run(matches: &ArgMatches, vault_root: &Path) -> ExitCode {
    match dispatch(matches, vault_root) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cairn admin connector: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "three subcommand arms (enable/disable/backfill); each is linear and best read inline"
)]
fn dispatch(matches: &ArgMatches, vault_root: &Path) -> Result<ExitCode> {
    let db_path = vault_root.join(".cairn").join("cairn.db");
    let admin = SqliteAdminStateStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("open admin state store")?;

    match matches.subcommand() {
        Some(("enable", sub)) => {
            let name = sub
                .get_one::<String>("name")
                .expect("clap required arg")
                .clone();
            let actor = parse_actor(sub)?;

            // Capability pre-check.
            let has_op = admin
                .has_any_operator()
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("query admin_roles")?;
            if let Err(e) =
                ensure_admin_capability(CAP_CONNECTOR_ENABLE, /*config_enabled=*/ true, has_op)
            {
                eprintln!("cairn admin connector enable: {e}");
                return Ok(ExitCode::from(e.exit_code()));
            }

            let ctx = AdminContext::new(actor, AdminRole::Operator);
            let req = ConnectorEnableRequest { name };
            match connector::enable(&ctx, &req, &admin) {
                Ok(resp) => {
                    println!("enabled {}", resp.row.connector_name);
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => Ok(exit_for(&e)),
            }
        }
        Some(("disable", sub)) => {
            let name = sub
                .get_one::<String>("name")
                .expect("clap required arg")
                .clone();
            let actor = parse_actor(sub)?;
            let reason = sub.get_one::<String>("reason").cloned();

            // Capability pre-check.
            let has_op = admin
                .has_any_operator()
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("query admin_roles")?;
            if let Err(e) = ensure_admin_capability(
                CAP_CONNECTOR_DISABLE,
                /*config_enabled=*/ true,
                has_op,
            ) {
                eprintln!("cairn admin connector disable: {e}");
                return Ok(ExitCode::from(e.exit_code()));
            }

            let ctx = AdminContext::new(actor, AdminRole::Operator);
            let req = ConnectorDisableRequest { name, reason };
            match connector::disable(&ctx, &req, &admin) {
                Ok(resp) => {
                    println!(
                        "disabled {} reason={}",
                        resp.row.connector_name,
                        resp.row.reason.as_deref().unwrap_or(""),
                    );
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => Ok(exit_for(&e)),
            }
        }
        Some(("backfill", sub)) => {
            let name = sub
                .get_one::<String>("name")
                .expect("clap required arg")
                .clone();
            let actor = parse_actor(sub)?;
            let from = sub
                .get_one::<String>("from")
                .expect("clap required arg")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .context("--from must be RFC3339 (e.g. 2026-01-01T00:00:00Z)")?;
            let to = sub
                .get_one::<String>("to")
                .expect("clap required arg")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .context("--to must be RFC3339")?;
            let rate: f64 = sub
                .get_one::<String>("rate")
                .map(|s| s.parse::<f64>())
                .transpose()
                .context("--rate-per-sec must be a positive float")?
                .unwrap_or(10.0);

            // Capability pre-check.
            let has_op = admin
                .has_any_operator()
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("query admin_roles")?;
            if let Err(e) = ensure_admin_capability(
                CAP_CONNECTOR_BACKFILL,
                /*config_enabled=*/ true,
                has_op,
            ) {
                eprintln!("cairn admin connector backfill: {e}");
                return Ok(ExitCode::from(e.exit_code()));
            }

            let ctx = AdminContext::new(actor, AdminRole::Operator);
            let spawner = NoopSpawner;
            let req = BackfillRequest {
                name,
                from,
                to,
                rate_per_sec: rate,
            };
            match connector::backfill(&ctx, &req, &admin, &spawner) {
                Ok(resp) => {
                    println!("backfill spawned workflow_id={}", resp.workflow_id);
                    eprintln!(
                        "note: BackfillSpawner adapter not yet wired; \
                         this is a no-op placeholder"
                    );
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => Ok(exit_for(&e)),
            }
        }
        _ => unreachable!(
            "clap subcommand_required(true) on admin connector ensures a subcommand is present"
        ),
    }
}

fn parse_actor(sub: &ArgMatches) -> Result<Identity> {
    let raw = sub.get_one::<String>("actor").expect("clap required arg");
    Identity::parse(raw).with_context(|| format!("parse --actor {raw}"))
}

fn exit_for(e: &AdminError) -> ExitCode {
    eprintln!("cairn admin connector: {e}");
    ExitCode::from(e.exit_code())
}

/// Placeholder spawner — `cairn.admin.v1` `connector_backfill` ships its verb
/// signature in v0.2; the actual scheduler/handler/jsonl bus is a follow-up.
/// The verb still validates role + connector-state and mints a `WorkflowId`;
/// the spawner just records that it was called.
struct NoopSpawner;

impl BackfillSpawner for NoopSpawner {
    fn spawn_backfill(&self, _spec: BackfillSpec) -> Result<(), StoreError> {
        Ok(())
    }
}
