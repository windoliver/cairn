//! Screen sensor diagnostics and capture commands.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use cairn_core::config::CairnConfig;
use cairn_core::domain::{
    BudgetObservation, CaptureEvent, CaptureEventId, LocalSensorName, SensorGateReason,
    SourceFamily, metrics::MetricEvent,
};
use cairn_sensors_local::screen::{
    ScreenCaptureOutcome, ScreenCaptureReceipt, ScreenCaptureSkip, ScreenDegradationCode,
    ScreenError, ScreenEventObservation, ScreenObservation,
    capture_png_snapshot_outcome_configured, screen_observation_budgeted_payload_bytes,
    screen_observation_observed_bytes,
};
use cairn_sensors_local::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings};
use clap::{Arg, ArgAction, ArgMatches};

use crate::sensor_gate::{
    SensorDropMetric, SensorGateStage, append_sensor_drop_metric, latest_sensor_consent_for_vault,
};

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
pub fn run(sub: &ArgMatches, vault_root: &Path, config: &CairnConfig) -> ExitCode {
    match sub.subcommand() {
        Some(("capture", capture)) => run_capture(capture, vault_root, config),
        _ => ExitCode::from(64),
    }
}

fn run_capture(sub: &ArgMatches, vault_root: &Path, config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let output_path = sub
        .get_one::<PathBuf>("output")
        .expect("clap requires --output");
    if let Err(reason) = enforce_screen_sensor_gate(vault_root, config) {
        if json {
            emit_json(&serde_json::json!({
                "status": "dropped",
                "sensor": "screen",
                "reason": reason.as_str(),
            }));
        } else {
            eprintln!(
                "cairn screen capture: screen sensor denied capture: {}",
                reason.as_str()
            );
        }
        return exit_code_for_gate_reason(reason);
    }
    let started = Instant::now();
    match capture_png_snapshot_outcome_configured(&config.sensors.screen, output_path) {
        Ok(ScreenCaptureOutcome::Captured(receipt)) => {
            emit_captured_receipt(vault_root, config, &receipt, started, json)
        }
        Ok(ScreenCaptureOutcome::Skipped(skip)) => {
            emit_skipped_capture(vault_root, config, skip, started, json)
        }
        Ok(ScreenCaptureOutcome::CleanupFailed { skip, error }) => {
            emit_cleanup_failed_capture(vault_root, config, skip, &error, started, json)
        }
        Err(err) => emit_failed_capture(vault_root, config, &err, started, json),
    }
}

