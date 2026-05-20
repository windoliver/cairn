//! Mockable runtime boundary for local screen capture.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", feature = "screenpipe-runtime"))]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cairn_core::config::{ScreenBackend, ScreenOcrEngine, ScreenSensorConfig};
use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::generated::common::Capabilities;
use serde_json::json;

use crate::event::build_auto_event;
use crate::policy::{PolicyAction, sanitize_text_payload};
use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind};

const FRAME_BUDGET_WINDOW: Duration = Duration::from_mins(1);

/// Bare sensor label body used by the in-process xcap backend.
pub const XCAP_SENSOR_LABEL_BODY: &str = "local:screen:xcap:v1";
/// Sensor label used by the in-process xcap backend.
pub const XCAP_SENSOR_LABEL: &str = "snr:local:screen:xcap:v1";
/// Bare sensor label body used by the optional screenpipe backend.
pub const SCREENPIPE_SENSOR_LABEL_BODY: &str = "local:screen:screenpipe:v1";
/// Sensor label used by the optional screenpipe backend.
pub const SCREENPIPE_SENSOR_LABEL: &str = "snr:local:screen:screenpipe:v1";

/// Runtime availability state for screen capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    /// Config disabled the screen sensor.
    Disabled,
    /// Capture is available.
    Enabled,
    /// Capture cannot run because OS permission is missing.
    PermissionMissing,
    /// Capture is configured but degraded.
    Degraded,
}

/// Capture mode resolved for this runtime boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    /// Capture is off.
    Off,
    /// Point-in-time screenshot plus OCR snapshot.
    Snapshot,
    /// Continuous capture mode.
    Continuous,
}

/// Runtime permission status for screen capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenPermission {
    /// Permission has not been requested.
    NotRequested,
    /// Permission is available.
    Granted,
    /// Permission is missing or denied.
    Denied,
    /// Permission was granted previously and later revoked.
    Revoked,
}

/// OCR engine after resolving `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedScreenOcrEngine {
    /// Apple Vision OCR.
    Vision,
    /// Windows Runtime OCR.
    Winrt,
    /// Tesseract OCR.
    Tesseract,
    /// OCR is disabled.
    Off,
}

impl ResolvedScreenOcrEngine {
    /// Resolve a configured OCR engine into the concrete runtime engine.
    #[must_use]
    pub fn from_config(engine: ScreenOcrEngine) -> Self {
        if engine == ScreenOcrEngine::Auto {
            return platform_default_ocr_engine();
        }

        match engine {
            ScreenOcrEngine::Vision => Self::Vision,
            ScreenOcrEngine::Winrt => Self::Winrt,
            ScreenOcrEngine::Tesseract => Self::Tesseract,
            _ => Self::Off,
        }
    }
}

/// Stable degradation codes for screen capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenDegradationCode {
    /// Sensor is explicitly disabled in config.
    Disabled,
    /// OS permission is missing or denied.
    PermissionMissing,
    /// Requested backend is not compiled into this binary.
    BackendUnavailable,
    /// Capture is degraded for a reason that does not have a narrower code.
    Degraded,
}

/// Screen capture degradation details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDegradation {
    /// Stable degradation code.
    pub code: ScreenDegradationCode,
    /// Operator-facing degradation message.
    pub message: String,
}

impl ScreenDegradation {
    /// Create a degradation from its stable code.
    #[must_use]
    pub fn new(code: ScreenDegradationCode) -> Self {
        Self {
            code,
            message: degradation_message(code).to_owned(),
        }
    }

    /// Create a degradation with a backend-specific operator message.
    #[must_use]
    pub fn with_message(code: ScreenDegradationCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Result of probing a configured screen backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenProbe {
    /// Configured backend.
    pub backend: ScreenBackend,
    /// Runtime state.
    pub state: ScreenState,
    /// Runtime capture mode.
    pub mode: ScreenMode,
    /// Runtime permission status.
    pub permission: ScreenPermission,
    /// Resolved OCR engine.
    pub ocr_engine: ResolvedScreenOcrEngine,
    /// Degradation if capture is not fully enabled.
    pub degradation: Option<ScreenDegradation>,
    /// Focused application name observed during probe, when available without capture.
    pub focused_app: Option<String>,
}

impl ScreenProbe {
    /// Return the stable degradation code, if any.
    #[must_use]
    pub fn degradation_code(&self) -> Option<ScreenDegradationCode> {
        self.degradation
            .as_ref()
            .map(|degradation| degradation.code)
    }
}

/// OCR bounding box in screen coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundingBox {
    /// Left coordinate.
    pub x: u32,
    /// Top coordinate.
    pub y: u32,
    /// Box width.
    pub width: u32,
    /// Box height.
    pub height: u32,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ScreenOcrResult {
    text: String,
    bounding_boxes: Vec<BoundingBox>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenOcrCapture {
    engine: ResolvedScreenOcrEngine,
    result: ScreenOcrResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SanitizedScreenFields {
    text: String,
    window_title: String,
    url: Option<String>,
}

/// Captured screen text and focused-window metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenObservation {
    /// OCR text.
    pub text: String,
    /// Focused application name.
    pub app: String,
    /// Focused window title.
    pub window_title: String,
    /// URL associated with the focused context, when known.
    pub url: Option<String>,
    /// OCR text bounding boxes.
    pub bounding_boxes: Vec<BoundingBox>,
    /// Capture timestamp.
    pub captured_at: String,
    /// Sensor label that produced this observation.
    pub sensor_label: String,
    /// Backend that produced this observation.
    pub backend: ScreenBackend,
    /// OCR engine that produced this observation.
    pub ocr_engine: ResolvedScreenOcrEngine,
}

/// Screen observation plus pipeline envelope metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenEventObservation {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Observation capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Runtime screen observation.
    pub observation: ScreenObservation,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
}

impl ScreenEventObservation {
    /// Convert a runtime observation into an event-ready observation.
    ///
    /// # Errors
    /// Returns [`ScreenError::CaptureFailed`] when the runtime timestamp is
    /// not a valid RFC-3339 timestamp.
    pub fn from_observation(
        event_id: CaptureEventId,
        observation: ScreenObservation,
        refs: Option<CaptureRefs>,
    ) -> Result<Self, ScreenError> {
        let captured_at =
            Rfc3339Timestamp::parse(observation.captured_at.clone()).map_err(|err| {
                ScreenError::CaptureFailed(format!("invalid screen timestamp: {err}"))
            })?;
        Ok(Self {
            event_id,
            captured_at,
            observation,
            refs,
        })
    }
}

/// Receipt for a screenshot written to disk by a screen backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCaptureReceipt {
    /// Path where the PNG snapshot was written.
    pub output_path: PathBuf,
    /// Captured image width in physical pixels.
    pub width: u32,
    /// Captured image height in physical pixels.
    pub height: u32,
    /// Metadata observation associated with the screenshot.
    pub observation: ScreenObservation,
}

/// Body-free reason a configured screen capture produced no artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenCaptureSkipReason {
    /// Screen capture is disabled in config.
    Disabled,
    /// The focused application did not match `allow_apps`.
    AllowList,
    /// Password-field or OCR-off privacy filtering dropped the artifact.
    PrivacyFiltered,
    /// A screen policy rejected the captured observation.
    PolicyRejected,
}

impl ScreenCaptureSkipReason {
    /// Convert to a local sensor drop reason for telemetry.
    #[must_use]
    pub fn drop_reason(self) -> DropReason {
        match self {
            Self::Disabled => DropReason::Disabled,
            Self::AllowList => DropReason::PolicyRejected("allow_apps".to_owned()),
            Self::PrivacyFiltered => DropReason::PolicyRejected("privacy_filtered".to_owned()),
            Self::PolicyRejected => DropReason::PolicyRejected("policy_rejected".to_owned()),
        }
    }

    /// Stable CLI/API reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::AllowList => "allow_apps",
            Self::PrivacyFiltered => "privacy_filtered",
            Self::PolicyRejected => "policy_rejected",
        }
    }

    /// Human-facing description that does not expose captured content.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Disabled => "screen sensor is disabled in config",
            Self::AllowList => "skipped by screen allow_apps policy",
            Self::PrivacyFiltered => "dropped by screen privacy filter",
            Self::PolicyRejected => "dropped by screen policy",
        }
    }
}

/// Body-free skip details for a screen PNG capture attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCaptureSkip {
    /// Stable skip reason.
    pub reason: ScreenCaptureSkipReason,
    /// Body-free count of bytes observed before the skip.
    pub observed_bytes: u64,
    /// Whether a capture artifact was created before the skip decision.
    pub artifact_created: bool,
}

impl ScreenCaptureSkip {
    const fn before_capture(reason: ScreenCaptureSkipReason) -> Self {
        Self {
            reason,
            observed_bytes: 0,
            artifact_created: false,
        }
    }

    fn after_capture(reason: ScreenCaptureSkipReason, observation: &ScreenObservation) -> Self {
        Self {
            reason,
            observed_bytes: screen_observation_observed_bytes(observation),
            artifact_created: true,
        }
    }
}

/// Result of a configured screen PNG capture attempt.
// Keep the receipt by value because callers immediately match on this
// short-lived runtime boundary result.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenCaptureOutcome {
    /// Capture produced a PNG receipt.
    Captured(ScreenCaptureReceipt),
    /// Capture was intentionally skipped before returning an artifact.
    Skipped(ScreenCaptureSkip),
    /// Capture was skipped after an artifact was written, but cleanup failed.
    CleanupFailed {
        /// Skip context that led to artifact cleanup.
        skip: ScreenCaptureSkip,
        /// Cleanup failure.
        error: ScreenError,
    },
}

impl ScreenCaptureOutcome {
    /// Convert to the legacy optional receipt shape while preserving cleanup failures.
    pub fn into_result_receipt(self) -> Result<Option<ScreenCaptureReceipt>, ScreenError> {
        match self {
            Self::Captured(receipt) => Ok(Some(receipt)),
            Self::Skipped(_) => Ok(None),
            Self::CleanupFailed { error, .. } => Err(error),
        }
    }
}

#[cfg(any(test, feature = "screenpipe-runtime"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenpipeFrameCapture {
    observation: ScreenObservation,
    frame_id: Option<u64>,
    file_path: Option<PathBuf>,
}

/// Errors emitted by the screen runtime boundary.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ScreenError {
    /// Capture is unavailable for the current configuration.
    #[error("screen capture unavailable: {0:?}")]
    Unavailable(ScreenDegradationCode),
    /// Backend-specific capture failure.
    #[error("screen capture failed: {0}")]
    CaptureFailed(String),
}

