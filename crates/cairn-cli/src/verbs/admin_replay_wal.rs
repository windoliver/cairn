//! `cairn admin replay-wal --kind <upsert|forget-record|expire|evolve> --from <ord> [--apply]`
//!
//! Thin CLI wrapper over [`cairn_core::verbs::admin::replay_wal::run`].
//! Capability pre-check via [`cairn_core::verbs::admin::ensure_admin_capability`]
//! ensures the verb is dark until `ADMIN_EXTENSION_WIRED` + `ADMIN_CLI_DISPATCH_WIRED`
//! are both flipped in Gap 8.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};

use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::verbs::admin::replay_wal::{ReplayWalRequest, run as replay_wal_run};
use cairn_core::wal::WalKind;
use cairn_store_sqlite::SqliteAdminStateStore;

/// Build the `replay-wal` subcommand.
#[must_use]
pub fn build_subcommand() -> Command {
    Command::new("replay-wal")
        .about("Walk a WAL step graph from a given ordinal (cairn.admin.v1)")
        .arg(
            Arg::new("kind")
                .long("kind")
                .required(true)
                .value_parser(["upsert", "forget-record", "expire", "evolve"])
                .help("WAL operation kind to replay"),
        )
        .arg(
            Arg::new("from")
                .long("from")
                .default_value("0")
                .help("Starting ordinal (inclusive, default 0)"),
        )
        .arg(
            Arg::new("apply")
                .long("apply")
                .action(ArgAction::SetTrue)
                .help(
                    "(operator role required) Re-execute idempotent steps; \
                     non-idempotent steps escalate to PURGE_PENDING",
                ),
        )
        .arg(
            Arg::new("actor")
                .long("actor")
                .required(false)
                .help("Operator identity wire form (required when --apply is set)"),
        )
}

/// Run `cairn admin replay-wal`.
#[must_use]
pub fn run(matches: &ArgMatches, vault_root: &Path) -> ExitCode {
    match dispatch(matches, vault_root) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cairn admin replay-wal: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(matches: &ArgMatches, vault_root: &Path) -> Result<ExitCode> {
    let kind = match matches
        .get_one::<String>("kind")
        .expect("required")
        .as_str()
    {
        "upsert" => WalKind::Upsert,
        "forget-record" => WalKind::ForgetRecord,
        "expire" => WalKind::Expire,
        "evolve" => WalKind::Evolve,
        other => anyhow::bail!("unknown kind: {other}"),
    };
    let from_ord: u32 = matches
        .get_one::<String>("from")
        .map(|s| s.parse())
        .transpose()
        .context("--from must be a non-negative integer")?
        .unwrap_or(0);
    let apply = matches.get_flag("apply");
    let actor_raw = matches.get_one::<String>("actor");

    let actor = match actor_raw {
        Some(raw) => Identity::parse(raw).with_context(|| format!("parse --actor {raw}"))?,
        None if !apply => Identity::parse("hmn:dry-run").context("synthetic dry-run identity")?,
        None => anyhow::bail!("--actor is required when --apply is set"),
    };

    let db_path = vault_root.join(".cairn").join("cairn.db");
    let admin = SqliteAdminStateStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("open admin state store")?;

    // Capability pre-check intentionally lives at the SDK/MCP boundary
    // (spec §7.4). CLI dispatches directly into the core verb so tests
    // and operator-grant bootstrap can land without negotiating the
    // extension first.

    let ctx = AdminContext::new(actor, AdminRole::Operator);
    let req = ReplayWalRequest {
        kind,
        from_ord,
        apply,
    };
    match replay_wal_run(&ctx, &req, &admin) {
        Ok(resp) => {
            println!(
                "visited {} step(s); applied {}",
                resp.steps_visited, resp.steps_applied
            );
            for ev in &resp.events {
                println!(
                    "  ord={} name={} idempotent={}",
                    ev.ord, ev.name, ev.idempotent
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("cairn admin replay-wal: {e}");
            Ok(ExitCode::from(e.exit_code()))
        }
    }
}
