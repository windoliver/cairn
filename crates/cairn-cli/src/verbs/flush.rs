//! `cairn flush list / apply / reject` — admin-style subcommands for the
//! human-review flow (brief §5.5). Not in IDL; CLI-only.
//!
//! Vault root resolved from `CAIRN_VAULT` env var. The plan files live
//! under `<vault>/.cairn/flush/{pending,applied,rejected}/`.
//!
//! `apply` and `reject` are intentionally stubbed in this commit; later
//! tasks (#9, #10) implement them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir};
use cairn_core::domain::flush_plan::{PersistedPlan, PlanStatus};
use clap::{Arg, ArgAction, ArgMatches, Command};

/// Build the `flush` subcommand group.
#[must_use]
pub fn command() -> Command {
    Command::new("flush")
        .about("Manage human-review FlushPlans (brief §5.5)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("list")
                .about("List FlushPlans under .cairn/flush/")
                .arg(
                    Arg::new("all")
                        .long("all")
                        .action(ArgAction::SetTrue)
                        .help("Include applied/ and rejected/ buckets"),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit machine-readable JSON"),
                ),
        )
        .subcommand(
            Command::new("apply")
                .about("Apply a pending plan to MemoryStore")
                .arg(Arg::new("id").required(true).help("Plan ULID to apply"))
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit machine-readable JSON"),
                ),
        )
        .subcommand(
            Command::new("reject")
                .about("Reject a pending plan; record a reason")
                .arg(Arg::new("id").required(true).help("Plan ULID to reject"))
                .arg(
                    Arg::new("reason")
                        .long("reason")
                        .required(true)
                        .help("Free-form reason recorded with the rejection"),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit machine-readable JSON"),
                ),
        )
}

/// Dispatch the `flush` group.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let vault = match resolve_vault() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush: {e}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };
    match sub.subcommand() {
        Some(("list", m)) => list(&vault, m),
        Some(("apply", _)) => {
            eprintln!("cairn flush apply: not yet implemented in this commit");
            ExitCode::from(70) // EX_SOFTWARE — Task 9 fills this in
        }
        Some(("reject", _)) => {
            eprintln!("cairn flush reject: not yet implemented in this commit");
            ExitCode::from(70) // EX_SOFTWARE — Task 10 fills this in
        }
        _ => ExitCode::from(64),
    }
}

fn resolve_vault() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CAIRN_VAULT") {
        return Ok(PathBuf::from(p));
    }
    Err("vault root not set: pass CAIRN_VAULT".into())
}

/// Summary row emitted by `flush list`.
#[derive(serde::Serialize)]
struct PlanSummary {
    id: String,
    bucket: &'static str,
    mode: String,
    mutations: usize,
    issued_at: String,
    status: String,
}

fn list(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    let buckets: Vec<Bucket> = if m.get_flag("all") {
        Bucket::all().to_vec()
    } else {
        vec![Bucket::Pending]
    };
    let mut rows: Vec<PlanSummary> = Vec::new();
    for b in buckets {
        let dir = bucket_dir(vault, b);
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(p) = serde_json::from_slice::<PersistedPlan>(&bytes) else {
                continue;
            };
            rows.push(PlanSummary {
                id: p.plan.operation_id.0.clone(),
                bucket: b.dir_name(),
                mode: format!("{:?}", p.plan.mode),
                mutations: p.plan.mutations.len(),
                issued_at: p.plan.issued_at.clone(),
                status: match p.status {
                    PlanStatus::Pending => "pending".into(),
                    PlanStatus::Applied { .. } => "applied".into(),
                    PlanStatus::Rejected { .. } => "rejected".into(),
                    _ => "unknown".into(),
                },
            });
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
    } else if rows.is_empty() {
        println!("(no plans)");
    } else {
        for r in &rows {
            println!(
                "{} {:<8} {:<14} mutations={} issued={} status={}",
                r.id, r.bucket, r.mode, r.mutations, r.issued_at, r.status
            );
        }
    }
    ExitCode::SUCCESS
}
