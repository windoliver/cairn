//! `cairn-docgen` — maintainer-time docs reference generator.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_cli::docgen::{RunMode, RunOpts, docgen_command, run};

fn main() -> ExitCode {
    let matches = docgen_command().get_matches();

    let workspace_root = matches
        .get_one::<PathBuf>("out")
        .cloned()
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("cairn-cli crate must have a parent (the `crates/` dir)")
                .parent()
                .expect("`crates/` must have a parent (the workspace root)")
                .to_path_buf()
        });

    let opts = RunOpts {
        workspace_root,
        mode: if matches.get_flag("check") {
            RunMode::Check
        } else {
            RunMode::Write
        },
        package_names: None,
    };

    match run(&opts) {
        Ok(report) if !report.drift.is_empty() => {
            eprintln!(
                "cairn-docgen: drift detected ({} file(s) differ from generated docs):",
                report.drift.len()
            );
            for (i, path) in report.drift.iter().enumerate() {
                if i >= 20 {
                    eprintln!("  ... and {} more", report.drift.len() - 20);
                    break;
                }
                eprintln!("  {}", path.display());
            }
            eprintln!(
                "Fix: run `cargo run -p cairn-cli --bin cairn-docgen -- --write` and commit the diff."
            );
            ExitCode::from(1)
        }
        Ok(report) => {
            if matches.get_flag("check") {
                eprintln!(
                    "cairn-docgen: clean - {} file(s) match.",
                    report.files_emitted
                );
            } else {
                eprintln!("cairn-docgen: wrote {} file(s).", report.files_emitted);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn-docgen: {e}");
            ExitCode::from(2)
        }
    }
}
