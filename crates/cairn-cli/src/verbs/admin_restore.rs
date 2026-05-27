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

    let restore_result =
        super::admin_snapshot::validate_non_overlapping_paths("backup", &from, "restore", &into)
            .and_then(|()| super::admin_snapshot::validate_backup_root(&from))
            .and_then(|()| super::admin_snapshot::materialize_backup_artifact(&from, &into))
            .and_then(|()| {
                // #161: forget-replay through the SqliteConsentLog adapter
                // that replaced the inline helper. Tombstones recorded
                // since the backup was taken are re-applied to the
                // restored DB before reads resume — same semantics as the
                // v0.1 `replay_current_forgets` inline path.
                use cairn_core::contract::snapshot_artifact::ConsentLog as _;
                use cairn_store_sqlite::SqliteConsentLog;
                let live_db = from.join(".cairn").join("cairn.db");
                let restored_db = into.join(".cairn").join("cairn.db");
                let consent = SqliteConsentLog::new(live_db, restored_db);
                consent
                    .apply_post_restore_purge()
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("replay-current-forgets via ConsentLog: {e}"))
            });
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
