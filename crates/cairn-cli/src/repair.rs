//! CLI dispatch for operator repair commands.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_store_sqlite::StoreError;
use cairn_store_sqlite::repair::consent_journal::{BlockerCode, delete_blocker, list_blockers};
use clap::ArgMatches;

use crate::vault::{ResolveOpts, VaultRegistryStore, resolve_vault};

/// Run the `repair` command tree.
pub fn run(matches: &ArgMatches, explicit_vault: Option<String>) -> ExitCode {
    match run_inner(matches, explicit_vault) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cairn repair: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

fn run_inner(matches: &ArgMatches, explicit_vault: Option<String>) -> Result<(), RepairCliError> {
    match matches.subcommand() {
        Some(("consent-journal", sub)) => run_consent_journal(sub, explicit_vault),
        _ => unreachable!("clap subcommand_required(true) on repair ensures a subcommand is set"),
    }
}

fn run_consent_journal(
    matches: &ArgMatches,
    explicit_vault: Option<String>,
) -> Result<(), RepairCliError> {
    let vault_path = resolve_vault_path(explicit_vault)?;
    let db_path = vault_path.join(".cairn").join("cairn.db");
    if !db_path.is_file() {
        return Err(RepairCliError::Config(anyhow::anyhow!(
            "vault database not found at {}",
            db_path.display()
        )));
    }

    let mut conn = rusqlite::Connection::open(&db_path)?;
    let json = matches.get_flag("json");

    if let Some(rowid) = matches.get_one::<i64>("delete-rowid").copied() {
        let reason = matches
            .get_one::<String>("reason")
            .expect("invariant: --delete-rowid requires --reason");
        let operator = operator_identity();
        let receipt = delete_blocker(&mut conn, rowid, reason, &operator)?;
        if json {
            print_json(serde_json::json!({ "deleted": receipt }))?;
        } else {
            println!(
                "cairn repair consent-journal: deleted rowid {} (repair_id {})",
                receipt.target_rowid, receipt.repair_id
            );
        }
        return Ok(());
    }

    let blockers = list_blockers(&conn)?;
    if json {
        print_json(serde_json::json!({ "blockers": blockers }))?;
    } else if blockers.is_empty() {
        println!("cairn repair consent-journal: no blockers found");
    } else {
        for row in blockers {
            println!(
                "rowid={} consent_id={} blockers={}",
                row.rowid,
                row.consent_id,
                render_blocker_codes(&row.blocker_codes)
            );
        }
    }
    Ok(())
}

fn resolve_vault_path(explicit_vault: Option<String>) -> Result<PathBuf, RepairCliError> {
    let store = registry_store()?;
    let vault_path = resolve_vault(ResolveOpts {
        explicit: explicit_vault,
        cwd: std::env::current_dir().ok(),
        store: &store,
    })
    .context("resolving vault for repair command")?;
    Ok(vault_path)
}

fn registry_store() -> anyhow::Result<VaultRegistryStore> {
    let path = if let Ok(p) = std::env::var("CAIRN_REGISTRY") {
        PathBuf::from(p)
    } else {
        VaultRegistryStore::default_path()?
    };
    Ok(VaultRegistryStore::new(path))
}

fn operator_identity() -> String {
    format!("hmn:{}", whoami::username())
}

fn print_json(value: serde_json::Value) -> Result<(), RepairCliError> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn render_blocker_codes(codes: &[BlockerCode]) -> String {
    codes
        .iter()
        .map(|code| match code {
            BlockerCode::NonPositiveRowid => "non_positive_rowid",
            BlockerCode::UnrenderableDecidedAt => "unrenderable_decided_at",
            BlockerCode::UnrenderableExpiresAt => "unrenderable_expires_at",
            BlockerCode::KindNullEventFieldDrift => "kind_null_event_field_drift",
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Debug, thiserror::Error)]
enum RepairCliError {
    #[error("{0:#}")]
    Config(#[from] anyhow::Error),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl RepairCliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => 78,
            Self::Store(StoreError::RepairNotEligible { .. }) => 65,
            Self::Store(_) | Self::Sqlite(_) | Self::Json(_) => 74,
        }
    }
}
