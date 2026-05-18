//! `cairn backup` operator registry commands.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::ArgMatches;
use serde::Serialize;

#[derive(Serialize)]
struct BackupListReceipt {
    backups: Vec<super::admin_snapshot::BackupRegistryReceipt>,
}

#[derive(Serialize)]
struct BackupForgetReceipt {
    forgotten: super::admin_snapshot::BackupRegistryReceipt,
}

/// Run `cairn backup`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    match sub.subcommand() {
        Some(("register", register)) => run_register(register, vault_root),
        Some(("list", list)) => run_list(list, vault_root),
        Some(("forget", forget)) => run_forget(forget, vault_root),
        _ => unreachable!("clap subcommand_required(true) on backup ensures a subcommand"),
    }
}

fn run_register(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let path = PathBuf::from(
        sub.get_one::<String>("path")
            .expect("invariant: clap requires path"),
    );
    let kind = sub
        .get_one::<String>("kind")
        .map_or("export", String::as_str);
    let json = sub.get_flag("json");

    match super::admin_snapshot::register_backup_artifact(vault_root, &path, kind) {
        Ok(receipt) => emit_registry_receipt("register", &receipt, json),
        Err(error) => {
            eprintln!("cairn backup register: {error:#}");
            ExitCode::from(74)
        }
    }
}

fn run_list(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = sub.get_flag("json");
    match super::admin_snapshot::list_backup_registry(vault_root) {
        Ok(backups) => {
            if json {
                match serde_json::to_string_pretty(&BackupListReceipt { backups }) {
                    Ok(output) => println!("{output}"),
                    Err(error) => {
                        eprintln!("cairn backup list: failed to render json — {error}");
                        return ExitCode::FAILURE;
                    }
                }
            } else if backups.is_empty() {
                println!("cairn backup: no registered backups");
            } else {
                for backup in backups {
                    println!(
                        "{}  {}  {}  {}",
                        backup.file_digest,
                        backup.backup_kind,
                        backup.backup_id,
                        backup.artifact_path
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cairn backup list: {error:#}");
            ExitCode::from(74)
        }
    }
}

fn run_forget(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let digest = sub
        .get_one::<String>("digest")
        .expect("invariant: clap requires digest");
    let json = sub.get_flag("json");

    match super::admin_snapshot::forget_backup_registry_entry(vault_root, digest) {
        Ok(Some(receipt)) => {
            if json {
                match serde_json::to_string_pretty(&BackupForgetReceipt { forgotten: receipt }) {
                    Ok(output) => println!("{output}"),
                    Err(error) => {
                        eprintln!("cairn backup forget: failed to render json — {error}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                println!("cairn backup: forgot registry entry {digest}");
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("cairn backup forget: no registered backup has digest {digest}");
            ExitCode::from(1)
        }
        Err(error) => {
            eprintln!("cairn backup forget: {error:#}");
            ExitCode::from(74)
        }
    }
}

fn emit_registry_receipt(
    verb: &str,
    receipt: &super::admin_snapshot::BackupRegistryReceipt,
    json: bool,
) -> ExitCode {
    if json {
        match serde_json::to_string_pretty(&receipt) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("cairn backup {verb}: failed to render json — {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!(
            "cairn backup: registered {} ({})",
            receipt.artifact_path, receipt.file_digest
        );
    }
    ExitCode::SUCCESS
}
