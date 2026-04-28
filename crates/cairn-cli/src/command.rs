//! Shared clap command tree for the runtime CLI and generated docs.

use crate::{generated, verbs};

/// Build the `cairn` command tree used by both `main.rs` and `cairn-docgen`.
#[must_use]
pub fn build_command() -> clap::Command {
    clap::Command::new("cairn")
        .about("Cairn — agent memory framework (cairn.mcp.v1)")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg_required_else_help(true)
        // Eight core verbs, each with --json added.
        .subcommand(verbs::with_json(generated::verbs::ingest_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::search_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::retrieve_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::summarize_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::assemble_hot_subcommand()))
        .subcommand(verbs::with_json(
            generated::verbs::capture_trace_subcommand(),
        ))
        .subcommand(verbs::with_json(generated::verbs::lint_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::forget_subcommand()))
        // Protocol preludes.
        .subcommand(verbs::with_json(generated::prelude::handshake_subcommand()))
        .subcommand(verbs::with_json(generated::prelude::status_subcommand()))
        // Management subcommand (plugins already has --json per sub-subcommand).
        .subcommand(plugins_subcommand())
        .subcommand(bootstrap_subcommand())
}

fn bootstrap_subcommand() -> clap::Command {
    clap::Command::new("bootstrap")
        .about("Write a default .cairn/config.yaml to a vault directory")
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .help("Vault root directory (default: current directory)"),
        )
}

fn plugins_subcommand() -> clap::Command {
    clap::Command::new("plugins")
        .about("Manage and inspect bundled plugins")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("list").about("List loaded plugins").arg(
                clap::Arg::new("json")
                    .long("json")
                    .action(clap::ArgAction::SetTrue)
                    .help("Emit JSON instead of a human-readable table"),
            ),
        )
        .subcommand(
            clap::Command::new("verify")
                .about("Run the conformance suite against every loaded plugin")
                .arg(
                    clap::Arg::new("strict")
                        .long("strict")
                        .action(clap::ArgAction::SetTrue)
                        .help("Treat tier-2 `pending` cases as failures"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON instead of a human-readable report"),
                ),
        )
}
