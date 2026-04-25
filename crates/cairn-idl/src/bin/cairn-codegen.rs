//! `cairn-codegen` — maintainer-time binary that re-emits SDK / CLI / MCP /
//! skill artefacts from the IDL.
//!
//! Modes:
//!   - default: write outputs to the workspace root (parent of `CARGO_MANIFEST_DIR`).
//!   - --check: compare emitter outputs to on-disk; non-zero exit on drift.
//!   - --out  : custom workspace root (used by tests).

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_idl::codegen::{run, RunMode, RunOpts};

#[derive(clap::Parser, Debug)]
#[command(name = "cairn-codegen", about = "Cairn IDL → Rust + JSON codegen")]
struct Cli {
    /// Run in check mode — compare emitted bytes against on-disk; exit 1 on drift.
    #[arg(long)]
    check: bool,

    /// Workspace root (defaults to the parent of `CARGO_MANIFEST_DIR`).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    use clap::Parser;
    let cli = Cli::parse();

    let workspace_root = cli.out.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("cairn-idl crate must have a parent (the `crates/` dir)")
            .parent()
            .expect("`crates/` must have a parent (the workspace root)")
            .to_path_buf()
    });

    let opts = RunOpts {
        workspace_root,
        mode: if cli.check { RunMode::Check } else { RunMode::Write },
    };

    match run(&opts) {
        Ok(report) if !report.drift.is_empty() => {
            eprintln!(
                "cairn-codegen: drift detected ({} file(s) differ from on-disk):",
                report.drift.len()
            );
            for (i, p) in report.drift.iter().enumerate() {
                if i >= 20 {
                    eprintln!("  … and {} more", report.drift.len() - 20);
                    break;
                }
                eprintln!("  {}", p.display());
            }
            eprintln!("Fix: run `cargo run -p cairn-idl --bin cairn-codegen` and commit the diff.");
            ExitCode::from(1)
        }
        Ok(report) => {
            if cli.check {
                eprintln!("cairn-codegen: clean — {} file(s) match.", report.files_emitted);
            } else {
                eprintln!("cairn-codegen: wrote {} file(s).", report.files_emitted);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn-codegen: {e}");
            ExitCode::from(2)
        }
    }
}
