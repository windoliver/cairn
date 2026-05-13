//! `cairn admin restore --from <PATH> --into <PATH> [--json]`

use std::path::Path;
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
pub fn run(sub: &ArgMatches, _vault_root: &Path) -> ExitCode {
    let from = sub
        .get_one::<String>("from")
        .expect("invariant: clap requires --from");
    let into = sub
        .get_one::<String>("into")
        .expect("invariant: clap requires --into");
    let json = sub.get_flag("json");

    let receipt = RestoreReceipt {
        from: from.clone(),
        into: into.clone(),
        status: "accepted",
    };

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
            from, into
        );
    }

    ExitCode::SUCCESS
}
