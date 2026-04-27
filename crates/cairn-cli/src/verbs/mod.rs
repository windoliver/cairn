//! Verb handler dispatch — one submodule per verb.

pub mod assemble_hot;
pub mod capture_trace;
pub mod envelope;
pub mod forget;
pub mod handshake;
pub mod ingest;
pub mod lint;
pub mod mcp_serve;
pub mod retrieve;
pub mod search;
pub mod status;
pub mod summarize;

/// Exported only for the smoke test; not part of the public API.
#[doc(hidden)]
pub fn smoke_fn() {}

/// Add `--json` flag to any generated subcommand without modifying generated files.
#[must_use]
pub fn with_json(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("json")
            .long("json")
            .action(clap::ArgAction::SetTrue)
            .help("Emit machine-readable JSON response envelope to stdout"),
    )
}