impl ScreenError {
    /// Return a stable code for policy/status mapping.
    #[must_use]
    pub fn code(&self) -> ScreenDegradationCode {
        match self {
            Self::Unavailable(code) => *code,
            Self::CaptureFailed(_) => ScreenDegradationCode::BackendUnavailable,
        }
    }
}

/// Backend implementation that can be mocked in tests.
pub trait ScreenBackendRuntime {
    /// Probe backend availability without capturing.
    fn probe(&self, config: &ScreenSensorConfig) -> ScreenProbe;

    /// Capture a single snapshot.
    fn capture_snapshot(
        &self,
        config: &ScreenSensorConfig,
    ) -> Result<ScreenObservation, ScreenError>;
}

/// Backend implementation that can write a screenshot artifact.
pub trait ScreenCaptureRuntime: ScreenBackendRuntime {
    /// Capture a single PNG snapshot and return its metadata receipt.
    fn capture_png_snapshot(
        &self,
        config: &ScreenSensorConfig,
        output_path: &Path,
    ) -> Result<ScreenCaptureReceipt, ScreenError>;
}

/// Policy applied after budget enforcement and before observations leave runtime.
pub trait ScreenPolicy {
    /// Apply redaction or filtering to an observation.
    fn apply(&self, observation: ScreenObservation) -> Result<ScreenObservation, ScreenError>;
}

/// Policy that leaves observations unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopScreenPolicy;

impl ScreenPolicy for NoopScreenPolicy {
    fn apply(&self, observation: ScreenObservation) -> Result<ScreenObservation, ScreenError> {
        Ok(observation)
    }
}

/// Basic fixture policy for early screen-runtime tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct BasicScreenPolicy;

impl ScreenPolicy for BasicScreenPolicy {
    fn apply(&self, mut observation: ScreenObservation) -> Result<ScreenObservation, ScreenError> {
        if observation.text.to_lowercase().contains("password=") {
            observation.text.clear();
            observation.text.push_str("[redacted]");
        }
        Ok(observation)
    }
}

/// Emit one screen OCR observation as a validated capture event.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: ScreenEventObservation) -> EmitOutcome {
    if !config.screen.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::Disabled,
        };
    }

    if observation.observation.app.trim().is_empty() {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::MalformedObservation("screen app is required".to_owned()),
        };
    }

    let Some(sensor_label_body) = sensor_label_body(&observation.observation.sensor_label) else {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::MalformedObservation(format!(
                "unknown screen sensor label `{}`",
                observation.observation.sensor_label
            )),
        };
    };

    let sanitized = match sanitize_screen_fields(&observation.observation) {
        Ok(sanitized) => sanitized,
        Err(reason) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Screen,
                reason,
            };
        }
    };

    let sanitized_payload = match raw_payload_bytes(
        &observation.observation,
        &sanitized.text,
        &sanitized.window_title,
        sanitized.url.as_deref(),
    ) {
        Ok(bytes) => bytes,
        Err(err) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Screen,
                reason: DropReason::MalformedObservation(format!(
                    "screen payload serialization failed: {err}"
                )),
            };
        }
    };

    if !config.screen.budget.allows(1, sanitized_payload.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::BudgetExceeded,
        };
    }

    let payload = CapturePayload::Screen {
        app: observation.observation.app,
        window_title: sanitized.window_title,
        url: sanitized.url,
    };

    match build_auto_event(
        observation.event_id,
        observation.captured_at,
        sensor_label_body,
        payload,
        SourceFamily::Screen,
        observation.refs,
        &sanitized_payload,
    ) {
        Ok(event) => EmitOutcome::Emitted(event),
        Err(err) => EmitOutcome::Dropped {
            sensor: SensorKind::Screen,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
    }
}

fn sensor_label_body(sensor_label: &str) -> Option<&'static str> {
    match sensor_label {
        XCAP_SENSOR_LABEL => Some(XCAP_SENSOR_LABEL_BODY),
        SCREENPIPE_SENSOR_LABEL => Some(SCREENPIPE_SENSOR_LABEL_BODY),
        _ => None,
    }
}

fn sanitize_screen_fields(
    observation: &ScreenObservation,
) -> Result<SanitizedScreenFields, DropReason> {
    let text = sanitize_screen_text(&observation.text)?;
    let window_title = sanitize_screen_text(&observation.window_title)?;
    let url = observation
        .url
        .as_deref()
        .map(sanitize_screen_text)
        .transpose()?;

    Ok(SanitizedScreenFields {
        text,
        window_title,
        url,
    })
}

fn sanitize_screen_text(text: &str) -> Result<String, DropReason> {
    match sanitize_text_payload(text) {
        PolicyAction::Sanitized(text) => Ok(text),
        PolicyAction::Rejected(reason) => Err(DropReason::PolicyRejected(reason)),
    }
}

/// Return the sanitized payload byte count used for screen budget enforcement.
///
/// # Errors
/// Returns [`DropReason`] when local privacy policy rejects a field, or
/// [`serde_json::Error`] if payload serialization fails.
pub fn screen_observation_budgeted_payload_bytes(
    observation: &ScreenObservation,
) -> Result<usize, DropReason> {
    let sanitized = sanitize_screen_fields(observation)?;
    raw_payload_bytes(
        observation,
        &sanitized.text,
        &sanitized.window_title,
        sanitized.url.as_deref(),
    )
    .map(|bytes| bytes.len())
    .map_err(|err| {
        DropReason::MalformedObservation(format!("screen payload serialization failed: {err}"))
    })
}

/// Return a body-free count of raw screen observation bytes.
#[must_use]
pub fn screen_observation_observed_bytes(observation: &ScreenObservation) -> u64 {
    let string_bytes = observation
        .text
        .len()
        .saturating_add(observation.app.len())
        .saturating_add(observation.window_title.len())
        .saturating_add(observation.url.as_ref().map_or(0, String::len))
        .saturating_add(observation.captured_at.len())
        .saturating_add(observation.sensor_label.len());
    let box_bytes = observation
        .bounding_boxes
        .len()
        .saturating_mul(std::mem::size_of::<BoundingBox>());
    u64::try_from(string_bytes.saturating_add(box_bytes)).unwrap_or(u64::MAX)
}

fn raw_payload_bytes(
    observation: &ScreenObservation,
    sanitized_text: &str,
    sanitized_window_title: &str,
    sanitized_url: Option<&str>,
) -> Result<Vec<u8>, serde_json::Error> {
    let bounding_boxes = observation
        .bounding_boxes
        .iter()
        .map(|box_| {
            json!({
                "height": box_.height,
                "width": box_.width,
                "x": box_.x,
                "y": box_.y,
            })
        })
        .collect::<Vec<_>>();

    serde_json::to_vec(&json!({
        "backend": screen_backend_name(observation.backend),
        "ocr": {
            "bounding_boxes": bounding_boxes,
            "engine": ocr_engine_name(observation.ocr_engine),
            "text": sanitized_text,
        },
        "screen": {
            "app": observation.app,
            "url": sanitized_url,
            "window_title": sanitized_window_title,
        },
        "sensor_label": observation.sensor_label,
        "timing": {
            "captured_at": observation.captured_at,
        },
    }))
}

fn should_drop_for_password_fields(
    config: &ScreenSensorConfig,
    observation: &ScreenObservation,
) -> bool {
    config.blur_password_fields
        && (contains_password_field_marker(&observation.text)
            || contains_password_field_marker(&observation.window_title)
            || observation
                .url
                .as_deref()
                .is_some_and(contains_password_field_marker))
}

fn contains_password_field_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password=",
        "password:",
        "\"password\"",
        "'password'",
        "type=\"password\"",
        "type='password'",
        "type=password",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn should_drop_capture_artifact(
    config: &ScreenSensorConfig,
    observation: &ScreenObservation,
) -> bool {
    should_drop_for_password_fields(config, observation)
        || (config.blur_password_fields && observation.ocr_engine == ResolvedScreenOcrEngine::Off)
}

fn remove_capture_artifact(output_path: &Path) -> Result<(), ScreenError> {
    if !output_path.exists() {
        return Ok(());
    }

    fs::remove_file(output_path).map_err(|err| {
        ScreenError::CaptureFailed(format!(
            "failed to remove dropped screen capture {}: {err}",
            output_path.display()
        ))
    })
}

#[cfg(any(test, feature = "screenpipe-runtime"))]
fn apply_screen_ocr_config(config: &ScreenSensorConfig, observation: &mut ScreenObservation) {
    let ocr_engine = ResolvedScreenOcrEngine::from_config(config.ocr.engine);
    observation.ocr_engine = ocr_engine;
    if ocr_engine == ResolvedScreenOcrEngine::Off {
        observation.text.clear();
        observation.bounding_boxes.clear();
    }
}

/// Mockable screen sensor that composes backend and policy.
#[derive(Debug)]
pub struct ScreenSensor<B, P> {
    backend: B,
    policy: P,
    frame_budget: SharedFrameBudget,
}

#[derive(Debug, Clone)]
enum SharedFrameBudget {
    InMemory(Arc<Mutex<VecDeque<Instant>>>),
    Persistent(PathBuf),
}

impl Default for SharedFrameBudget {
    fn default() -> Self {
        Self::InMemory(Arc::new(Mutex::new(VecDeque::new())))
    }
}

impl SharedFrameBudget {
    #[cfg(test)]
    fn persistent_at(path: PathBuf) -> Self {
        Self::Persistent(path)
    }

    fn persistent(key: &str) -> Self {
        Self::Persistent(persistent_frame_budget_path(key))
    }

    fn admit(&self, max_frames_per_minute: u32) -> Result<(), ScreenError> {
        match self {
            Self::InMemory(frame_timestamps) => {
                Self::admit_in_memory(frame_timestamps, max_frames_per_minute)
            }
            Self::Persistent(path) => Self::admit_persistent(path, max_frames_per_minute),
        }
    }

    fn admit_in_memory(
        frame_timestamps: &Mutex<VecDeque<Instant>>,
        max_frames_per_minute: u32,
    ) -> Result<(), ScreenError> {
        let now = Instant::now();
        let mut timestamps = frame_timestamps
            .lock()
            .map_err(|_| ScreenError::Unavailable(ScreenDegradationCode::Degraded))?;

        while timestamps
            .front()
            .is_some_and(|timestamp| now.duration_since(*timestamp) >= FRAME_BUDGET_WINDOW)
        {
            timestamps.pop_front();
        }

        if timestamps.len() >= max_frames_per_minute as usize {
            return Err(ScreenError::Unavailable(ScreenDegradationCode::Degraded));
        }

        timestamps.push_back(now);
        Ok(())
    }

