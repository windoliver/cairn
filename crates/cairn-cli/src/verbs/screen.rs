//! Screen sensor diagnostics and capture commands.

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use cairn_core::domain::{CaptureEvent, CaptureEventId};
use cairn_sensors_local::screen::{
    ScreenDegradationCode, ScreenError, ScreenEventObservation, ScreenObservation,
    capture_png_snapshot_configured,
};
use cairn_sensors_local::{EmitOutcome, LocalSensorConfig, SensorSettings};
use clap::{Arg, ArgAction, ArgMatches};

use super::envelope::{emit_json, new_operation_id};

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
    match capture_png_snapshot_configured(&config.sensors.screen, output_path) {
        Ok(Some(receipt)) => {
            let capture_event = match capture_event_for_observation(receipt.observation.clone()) {
                Ok(event) => event,
                Err(err) => {
                    eprintln!("cairn screen capture: failed to build capture event: {err}");
                    return ExitCode::from(70);
                }
            };
            if json {
                emit_json(&serde_json::json!({
                    "status": "captured",
                    "output_path": receipt.output_path,
                    "width": receipt.width,
                    "height": receipt.height,
                    "backend": format!("{:?}", receipt.observation.backend).to_ascii_lowercase(),
                    "ocr_engine": format!("{:?}", receipt.observation.ocr_engine).to_ascii_lowercase(),
                    "sensor_label": receipt.observation.sensor_label,
                    "captured_at": receipt.observation.captured_at,
                    "app": receipt.observation.app,
                    "window_title": receipt.observation.window_title,
                    "url": receipt.observation.url,
                    "ocr_text_bytes": receipt.observation.text.len(),
                    "bounding_boxes_count": receipt.observation.bounding_boxes.len(),
                    "capture_event": capture_event,
                }));
            } else {
                println!(
                    "captured {} ({}x{}, app={}, ocr_bytes={})",
                    receipt.output_path.display(),
                    receipt.width,
                    receipt.height,
                    receipt.observation.app,
                    receipt.observation.text.len()
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

fn capture_event_for_observation(observation: ScreenObservation) -> Result<CaptureEvent, String> {
    let event_id = CaptureEventId::parse(new_operation_id().0).map_err(|err| err.to_string())?;
    let event_observation = ScreenEventObservation::from_observation(event_id, observation, None)
        .map_err(|err| err.to_string())?;
    let mut local_config = LocalSensorConfig::all_disabled();
    local_config.screen = SensorSettings::enabled();

    match cairn_sensors_local::screen::emit(&local_config, event_observation) {
        EmitOutcome::Emitted(event) => Ok(event),
        EmitOutcome::Dropped { reason, .. } => Err(format!("{reason:?}")),
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
