//! `cairn admin grant <identity>` — bootstrap a human identity as `Operator`.
//!
//! v0.2 minimum-viable operator-role bootstrap. The granter is recorded as
//! the same identity (self-grant) — `cairn admin grant` is intended as a
//! local-machine first-run helper. Future hardware-countersign adapter will
//! replace this with a stronger flow.
//!
//! **Capability carve-out:** `admin grant` is deliberately excluded from the
//! `ensure_admin_capability` pre-check that guards every other admin verb. The
//! grant subcommand is the bootstrap primitive that satisfies *one* of the two
//! runtime preconditions (`has_operator`) required by those other verbs; calling
//! the capability gate before grant would create a chicken-and-egg deadlock. The
//! operator must use `admin grant` first, then flip `config.admin.enabled = true`
//! in `.cairn/config.yaml`, to unlock the rest of the admin surface.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Arg, ArgMatches, Command};

use cairn_core::contract::AdminStateStore as _;
use cairn_core::domain::Identity;
use cairn_core::domain::admin::AdminRole;
use cairn_store_sqlite::SqliteAdminStateStore;

/// Build the `grant` subcommand.
#[must_use]
pub fn build_subcommand() -> Command {
    Command::new("grant")
        .about("Grant operator role to an identity (cairn.admin.v1 bootstrap)")
        .arg(
            Arg::new("identity")
                .required(true)
                .help("Identity wire form, e.g. hmn:alice"),
        )
}

/// Run `cairn admin grant <identity>`.
#[must_use]
pub fn run(matches: &ArgMatches, vault_root: &Path) -> ExitCode {
    let raw = matches
        .get_one::<String>("identity")
        .expect("clap required arg");
    match grant(raw, vault_root) {
        Ok(()) => {
            println!("granted operator role to {raw}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn admin grant: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn grant(raw: &str, vault_root: &Path) -> Result<()> {
    let identity = Identity::parse(raw).with_context(|| format!("parse identity {raw}"))?;
    let db_path = vault_root.join(".cairn").join("cairn.db");
    let admin = SqliteAdminStateStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("open admin state store")?;
    admin
        .grant_role(&identity, AdminRole::Operator, &identity)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("write admin_roles row")?;
    Ok(())
}