    fn admit_persistent(path: &Path, max_frames_per_minute: u32) -> Result<(), ScreenError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| ScreenError::CaptureFailed(format!("system clock before epoch: {err}")))?
            .as_millis();
        let window = FRAME_BUDGET_WINDOW.as_millis();
        let mut timestamps = read_persistent_frame_timestamps(path)?
            .into_iter()
            .filter(|timestamp| now.saturating_sub(*timestamp) < window)
            .collect::<Vec<_>>();

        if timestamps.len() >= max_frames_per_minute as usize {
            return Err(ScreenError::Unavailable(ScreenDegradationCode::Degraded));
        }

        timestamps.push(now);
        write_persistent_frame_timestamps(path, &timestamps)
    }
}

fn persistent_frame_budget_path(key: &str) -> PathBuf {
    std::env::temp_dir().join(format!("cairn-screen-frame-budget-{key}.txt"))
}

fn read_persistent_frame_timestamps(path: &Path) -> Result<Vec<u128>, ScreenError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .filter_map(|line| line.trim().parse::<u128>().ok())
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(ScreenError::CaptureFailed(format!(
            "failed to read screen frame budget {}: {err}",
            path.display()
        ))),
    }
}

fn write_persistent_frame_timestamps(path: &Path, timestamps: &[u128]) -> Result<(), ScreenError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| {
            ScreenError::CaptureFailed(format!(
                "failed to create screen frame budget directory {}: {err}",
                parent.display()
            ))
        })?;
    }

    let contents = timestamps
        .iter()
        .map(u128::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, format!("{contents}\n")).map_err(|err| {
        ScreenError::CaptureFailed(format!(
            "failed to write screen frame budget {}: {err}",
            temp_path.display()
        ))
    })?;
    fs::rename(&temp_path, path).map_err(|err| {
        ScreenError::CaptureFailed(format!(
            "failed to update screen frame budget {}: {err}",
            path.display()
        ))
    })
}

impl<B, P> ScreenSensor<B, P>
where
    B: ScreenBackendRuntime,
    P: ScreenPolicy,
{
    /// Create a screen sensor from a backend runtime and policy.
    #[must_use]
    pub fn new(backend: B, policy: P) -> Self {
        Self::with_frame_budget(backend, policy, SharedFrameBudget::default())
    }

    fn with_frame_budget(backend: B, policy: P, frame_budget: SharedFrameBudget) -> Self {
        Self {
            backend,
            policy,
            frame_budget,
        }
    }

    /// Capture a single snapshot, returning `None` when the sensor is disabled.
    pub fn capture_snapshot(
        &self,
        config: &ScreenSensorConfig,
    ) -> Result<Option<ScreenObservation>, ScreenError> {
        if !config.enabled {
            return Ok(None);
        }

        let probe = self.backend.probe(config);
        if probe.state != ScreenState::Enabled {
            return Err(ScreenError::Unavailable(
                probe
                    .degradation_code()
                    .unwrap_or(ScreenDegradationCode::BackendUnavailable),
            ));
        }

        if !config.allow_apps.is_empty()
            && !probe
                .focused_app
                .as_ref()
                .is_some_and(|app| config.allow_apps.contains(app))
        {
            return Ok(None);
        }

        self.admit_frame(config.budget.max_frames_per_minute)?;

        let mut observation = self.backend.capture_snapshot(config)?;
        if !config.allow_apps.is_empty() && !config.allow_apps.contains(&observation.app) {
            return Ok(None);
        }
        if should_drop_for_password_fields(config, &observation) {
            return Ok(None);
        }
        truncate_text_to_budget(
            &mut observation.text,
            config.budget.max_text_bytes_per_event,
        );
        Ok(Some(self.policy.apply(observation)?))
    }

    fn admit_frame(&self, max_frames_per_minute: u32) -> Result<(), ScreenError> {
        self.frame_budget.admit(max_frames_per_minute)
    }
}

impl<B, P> ScreenSensor<B, P>
where
    B: ScreenCaptureRuntime,
    P: ScreenPolicy,
{
    /// Capture a single PNG snapshot with a structured skip outcome.
    pub fn capture_png_snapshot_outcome(
        &self,
        config: &ScreenSensorConfig,
        output_path: &Path,
    ) -> Result<ScreenCaptureOutcome, ScreenError> {
        if !config.enabled {
            return Ok(ScreenCaptureOutcome::Skipped(
                ScreenCaptureSkip::before_capture(ScreenCaptureSkipReason::Disabled),
            ));
        }

        let probe = self.backend.probe(config);
        if probe.state != ScreenState::Enabled {
            return Err(ScreenError::Unavailable(
                probe
                    .degradation_code()
                    .unwrap_or(ScreenDegradationCode::BackendUnavailable),
            ));
        }

        if !config.allow_apps.is_empty()
            && !probe
                .focused_app
                .as_ref()
                .is_some_and(|app| config.allow_apps.contains(app))
        {
            return Ok(ScreenCaptureOutcome::Skipped(
                ScreenCaptureSkip::before_capture(ScreenCaptureSkipReason::AllowList),
            ));
        }

        self.admit_frame(config.budget.max_frames_per_minute)?;

        let mut receipt = self.backend.capture_png_snapshot(config, output_path)?;
        if !config.allow_apps.is_empty() && !config.allow_apps.contains(&receipt.observation.app) {
            let skip = ScreenCaptureSkip::after_capture(
                ScreenCaptureSkipReason::AllowList,
                &receipt.observation,
            );
            if let Err(error) = remove_capture_artifact(&receipt.output_path) {
                return Ok(ScreenCaptureOutcome::CleanupFailed { skip, error });
            }
            return Ok(ScreenCaptureOutcome::Skipped(skip));
        }
        if should_drop_capture_artifact(config, &receipt.observation) {
            let skip = ScreenCaptureSkip::after_capture(
                ScreenCaptureSkipReason::PrivacyFiltered,
                &receipt.observation,
            );
            if let Err(error) = remove_capture_artifact(&receipt.output_path) {
                return Ok(ScreenCaptureOutcome::CleanupFailed { skip, error });
            }
            return Ok(ScreenCaptureOutcome::Skipped(skip));
        }
        truncate_text_to_budget(
            &mut receipt.observation.text,
            config.budget.max_text_bytes_per_event,
        );
        let pre_policy_observation = receipt.observation;
        receipt.observation = match self.policy.apply(pre_policy_observation.clone()) {
            Ok(observation) => observation,
            Err(error) => {
                let skip = ScreenCaptureSkip::after_capture(
                    ScreenCaptureSkipReason::PolicyRejected,
                    &pre_policy_observation,
                );
                if let Err(cleanup_error) = remove_capture_artifact(&receipt.output_path) {
                    return Ok(ScreenCaptureOutcome::CleanupFailed {
                        skip,
                        error: cleanup_error,
                    });
                }
                return Err(error);
            }
        };
        Ok(ScreenCaptureOutcome::Captured(receipt))
    }

    /// Capture a single PNG snapshot, returning `None` when disabled or filtered out.
    pub fn capture_png_snapshot(
        &self,
        config: &ScreenSensorConfig,
        output_path: &Path,
    ) -> Result<Option<ScreenCaptureReceipt>, ScreenError> {
        self.capture_png_snapshot_outcome(config, output_path)?
            .into_result_receipt()
    }
}

/// In-process xcap backend runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct XcapBackendRuntime;

impl ScreenBackendRuntime for XcapBackendRuntime {
    fn probe(&self, config: &ScreenSensorConfig) -> ScreenProbe {
        xcap_probe(config)
    }

    fn capture_snapshot(
        &self,
        config: &ScreenSensorConfig,
    ) -> Result<ScreenObservation, ScreenError> {
        capture_xcap_observation(config)
    }
}

impl ScreenCaptureRuntime for XcapBackendRuntime {
    fn capture_png_snapshot(
        &self,
        config: &ScreenSensorConfig,
        output_path: &Path,
    ) -> Result<ScreenCaptureReceipt, ScreenError> {
        capture_xcap_png_snapshot(config, output_path)
    }
}

/// Optional screenpipe backend runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenpipeBackendRuntime;

impl ScreenBackendRuntime for ScreenpipeBackendRuntime {
    fn probe(&self, config: &ScreenSensorConfig) -> ScreenProbe {
        screenpipe_probe(config)
    }

    fn capture_snapshot(
        &self,
        config: &ScreenSensorConfig,
    ) -> Result<ScreenObservation, ScreenError> {
        screenpipe_capture_observation(config)
    }
}

impl ScreenCaptureRuntime for ScreenpipeBackendRuntime {
    fn capture_png_snapshot(
        &self,
        config: &ScreenSensorConfig,
        output_path: &Path,
    ) -> Result<ScreenCaptureReceipt, ScreenError> {
        screenpipe_capture_png_snapshot(config, output_path)
    }
}

/// Capture one screenshot using the configured primary backend, falling back
/// from screenpipe to in-process xcap when the primary path is unavailable.
pub fn capture_png_snapshot_configured(
    config: &ScreenSensorConfig,
    output_path: &Path,
) -> Result<Option<ScreenCaptureReceipt>, ScreenError> {
    capture_png_snapshot_outcome_configured(config, output_path)?.into_result_receipt()
}

/// Capture a PNG snapshot through configured backends with structured skip reasons.
pub fn capture_png_snapshot_outcome_configured(
    config: &ScreenSensorConfig,
    output_path: &Path,
) -> Result<ScreenCaptureOutcome, ScreenError> {
    match config.backend {
        ScreenBackend::Xcap => ScreenSensor::with_frame_budget(
            XcapBackendRuntime,
            NoopScreenPolicy,
            configured_frame_budget(ScreenBackend::Xcap),
        )
        .capture_png_snapshot_outcome(config, output_path),
        ScreenBackend::Screenpipe => {
            let primary = ScreenSensor::with_frame_budget(
                ScreenpipeBackendRuntime,
                NoopScreenPolicy,
                configured_frame_budget(ScreenBackend::Screenpipe),
            );
            match primary.capture_png_snapshot_outcome(config, output_path) {
                Ok(outcome) => Ok(outcome),
                Err(err) if can_fallback_from_screenpipe(&err) => {
                    let mut fallback_config = config.clone();
                    fallback_config.backend = ScreenBackend::Xcap;
                    ScreenSensor::with_frame_budget(
                        XcapBackendRuntime,
                        NoopScreenPolicy,
                        configured_frame_budget(ScreenBackend::Xcap),
                    )
                    .capture_png_snapshot_outcome(&fallback_config, output_path)
                }
                Err(err) => Err(err),
            }
        }
        _ => Err(ScreenError::Unavailable(
            ScreenDegradationCode::BackendUnavailable,
        )),
    }
}

