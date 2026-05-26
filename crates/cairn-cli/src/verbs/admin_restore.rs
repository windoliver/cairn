//! `cairn admin restore --from <PATH> --into <PATH> [--json]`

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::ArgMatches;
use serde::Serialize;

#[derive(Serialize)]
struct RestoreReceipt {
    from: String,
    into: String,
    status: &'static str,
}

/// Run `cairn admin restore`.
#[must_use]
pub fn run(sub: &ArgMatches, _vault_root: &Path) -> ExitCode {
    let from = PathBuf::from(
        sub.get_one::<String>("from")
            .expect("invariant: clap requires --from"),
    );
    let into = PathBuf::from(
        sub.get_one::<String>("into")
            .expect("invariant: clap requires --into"),
    );
    let json = sub.get_flag("json");

    let receipt = RestoreReceipt {
        from: from.display().to_string(),
        into: into.display().to_string(),
        status: "accepted",
    };

    // NOTE: forget-replay on restore is temporarily skipped here; it is
    // re-wired through the new verb fn + SqliteConsentLog in Tasks 2.5/2.6.
    let restore_result =
        super::admin_snapshot::validate_non_overlapping_paths("backup", &from, "restore", &into)
            .and_then(|()| super::admin_snapshot::validate_backup_root(&from))
            .and_then(|()| super::admin_snapshot::materialize_backup_artifact(&from, &into));
    if let Err(error) = restore_result {
        eprintln!("cairn admin restore: {error:#}");
        return ExitCode::from(74);
    }

    if json {
        match serde_json::to_string_pretty(&receipt) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("cairn admin restore: failed to render json — {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        println!(
            "cairn admin restore: accepted restore from {} into {}",
            receipt.from, receipt.into
        );
    }

    ExitCode::SUCCESS
}
