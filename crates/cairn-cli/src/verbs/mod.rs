//! Verb handler dispatch — one submodule per verb.

pub mod assemble_hot;
pub mod capture_trace;
pub mod envelope;
pub mod forget;
pub mod handshake;
pub mod ingest;
pub mod lint;
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

/// Add `--fix-markdown` flag to the `lint` subcommand.
///
/// Augments the generated subcommand builder without touching generated files,
/// using the same pattern as `with_json`.
#[must_use]
pub fn with_fix_markdown(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("fix-markdown")
            .long("fix-markdown")
            .action(clap::ArgAction::SetTrue)
            .help("Regenerate missing or stale markdown projections for all active records"),
    )
}

/// Add `--fix-folders` flag to the `lint` subcommand.
///
/// Augments the generated subcommand builder without touching generated
/// files, using the same pattern as [`with_fix_markdown`].
#[must_use]
pub fn with_fix_folders(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("fix-folders")
            .long("fix-folders")
            .action(clap::ArgAction::SetTrue)
            .help(
                "Regenerate folder _index.md sidecars and backlinks for every \
                 non-empty folder (brief §3.4, #44)",
            ),
    )
}

/// Augments the `ingest` subcommand with the `--resync <path>` flag.
///
/// Uses the same pattern as [`with_json`] and [`with_fix_markdown`]: the
/// generated subcommand builder is wrapped rather than modified.
#[must_use]
pub fn with_resync(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("resync")
            .long("resync")
            .value_name("PATH")
            .help("Re-ingest an out-of-band edited markdown projection (brief §3.0, #43)")
            .action(clap::ArgAction::Set)
            .value_parser(clap::value_parser!(std::path::PathBuf)),
    )
}
