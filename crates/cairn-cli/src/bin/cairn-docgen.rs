//! `cairn-docgen` — maintainer-time docs reference generator.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_cli::docgen::{RunMode, RunOpts, run};

#[derive(clap::Parser, Debug)]
#[command(
    name = "cairn-docgen",
    about = "Generate Cairn docs reference Markdown"
)]
struct Cli {
    /// Run in check mode — compare emitted docs against on-disk files.
    #[arg(long, conflicts_with = "write")]
    check: bool,

    /// Write generated docs. This is the default when neither flag is set.
    #[arg(long)]
    write: bool,

    /// Workspace root (defaults to the parent of `CARGO_MANIFEST_DIR`).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    use clap::Parser as _;
    let cli = Cli::parse();

    let workspace_root = cli.out.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("cairn-cli crate must have a parent (the `crates/` dir)")
            .parent()
            .expect("`crates/` must have a parent (the workspace root)")
            .to_path_buf()
    });

    let opts = RunOpts {
        workspace_root,
        mode: if cli.check {
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
            if cli.check {
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
