//! Screen sensor diagnostics and capture commands.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_sensors_local::screen::{
    NoopScreenPolicy, ScreenDegradationCode, ScreenError, ScreenSensor, XcapBackendRuntime,
};
use clap::{Arg, ArgAction, ArgMatches};

use super::envelope::emit_json;

/// Build the `cairn screen` subcommand.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("screen")
        .about("Screen sensor diagnostics")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("capture")
                .about("Capture one screenshot through the configured screen backend")
                .arg(
                    Arg::new("output")
                        .long("output")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(clap::value_parser!(PathBuf))
                        .help("PNG file path to write"),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .action(ArgAction::SetTrue)
                        .help("Emit JSON receipt instead of human-readable output"),
                ),
        )
}

/// Run a `cairn screen` subcommand.
#[must_use]
pub fn run(sub: &ArgMatches, config: &CairnConfig) -> ExitCode {
    match sub.subcommand() {
        Some(("capture", capture)) => run_capture(capture, config),
        _ => ExitCode::from(64),
    }
}

fn run_capture(sub: &ArgMatches, config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let output_path = sub
        .get_one::<PathBuf>("output")
        .expect("clap requires --output");
    let sensor = ScreenSensor::new(XcapBackendRuntime, NoopScreenPolicy);

    match sensor.capture_png_snapshot(&config.sensors.screen, output_path) {
        Ok(Some(receipt)) => {
            if json {
                emit_json(&serde_json::json!({
                    "status": "captured",
                    "output_path": receipt.output_path,
                    "width": receipt.width,
                    "height": receipt.height,
                    "backend": "xcap",
                    "ocr_engine": format!("{:?}", receipt.observation.ocr_engine).to_ascii_lowercase(),
                    "sensor_label": receipt.observation.sensor_label,
                    "captured_at": receipt.observation.captured_at,
                }));
            } else {
                println!(
                    "captured {} ({}x{})",
                    receipt.output_path.display(),
                    receipt.width,
                    receipt.height
                );
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            if config.sensors.screen.enabled {
                eprintln!("cairn screen capture: skipped by screen allow_apps policy");
            } else {
                eprintln!("cairn screen capture: screen sensor is disabled in config");
            }
            ExitCode::from(78)
        }
        Err(err) => {
            eprintln!("cairn screen capture: {err}");
            exit_code_for_screen_error(&err)
        }
    }
}

fn exit_code_for_screen_error(err: &ScreenError) -> ExitCode {
    match err.code() {
        ScreenDegradationCode::PermissionMissing => ExitCode::from(77),
        ScreenDegradationCode::Disabled => ExitCode::from(78),
        ScreenDegradationCode::BackendUnavailable | ScreenDegradationCode::Degraded => {
            ExitCode::from(69)
        }
    }
}