fn emit_captured_receipt(
    vault_root: &Path,
    config: &CairnConfig,
    receipt: &ScreenCaptureReceipt,
    started: Instant,
    json: bool,
) -> ExitCode {
    let capture_event = match capture_event_for_observation(
        vault_root,
        config,
        receipt.observation.clone(),
        started,
    ) {
        Ok(event) => event,
        Err(err) => {
            if let Err(cleanup_err) = remove_screen_capture_artifact(&receipt.output_path) {
                emit_artifact_cleanup_failed_metric(
                    vault_root,
                    config,
                    screen_observation_observed_bytes(&receipt.observation),
                    &cleanup_err,
                    started,
                );
                if json {
                    emit_json(&post_capture_cleanup_failed_payload(&cleanup_err));
                } else {
                    eprintln!(
                        "cairn screen capture: failed to build capture event: {err}; failed to remove capture artifact: {cleanup_err}"
                    );
                }
                return exit_code_for_screen_error(&cleanup_err);
            }
            return emit_post_capture_drop(&err, json);
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

fn emit_post_capture_drop(err: &CaptureEventBuildError, json: bool) -> ExitCode {
    let (reason, error, code) = match err {
        CaptureEventBuildError::Dropped(DropReason::PolicyRejected(_)) => {
            ("policy_rejected", "policy_rejected", ExitCode::from(78))
        }
        CaptureEventBuildError::Dropped(DropReason::BudgetExceeded) => {
            ("budget_exceeded", "budget_exceeded", ExitCode::from(77))
        }
        CaptureEventBuildError::Dropped(DropReason::Disabled) => {
            ("disabled", "disabled", ExitCode::from(78))
        }
        CaptureEventBuildError::Dropped(DropReason::MalformedObservation(_))
        | CaptureEventBuildError::BuildFailed(_) => (
            "post_capture_validation_failed",
            "validation_failed",
            ExitCode::from(70),
        ),
    };
    if json {
        emit_json(&post_capture_drop_payload(reason, error));
    } else {
        eprintln!("cairn screen capture: failed to build capture event: {error}");
    }
    code
}

fn post_capture_drop_payload(reason: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "status": "dropped",
        "sensor": "screen",
        "reason": reason,
        "error": error,
        "artifact_created": true,
        "artifact_removed": true,
    })
}

fn emit_skipped_capture(
    vault_root: &Path,
    config: &CairnConfig,
    skip: ScreenCaptureSkip,
    started: Instant,
    json: bool,
) -> ExitCode {
    let outcome = EmitOutcome::Dropped {
        sensor: SensorKind::Screen,
        reason: skip.reason.drop_reason(),
    };
    emit_screen_sensor_outcome_metric(
        vault_root,
        config,
        &outcome,
        started,
        skip.observed_bytes,
        Some(screen_text_budget_bytes(config)),
    );
    if json {
        emit_json(&serde_json::json!({
            "status": "dropped",
            "sensor": "screen",
            "reason": skip.reason.as_str(),
        }));
    } else {
        eprintln!("cairn screen capture: {}", skip.reason.message());
    }
    ExitCode::from(78)
}

fn emit_failed_capture(
    vault_root: &Path,
    config: &CairnConfig,
    err: &ScreenError,
    started: Instant,
    json: bool,
) -> ExitCode {
    emit_screen_sensor_metric(
        vault_root,
        config,
        ScreenSensorMetric {
            status: "dropped",
            error: Some(screen_error_metric_class(err).to_owned()),
            degradation_state: Some(screen_error_degradation_state(err).to_owned()),
            started,
            observed_bytes: 0,
            budget_bytes: Some(screen_text_budget_bytes(config)),
        },
    );
    if json {
        emit_json(&screen_error_payload(err));
    } else {
        eprintln!("cairn screen capture: {err}");
    }
    exit_code_for_screen_error(err)
}

fn emit_cleanup_failed_capture(
    vault_root: &Path,
    config: &CairnConfig,
    skip: ScreenCaptureSkip,
    err: &ScreenError,
    started: Instant,
    json: bool,
) -> ExitCode {
    emit_screen_sensor_metric(
        vault_root,
        config,
        ScreenSensorMetric {
            status: "dropped",
            error: Some("artifact_cleanup_failed".to_owned()),
            degradation_state: Some("cleanup_failed".to_owned()),
            started,
            observed_bytes: skip.observed_bytes,
            budget_bytes: Some(screen_text_budget_bytes(config)),
        },
    );
    if json {
        emit_json(&cleanup_failed_payload(skip, err));
    } else {
        eprintln!(
            "cairn screen capture: {} but failed to remove capture artifact: {err}",
            skip.reason.message()
        );
    }
    exit_code_for_screen_error(err)
}

fn emit_artifact_cleanup_failed_metric(
    vault_root: &Path,
    config: &CairnConfig,
    observed_bytes: u64,
    _err: &ScreenError,
    started: Instant,
) {
    emit_screen_sensor_metric(
        vault_root,
        config,
        ScreenSensorMetric {
            status: "dropped",
            error: Some("artifact_cleanup_failed".to_owned()),
            degradation_state: Some("cleanup_failed".to_owned()),
            started,
            observed_bytes,
            budget_bytes: Some(screen_text_budget_bytes(config)),
        },
    );
}

fn cleanup_failed_payload(skip: ScreenCaptureSkip, err: &ScreenError) -> serde_json::Value {
    serde_json::json!({
        "status": "dropped",
        "sensor": "screen",
        "reason": "artifact_cleanup_failed",
        "skip_reason": skip.reason.as_str(),
        "artifact_created": skip.artifact_created,
        "artifact_removed": false,
        "observed_bytes": skip.observed_bytes,
        "error": screen_error_metric_class(err),
        "degradation_state": "cleanup_failed",
    })
}

fn post_capture_cleanup_failed_payload(err: &ScreenError) -> serde_json::Value {
    serde_json::json!({
        "status": "dropped",
        "sensor": "screen",
        "reason": "artifact_cleanup_failed",
        "skip_reason": "post_capture_validation_failed",
        "artifact_created": true,
        "artifact_removed": false,
        "error": screen_error_metric_class(err),
        "degradation_state": "cleanup_failed",
    })
}

fn screen_error_payload(err: &ScreenError) -> serde_json::Value {
    serde_json::json!({
        "status": "error",
        "sensor": "screen",
        "reason": screen_error_metric_class(err),
        "error": screen_error_metric_class(err),
        "degradation_state": screen_error_degradation_state(err),
    })
}

fn remove_screen_capture_artifact(output_path: &Path) -> Result<(), ScreenError> {
    if !output_path.exists() {
        return Ok(());
    }
    std::fs::remove_file(output_path).map_err(|err| {
        ScreenError::CaptureFailed(format!(
            "failed to remove dropped screen capture {}: {err}",
            output_path.display()
        ))
    })
}

fn enforce_screen_sensor_gate(
    vault_root: &Path,
    config: &CairnConfig,
) -> Result<(), SensorGateReason> {
    if !vault_root.join(".cairn").join("vault.id").exists() {
        return Ok(());
    }
    let observation = BudgetObservation { items: 1, bytes: 0 };
    let consent = match block_on(latest_sensor_consent_for_vault(
        vault_root,
        LocalSensorName::Screen,
    )) {
        Ok(consent) => consent,
        Err(error) => {
            eprintln!("cairn screen capture: failed to load screen sensor consent: {error:#}");
            return Err(SensorGateReason::PrivacyDenied);
        }
    };
    match crate::sensor_gate::evaluate_sensor_gate(
        config,
        consent,
        LocalSensorName::Screen,
        observation,
    ) {
        Ok(()) => Ok(()),
        Err(reason) => {
            let metric = SensorDropMetric {
                event: crate::sensor_gate::SENSOR_DROP_EVENT,
                sensor: LocalSensorName::Screen,
                source_family: Some(SourceFamily::Screen),
                reason,
                stage: SensorGateStage::PreCapture,
                operation_id: Some(new_operation_id().0),
                session_id: None,
                turn_id: None,
                budget: None,
            };
            if let Err(error) = append_sensor_drop_metric(vault_root, &metric) {
                eprintln!("cairn screen capture: failed to write screen drop metric: {error:#}");
            }
            Err(reason)
        }
    }
}

fn block_on<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)?
        .block_on(future)
}

