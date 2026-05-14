//! Shared clap command tree for the runtime CLI and generated docs.

use crate::{doctor, generated, hooks, identity, skill, verbs};

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
            verbs::with_flush_modes(verbs::with_ingest_runtime_overrides(
                generated::verbs::ingest_subcommand(),
            )),
        )))
        .subcommand(verbs::with_json(verbs::with_search_scope(
            generated::verbs::search_subcommand(),
        )))
        .subcommand(verbs::with_json(generated::verbs::retrieve_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::summarize_subcommand()))
        .subcommand(verbs::with_json(generated::verbs::assemble_hot_subcommand()))
        .subcommand(verbs::with_json(
            generated::verbs::capture_trace_subcommand(),
        ))
        .subcommand(verbs::with_json(verbs::with_fix_markdown(
            verbs::with_fix_folders(verbs::with_lint_plan(generated::verbs::lint_subcommand())),
        )))
        .subcommand(verbs::with_json(verbs::with_flush_modes(
            generated::verbs::forget_subcommand(),
        )))
        .subcommand(hooks::command())
        // Protocol preludes.
        .subcommand(verbs::with_json(with_issuer(
            generated::prelude::handshake_subcommand(),
        )))
        .subcommand(verbs::with_json(generated::prelude::status_subcommand()))
        // Management subcommand (plugins already has --json per sub-subcommand).
        .subcommand(plugins_subcommand())
        .subcommand(bootstrap_subcommand())
        .subcommand(doctor_subcommand())
        .subcommand(mcp_subcommand())
        .subcommand(vault_subcommand())
        .subcommand(skill_subcommand())
        .subcommand(admin_subcommand())
        .subcommand(llm_subcommand())
        .subcommand(verbs::screen::command())
        .subcommand(repair_subcommand())
        .subcommand(identity::cli::identity_subcommand())
        .subcommand(verbs::flush::command())
}

/// Attach `--issuer` to the generated handshake subcommand. The IDL
/// schema describes only the response shape; the request body is
/// CLI-shaped and the issuer is needed so the minted challenge binds
/// to a row in `outstanding_challenges` (brief §4.2). Optional — when
/// absent the CLI keeps its pre-#52 ephemeral behaviour and prints a
/// warning explaining the challenge will not be redeemable.
fn with_issuer(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("issuer")
            .long("issuer")
            .value_name("IDENTITY")
            .help(
                "Issuer identity to bind the minted challenge to (e.g. `hmn:alice` or \
                 `agt:claude-code:opus-4-7:reviewer:v1`). When omitted the challenge is \
                 generated but not persisted — a warning is emitted and the resulting nonce \
                 cannot be redeemed by a signed envelope.",
            ),
    )
}

fn repair_subcommand() -> clap::Command {
    clap::Command::new("repair")
        .about("Operator repair commands for blocked vault state")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("consent-journal")
                .about("List or delete legacy consent_journal rows that block migration 0021")
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                )
                .arg(
                    clap::Arg::new("delete-rowid")
                        .long("delete-rowid")
                        .value_name("ROWID")
                        .value_parser(clap::value_parser!(i64))
                        .requires("reason")
                        .requires("yes")
                        .help("Delete one repair-eligible consent_journal rowid"),
                )
                .arg(
                    clap::Arg::new("reason")
                        .long("reason")
                        .value_name("TEXT")
                        .value_parser(clap::builder::NonEmptyStringValueParser::new())
                        .requires("delete-rowid")
                        .help("Operator reason for a delete repair"),
                )
                .arg(
                    clap::Arg::new("yes")
                        .long("yes")
                        .action(clap::ArgAction::SetTrue)
                        .requires("delete-rowid")
                        .help("Confirm the requested delete repair"),
                ),
        )
}

