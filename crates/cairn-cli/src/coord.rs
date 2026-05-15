//! `cairn coord` command surface for the `cairn.coord.v1` extension.
//!
//! The command tree is hidden before the runtime is wired, and execution fails
//! closed with `CapabilityUnavailable` so status/capability negotiation remains
//! truthful.

use std::process::ExitCode;

use cairn_core::domain::{Identity, TargetId};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;

const COORD_CAPABILITY: &str = "cairn.mcp.v1.extension.coord";

/// Build the `cairn coord` subcommand tree.
#[must_use]
pub fn command() -> Command {
    Command::new("coord")
        .about("Coordinate multi-agent work over a shared vault")
        .hide(!(cairn_core::status::wiring::coord_extension_ready() && dispatch_ready()))
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new("json")
                .long("json")
                .action(ArgAction::SetTrue)
                .global(true)
                .help("Emit JSON output"),
        )
        .subcommand(lease_command())
        .subcommand(signal_command())
        .subcommand(action_command())
        .subcommand(routine_command())
        .subcommand(
            Command::new("frontier")
                .about("List recommended unblocked actions")
                .arg(actor_arg())
                .arg(
                    Arg::new("limit")
                        .long("limit")
                        .value_name("N")
                        .value_parser(clap::value_parser!(u32))
                        .default_value("5")
                        .help("Maximum number of actions to return"),
                ),
        )
        .subcommand(
            Command::new("next")
                .about("Return the single highest-priority unblocked action")
                .arg(actor_arg()),
        )
}

/// True when `cairn coord` has real command dispatch behind the parser.
#[must_use]
pub const fn dispatch_ready() -> bool {
    false
}

fn lease_command() -> Command {
    Command::new("lease")
        .about("Manage exclusive coordination leases")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("acquire")
                .about("Acquire an exclusive lease for an action")
                .arg(action_id_arg())
                .arg(
                    Arg::new("ttl")
                        .long("ttl")
                        .value_name("ISO8601_DURATION")
                        .value_parser(parse_iso8601_duration)
                        .required(true)
                        .help("Lease time-to-live, such as PT5M"),
                )
                .arg(
                    Arg::new("steal-after")
                        .long("steal-after")
                        .value_name("ISO8601_DURATION")
                        .value_parser(parse_iso8601_duration)
                        .help("Reclaim a stuck lease after this age"),
                ),
        )
        .subcommand(
            Command::new("release")
                .about("Release an action lease")
                .arg(action_id_arg()),
        )
        .subcommand(
            Command::new("list")
                .about("List active leases")
                .arg(actor_arg()),
        )
}

fn signal_command() -> Command {
    Command::new("signal")
        .about("Send and receive inter-agent coordination signals")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("send")
                .about("Append a signal for another actor")
                .arg(
                    Arg::new("to")
                        .long("to")
                        .value_name("ACTOR_ID")
                        .value_parser(parse_identity)
                        .required(true)
                        .help("Actor that should observe the signal"),
                )
                .arg(signal_kind_arg().required(true))
                .arg(
                    Arg::new("payload_id")
                        .long("payload-id")
                        .value_name("TARGET_ID")
                        .value_parser(parse_target_id)
                        .help("Payload record id to attach to the signal"),
                ),
        )
        .subcommand(
            Command::new("recv")
                .about("Read coordination signals")
                .arg(
                    Arg::new("cursor")
                        .long("cursor")
                        .value_name("TOKEN")
                        .help("Opaque signal resume token returned by the previous receive"),
                )
                .arg(signal_kind_arg()),
        )
}

fn action_command() -> Command {
    Command::new("action")
        .about("Manage coordination action DAG nodes")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("create")
                .about("Create an action")
                .arg(
                    Arg::new("title")
                        .long("title")
                        .value_name("TEXT")
                        .required(true)
                        .help("Action title"),
                )
                .arg(
                    Arg::new("depends-on")
                        .long("depends-on")
                        .value_name("ACTION_ID")
                        .value_parser(parse_target_id)
                        .action(ArgAction::Append)
                        .help("Action dependency; repeat for multiple dependencies"),
                )
                .arg(
                    Arg::new("priority")
                        .long("priority")
                        .value_name("INT")
                        .value_parser(clap::value_parser!(i32))
                        .default_value("0")
                        .help("Higher priority sorts earlier"),
                ),
        )
        .subcommand(
            Command::new("update")
                .about("Update an action status")
                .arg(action_id_arg())
                .arg(
                    Arg::new("status")
                        .long("status")
                        .value_name("STATUS")
                        .required(true)
                        .value_parser([
                            "pending",
                            "in_progress",
                            "completed",
                            "blocked",
                            "cancelled",
                        ])
                        .help("New action status"),
                ),
        )
        .subcommand(
            Command::new("graph")
                .about("Show the action dependency graph")
                .arg(
                    Arg::new("root")
                        .long("root")
                        .value_name("ACTION_ID")
                        .value_parser(parse_target_id)
                        .help("Root action id for a graph subset"),
                ),
        )
}

fn routine_command() -> Command {
    Command::new("routine")
        .about("Instantiate declarative coordination routines")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("instantiate")
                .about("Expand a routine template into actions")
                .arg(
                    Arg::new("routine_name")
                        .value_name("ROUTINE_NAME")
                        .required(true)
                        .help("Routine template name"),
                )
                .arg(
                    Arg::new("vars")
                        .long("vars")
                        .value_name("KEY=VALUE")
                        .action(ArgAction::Append)
                        .help("Template variable; repeat for multiple values"),
                ),
        )
}