fn exit_code_for_gate_reason(reason: SensorGateReason) -> ExitCode {
    match reason {
        SensorGateReason::Disabled => ExitCode::from(78),
        SensorGateReason::PrivacyDenied | SensorGateReason::BudgetExceeded => ExitCode::from(77),
    }
}

fn screen_text_budget_bytes(config: &CairnConfig) -> u64 {
    u64::from(config.sensors.screen.budget.max_text_bytes_per_event)
}

#[derive(Debug)]
enum CaptureEventBuildError {
    Dropped(DropReason),
    BuildFailed(String),
}

impl std::fmt::Display for CaptureEventBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dropped(reason) => write!(f, "{reason:?}"),
            Self::BuildFailed(err) => f.write_str(err),
        }
    }
}

fn capture_event_for_observation(
    vault_root: &Path,
    config: &CairnConfig,
    observation: ScreenObservation,
    started: Instant,
) -> Result<CaptureEvent, CaptureEventBuildError> {
    let event_id = CaptureEventId::parse(new_operation_id().0)
        .map_err(|err| CaptureEventBuildError::BuildFailed(err.to_string()))?;
    let observed_bytes = screen_observation_budgeted_payload_bytes(&observation).map_or_else(
        |_| screen_observation_observed_bytes(&observation),
        |bytes| u64::try_from(bytes).unwrap_or(u64::MAX),
    );
    let budget_bytes = Some(screen_text_budget_bytes(config));
    let Ok(event_observation) =
        ScreenEventObservation::from_observation(event_id, observation, None)
    else {
        let outcome = EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::MalformedObservation("invalid_timestamp".to_owned()),
        };
        emit_screen_sensor_outcome_metric(
            vault_root,
            config,
            &outcome,
            started,
            observed_bytes,
            budget_bytes,
        );
        return Err(CaptureEventBuildError::Dropped(
            DropReason::MalformedObservation("invalid_timestamp".to_owned()),
        ));
    };
    let mut local_config = LocalSensorConfig::from_core(&config.sensors);
    local_config.screen = SensorSettings::enabled();
    let outcome = cairn_sensors_local::screen::emit(&local_config, event_observation);
    emit_screen_sensor_outcome_metric(
        vault_root,
        config,
        &outcome,
        started,
        observed_bytes,
        budget_bytes,
    );

    match outcome {
        EmitOutcome::Emitted(event) => Ok(event),
        EmitOutcome::Dropped { reason, .. } => Err(CaptureEventBuildError::Dropped(reason)),
    }
}

