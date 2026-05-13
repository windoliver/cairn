//! `cairn admin snapshot --backup <PATH> [--json]`

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::ArgMatches;
use serde::Serialize;

#[derive(Serialize)]
struct SnapshotReceipt {
    backup_path: String,
    registry_dir: String,
    registry_entry: String,
}

/// Run `cairn admin snapshot`.
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let backup_path = std::path::PathBuf::from(
        sub.get_one::<String>("backup")
            .expect("invariant: clap requires --backup"),
    );
    let json = sub.get_flag("json");
    let registry_dir = vault_root.join(".cairn").join("backups");

    if let Err(error) = std::fs::create_dir_all(&registry_dir) {
        eprintln!("cairn admin snapshot: failed to create backup registry — {error}");
        return ExitCode::from(74);
    }

    if let Err(error) = std::fs::create_dir_all(&backup_path) {
        eprintln!("cairn admin snapshot: failed to create backup path — {error}");
        return ExitCode::from(74);
    }

    let registry_entry = registry_dir.join(format!("snapshot-{}.json", timestamp_millis()));
    let receipt = SnapshotReceipt {
        backup_path: backup_path.display().to_string(),
        registry_dir: registry_dir.display().to_string(),
        registry_entry: registry_entry.display().to_string(),
    };

    let payload = match serde_json::to_vec_pretty(&receipt) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("cairn admin snapshot: failed to serialize receipt — {error}");
            return ExitCode::from(1);
        }
    };

    if let Err(error) = std::fs::write(&registry_entry, payload) {
        eprintln!("cairn admin snapshot: failed to write registry entry — {error}");
        return ExitCode::from(74);
    }

    if json {
        match serde_json::to_string_pretty(&receipt) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("cairn admin snapshot: failed to render json — {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        println!(
            "cairn admin snapshot: prepared backup at {}",
            backup_path.display()
        );
    }

    ExitCode::SUCCESS
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}