fn action_id_arg() -> Arg {
    Arg::new("action_id")
        .value_name("ACTION_ID")
        .value_parser(parse_target_id)
        .required(true)
        .help("Coordination action id")
}

fn actor_arg() -> Arg {
    Arg::new("actor")
        .long("actor")
        .value_name("ACTOR_ID")
        .value_parser(parse_identity)
        .help("Filter by actor id")
}

fn parse_target_id(value: &str) -> Result<String, String> {
    TargetId::parse(value)
        .map(|_| value.to_owned())
        .map_err(|_| "expected canonical 26-character Crockford ULID target id".to_owned())
}

fn parse_identity(value: &str) -> Result<String, String> {
    Identity::parse(value)
        .map(|_| value.to_owned())
        .map_err(|err| format!("expected canonical Cairn identity: {err}"))
}

fn parse_iso8601_duration(value: &str) -> Result<String, String> {
    if value.starts_with('P') && value.len() > 1 {
        Ok(value.to_owned())
    } else {
        Err("expected ISO-8601 duration such as PT5M".to_owned())
    }
}

fn signal_kind_arg() -> Arg {
    Arg::new("kind")
        .long("kind")
        .value_name("KIND")
        .value_parser([
            "task_completed",
            "lease_released",
            "request_review",
            "user_input_needed",
            "error",
            "info",
        ])
        .help("Signal kind")
}

/// Execute a parsed `coord` command.
#[must_use]
pub fn run(matches: &ArgMatches) -> ExitCode {
    if dispatch_ready() {
        eprintln!("cairn coord: internal error: coord dispatch marked ready without a handler");
        return ExitCode::from(70);
    }
    let remediation = cairn_core::status::remediation_for(COORD_CAPABILITY);
    if matches.get_flag("json") {
        let error = CapabilityError {
            code: "CapabilityUnavailable",
            capability: COORD_CAPABILITY,
            remediation,
        };
        println!(
            "{}",
            serde_json::to_string(&error).expect("coord capability error serializes")
        );
    } else {
        eprintln!("cairn coord: CapabilityUnavailable: {COORD_CAPABILITY} is not advertised");
        if let Some(remediation) = remediation {
            eprintln!("hint: {remediation}");
        }
    }
    ExitCode::from(69)
}

#[derive(Serialize)]
struct CapabilityError<'a> {
    code: &'static str,
    capability: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    remediation: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_acquire_parses() {
        let matches = command()
            .try_get_matches_from([
                "coord",
                "lease",
                "acquire",
                "01HQZK000000000000000ACTN1",
                "--ttl",
                "PT5M",
            ])
            .expect("lease acquire parses");
        let (_, lease) = matches.subcommand().expect("lease subcommand");
        let (_, acquire) = lease.subcommand().expect("acquire subcommand");
        assert_eq!(
            acquire.get_one::<String>("action_id").map(String::as_str),
            Some("01HQZK000000000000000ACTN1")
        );
        assert_eq!(
            acquire.get_one::<String>("ttl").map(String::as_str),
            Some("PT5M")
        );
    }

    #[test]
    fn action_update_rejects_unknown_status() {
        let err = command()
            .try_get_matches_from([
                "coord",
                "action",
                "update",
                "01HQZK000000000000000ACTN1",
                "--status",
                "started",
            ])
            .expect_err("unknown action status must be rejected by clap");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn signal_send_parses_closed_kind() {
        let matches = command()
            .try_get_matches_from([
                "coord",
                "signal",
                "send",
                "--to",
                "agt:codex:worker:v1",
                "--kind",
                "request_review",
            ])
            .expect("signal send parses");
        let (_, signal) = matches.subcommand().expect("signal subcommand");
        let (_, send) = signal.subcommand().expect("send subcommand");
        assert_eq!(
            send.get_one::<String>("kind").map(String::as_str),
            Some("request_review")
        );
    }

    #[test]
    fn all_coord_primitives_are_parseable() {
        let cases: &[&[&str]] = &[
            &[
                "coord",
                "lease",
                "acquire",
                "01HQZK000000000000000ACTN1",
                "--ttl",
                "PT5M",
                "--steal-after",
                "PT30M",
            ],
            &["coord", "lease", "release", "01HQZK000000000000000ACTN1"],
            &["coord", "lease", "list", "--actor", "agt:codex:worker:v1"],
            &[
                "coord",
                "signal",
                "send",
                "--to",
                "agt:codex:worker:v1",
                "--kind",
                "info",
                "--payload-id",
                "01HQZK000000000000000PAY01",
            ],
            &[
                "coord",
                "signal",
                "recv",
                "--cursor",
                "sig:17",
                "--kind",
                "task_completed",
            ],
            &[
                "coord",
                "action",
                "create",
                "--title",
                "Review issue 314",
                "--depends-on",
                "01HQZK000000000000000ACTN1",
                "--depends-on",
                "01HQZK000000000000000ACTN2",
                "--priority",
                "10",
            ],
            &[
                "coord",
                "action",
                "update",
                "01HQZK000000000000000ACTN3",
                "--status",
                "completed",
            ],
            &[
                "coord",
                "action",
                "graph",
                "--root",
                "01HQZK000000000000000ACTN1",
            ],
            &[
                "coord",
                "routine",
                "instantiate",
                "code-review",
                "--vars",
                "pr=314",
            ],
            &[
                "coord",
                "frontier",
                "--actor",
                "agt:codex:worker:v1",
                "--limit",
                "5",
            ],
            &["coord", "next", "--actor", "agt:codex:worker:v1"],
        ];

        for case in cases {
            command()
                .try_get_matches_from(*case)
                .unwrap_or_else(|err| panic!("coord command should parse {case:?}: {err}"));
        }
    }
}