fn emit_screen_sensor_outcome_metric(
    vault_root: &Path,
    config: &CairnConfig,
    outcome: &EmitOutcome,
    started: Instant,
    observed_bytes: u64,
    budget_bytes: Option<u64>,
) {
    if !config.observability.enabled || !config.observability.local_metrics {
        return;
    }
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let event = outcome.metric_event(
        crate::metrics::now_ms(),
        latency_ms,
        observed_bytes,
        budget_bytes,
    );
    if let Err(err) = crate::metrics::append_local_event_sync(vault_root, &event) {
        tracing::warn!(error = %err, "screen sensor metric emit failed");
    }
}

struct ScreenSensorMetric {
    status: &'static str,
    error: Option<String>,
    degradation_state: Option<String>,
    started: Instant,
    observed_bytes: u64,
    budget_bytes: Option<u64>,
}

fn emit_screen_sensor_metric(vault_root: &Path, config: &CairnConfig, metric: ScreenSensorMetric) {
    if !config.observability.enabled || !config.observability.local_metrics {
        return;
    }
    let latency_ms = u64::try_from(metric.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let budget_used_ratio = metric
        .budget_bytes
        .and_then(|budget| screen_budget_ratio(metric.observed_bytes, budget));
    let event = MetricEvent::SensorEmission {
        ts_ms: crate::metrics::now_ms(),
        sensor: SensorKind::Screen.as_str().to_owned(),
        status: metric.status.to_owned(),
        latency_ms,
        bytes: metric.observed_bytes,
        budget_bytes: metric.budget_bytes,
        budget_used_ratio,
        error: metric.error,
        degradation_state: metric.degradation_state,
    };
    if let Err(err) = crate::metrics::append_local_event_sync(vault_root, &event) {
        tracing::warn!(error = %err, "screen sensor metric emit failed");
    }
}

fn screen_budget_ratio(bytes: u64, budget: u64) -> Option<f64> {
    if budget == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(bytes as f64 / budget as f64)
}

fn screen_error_metric_class(err: &ScreenError) -> &'static str {
    match err {
        ScreenError::Unavailable(ScreenDegradationCode::PermissionMissing) => "permission_missing",
        ScreenError::Unavailable(ScreenDegradationCode::Disabled) => "disabled",
        ScreenError::Unavailable(ScreenDegradationCode::BackendUnavailable) => {
            "backend_unavailable"
        }
        ScreenError::Unavailable(ScreenDegradationCode::Degraded) => "degraded",
        ScreenError::CaptureFailed(_) => "capture_failed",
    }
}