fn configured_frame_budget(backend: ScreenBackend) -> SharedFrameBudget {
    match backend {
        ScreenBackend::Screenpipe => SharedFrameBudget::persistent("screenpipe"),
        _ => SharedFrameBudget::persistent("xcap"),
    }
}

fn can_fallback_from_screenpipe(err: &ScreenError) -> bool {
    !matches!(
        err,
        ScreenError::Unavailable(
            ScreenDegradationCode::Disabled | ScreenDegradationCode::PermissionMissing
        )
    )
}

/// Capabilities compiled into this crate for screen capture.
#[must_use]
pub fn compiled_capabilities() -> Vec<Capabilities> {
    let mut capabilities = vec![
        Capabilities::CairnSensorV1ScreenXcap,
        platform_ocr_capability(),
    ];
    if cfg!(feature = "screenpipe-runtime") {
        capabilities.push(Capabilities::CairnSensorV1ScreenScreenpipe);
    }
    capabilities.sort_by_key(|capability| serde_json::to_string(capability).unwrap_or_default());
    capabilities.dedup();
    capabilities
}

/// Probe a screen config, using the configured runtime when available.
#[must_use]
pub fn probe_config(config: &ScreenSensorConfig) -> ScreenProbe {
    let ocr_engine = ResolvedScreenOcrEngine::from_config(config.ocr.engine);
    if !config.enabled {
        return ScreenProbe {
            backend: config.backend,
            state: ScreenState::Disabled,
            mode: ScreenMode::Off,
            permission: ScreenPermission::NotRequested,
            ocr_engine,
            degradation: Some(ScreenDegradation::new(ScreenDegradationCode::Disabled)),
            focused_app: None,
        };
    }

    match config.backend {
        ScreenBackend::Xcap => XcapBackendRuntime.probe(config),
        ScreenBackend::Screenpipe => screenpipe_probe(config),
        _ => degraded_probe(config.backend, ocr_engine),
    }
}

#[cfg(target_os = "macos")]
fn permission_missing_probe(
    backend: ScreenBackend,
    ocr_engine: ResolvedScreenOcrEngine,
) -> ScreenProbe {
    ScreenProbe {
        backend,
        state: ScreenState::PermissionMissing,
        mode: ScreenMode::Snapshot,
        permission: ScreenPermission::Denied,
        ocr_engine,
        degradation: Some(ScreenDegradation::new(
            ScreenDegradationCode::PermissionMissing,
        )),
        focused_app: None,
    }
}

fn backend_unavailable_probe(
    backend: ScreenBackend,
    ocr_engine: ResolvedScreenOcrEngine,
) -> ScreenProbe {
    ScreenProbe {
        backend,
        state: ScreenState::Degraded,
        mode: ScreenMode::Off,
        permission: ScreenPermission::NotRequested,
        ocr_engine,
        degradation: Some(ScreenDegradation::new(
            ScreenDegradationCode::BackendUnavailable,
        )),
        focused_app: None,
    }
}

fn degraded_probe(backend: ScreenBackend, ocr_engine: ResolvedScreenOcrEngine) -> ScreenProbe {
    ScreenProbe {
        backend,
        state: ScreenState::Degraded,
        mode: ScreenMode::Off,
        permission: ScreenPermission::NotRequested,
        ocr_engine,
        degradation: Some(ScreenDegradation::new(ScreenDegradationCode::Degraded)),
        focused_app: None,
    }
}

#[cfg(target_os = "macos")]
fn xcap_probe(config: &ScreenSensorConfig) -> ScreenProbe {
    let ocr_engine = ResolvedScreenOcrEngine::from_config(config.ocr.engine);
    match primary_xcap_monitor().and_then(|monitor| {
        monitor
            .capture_region(0, 0, 1, 1)
            .map(|_| ())
            .map_err(screen_error_from_xcap)
    }) {
        Ok(()) => {
            let degradation = ocr_probe_degradation(config, ocr_engine);
            let state = if degradation.is_some() && config.ocr.engine != ScreenOcrEngine::Auto {
                ScreenState::Degraded
            } else {
                ScreenState::Enabled
            };
            ScreenProbe {
                backend: ScreenBackend::Xcap,
                state,
                mode: ScreenMode::Snapshot,
                permission: ScreenPermission::Granted,
                ocr_engine,
                degradation,
                focused_app: focused_window_metadata().map(|metadata| metadata.app),
            }
        }
        Err(err) => probe_from_screen_error(ScreenBackend::Xcap, ocr_engine, err.code()),
    }
}

#[cfg(not(target_os = "macos"))]
fn xcap_probe(config: &ScreenSensorConfig) -> ScreenProbe {
    backend_unavailable_probe(
        config.backend,
        ResolvedScreenOcrEngine::from_config(config.ocr.engine),
    )
}

#[cfg(target_os = "macos")]
fn ocr_probe_degradation(
    config: &ScreenSensorConfig,
    ocr_engine: ResolvedScreenOcrEngine,
) -> Option<ScreenDegradation> {
    match ocr_engine {
        ResolvedScreenOcrEngine::Tesseract if !tesseract_available() => {
            let message = if config.ocr.engine == ScreenOcrEngine::Auto {
                "tesseract OCR is unavailable; xcap screenshots will still capture metadata-only observations"
            } else {
                "tesseract OCR is unavailable; install tesseract or set sensors.screen.ocr.engine: off"
            };
            Some(ScreenDegradation::with_message(
                ScreenDegradationCode::BackendUnavailable,
                message,
            ))
        }
        ResolvedScreenOcrEngine::Vision | ResolvedScreenOcrEngine::Winrt => {
            Some(ScreenDegradation::with_message(
                ScreenDegradationCode::BackendUnavailable,
                "requested screen OCR engine is unavailable in this build",
            ))
        }
        ResolvedScreenOcrEngine::Off | ResolvedScreenOcrEngine::Tesseract => None,
    }
}

#[cfg(target_os = "macos")]
fn tesseract_available() -> bool {
    Command::new("tesseract")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(feature = "screenpipe-runtime"))]
