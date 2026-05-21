//! Public `cairn bench` wrapper.
//!
//! The benchmark harness lives in the separate `cairn-bench` binary so it can
//! carry evaluation-only dependencies without bloating the runtime CLI. This
//! module exposes the public command requested by the release issue and
//! delegates argv to the sibling harness executable.

use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Arg, ArgAction, ArgMatches, Command};

/// Build the public benchmark wrapper subcommand.
#[must_use]
pub fn command() -> Command {
    Command::new("bench")
        .about("Run Cairn benchmark scorecards and release gates via cairn-bench")
        .long_about(
            "Run Cairn benchmark scorecards and release gates via the separate \
             `cairn-bench` harness binary. Arguments after `bench` are passed \
             through unchanged, for example: `cairn bench scorecard --skip-openai`. \
             Set CAIRN_BENCH_BIN to override the harness binary path.",
        )
        .arg_required_else_help(true)
        .arg(
            Arg::new("args")
                .help("Arguments forwarded to cairn-bench")
                .num_args(1..)
                .allow_hyphen_values(true)
                .trailing_var_arg(true)
                .action(ArgAction::Append),
        )
}

/// Delegate `cairn bench ...` to `cairn-bench ...`.
pub fn run(matches: &ArgMatches) -> ExitCode {
    let args: Vec<&str> = matches
        .get_many::<String>("args")
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let bench_bin = bench_binary();
    let status = ProcessCommand::new(&bench_bin).args(args).status();
    match status {
        Ok(status) => status
            .code()
            .map_or_else(|| ExitCode::from(1), exit_code_from_process_code),
        Err(err) => {
            eprintln!(
                "cairn bench: failed to execute `{}`: {err}",
                bench_bin.to_string_lossy()
            );
            ExitCode::from(69)
        }
    }
}

fn exit_code_from_process_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or_else(|_| ExitCode::from(1), ExitCode::from)
}

fn bench_binary() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CAIRN_BENCH_BIN") {
        return std::path::PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(format!("cairn-bench{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return candidate;
        }
    }
    std::path::PathBuf::from(format!("cairn-bench{}", std::env::consts::EXE_SUFFIX))
}
