//! Mockable runtime boundary for local screen capture.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::config::{ScreenBackend, ScreenOcrEngine, ScreenSensorConfig};
use cairn_core::generated::common::Capabilities;

const FRAME_BUDGET_WINDOW: Duration = Duration::from_mins(1);

/// Sensor label used by the in-process xcap backend.
pub const XCAP_SENSOR_LABEL: &str = "snr:local:screen:xcap:v1";
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

/// Errors emitted by the screen runtime boundary.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
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

/// Mockable screen sensor that composes backend and policy.
#[derive(Debug)]
pub struct ScreenSensor<B, P> {
    backend: B,
    policy: P,
    frame_timestamps: Mutex<VecDeque<Instant>>,
}

impl<B, P> ScreenSensor<B, P>
where
    B: ScreenBackendRuntime,
    P: ScreenPolicy,
{
    /// Create a screen sensor from a backend runtime and policy.
    #[must_use]
    pub fn new(backend: B, policy: P) -> Self {
        Self {
            backend,
            policy,
            frame_timestamps: Mutex::new(VecDeque::new()),
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
        truncate_text_to_budget(
            &mut observation.text,
            config.budget.max_text_bytes_per_event,
        );
        Ok(Some(self.policy.apply(observation)?))
    }

    fn admit_frame(&self, max_frames_per_minute: u32) -> Result<(), ScreenError> {
        let now = Instant::now();
        let mut timestamps = self
            .frame_timestamps
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
}

impl<B, P> ScreenSensor<B, P>
where
    B: ScreenCaptureRuntime,
    P: ScreenPolicy,
{
    /// Capture a single PNG snapshot, returning `None` when disabled or filtered out.
    pub fn capture_png_snapshot(
        &self,
        config: &ScreenSensorConfig,
        output_path: &Path,
    ) -> Result<Option<ScreenCaptureReceipt>, ScreenError> {
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

        let mut receipt = self.backend.capture_png_snapshot(config, output_path)?;
        if !config.allow_apps.is_empty() && !config.allow_apps.contains(&receipt.observation.app) {
            return Ok(None);
        }
        truncate_text_to_budget(
            &mut receipt.observation.text,
            config.budget.max_text_bytes_per_event,
        );
        receipt.observation = self.policy.apply(receipt.observation)?;
        Ok(Some(receipt))
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
        ScreenBackend::Screenpipe => {
            if cfg!(feature = "screenpipe-runtime") {
                permission_missing_probe(config.backend, ocr_engine)
            } else {
                backend_unavailable_probe(config.backend, ocr_engine)
            }
        }
        _ => degraded_probe(config.backend, ocr_engine),
    }
}

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
        Ok(()) => ScreenProbe {
            backend: ScreenBackend::Xcap,
            state: ScreenState::Enabled,
            mode: ScreenMode::Snapshot,
            permission: ScreenPermission::Granted,
            ocr_engine,
            degradation: None,
            focused_app: None,
        },
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
fn capture_xcap_observation(config: &ScreenSensorConfig) -> Result<ScreenObservation, ScreenError> {
    let monitor = primary_xcap_monitor()?;
    let image = monitor.capture_image().map_err(screen_error_from_xcap)?;
    Ok(observation_from_monitor(
        config,
        &monitor,
        image.width(),
        image.height(),
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
        std::fs::create_dir_all(parent)
            .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;
    }
    image
        .save(output_path)
        .map_err(|err| ScreenError::CaptureFailed(err.to_string()))?;

    Ok(ScreenCaptureReceipt {
        output_path: output_path.to_path_buf(),
        width,
        height,
        observation: observation_from_monitor(config, &monitor, width, height),
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
fn observation_from_monitor(
    config: &ScreenSensorConfig,
    monitor: &xcap::Monitor,
    width: u32,
    height: u32,
) -> ScreenObservation {
    let monitor_name = monitor.name().unwrap_or_else(|_| "screen".to_owned());
    ScreenObservation {
        text: String::new(),
        app: "unknown".to_owned(),
        window_title: format!("{monitor_name} ({width}x{height})"),
        url: None,
        bounding_boxes: Vec::new(),
        captured_at: unix_timestamp_now(),
        sensor_label: XCAP_SENSOR_LABEL.to_owned(),
        backend: ScreenBackend::Xcap,
        ocr_engine: ResolvedScreenOcrEngine::from_config(config.ocr.engine),
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
fn unix_timestamp_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format!("unix:{seconds}")
}

fn platform_default_ocr_engine() -> ResolvedScreenOcrEngine {
    if cfg!(target_os = "macos") {
        ResolvedScreenOcrEngine::Vision
    } else if cfg!(target_os = "windows") {
        ResolvedScreenOcrEngine::Winrt
    } else {
        ResolvedScreenOcrEngine::Tesseract
    }
}

fn platform_ocr_capability() -> Capabilities {
    if cfg!(target_os = "macos") {
        Capabilities::CairnSensorV1ScreenOcrVision
    } else if cfg!(target_os = "windows") {
        Capabilities::CairnSensorV1ScreenOcrWinrt
    } else {
        Capabilities::CairnSensorV1ScreenOcrTesseract
    }
}

#[cfg(test)]
mod tests {
    use cairn_core::config::{ScreenBackend, ScreenSensorConfig};

    use super::*;

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
    fn allow_list_drops_unlisted_apps() {
        let mut config = enabled_config();
        config.allow_apps = vec!["Terminal".to_owned()];
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("meeting notes"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);

        let result = sensor.capture_snapshot(&config).unwrap();

        assert!(result.is_none());
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
            assert_ne!(
                probe.degradation_code(),
                Some(ScreenDegradationCode::BackendUnavailable)
            );
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
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("password=abc123"),
        };
        let sensor = ScreenSensor::new(backend, BasicScreenPolicy);
        let result = sensor.capture_snapshot(&enabled_config()).unwrap().unwrap();
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
}