fn screenpipe_probe(config: &ScreenSensorConfig) -> ScreenProbe {
    backend_unavailable_probe(
        config.backend,
        ResolvedScreenOcrEngine::from_config(config.ocr.engine),
    )
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_probe(config: &ScreenSensorConfig) -> ScreenProbe {
    let ocr_engine = ResolvedScreenOcrEngine::from_config(config.ocr.engine);
    match ensure_screenpipe_ready() {
        Ok(()) => ScreenProbe {
            backend: ScreenBackend::Screenpipe,
            state: ScreenState::Enabled,
            mode: ScreenMode::Continuous,
            permission: ScreenPermission::Granted,
            ocr_engine,
            degradation: None,
            focused_app: None,
        },
        Err(err) => ScreenProbe {
            backend: ScreenBackend::Screenpipe,
            state: ScreenState::Degraded,
            mode: ScreenMode::Off,
            permission: ScreenPermission::NotRequested,
            ocr_engine,
            degradation: Some(ScreenDegradation::with_message(
                err.code(),
                format!(
                    "screenpipe daemon unavailable at {}; start it with `npx screenpipe@latest record` or set CAIRN_SCREENPIPE_SPAWN=1",
                    screenpipe_base_url()
                ),
            )),
            focused_app: None,
        },
    }
}

#[cfg(not(feature = "screenpipe-runtime"))]
fn screenpipe_capture_observation(
    _config: &ScreenSensorConfig,
) -> Result<ScreenObservation, ScreenError> {
    Err(ScreenError::Unavailable(
        ScreenDegradationCode::BackendUnavailable,
    ))
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_capture_observation(
    config: &ScreenSensorConfig,
) -> Result<ScreenObservation, ScreenError> {
    Ok(screenpipe_capture_frame(config)?.observation)
}

#[cfg(not(feature = "screenpipe-runtime"))]
fn screenpipe_capture_png_snapshot(
    _config: &ScreenSensorConfig,
    _output_path: &Path,
) -> Result<ScreenCaptureReceipt, ScreenError> {
    Err(ScreenError::Unavailable(
        ScreenDegradationCode::BackendUnavailable,
    ))
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_capture_png_snapshot(
    config: &ScreenSensorConfig,
    output_path: &Path,
) -> Result<ScreenCaptureReceipt, ScreenError> {
    let capture = screenpipe_capture_frame(config)?;
    let bytes = if let Some(frame_id) = capture.frame_id {
        screenpipe_fetch_frame(frame_id)?
    } else if let Some(file_path) = capture.file_path.as_ref() {
        if !file_path.is_absolute() {
            return Err(ScreenError::CaptureFailed(format!(
                "screenpipe frame path is not absolute: {}",
                file_path.display()
            )));
        }
        fs::read(file_path).map_err(|err| {
            ScreenError::CaptureFailed(format!(
                "screenpipe frame read {}: {err}",
                file_path.display()
            ))
        })?
    } else {
        return Err(ScreenError::CaptureFailed(
            "screenpipe OCR content did not include frame_id or file_path".to_owned(),
        ));
    };

    let (width, height) = image_dimensions_from_bytes(&bytes)?;
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
    }
    fs::write(output_path, &bytes).map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;

    Ok(ScreenCaptureReceipt {
        output_path: output_path.to_path_buf(),
        width,
        height,
        observation: capture.observation,
    })
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_capture_frame(
    config: &ScreenSensorConfig,
) -> Result<ScreenpipeFrameCapture, ScreenError> {
    ensure_screenpipe_ready()?;
    let body = screenpipe_runtime_block_on(async {
        let client = reqwest::Client::new();
        let url = format!(
            "{}/search?content_type=vision&limit=1&offset=0&min_length=1",
            screenpipe_base_url()
        );
        let response = client
            .get(url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe search: {err}")))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe search body: {err}")))?;
        if !status.is_success() {
            return Err(ScreenError::CaptureFailed(format!(
                "screenpipe search returned HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        Ok(bytes.to_vec())
    })?;
    let mut capture = screenpipe_frame_capture_from_search_json(&body)?;
    apply_screen_ocr_config(config, &mut capture.observation);
    Ok(capture)
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_fetch_frame(frame_id: u64) -> Result<Vec<u8>, ScreenError> {
    screenpipe_runtime_block_on(async {
        let client = reqwest::Client::new();
        let url = format!("{}/frames/{frame_id}", screenpipe_base_url());
        let response = client
            .get(url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe frame: {err}")))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe frame body: {err}")))?;
        if status.is_success() {
            Ok(bytes.to_vec())
        } else {
            Err(ScreenError::CaptureFailed(format!(
                "screenpipe frame {frame_id} returned HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            )))
        }
    })
}

#[cfg(target_os = "macos")]
fn capture_xcap_observation(config: &ScreenSensorConfig) -> Result<ScreenObservation, ScreenError> {
    let monitor = primary_xcap_monitor()?;
    let image = monitor.capture_image().map_err(screen_error_from_xcap)?;
    let width = image.width();
    let height = image.height();
    let output_path = temp_ocr_image_path();
    image
        .save(&output_path)
        .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
    let ocr = recognize_png_ocr(config, &output_path, width, height)?;
    let _ = fs::remove_file(&output_path);
    Ok(observation_from_monitor(
        config, &monitor, width, height, ocr,
    ))
}

#[cfg(not(target_os = "macos"))]
fn capture_xcap_observation(
    _config: &ScreenSensorConfig,
) -> Result<ScreenObservation, ScreenError> {
    Err(ScreenError::Unavailable(
        ScreenDegradationCode::BackendUnavailable,
    ))
}

#[cfg(target_os = "macos")]
fn capture_xcap_png_snapshot(
    config: &ScreenSensorConfig,
    output_path: &Path,
) -> Result<ScreenCaptureReceipt, ScreenError> {
    let monitor = primary_xcap_monitor()?;
    let image = monitor.capture_image().map_err(screen_error_from_xcap)?;
    let width = image.width();
    let height = image.height();
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
    }
    image
        .save(output_path)
        .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
    let ocr = recognize_png_ocr(config, output_path, width, height)?;

    Ok(ScreenCaptureReceipt {
        output_path: output_path.to_path_buf(),
        width,
        height,
        observation: observation_from_monitor(config, &monitor, width, height, ocr),
    })
}

#[cfg(not(target_os = "macos"))]
fn capture_xcap_png_snapshot(
    _config: &ScreenSensorConfig,
    _output_path: &Path,
) -> Result<ScreenCaptureReceipt, ScreenError> {
    Err(ScreenError::Unavailable(
        ScreenDegradationCode::BackendUnavailable,
    ))
}

#[cfg(target_os = "macos")]
fn primary_xcap_monitor() -> Result<xcap::Monitor, ScreenError> {
    let mut monitors = xcap::Monitor::all().map_err(screen_error_from_xcap)?;
    if monitors.is_empty() {
        return Err(ScreenError::Unavailable(
            ScreenDegradationCode::BackendUnavailable,
        ));
    }
    let primary = monitors
        .iter()
        .position(|monitor| monitor.is_primary().unwrap_or(false))
        .unwrap_or(0);
    Ok(monitors.swap_remove(primary))
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FocusedWindowMetadata {
    app: String,
    title: String,
}

#[cfg(target_os = "macos")]
fn focused_window_metadata() -> Option<FocusedWindowMetadata> {
    xcap::Window::all()
        .ok()?
        .into_iter()
        .find(|window| window.is_focused().unwrap_or(false))
        .map(|window| FocusedWindowMetadata {
            app: window
                .app_name()
                .unwrap_or_else(|_| "unknown".to_owned())
                .trim()
                .to_owned(),
            title: window.title().unwrap_or_default(),
        })
        .filter(|metadata| !metadata.app.is_empty())
}

#[cfg(target_os = "macos")]
fn temp_ocr_image_path() -> PathBuf {
    let id = cairn_core::time::new_operation_id().0;
    std::env::temp_dir().join(format!("cairn-screen-ocr-{id}.png"))
}

#[cfg(target_os = "macos")]
fn recognize_png_ocr(
    config: &ScreenSensorConfig,
    image_path: &Path,
    _width: u32,
    _height: u32,
) -> Result<ScreenOcrCapture, ScreenError> {
    let requested = config.ocr.engine;
    let engine = ResolvedScreenOcrEngine::from_config(requested);
    match engine {
        ResolvedScreenOcrEngine::Off => Ok(ScreenOcrCapture {
            engine,
            result: ScreenOcrResult::default(),
        }),
        ResolvedScreenOcrEngine::Vision | ResolvedScreenOcrEngine::Winrt => Err(
            ScreenError::Unavailable(ScreenDegradationCode::BackendUnavailable),
        ),
        ResolvedScreenOcrEngine::Tesseract => {
            let result = match recognize_png_with_tesseract(image_path) {
                Ok(result) => result,
                Err(_err) if requested == ScreenOcrEngine::Auto => {
                    return Ok(ScreenOcrCapture {
                        engine: ResolvedScreenOcrEngine::Off,
                        result: ScreenOcrResult::default(),
                    });
                }
                Err(err) => return Err(err),
            };
            Ok(ScreenOcrCapture { engine, result })
        }
    }
}

#[cfg(target_os = "macos")]
fn recognize_png_with_tesseract(image_path: &Path) -> Result<ScreenOcrResult, ScreenError> {
    let output = Command::new("tesseract")
        .arg(image_path)
        .arg("stdout")
        .arg("--psm")
        .arg("6")
        .arg("tsv")
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ScreenError::Unavailable(ScreenDegradationCode::BackendUnavailable)
            } else {
                ScreenError::CaptureFailed(format!("failed to run tesseract: {err}"))
            }
        })?;

    if !output.status.success() {
        return Err(ScreenError::CaptureFailed(format!(
            "tesseract OCR failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|err| ScreenError::CaptureFailed(format!("tesseract output UTF-8: {err}")))?;
    ocr_result_from_tesseract_tsv(stdout)
}

#[cfg(any(target_os = "macos", test))]
fn ocr_result_from_tesseract_tsv(tsv: &str) -> Result<ScreenOcrResult, ScreenError> {
    let mut lines = tsv.lines();
    let header = lines
        .next()
        .ok_or_else(|| ScreenError::CaptureFailed("tesseract TSV missing header".to_owned()))?;
    let columns = header.split('\t').collect::<Vec<_>>();
    let index = |name: &str| {
        columns
            .iter()
            .position(|column| *column == name)
            .ok_or_else(|| {
                ScreenError::CaptureFailed(format!("tesseract TSV missing `{name}` column"))
            })
    };
    let left = index("left")?;
    let top = index("top")?;
    let width = index("width")?;
    let height = index("height")?;
    let conf = index("conf")?;
    let text = index("text")?;

    let mut words = Vec::new();
    let mut bounding_boxes = Vec::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() <= text {
            continue;
        }
        let word = fields[text].trim();
        if word.is_empty() || fields[conf].trim() == "-1" {
            continue;
        }
        let Some(box_) = tesseract_box(&fields, left, top, width, height) else {
            continue;
        };
        words.push(word.to_owned());
        bounding_boxes.push(box_);
    }

    Ok(ScreenOcrResult {
        text: words.join(" "),
        bounding_boxes,
    })
}

#[cfg(any(target_os = "macos", test))]
fn tesseract_box(
    fields: &[&str],
    left: usize,
    top: usize,
    width: usize,
    height: usize,
) -> Option<BoundingBox> {
    Some(BoundingBox {
        x: fields.get(left)?.parse().ok()?,
        y: fields.get(top)?.parse().ok()?,
        width: fields.get(width)?.parse().ok()?,
        height: fields.get(height)?.parse().ok()?,
    })
}

#[cfg(target_os = "macos")]
fn observation_from_monitor(
    _config: &ScreenSensorConfig,
    monitor: &xcap::Monitor,
    width: u32,
    height: u32,
    ocr: ScreenOcrCapture,
) -> ScreenObservation {
    let monitor_name = monitor.name().unwrap_or_else(|_| "screen".to_owned());
    let focused_window = focused_window_metadata();
    ScreenObservation {
        text: ocr.result.text,
        app: focused_window
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |window| window.app.clone()),
        window_title: focused_window.map_or_else(
            || format!("{monitor_name} ({width}x{height})"),
            |window| window.title,
        ),
        url: None,
        bounding_boxes: ocr.result.bounding_boxes,
        captured_at: rfc3339_timestamp_now(),
        sensor_label: XCAP_SENSOR_LABEL.to_owned(),
        backend: ScreenBackend::Xcap,
        ocr_engine: ocr.engine,
    }
}

#[cfg(target_os = "macos")]
fn probe_from_screen_error(
    backend: ScreenBackend,
    ocr_engine: ResolvedScreenOcrEngine,
    code: ScreenDegradationCode,
) -> ScreenProbe {
    match code {
        ScreenDegradationCode::PermissionMissing => permission_missing_probe(backend, ocr_engine),
        ScreenDegradationCode::BackendUnavailable => backend_unavailable_probe(backend, ocr_engine),
        ScreenDegradationCode::Disabled => ScreenProbe {
            backend,
            state: ScreenState::Disabled,
            mode: ScreenMode::Off,
            permission: ScreenPermission::NotRequested,
            ocr_engine,
            degradation: Some(ScreenDegradation::new(ScreenDegradationCode::Disabled)),
            focused_app: None,
        },
        ScreenDegradationCode::Degraded => degraded_probe(backend, ocr_engine),
    }
}

#[cfg(target_os = "macos")]
fn screen_error_from_xcap(err: impl std::fmt::Display) -> ScreenError {
    let message = err.to_string();
    if is_screen_permission_error(&message) {
        ScreenError::Unavailable(ScreenDegradationCode::PermissionMissing)
    } else {
        ScreenError::CaptureFailed(message)
    }
}

#[cfg(target_os = "macos")]
fn is_screen_permission_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("permission")
        || message.contains("denied")
        || message.contains("not granted")
        || message.contains("not authorized")
        || message.contains("screen recording")
        || message.contains("failed to copy data")
}

fn degradation_message(code: ScreenDegradationCode) -> &'static str {
    match code {
        ScreenDegradationCode::Disabled => "screen sensor is disabled in config",
        ScreenDegradationCode::PermissionMissing => "screen capture permission is missing",
        ScreenDegradationCode::BackendUnavailable => "screen backend is unavailable",
        ScreenDegradationCode::Degraded => "screen sensor is degraded",
    }
}

fn screen_backend_name(backend: ScreenBackend) -> &'static str {
    match backend {
        ScreenBackend::Xcap => "xcap",
        ScreenBackend::Screenpipe => "screenpipe",
        _ => "custom",
    }
}

fn ocr_engine_name(engine: ResolvedScreenOcrEngine) -> &'static str {
    match engine {
        ResolvedScreenOcrEngine::Vision => "vision",
        ResolvedScreenOcrEngine::Winrt => "winrt",
        ResolvedScreenOcrEngine::Tesseract => "tesseract",
        ResolvedScreenOcrEngine::Off => "off",
    }
}

#[cfg(test)]
fn screenpipe_observation_from_search_json(body: &[u8]) -> Result<ScreenObservation, ScreenError> {
    Ok(screenpipe_frame_capture_from_search_json(body)?.observation)
}

#[cfg(any(test, feature = "screenpipe-runtime"))]
fn screenpipe_frame_capture_from_search_json(
    body: &[u8],
) -> Result<ScreenpipeFrameCapture, ScreenError> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe response JSON: {err}")))?;
    let items = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ScreenError::CaptureFailed("screenpipe response missing data".to_owned()))?;
    let content = items
        .iter()
        .filter(|item| {
            item.get("type")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|ty| ty.eq_ignore_ascii_case("OCR"))
        })
        .find_map(|item| item.get("content"))
        .ok_or_else(|| {
            ScreenError::CaptureFailed("screenpipe response contained no OCR content".to_owned())
        })?;

    let text = content
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let app = content
        .get("app_name")
        .or_else(|| content.get("app"))
        .and_then(serde_json::Value::as_str)
        .filter(|app| !app.trim().is_empty())
        .unwrap_or("unknown")
        .to_owned();
    let window_title = content
        .get("window_name")
        .or_else(|| content.get("window_title"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let url = content
        .get("browser_url")
        .or_else(|| content.get("url"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(str::to_owned);
    let captured_at = content
        .get("timestamp")
        .or_else(|| content.get("captured_at"))
        .and_then(serde_json::Value::as_str)
        .filter(|timestamp| Rfc3339Timestamp::parse(*timestamp).is_ok())
        .map_or_else(cairn_core::time::now_rfc3339_seconds, str::to_owned);
    let frame_id = content.get("frame_id").and_then(json_u64).or_else(|| {
        content
            .get("frame")
            .and_then(|frame| frame.get("id"))
            .and_then(json_u64)
    });
    let file_path = content
        .get("file_path")
        .or_else(|| content.get("path"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from);

    Ok(ScreenpipeFrameCapture {
        observation: ScreenObservation {
            text,
            app,
            window_title,
            url,
            bounding_boxes: Vec::new(),
            captured_at,
            sensor_label: SCREENPIPE_SENSOR_LABEL.to_owned(),
            backend: ScreenBackend::Screenpipe,
            ocr_engine: ResolvedScreenOcrEngine::Off,
        },
        frame_id,
        file_path,
    })
}

#[cfg(any(test, feature = "screenpipe-runtime"))]
fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

#[cfg(any(test, feature = "screenpipe-runtime"))]
fn image_dimensions_from_bytes(bytes: &[u8]) -> Result<(u32, u32), ScreenError> {
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Ok((width, height));
    }

    Err(ScreenError::CaptureFailed(
        "screenpipe frame was not a PNG image".to_owned(),
    ))
}

#[cfg(feature = "screenpipe-runtime")]
fn ensure_screenpipe_ready() -> Result<(), ScreenError> {
    if screenpipe_health().is_ok() {
        return Ok(());
    }

    maybe_spawn_screenpipe()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if screenpipe_health().is_ok() {
            return Ok(());
        }
    }

    Err(ScreenError::Unavailable(
        ScreenDegradationCode::BackendUnavailable,
    ))
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_health() -> Result<(), ScreenError> {
    screenpipe_runtime_block_on(async {
        let url = format!("{}/health", screenpipe_base_url());
        let response = reqwest::Client::new()
            .get(url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe health: {err}")))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(ScreenError::Unavailable(
                ScreenDegradationCode::BackendUnavailable,
            ))
        }
    })
}

#[cfg(feature = "screenpipe-runtime")]
fn maybe_spawn_screenpipe() -> Result<(), ScreenError> {
    if !screenpipe_spawn_enabled() {
        return Err(ScreenError::Unavailable(
            ScreenDegradationCode::BackendUnavailable,
        ));
    }

    let mut direct = Command::new("screenpipe");
    direct
        .arg("record")
        .env("SCREENPIPE_NO_REMINDERS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match direct.spawn() {
        Ok(_child) => return Ok(()),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(ScreenError::CaptureFailed(format!(
                "failed to start screenpipe: {err}"
            )));
        }
        Err(_) => {}
    }

    Command::new("npx")
        .args(["screenpipe@latest", "record"])
        .env("SCREENPIPE_NO_REMINDERS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ScreenError::Unavailable(ScreenDegradationCode::BackendUnavailable)
            } else {
                ScreenError::CaptureFailed(format!("failed to start screenpipe via npx: {err}"))
            }
        })
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_spawn_enabled() -> bool {
    std::env::var("CAIRN_SCREENPIPE_SPAWN")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_base_url() -> String {
    std::env::var("CAIRN_SCREENPIPE_URL")
        .unwrap_or_else(|_| "http://localhost:3030".to_owned())
        .trim_end_matches('/')
        .to_owned()
}

#[cfg(feature = "screenpipe-runtime")]
fn screenpipe_runtime_block_on<T>(
    future: impl std::future::Future<Output = Result<T, ScreenError>>,
) -> Result<T, ScreenError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| ScreenError::CaptureFailed(format!("screenpipe runtime: {err}")))?
        .block_on(future)
}