fn llm_subcommand() -> clap::Command {
    clap::Command::new("llm")
        .about("LLM provider diagnostics (ADR 0001)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("probe")
                .about(
                    "Build the configured LLMProvider and issue one completion. \
                     Exits 78 (EX_CONFIG) when no provider is configured, \
                     69 (EX_UNAVAILABLE) when the endpoint is unreachable or lacks \
                     a required capability, 77 (EX_NOPERM) on auth failure.",
                )
                .arg(
                    clap::Arg::new("prompt")
                        .long("prompt")
                        .value_name("TEXT")
                        .help("Prompt to send (default: \"Reply with the single word: ping\")"),
                )
                .arg(
                    clap::Arg::new("schema-file")
                        .long("schema-file")
                        .value_name("PATH")
                        .help(
                            "Path to a JSON Schema file. When set, the probe \
                             requests JSON output and validates the response \
                             against the schema (exits 1 on validation failure).",
                        ),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt instead of human-readable output"),
                ),
        )
}

fn doctor_subcommand() -> clap::Command {
    clap::Command::new("doctor")
        .about("Reference-consumer diagnostics")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("claude-code")
                .about(
                    "Verify Claude Code can discover the configured Cairn MCP server, start it, \
                     call `status`, and find the five expected hook entries without mutating config.",
                )
                .arg(
                    clap::Arg::new("project-dir")
                        .long("project-dir")
                        .value_name("PATH")
                        .help("Project directory used for .mcp.json and .claude/settings*.json lookups"),
                )
                .arg(
                    clap::Arg::new("home-dir")
                        .long("home-dir")
                        .value_name("PATH")
                        .help("Override the user home directory for ~/.claude.json and ~/.claude/settings.json"),
                )
                .arg(
                    clap::Arg::new("server-name")
                        .long("server-name")
                        .value_name("NAME")
                        .default_value(doctor::DEFAULT_SERVER_NAME)
                        .help("Claude Code MCP server name to verify"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt instead of human-readable output"),
                ),
        )
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

fn admin_subcommand() -> clap::Command {
    clap::Command::new("admin")
        .about("Administrative operations (model management, reindex, backup substrate)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("model")
                .about("Embedding model management")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    clap::Command::new("fetch")
                        .about(
                            "Download the configured embedding model (~25 MB) from HuggingFace Hub",
                        )
                        .arg(
                            clap::Arg::new("force")
                                .long("force")
                                .action(clap::ArgAction::SetTrue)
                                .help("Re-fetch even if model files are already on disk"),
                        )
                        .arg(
                            clap::Arg::new("json")
                                .long("json")
                                .action(clap::ArgAction::SetTrue)
                                .help("Emit JSON output"),
                        ),
                ),
        )
        .subcommand(
            clap::Command::new("reindex")
                .about("Re-embed records into the ANN index")
                .arg(
                    clap::Arg::new("semantic")
                        .long("semantic")
                        .action(clap::ArgAction::SetTrue)
                        .help("Reindex semantic (ANN) vectors"),
                )
                .arg(
                    clap::Arg::new("all")
                        .long("all")
                        .action(clap::ArgAction::SetTrue)
                        .help("Enqueue ALL active records before draining (use after model swap)"),
                )
                .arg(
                    clap::Arg::new("from-db")
                        .long("from-db")
                        .action(clap::ArgAction::SetTrue)
                        .help(
                            "Rebuild FTS5 + vector indexes from the authoritative records table \
                             (use after derived indexes are deleted or corrupted).",
                        ),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("snapshot")
                .about("Prepare a vault backup snapshot")
                .arg(
                    clap::Arg::new("backup")
                        .long("backup")
                        .required(true)
                        .value_name("PATH")
                        .help("Destination path for the prepared backup"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON output"),
                ),
        )
        .subcommand(
            clap::Command::new("restore")
                .about("Prepare a restore operation from a backup")
                .arg(
                    clap::Arg::new("from")
                        .long("from")
                        .required(true)
                        .value_name("PATH")
                        .help("Source backup path to restore from"),
                )
                .arg(
                    clap::Arg::new("into")
                        .long("into")
                        .required(true)
                        .value_name("PATH")
                        .help("Destination vault path to restore into"),
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
