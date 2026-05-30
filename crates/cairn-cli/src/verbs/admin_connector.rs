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

use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminError, AdminRole};
use cairn_core::verbs::admin::connector::{self, ConnectorDisableRequest, ConnectorEnableRequest};
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
    crate::admin::ensure_main_store_schema(&db_path)?;
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
            let name = sub.get_one::<String>("name").expect("clap required arg");
            // Validate inputs for clean errors (mirrors enable/disable UX)
            // before failing closed.
            parse_actor(sub)?;
            sub.get_one::<String>("from")
                .expect("clap required arg")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .context("--from must be RFC3339 (e.g. 2026-01-01T00:00:00Z)")?;
            sub.get_one::<String>("to")
                .expect("clap required arg")
                .parse::<chrono::DateTime<chrono::Utc>>()
                .context("--to must be RFC3339")?;
            if let Some(rate) = sub.get_one::<String>("rate") {
                rate.parse::<f64>()
                    .context("--rate-per-sec must be a positive float")?;
            }

            // Fail closed: no real `BackfillSpawner` is wired, so calling the
            // verb with a no-op spawner would mint a `WorkflowId` and report a
            // "spawned" backfill that has no durable job or progress stream —
            // a misleading success. Refuse with EX_UNAVAILABLE instead, and do
            // NOT print a workflow id (round-4 review #4). The MCP surface
            // fails closed the same way; remove once a real spawner lands.
            eprintln!(
                "cairn admin connector backfill: unavailable — no BackfillSpawner is \
                 wired in this build, so no durable backfill job would be created for \
                 connector '{name}'. Refusing to report a spawned backfill \
                 (tracked: scheduler integration)."
            );
            Ok(ExitCode::from(69)) // EX_UNAVAILABLE (sysexits)
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