fn truncate_text_to_budget(text: &mut String, max_bytes: u32) {
    let max_bytes = max_bytes as usize;
    if text.len() <= max_bytes {
        return;
    }

    let mut truncate_at = max_bytes;
    while !text.is_char_boundary(truncate_at) {
        truncate_at -= 1;
    }
    text.truncate(truncate_at);
}

#[cfg(target_os = "macos")]
fn rfc3339_timestamp_now() -> String {
    cairn_core::time::now_rfc3339_seconds()
}

fn platform_default_ocr_engine() -> ResolvedScreenOcrEngine {
    if cfg!(target_os = "windows") {
        ResolvedScreenOcrEngine::Winrt
    } else {
        ResolvedScreenOcrEngine::Tesseract
    }
}

fn platform_ocr_capability() -> Capabilities {
    if cfg!(target_os = "windows") {
        Capabilities::CairnSensorV1ScreenOcrWinrt
    } else {
        Capabilities::CairnSensorV1ScreenOcrTesseract
    }
}

#[cfg(test)]
mod tests {
    use cairn_core::config::{ScreenBackend, ScreenOcrEngine, ScreenSensorConfig};
    use cairn_core::domain::{CaptureEventId, CapturePayload, Rfc3339Timestamp, SourceFamily};
    use sha2::{Digest as _, Sha256};

