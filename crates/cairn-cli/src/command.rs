//! Shared clap command tree for the runtime CLI and generated docs.

use crate::{generated, skill, verbs};

/// Build the `cairn` command tree used by both `main.rs` and `cairn-docgen`.
#[must_use]
pub fn build_command() -> clap::Command {
    clap::Command::new("cairn")
        .about("Cairn — agent memory framework (cairn.mcp.v1)")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            clap::Arg::new("vault")
                .long("vault")
                .value_name("NAME_OR_PATH")
                .global(true)
                .help(
                    "Active vault: name from registry or filesystem path (overrides CAIRN_VAULT)",
                ),
        )
        // Eight core verbs, each with --json added.
        .subcommand(verbs::with_json(verbs::with_resync(
            generated::verbs::ingest_subcommand(),
        )))
        .subcommand(verbs::with_json(generated::verbs::search_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::retrieve_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::summarize_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::assemble_hot_subcommand()))
        .subcommand(verbs::with_json(
            generated::verbs::capture_trace_subcommand(),
        ))
        .subcommand(verbs::with_json(verbs::with_fix_markdown(
            verbs::with_fix_folders(generated::verbs::lint_subcommand()),
        )))
        .subcommand(verbs::with_json(generated::verbs::forget_subcommand()))
        // Protocol preludes.
        .subcommand(verbs::with_json(generated::prelude::handshake_subcommand()))
        .subcommand(verbs::with_json(generated::prelude::status_subcommand()))
        // Management subcommand (plugins already has --json per sub-subcommand).
        .subcommand(plugins_subcommand())
        .subcommand(bootstrap_subcommand())
        .subcommand(mcp_subcommand())
        .subcommand(vault_subcommand())
        .subcommand(skill_subcommand())
}

fn bootstrap_subcommand() -> clap::Command {
    clap::Command::new("bootstrap")
        .about("Initialize a vault directory tree with the §3 layout")
        .arg(
            clap::Arg::new("vault-path")
                .long("vault-path")
                .default_value(".")
                .value_name("PATH")
                .help("Vault root directory (default: current directory)"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit JSON receipt instead of human-readable output"),
        )
        .arg(
            clap::Arg::new("force")
                .long("force")
                .action(clap::ArgAction::SetTrue)
                .help("Overwrite existing placeholder files"),
        )
}

fn mcp_subcommand() -> clap::Command {
    clap::Command::new("mcp").about(
        "Start an MCP stdio server. Reads MCP frames from stdin, \
             dispatches to the eight cairn verbs, writes responses to \
             stdout. Blocks until stdin closes.",
    )
}

fn skill_subcommand() -> clap::Command {
    clap::Command::new("skill")
        .about("Manage the Cairn skill bundle")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("install")
                .about("Install the Cairn skill bundle into the harness skill directory (§18.d)")
                .arg(
                    clap::Arg::new("harness")
                        .long("harness")
                        .required(true)
                        .value_name("HARNESS")
                        .value_parser(clap::builder::EnumValueParser::<skill::Harness>::new())
                        .help(
                            "Target harness (claude-code, codex, gemini, opencode, cursor, custom)",
                        ),
                )
                .arg(
                    clap::Arg::new("target-dir")
                        .long("target-dir")
                        .value_name("PATH")
                        .help("Override the default install path (~/.cairn/skills/cairn/)"),
                )
                .arg(
                    clap::Arg::new("force")
                        .long("force")
                        .action(clap::ArgAction::SetTrue)
                        .help("Overwrite generated files even if the version matches"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt instead of human-readable output"),
                ),
        )
}

fn vault_subcommand() -> clap::Command {
    clap::Command::new("vault")
        .about("Manage the vault registry (brief §3.3)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("add")
                .about("Register a vault in the registry")
                .arg(
                    clap::Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .help("Filesystem path to the vault root"),
                )
                .arg(
                    clap::Arg::new("name")
                        .long("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Short identifier for the vault"),
                )
                .arg(
                    clap::Arg::new("label")
                        .long("label")
                        .value_name("LABEL")
                        .help("Human-readable description"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("list")
                .about("List registered vaults")
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("switch")
                .about("Set the default vault")
                .arg(
                    clap::Arg::new("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Name of the vault to make default"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("remove")
                .about("Remove a vault from the registry (does not delete files)")
                .arg(
                    clap::Arg::new("name")
                        .value_name("NAME")
                        .required(true)
                        .help("Name of the vault to remove"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
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