fn screen_error_degradation_state(err: &ScreenError) -> &'static str {
    match err.code() {
        ScreenDegradationCode::PermissionMissing => "permission_missing",
        ScreenDegradationCode::Disabled => "disabled",
        ScreenDegradationCode::BackendUnavailable => "backend_unavailable",
        ScreenDegradationCode::Degraded => "degraded",
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

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::config::{ScreenBackend, ScreenOcrEngine};
    use cairn_sensors_local::screen::{BoundingBox, ResolvedScreenOcrEngine, XCAP_SENSOR_LABEL};

    fn observation_with_secret_text() -> ScreenObservation {
        ScreenObservation {
            text: "screen body with password=super-secret".to_owned(),
            app: "Code".to_owned(),
            window_title: "Telemetry Review".to_owned(),
            url: Some("file:///tmp/secret.md".to_owned()),
            bounding_boxes: vec![BoundingBox {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            }],
            captured_at: "2026-05-20T08:00:00Z".to_owned(),
            sensor_label: XCAP_SENSOR_LABEL.to_owned(),
            backend: ScreenBackend::Xcap,
            ocr_engine: ResolvedScreenOcrEngine::Tesseract,
        }
    }

    fn screen_config_with_text_budget(max_text_bytes: u32) -> CairnConfig {
        let mut config = CairnConfig::default();
        config.sensors.screen.enabled = true;
        config.sensors.screen.ocr.engine = ScreenOcrEngine::Tesseract;
        config.sensors.screen.budget.max_text_bytes_per_event = max_text_bytes;
        config
    }

    fn first_sensor_metric(vault: &Path) -> serde_json::Value {
        let metrics =
            std::fs::read_to_string(vault.join(".cairn/metrics.jsonl")).expect("metrics file");
        metrics
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("metric json"))
            .find(|row| row["event"] == "sensor_emission")
            .expect("sensor emission metric")
    }

    #[test]
    fn screen_capture_event_emits_body_free_sensor_metric() {
        let vault = tempfile::tempdir().expect("tempdir");
        let payload_len = u32::try_from(
            screen_observation_budgeted_payload_bytes(&observation_with_secret_text())
                .expect("payload bytes"),
        )
        .expect("payload len fits u32");
        let config = screen_config_with_text_budget(payload_len);

        let event = capture_event_for_observation(
            vault.path(),
            &config,
            observation_with_secret_text(),
            Instant::now(),
        )
        .expect("screen capture event");
        assert_eq!(event.source_family, SourceFamily::Screen);

        let metrics = std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl"))
            .expect("metrics file");
        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["sensor"], "screen");
        assert_eq!(metric["status"], "emitted");
        assert_eq!(metric["bytes"], u64::from(payload_len));
        assert_eq!(metric["budget_bytes"], u64::from(payload_len));
        assert!(metric["latency_ms"].as_u64().is_some());
        assert!(
            !metrics.contains("password=super-secret")
                && !metrics.contains("screen body")
                && !metrics.contains("secret.md"),
            "screen sensor metric must not export payload fields: {metrics}"
        );
    }

    #[test]
    fn screen_capture_event_emits_when_payload_exceeds_configured_budget() {
        let vault = tempfile::tempdir().expect("tempdir");
        let config = screen_config_with_text_budget(1);

        let event = capture_event_for_observation(
            vault.path(),
            &config,
            observation_with_secret_text(),
            Instant::now(),
        )
        .expect("diagnostic capture event should not fail on metric budget");
        assert_eq!(event.source_family, SourceFamily::Screen);

        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["sensor"], "screen");
        assert_eq!(metric["status"], "emitted");
        assert!(metric["bytes"].as_u64().expect("metric bytes") > 1);
        assert_eq!(metric["budget_bytes"], 1);
        assert!(metric["budget_used_ratio"].as_f64().expect("budget ratio") > 1.0);
    }

    #[test]
    fn screen_capture_event_metric_uses_capture_start_latency() {
        let vault = tempfile::tempdir().expect("tempdir");
        let payload_len = u32::try_from(
            screen_observation_budgeted_payload_bytes(&observation_with_secret_text())
                .expect("payload bytes"),
        )
        .expect("payload len fits u32");
        let config = screen_config_with_text_budget(payload_len);
        let started = Instant::now()
            .checked_sub(std::time::Duration::from_millis(25))
            .expect("instant can move back for test");

        capture_event_for_observation(
            vault.path(),
            &config,
            observation_with_secret_text(),
            started,
        )
        .expect("screen capture event");

        let metric = first_sensor_metric(vault.path());
        assert!(metric["latency_ms"].as_u64().expect("latency") >= 25);
    }

    #[test]
    fn screen_capture_event_emits_malformed_timestamp_metric() {
        let vault = tempfile::tempdir().expect("tempdir");
        let config = screen_config_with_text_budget(1024);
        let mut observation = observation_with_secret_text();
        observation.captured_at = "not-a-timestamp".to_owned();

        let err = capture_event_for_observation(vault.path(), &config, observation, Instant::now())
            .expect_err("screen event should reject malformed timestamp");
        assert!(
            err.to_string().contains("invalid_timestamp"),
            "expected timestamp failure, got {err}"
        );

        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["sensor"], "screen");
        assert_eq!(metric["status"], "dropped");
        assert_eq!(metric["error"], "malformed_observation");
        assert_eq!(metric["budget_bytes"], 1024);
    }

    #[test]
    fn captured_receipt_validation_failure_removes_artifact() {
        let vault = tempfile::tempdir().expect("tempdir");
        let output_path = vault.path().join("screen.png");
        std::fs::write(&output_path, b"fake-png").expect("write fake capture");
        let config = screen_config_with_text_budget(1024);
        let mut observation = observation_with_secret_text();
        observation.captured_at = "not-a-timestamp".to_owned();
        let receipt = ScreenCaptureReceipt {
            output_path: output_path.clone(),
            width: 10,
            height: 20,
            observation,
        };

        let code = emit_captured_receipt(vault.path(), &config, &receipt, Instant::now(), false);

        assert_eq!(code, ExitCode::from(70));
        assert!(!output_path.exists());
    }

    #[test]
    fn captured_receipt_policy_rejection_removes_artifact_as_drop() {
        let vault = tempfile::tempdir().expect("tempdir");
        let output_path = vault.path().join("screen.png");
        std::fs::write(&output_path, b"fake-png").expect("write fake capture");
        let config = screen_config_with_text_budget(4096);
        let mut observation = observation_with_secret_text();
        observation.text = "-----BEGIN PRIVATE KEY-----\nsecret".to_owned();
        let receipt = ScreenCaptureReceipt {
            output_path: output_path.clone(),
            width: 10,
            height: 20,
            observation,
        };

        let code = emit_captured_receipt(vault.path(), &config, &receipt, Instant::now(), true);

        assert_eq!(code, ExitCode::from(78));
        assert!(!output_path.exists());
        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["status"], "dropped");
        assert_eq!(metric["error"], "policy_rejected");
    }

    #[test]
    fn screen_capture_skip_emits_body_free_privacy_metric() {
        let vault = tempfile::tempdir().expect("tempdir");
        let config = screen_config_with_text_budget(1024);
        let outcome = EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::PolicyRejected("privacy_filtered".to_owned()),
        };

        emit_screen_sensor_outcome_metric(
            vault.path(),
            &config,
            &outcome,
            Instant::now(),
            73,
            Some(screen_text_budget_bytes(&config)),
        );

        let metrics = std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl"))
            .expect("metrics file");
        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["sensor"], "screen");
        assert_eq!(metric["status"], "dropped");
        assert_eq!(metric["error"], "policy_rejected");
        assert_eq!(metric["bytes"], 73);
        assert_eq!(metric["budget_bytes"], 1024);
        assert!(
            !metrics.contains("privacy_filtered"),
            "privacy metric must not export internal reason details: {metrics}"
        );
    }

    #[test]
    fn screen_capture_failure_emits_runtime_failure_metric() {
        let vault = tempfile::tempdir().expect("tempdir");
        let config = screen_config_with_text_budget(1024);

        let code = emit_failed_capture(
            vault.path(),
            &config,
            &ScreenError::Unavailable(ScreenDegradationCode::PermissionMissing),
            Instant::now(),
            false,
        );
        assert_eq!(code, ExitCode::from(77));

        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["sensor"], "screen");
        assert_eq!(metric["status"], "dropped");
        assert_eq!(metric["error"], "permission_missing");
        assert_eq!(metric["degradation_state"], "permission_missing");
        assert_eq!(metric["budget_bytes"], 1024);
    }

    #[test]
    fn screen_capture_cleanup_failure_preserves_skip_bytes_metric() {
        let vault = tempfile::tempdir().expect("tempdir");
        let config = screen_config_with_text_budget(1024);
        let skip = ScreenCaptureSkip {
            reason: cairn_sensors_local::screen::ScreenCaptureSkipReason::PrivacyFiltered,
            observed_bytes: 321,
            artifact_created: true,
        };

        let code = emit_cleanup_failed_capture(
            vault.path(),
            &config,
            skip,
            &ScreenError::CaptureFailed("failed to remove dropped screen capture".to_owned()),
            Instant::now(),
            false,
        );
        assert_eq!(code, ExitCode::from(69));

        let metric = first_sensor_metric(vault.path());
        assert_eq!(metric["sensor"], "screen");
        assert_eq!(metric["status"], "dropped");
        assert_eq!(metric["error"], "artifact_cleanup_failed");
        assert_eq!(metric["degradation_state"], "cleanup_failed");
        assert_eq!(metric["bytes"], 321);
        assert_eq!(metric["budget_bytes"], 1024);
    }

    #[test]
    fn cleanup_failure_payload_is_structured_for_json_callers() {
        let skip = ScreenCaptureSkip {
            reason: cairn_sensors_local::screen::ScreenCaptureSkipReason::PrivacyFiltered,
            observed_bytes: 321,
            artifact_created: true,
        };

        let payload = cleanup_failed_payload(
            skip,
            &ScreenError::CaptureFailed("failed to remove dropped screen capture".to_owned()),
        );

        assert_eq!(payload["status"], "dropped");
        assert_eq!(payload["sensor"], "screen");
        assert_eq!(payload["reason"], "artifact_cleanup_failed");
        assert_eq!(payload["skip_reason"], "privacy_filtered");
        assert_eq!(payload["artifact_created"], true);
        assert_eq!(payload["artifact_removed"], false);
        assert_eq!(payload["observed_bytes"], 321);
        assert_eq!(payload["error"], "capture_failed");
        assert_eq!(payload["degradation_state"], "cleanup_failed");
        assert!(!payload.to_string().contains("super-secret"));
    }

    #[test]
    fn post_capture_cleanup_failure_payload_uses_cleanup_state() {
        let payload = post_capture_cleanup_failed_payload(&ScreenError::CaptureFailed(
            "failed to remove dropped screen capture".to_owned(),
        ));

        assert_eq!(payload["reason"], "artifact_cleanup_failed");
        assert_eq!(payload["artifact_created"], true);
        assert_eq!(payload["artifact_removed"], false);
        assert_eq!(payload["error"], "capture_failed");
        assert_eq!(payload["degradation_state"], "cleanup_failed");
    }

    #[test]
    fn post_capture_drop_payload_reports_created_then_removed_artifact() {
        let payload = post_capture_drop_payload("policy_rejected", "policy_rejected");

        assert_eq!(payload["status"], "dropped");
        assert_eq!(payload["reason"], "policy_rejected");
        assert_eq!(payload["artifact_created"], true);
        assert_eq!(payload["artifact_removed"], true);
    }

    #[test]
    fn runtime_error_payload_is_structured_for_json_callers() {
        let payload = screen_error_payload(&ScreenError::Unavailable(
            ScreenDegradationCode::PermissionMissing,
        ));

        assert_eq!(payload["status"], "error");
        assert_eq!(payload["sensor"], "screen");
        assert_eq!(payload["reason"], "permission_missing");
        assert_eq!(payload["error"], "permission_missing");
        assert_eq!(payload["degradation_state"], "permission_missing");
    }

    #[cfg(unix)]
    #[test]
    fn captured_receipt_json_cleanup_failure_leaves_artifact_and_metrics_cleanup_state() {
        use std::os::unix::fs::PermissionsExt as _;

        let vault = tempfile::tempdir().expect("tempdir");
        let output_dir = vault.path().join("captures");
        std::fs::create_dir(&output_dir).expect("create output directory");
        let output_path = output_dir.join("screen.png");
        std::fs::write(&output_path, b"fake-png").expect("write fake capture");
        std::fs::set_permissions(&output_dir, std::fs::Permissions::from_mode(0o500))
            .expect("make output directory non-writable");
        let config = screen_config_with_text_budget(1024);
        let mut observation = observation_with_secret_text();
        observation.captured_at = "not-a-timestamp".to_owned();
        let receipt = ScreenCaptureReceipt {
            output_path: output_path.clone(),
            width: 10,
            height: 20,
            observation,
        };

        let code = emit_captured_receipt(vault.path(), &config, &receipt, Instant::now(), true);
        std::fs::set_permissions(&output_dir, std::fs::Permissions::from_mode(0o700))
            .expect("restore directory permissions");

        assert_eq!(code, ExitCode::from(69));
        assert!(output_path.exists());
        let metrics = std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl"))
            .expect("metrics file");
        let metric = metrics
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("metric json"))
            .find(|row| row["error"] == "artifact_cleanup_failed")
            .expect("artifact cleanup metric");
        assert_eq!(metric["error"], "artifact_cleanup_failed");
        assert_eq!(metric["degradation_state"], "cleanup_failed");
    }
}