    use super::*;
    use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings};

    #[derive(Clone)]
    struct FakeBackend {
        probe: ScreenProbe,
        observation: ScreenObservation,
    }

    #[derive(Debug, Clone, Copy)]
    struct RejectingPolicy;

    impl ScreenBackendRuntime for FakeBackend {
        fn probe(&self, _config: &ScreenSensorConfig) -> ScreenProbe {
            self.probe.clone()
        }

        fn capture_snapshot(
            &self,
            _config: &ScreenSensorConfig,
        ) -> Result<ScreenObservation, ScreenError> {
            Ok(self.observation.clone())
        }
    }

    impl ScreenPolicy for RejectingPolicy {
        fn apply(&self, _observation: ScreenObservation) -> Result<ScreenObservation, ScreenError> {
            Err(ScreenError::CaptureFailed("rejected by policy".to_owned()))
        }
    }

    fn enabled_config() -> ScreenSensorConfig {
        ScreenSensorConfig {
            enabled: true,
            ..ScreenSensorConfig::default()
        }
    }

    fn fake_probe() -> ScreenProbe {
        ScreenProbe {
            backend: ScreenBackend::Xcap,
            state: ScreenState::Enabled,
            mode: ScreenMode::Snapshot,
            permission: ScreenPermission::Granted,
            ocr_engine: ResolvedScreenOcrEngine::Tesseract,
            degradation: None,
            focused_app: Some("Code".to_owned()),
        }
    }

    fn fake_observation(text: &str) -> ScreenObservation {
        ScreenObservation {
            text: text.to_owned(),
            app: "Code".to_owned(),
            window_title: "screen.rs".to_owned(),
            url: Some("file:///tmp/screen.rs".to_owned()),
            bounding_boxes: vec![BoundingBox {
                x: 1,
                y: 2,
                width: 300,
                height: 40,
            }],
            captured_at: "2026-05-12T12:00:00Z".to_owned(),
            sensor_label: XCAP_SENSOR_LABEL.to_owned(),
            backend: ScreenBackend::Xcap,
            ocr_engine: ResolvedScreenOcrEngine::Tesseract,
        }
    }

    fn event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid test ULID")
    }

    fn captured_at() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-05-12T12:00:00Z").expect("valid test timestamp")
    }

    fn screen_enabled_local_config() -> LocalSensorConfig {
        let mut config = LocalSensorConfig::all_disabled();
        config.screen = SensorSettings::enabled();
        config
    }

    fn screen_local_config_with_byte_budget(max_bytes: usize) -> LocalSensorConfig {
        let mut config = screen_enabled_local_config();
        config.screen.budget.max_bytes = Some(max_bytes);
        config
    }

    fn payload_hash(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    struct CaptureMustNotRunBackend {
        probe: ScreenProbe,
    }

    impl ScreenBackendRuntime for CaptureMustNotRunBackend {
        fn probe(&self, _config: &ScreenSensorConfig) -> ScreenProbe {
            self.probe.clone()
        }

        fn capture_snapshot(
            &self,
            _config: &ScreenSensorConfig,
        ) -> Result<ScreenObservation, ScreenError> {
            Err(ScreenError::CaptureFailed(
                "capture should not run for disallowed app".to_owned(),
            ))
        }
    }

    #[test]
    fn disabled_sensor_emits_nothing() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("ignored"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);
        let result = sensor
            .capture_snapshot(&ScreenSensorConfig::default())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn fake_backend_emits_ocr_text_and_metadata() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);
        let result = sensor.capture_snapshot(&enabled_config()).unwrap().unwrap();
        assert_eq!(result.text, "meeting notes");
        assert_eq!(result.app, "Code");
        assert_eq!(result.window_title, "screen.rs");
        assert_eq!(result.sensor_label, XCAP_SENSOR_LABEL);
        assert_eq!(result.bounding_boxes.len(), 1);
    }

    impl ScreenCaptureRuntime for FakeBackend {
        fn capture_png_snapshot(
            &self,
            _config: &ScreenSensorConfig,
            output_path: &std::path::Path,
        ) -> Result<ScreenCaptureReceipt, ScreenError> {
            std::fs::write(output_path, b"fake-png")
                .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
            Ok(ScreenCaptureReceipt {
                output_path: output_path.to_path_buf(),
                width: 10,
                height: 20,
                observation: self.observation.clone(),
            })
        }
    }

    #[cfg(unix)]
    #[derive(Clone)]
    struct CleanupFailingBackend {
        probe: ScreenProbe,
        observation: ScreenObservation,
    }

    #[cfg(unix)]
    impl ScreenBackendRuntime for CleanupFailingBackend {
        fn probe(&self, _config: &ScreenSensorConfig) -> ScreenProbe {
            self.probe.clone()
        }

        fn capture_snapshot(
            &self,
            _config: &ScreenSensorConfig,
        ) -> Result<ScreenObservation, ScreenError> {
            Ok(self.observation.clone())
        }
    }

    #[cfg(unix)]
    impl ScreenCaptureRuntime for CleanupFailingBackend {
        fn capture_png_snapshot(
            &self,
            _config: &ScreenSensorConfig,
            output_path: &std::path::Path,
        ) -> Result<ScreenCaptureReceipt, ScreenError> {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::write(output_path, b"fake-png")
                .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
            let parent = output_path
                .parent()
                .expect("test output path has parent directory");
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500))
                .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
            Ok(ScreenCaptureReceipt {
                output_path: output_path.to_path_buf(),
                width: 10,
                height: 20,
                observation: self.observation.clone(),
            })
        }
    }

    #[test]
    fn fake_backend_writes_png_receipt_and_metadata() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let receipt = sensor
            .capture_png_snapshot(&enabled_config(), &output_path)
            .unwrap()
            .unwrap();

        assert_eq!(receipt.output_path, output_path);
        assert_eq!(receipt.width, 10);
        assert_eq!(receipt.height, 20);
        assert_eq!(receipt.observation.text, "meeting notes");
        assert_eq!(std::fs::read(&receipt.output_path).unwrap(), b"fake-png");
    }

    #[test]
    fn blur_password_fields_drops_text_snapshot_before_policy() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("password=abc123"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor.capture_snapshot(&enabled_config()).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn blur_password_fields_removes_png_artifact_and_drops_capture() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("password=abc123"),
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor
            .capture_png_snapshot(&enabled_config(), &output_path)
            .unwrap();

        assert!(result.is_none());
        assert!(!output_path.exists());
    }

    #[test]
    fn png_capture_outcome_distinguishes_privacy_filtered_skip() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("password=abc123"),
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let outcome = sensor
            .capture_png_snapshot_outcome(&enabled_config(), &output_path)
            .unwrap();

        let ScreenCaptureOutcome::Skipped(skip) = outcome else {
            panic!("expected privacy skip");
        };
        assert_eq!(skip.reason, ScreenCaptureSkipReason::PrivacyFiltered);
        assert!(skip.artifact_created);
        assert!(skip.observed_bytes > 0);
        assert!(!output_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn png_capture_cleanup_failure_preserves_privacy_skip_context() {
        use std::os::unix::fs::PermissionsExt as _;

        let backend = CleanupFailingBackend {
            probe: fake_probe(),
            observation: fake_observation("password=abc123"),
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let outcome = sensor
            .capture_png_snapshot_outcome(&enabled_config(), &output_path)
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        let ScreenCaptureOutcome::CleanupFailed { skip, error } = outcome else {
            panic!("expected cleanup failure");
        };
        assert_eq!(skip.reason, ScreenCaptureSkipReason::PrivacyFiltered);
        assert!(skip.artifact_created);
        assert!(skip.observed_bytes > 0);
        assert!(matches!(error, ScreenError::CaptureFailed(_)));
        assert!(output_path.exists());
    }

    #[test]
    fn cleanup_failure_conversion_preserves_error() {
        let skip = ScreenCaptureSkip {
            reason: ScreenCaptureSkipReason::PrivacyFiltered,
            observed_bytes: 42,
            artifact_created: true,
        };
        let outcome = ScreenCaptureOutcome::CleanupFailed {
            skip,
            error: ScreenError::CaptureFailed("cleanup failed".to_owned()),
        };

        let err = outcome
            .into_result_receipt()
            .expect_err("cleanup failure must not become None");
        assert_eq!(err, ScreenError::CaptureFailed("cleanup failed".to_owned()));
    }

    #[test]
    fn blur_password_fields_drops_png_when_ocr_is_off() {
        let mut observation = fake_observation("meeting notes");
        observation.ocr_engine = ResolvedScreenOcrEngine::Off;
        let backend = FakeBackend {
            probe: fake_probe(),
            observation,
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor
            .capture_png_snapshot(&enabled_config(), &output_path)
            .unwrap();

        assert!(result.is_none());
        assert!(!output_path.exists());
    }

    #[test]
    fn frame_budget_rejects_second_capture_from_same_sensor() {
        let mut config = enabled_config();
        config.budget.max_frames_per_minute = 1;
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let first = sensor.capture_snapshot(&config).unwrap();
        assert!(first.is_some());

        let err = sensor.capture_snapshot(&config).unwrap_err();
        assert_eq!(err.code(), ScreenDegradationCode::Degraded);
    }

    #[test]
    fn shared_frame_budget_rejects_across_sensor_instances() {
        let mut config = enabled_config();
        config.budget.max_frames_per_minute = 1;
        let frame_budget = SharedFrameBudget::default();
        let first_sensor = ScreenSensor::with_frame_budget(
            FakeBackend {
                probe: fake_probe(),
                observation: fake_observation("first"),
            },
            NoopScreenPolicy,
            frame_budget.clone(),
        );
        let second_sensor = ScreenSensor::with_frame_budget(
            FakeBackend {
                probe: fake_probe(),
                observation: fake_observation("second"),
            },
            NoopScreenPolicy,
            frame_budget,
        );

        let first = first_sensor.capture_snapshot(&config).unwrap();
        assert!(first.is_some());

        let err = second_sensor.capture_snapshot(&config).unwrap_err();
        assert_eq!(err.code(), ScreenDegradationCode::Degraded);
    }

    #[test]
    fn persistent_frame_budget_rejects_across_budget_instances() {
        let dir = tempfile::tempdir().unwrap();
        let budget_path = dir.path().join("screen-budget");
        let first_budget = SharedFrameBudget::persistent_at(budget_path.clone());
        let second_budget = SharedFrameBudget::persistent_at(budget_path);

        first_budget.admit(1).unwrap();

        let err = second_budget.admit(1).unwrap_err();
        assert_eq!(err.code(), ScreenDegradationCode::Degraded);
    }

    #[test]
    fn allow_list_drops_unlisted_apps() {
        let mut config = enabled_config();
        config.allow_apps = vec!["Terminal".to_owned()];
        let mut probe = fake_probe();
        probe.focused_app = Some("Terminal".to_owned());
        let backend = FakeBackend {
            probe,
            observation: fake_observation("meeting notes"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor.capture_snapshot(&config).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn png_capture_outcome_distinguishes_allow_list_skip() {
        let mut config = enabled_config();
        config.allow_apps = vec!["Terminal".to_owned()];
        let mut probe = fake_probe();
        probe.focused_app = Some("Terminal".to_owned());
        let backend = FakeBackend {
            probe,
            observation: fake_observation("meeting notes"),
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let outcome = sensor
            .capture_png_snapshot_outcome(&config, &output_path)
            .unwrap();

        let ScreenCaptureOutcome::Skipped(skip) = outcome else {
            panic!("expected allow-list skip");
        };
        assert_eq!(skip.reason, ScreenCaptureSkipReason::AllowList);
        assert!(skip.artifact_created);
        assert!(skip.observed_bytes > 0);
        assert!(!output_path.exists());
    }

    #[test]
    fn allow_list_drops_disallowed_preflight_app_before_capture() {
        let mut config = enabled_config();
        config.allow_apps = vec!["Terminal".to_owned()];
        let backend = CaptureMustNotRunBackend {
            probe: fake_probe(),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor.capture_snapshot(&config).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn allow_list_keeps_matching_apps() {
        let mut config = enabled_config();
        config.allow_apps = vec!["Code".to_owned()];
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor.capture_snapshot(&config).unwrap().unwrap();

        assert_eq!(result.app, "Code");
    }

    #[test]
    fn disabled_probe_has_status_ready_shape() {
        let probe = probe_config(&ScreenSensorConfig::default());
        assert_eq!(probe.backend, ScreenBackend::Xcap);
        assert_eq!(probe.state, ScreenState::Disabled);
        assert_eq!(probe.mode, ScreenMode::Off);
        assert_eq!(probe.permission, ScreenPermission::NotRequested);
        let degradation = probe.degradation.unwrap();
        assert_eq!(degradation.code, ScreenDegradationCode::Disabled);
        assert_eq!(degradation.message, "screen sensor is disabled in config");
    }

    #[test]
    fn compiled_capabilities_include_xcap_and_one_platform_ocr() {
        let capabilities = compiled_capabilities();
        assert!(capabilities.contains(&Capabilities::CairnSensorV1ScreenXcap));

        let ocr_capabilities = capabilities
            .iter()
            .filter(|capability| {
                matches!(
                    capability,
                    Capabilities::CairnSensorV1ScreenOcrVision
                        | Capabilities::CairnSensorV1ScreenOcrWinrt
                        | Capabilities::CairnSensorV1ScreenOcrTesseract
                )
            })
            .count();
        assert_eq!(ocr_capabilities, 1);

        assert_eq!(
            capabilities.contains(&Capabilities::CairnSensorV1ScreenScreenpipe),
            cfg!(feature = "screenpipe-runtime")
        );
    }

    #[test]
    fn screenpipe_without_feature_reports_backend_unavailable() {
        let config = ScreenSensorConfig {
            enabled: true,
            backend: ScreenBackend::Screenpipe,
            ..ScreenSensorConfig::default()
        };
        let probe = probe_config(&config);
        if cfg!(feature = "screenpipe-runtime") {
            assert_eq!(probe.backend, ScreenBackend::Screenpipe);
            assert_ne!(probe.state, ScreenState::Disabled);
        } else {
            assert_eq!(probe.state, ScreenState::Degraded);
            assert_eq!(
                probe.degradation_code(),
                Some(ScreenDegradationCode::BackendUnavailable)
            );
        }
    }

    #[test]
    fn permission_missing_probe_blocks_capture() {
        let backend = FakeBackend {
            probe: ScreenProbe {
                state: ScreenState::PermissionMissing,
                permission: ScreenPermission::Denied,
                degradation: Some(ScreenDegradation::new(
                    ScreenDegradationCode::PermissionMissing,
                )),
                ..fake_probe()
            },
            observation: fake_observation("not emitted"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);
        let err = sensor.capture_snapshot(&enabled_config()).unwrap_err();
        assert_eq!(err.code(), ScreenDegradationCode::PermissionMissing);
    }

    #[test]
    fn text_budget_truncates_on_utf8_boundary() {
        let mut config = enabled_config();
        config.budget.max_text_bytes_per_event = 5;
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("éclair"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);
        let result = sensor.capture_snapshot(&config).unwrap().unwrap();
        assert_eq!(result.text, "écla");
    }

    #[test]
    fn policy_redacts_password_text() {
        let mut config = enabled_config();
        config.blur_password_fields = false;
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("password=abc123"),
        };
        let sensor = ScreenSensor::new(backend, BasicScreenPolicy);
        let result = sensor.capture_snapshot(&config).unwrap().unwrap();
        assert_eq!(result.text, "[redacted]");
    }

    #[test]
    fn rejecting_policy_returns_error() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let sensor = ScreenSensor::new(backend, RejectingPolicy);

        let err = sensor.capture_snapshot(&enabled_config()).unwrap_err();

        assert_eq!(
            err,
            ScreenError::CaptureFailed("rejected by policy".to_owned())
        );
    }

    #[test]
    fn rejecting_policy_removes_png_artifact() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("snapshot.png");
        let sensor = ScreenSensor::new(backend, RejectingPolicy);

        let err = sensor
            .capture_png_snapshot_outcome(&enabled_config(), &output_path)
            .unwrap_err();

        assert_eq!(
            err,
            ScreenError::CaptureFailed("rejected by policy".to_owned())
        );
        assert!(!output_path.exists());
    }

    #[test]
    fn screen_observation_emits_valid_capture_event() {
        let outcome = emit(
            &screen_enabled_local_config(),
            ScreenEventObservation {
                event_id: event_id(),
                captured_at: captured_at(),
                observation: fake_observation("meeting notes"),
                refs: None,
            },
        );

        let event = match outcome {
            EmitOutcome::Emitted(event) => event,
            other @ EmitOutcome::Dropped { .. } => panic!("expected emitted event, got {other:?}"),
        };

        assert_eq!(event.source_family, SourceFamily::Screen);
        assert_eq!(event.sensor_id.as_str(), XCAP_SENSOR_LABEL);
        assert_eq!(
            event.payload_ref,
            "sources/screen/01ARZ3NDEKTSV4RRFFQ69G5FAV.json"
        );
        match event.payload {
            CapturePayload::Screen {
                ref app,
                ref window_title,
                ref url,
            } => {
                assert_eq!(app, "Code");
                assert_eq!(window_title, "screen.rs");
                assert_eq!(url.as_deref(), Some("file:///tmp/screen.rs"));
            }
            other => panic!("expected screen payload, got {other:?}"),
        }
        event
            .validate_for_capture()
            .expect("screen event validates");
    }

    #[test]
    fn screen_emit_budgets_sanitized_payload_not_only_ocr_text() {
        let mut observation = fake_observation("ok");
        observation.window_title = "w".repeat(512);
        let payload_bytes =
            screen_observation_budgeted_payload_bytes(&observation).expect("payload bytes");

        let outcome = emit(
            &screen_local_config_with_byte_budget(payload_bytes - 1),
            ScreenEventObservation {
                event_id: event_id(),
                captured_at: captured_at(),
                observation,
                refs: None,
            },
        );

        assert_eq!(
            outcome,
            EmitOutcome::Dropped {
                sensor: SensorKind::Screen,
                reason: DropReason::BudgetExceeded,
            }
        );
    }

    #[test]
    fn screen_emit_redacts_ocr_text_before_payload_hashing() {
        let mut observation = fake_observation("SCREEN_TOKEN=supersecret");
        observation.window_title = "PASSWORD=window-secret".to_owned();

        let outcome = emit(
            &screen_enabled_local_config(),
            ScreenEventObservation {
                event_id: event_id(),
                captured_at: captured_at(),
                observation: observation.clone(),
                refs: None,
            },
        );
        let event = outcome.event().expect("event emitted");

        let unredacted = raw_payload_bytes(
            &observation,
            "SCREEN_TOKEN=supersecret",
            "PASSWORD=window-secret",
            observation.url.as_deref(),
        )
        .expect("serialize unredacted fixture");
        let redacted = raw_payload_bytes(
            &observation,
            "SCREEN_TOKEN=[REDACTED]",
            "PASSWORD=[REDACTED]",
            observation.url.as_deref(),
        )
        .expect("serialize redacted fixture");

        assert_ne!(event.payload_hash.as_str(), payload_hash(&unredacted));
        assert_eq!(event.payload_hash.as_str(), payload_hash(&redacted));
    }

    #[test]
    fn screen_emit_rejects_private_key_ocr_text() {
        let outcome = emit(
            &screen_enabled_local_config(),
            ScreenEventObservation {
                event_id: event_id(),
                captured_at: captured_at(),
                observation: fake_observation("-----BEGIN PRIVATE KEY-----\nsecret"),
                refs: None,
            },
        );

        assert_eq!(
            outcome,
            EmitOutcome::Dropped {
                sensor: SensorKind::Screen,
                reason: DropReason::PolicyRejected("private key block".to_owned()),
            }
        );
    }

    #[test]
    fn screen_emit_respects_local_screen_enablement() {
        let outcome = emit(
            &LocalSensorConfig::all_disabled(),
            ScreenEventObservation {
                event_id: event_id(),
                captured_at: captured_at(),
                observation: fake_observation("meeting notes"),
                refs: None,
            },
        );

        assert_eq!(
            outcome,
            EmitOutcome::Dropped {
                sensor: SensorKind::Screen,
                reason: DropReason::Disabled,
            }
        );
    }

    #[test]
    fn screenpipe_ocr_json_maps_to_screen_observation() {
        let body = br#"{
            "data": [
                {
                    "type": "OCR",
                    "content": {
                        "text": "build failed in screen.rs",
                        "timestamp": "2026-05-12T12:00:00Z",
                        "frame_id": "42",
                        "file_path": "/tmp/screenpipe-frame.png",
                        "app_name": "Code",
                        "window_name": "screen.rs",
                        "browser_url": "file:///tmp/screen.rs",
                        "focused": true
                    }
                }
            ],
            "pagination": { "limit": 1, "offset": 0, "total": 1 }
        }"#;

        let observation =
            screenpipe_observation_from_search_json(body).expect("screenpipe OCR response parses");

        assert_eq!(observation.text, "build failed in screen.rs");
        assert_eq!(observation.app, "Code");
        assert_eq!(observation.window_title, "screen.rs");
        assert_eq!(observation.url.as_deref(), Some("file:///tmp/screen.rs"));
        assert_eq!(observation.sensor_label, SCREENPIPE_SENSOR_LABEL);
        assert_eq!(observation.backend, ScreenBackend::Screenpipe);

        let capture =
            screenpipe_frame_capture_from_search_json(body).expect("screenpipe frame parses");
        assert_eq!(capture.frame_id, Some(42));
        assert_eq!(
            capture.file_path.as_deref(),
            Some(Path::new("/tmp/screenpipe-frame.png"))
        );
    }

    #[test]
    fn screenpipe_configured_ocr_off_drops_text_from_search_json() {
        let body = br#"{
            "data": [
                {
                    "type": "OCR",
                    "content": {
                        "text": "secret project notes",
                        "timestamp": "2026-05-12T12:00:00Z",
                        "frame_id": "42",
                        "app_name": "Code",
                        "window_name": "screen.rs"
                    }
                }
            ],
            "pagination": { "limit": 1, "offset": 0, "total": 1 }
        }"#;
        let mut config = enabled_config();
        config.backend = ScreenBackend::Screenpipe;
        config.ocr.engine = ScreenOcrEngine::Off;
        let mut capture =
            screenpipe_frame_capture_from_search_json(body).expect("screenpipe frame parses");

        apply_screen_ocr_config(&config, &mut capture.observation);

        assert_eq!(capture.observation.text, "");
        assert!(capture.observation.bounding_boxes.is_empty());
        assert_eq!(capture.observation.ocr_engine, ResolvedScreenOcrEngine::Off);
    }

    #[test]
    fn screenpipe_frame_dimensions_parse_png_and_reject_jpeg() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640_u32.to_be_bytes());
        png.extend_from_slice(&480_u32.to_be_bytes());
        assert_eq!(
            image_dimensions_from_bytes(&png).expect("png dimensions"),
            (640, 480)
        );

        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x01,
            0x00, 0x48, 0x00, 0x48, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0x2c, 0x02,
            0x58, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01,
        ];
        let err = image_dimensions_from_bytes(&jpeg).expect_err("jpeg should not satisfy PNG API");
        assert_eq!(
            err,
            ScreenError::CaptureFailed("screenpipe frame was not a PNG image".to_owned())
        );
    }

    #[test]
    fn tesseract_tsv_parses_text_and_boxes() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t10\t20\t30\t40\t88.5\tHello\n\
                   5\t1\t1\t1\t1\t2\t45\t20\t50\t40\t91.0\tworld\n";

        let result = ocr_result_from_tesseract_tsv(tsv).expect("valid tesseract TSV parses");

        assert_eq!(result.text, "Hello world");
        assert_eq!(
            result.bounding_boxes,
            vec![
                BoundingBox {
                    x: 10,
                    y: 20,
                    width: 30,
                    height: 40,
                },
                BoundingBox {
                    x: 45,
                    y: 20,
                    width: 50,
                    height: 40,
                },
            ]
        );
    }
}
