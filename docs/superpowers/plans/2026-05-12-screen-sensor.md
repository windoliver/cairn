# Screen Sensor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the issue #86 screen sensor contract slice: default-off config, xcap default backend, optional screenpipe capability, mocked OCR observations, degradation reporting, and `cairn status` sensor state.

**Architecture:** Keep capture mechanics in `cairn-sensors-local` behind a mockable backend trait, keep config/status data types in `cairn-core`, and let `cairn-cli status` assemble the generated status response from config plus sensor probes. Real desktop APIs are represented by feature-gated slots; tests use fake backends only.

**Tech Stack:** Rust 2024, serde, thiserror, JSON Schema IDL, `cairn-codegen`, `cargo nextest`, `insta`.

---

## File Structure

- Modify `crates/cairn-core/src/config/mod.rs`: add typed screen config, screen enums, budget validation, and config tests.
- Modify `crates/cairn-cli/tests/config.rs`: verify env overrides for nested screen config.
- Modify `crates/cairn-idl/schema/capabilities/capabilities.json`: add screen sensor capability strings.
- Modify `crates/cairn-idl/schema/prelude/status.json`: add `sensors.screen` status block.
- Regenerate `crates/cairn-core/src/generated/common/mod.rs`, `crates/cairn-core/src/generated/status.rs`, `crates/cairn-core/src/generated/verbs/mod.rs`, and `crates/cairn-mcp/src/generated/schemas/**` through `cairn-codegen`.
- Modify `crates/cairn-sensors-local/Cargo.toml`: add an empty `screenpipe-runtime` feature and `thiserror`.
- Create `crates/cairn-sensors-local/src/screen.rs`: screen probe/status helpers, backend trait, observation type, policy hook, budget enforcement, tests.
- Modify `crates/cairn-sensors-local/src/lib.rs`: expose the screen module.
- Modify `crates/cairn-cli/src/verbs/status.rs`: build status from active config and screen probe data.
- Modify `crates/cairn-cli/tests/status_snapshot.rs`: assert screen status/capability output.

---

### Task 1: Add Typed Screen Config

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Modify: `crates/cairn-cli/tests/config.rs`

- [ ] **Step 1: Write failing core config tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `crates/cairn-core/src/config/mod.rs`:

```rust
#[test]
fn default_screen_config_is_safe_and_off() {
    let screen = &CairnConfig::default().sensors.screen;
    assert!(!screen.enabled);
    assert_eq!(screen.backend, ScreenBackend::Xcap);
    assert_eq!(screen.ocr.engine, ScreenOcrEngine::Auto);
    assert!(screen.allow_apps.is_empty());
    assert!(screen.blur_password_fields);
    assert_eq!(screen.budget.max_frames_per_minute, 12);
    assert_eq!(screen.budget.max_text_bytes_per_event, 16_384);
}

#[test]
fn screen_toggle_shape_still_deserializes() {
    let json = r#"{"sensors":{"screen":{"enabled":false}}}"#;
    let config: CairnConfig = serde_json::from_str(json).unwrap();
    assert!(!config.sensors.screen.enabled);
    assert_eq!(config.sensors.screen.backend, ScreenBackend::Xcap);
    assert_eq!(config.sensors.screen.ocr.engine, ScreenOcrEngine::Auto);
}

#[test]
fn validate_rejects_zero_screen_frame_budget() {
    let mut config = CairnConfig::default();
    config.sensors.screen.budget.max_frames_per_minute = 0;
    let err = config.validate().unwrap_err();
    assert!(matches!(
        err,
        ConfigError::InvalidBudget {
            field: "sensors.screen.budget.max_frames_per_minute",
            value: 0
        }
    ));
}

#[test]
fn validate_rejects_zero_screen_text_budget() {
    let mut config = CairnConfig::default();
    config.sensors.screen.budget.max_text_bytes_per_event = 0;
    let err = config.validate().unwrap_err();
    assert!(matches!(
        err,
        ConfigError::InvalidBudget {
            field: "sensors.screen.budget.max_text_bytes_per_event",
            value: 0
        }
    ));
}
```

- [ ] **Step 2: Write failing CLI config env test**

Add this test to `crates/cairn-cli/tests/config.rs`:

```rust
#[test]
fn cairn_env_override_sets_screen_backend() {
    let dir = tempfile::tempdir().unwrap();
    temp_env::with_var("CAIRN_SENSORS__SCREEN__BACKEND", Some("screenpipe"), || {
        let config = load(dir.path(), &CliOverrides::default()).unwrap();
        assert_eq!(config.sensors.screen.backend, cairn_core::config::ScreenBackend::Screenpipe);
    });
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```bash
cargo test -p cairn-core config::tests::default_screen_config_is_safe_and_off --locked
cargo test -p cairn-cli --test config cairn_env_override_sets_screen_backend --locked
```

Expected: the first command fails because `ScreenBackend`, `ScreenOcrEngine`, and screen budget fields do not exist; the second command fails for the same missing config type.

- [ ] **Step 4: Implement screen config types**

In `crates/cairn-core/src/config/mod.rs`, replace `screen: SensorToggle` with `screen: ScreenSensorConfig` in `SensorsConfig`, update `SensorsConfig::default()`, and add these types in the Sensors section:

```rust
/// Screen sensor backend selection (brief §9.1, ADR 0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ScreenBackend {
    /// In-process xcap capture path. Default P0 backend.
    #[default]
    Xcap,
    /// Optional screenpipe subprocess path behind `screenpipe-runtime`.
    Screenpipe,
}

/// Screen OCR engine requested in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ScreenOcrEngine {
    /// Resolve to the platform default at runtime.
    #[default]
    Auto,
    /// Apple Vision OCR.
    Vision,
    /// Windows Runtime OCR.
    Winrt,
    /// Tesseract OCR.
    Tesseract,
    /// Capture metadata without OCR text.
    Off,
}

/// OCR-specific screen sensor configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScreenOcrConfig {
    /// Requested OCR engine.
    pub engine: ScreenOcrEngine,
}

/// Capture budgets enforced before screen observations enter policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenCaptureBudget {
    /// Maximum frames accepted per minute.
    pub max_frames_per_minute: u32,
    /// Maximum OCR text bytes retained per event.
    pub max_text_bytes_per_event: u32,
}

impl Default for ScreenCaptureBudget {
    fn default() -> Self {
        Self {
            max_frames_per_minute: 12,
            max_text_bytes_per_event: 16_384,
        }
    }
}

/// Screen sensor configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenSensorConfig {
    /// Whether the screen sensor may capture.
    pub enabled: bool,
    /// Capture backend.
    pub backend: ScreenBackend,
    /// OCR engine configuration.
    pub ocr: ScreenOcrConfig,
    /// Focused apps allowed for capture. Empty means no app restriction.
    pub allow_apps: Vec<String>,
    /// Whether password fields are blurred or dropped before policy.
    pub blur_password_fields: bool,
    /// Capture budget limits.
    pub budget: ScreenCaptureBudget,
}

impl Default for ScreenSensorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: ScreenBackend::Xcap,
            ocr: ScreenOcrConfig::default(),
            allow_apps: Vec::new(),
            blur_password_fields: true,
            budget: ScreenCaptureBudget::default(),
        }
    }
}
```

Update `SensorsConfig::default()`:

```rust
impl Default for SensorsConfig {
    fn default() -> Self {
        Self {
            hooks: SensorToggle { enabled: true },
            ide: SensorToggle { enabled: true },
            screen: ScreenSensorConfig::default(),
            slack: SlackSensorConfig::default(),
        }
    }
}
```

Add these budget checks in `CairnConfig::validate()` after the extractor budget checks:

```rust
if self.sensors.screen.budget.max_frames_per_minute == 0 {
    return Err(ConfigError::InvalidBudget {
        field: "sensors.screen.budget.max_frames_per_minute",
        value: 0_u64,
    });
}
if self.sensors.screen.budget.max_text_bytes_per_event == 0 {
    return Err(ConfigError::InvalidBudget {
        field: "sensors.screen.budget.max_text_bytes_per_event",
        value: 0_u64,
    });
}
```

- [ ] **Step 5: Update capability set for screen enablement**

Add this field to `CapabilitySet` in `crates/cairn-core/src/config/mod.rs`:

```rust
/// True iff config explicitly enables screen capture.
pub screen_capture_enabled: bool,
```

Set it in `CairnConfig::capabilities()`:

```rust
screen_capture_enabled: self.sensors.screen.enabled,
```

Update existing capability tests with:

```rust
assert!(!caps.screen_capture_enabled, "screen capture is off by default");
```

- [ ] **Step 6: Run tests to verify pass**

Run:

```bash
cargo test -p cairn-core config::tests::default_screen_config_is_safe_and_off --locked
cargo test -p cairn-core config::tests::screen_toggle_shape_still_deserializes --locked
cargo test -p cairn-core config::tests::validate_rejects_zero_screen_frame_budget --locked
cargo test -p cairn-core config::tests::validate_rejects_zero_screen_text_budget --locked
cargo test -p cairn-cli --test config cairn_env_override_sets_screen_backend --locked
```

Expected: all listed tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs crates/cairn-cli/tests/config.rs
git commit -m "feat(config): add screen sensor settings"
```

---

### Task 2: Add Status IDL For Screen Sensor

**Files:**
- Modify: `crates/cairn-idl/schema/capabilities/capabilities.json`
- Modify: `crates/cairn-idl/schema/prelude/status.json`
- Generated: `crates/cairn-core/src/generated/common/mod.rs`
- Generated: `crates/cairn-core/src/generated/status.rs`
- Generated: `crates/cairn-core/src/generated/verbs/mod.rs`
- Generated: `crates/cairn-mcp/src/generated/schemas/prelude/status.json`
- Generated: `crates/cairn-mcp/src/generated/schemas/capabilities/capabilities.json`

- [ ] **Step 1: Write failing schema test expectation**

Run the existing generated-wire test before changing schemas:

```bash
cargo test -p cairn-core --test generated_wire status --locked
```

Expected: current tests pass before the schema change. This gives a baseline.

- [ ] **Step 2: Add screen capability strings**

In `crates/cairn-idl/schema/capabilities/capabilities.json`, append these entries to `oneOf`:

```json
{ "const": "cairn.sensor.v1.screen.xcap",             "x-cairn-since": "v0.1" },
{ "const": "cairn.sensor.v1.screen.screenpipe",       "x-cairn-since": "v0.1" },
{ "const": "cairn.sensor.v1.screen.ocr.vision",       "x-cairn-since": "v0.1" },
{ "const": "cairn.sensor.v1.screen.ocr.winrt",        "x-cairn-since": "v0.1" },
{ "const": "cairn.sensor.v1.screen.ocr.tesseract",    "x-cairn-since": "v0.1" }
```

- [ ] **Step 3: Add the status `sensors.screen` schema**

In `crates/cairn-idl/schema/prelude/status.json`:

Add `"sensors"` to the top-level `required` list.

Add this top-level property next to `extensions`:

```json
"sensors": {
  "type": "object",
  "additionalProperties": false,
  "required": ["screen"],
  "properties": {
    "screen": {
      "type": "object",
      "additionalProperties": false,
      "required": ["backend", "state", "mode", "ocr_engine", "permission"],
      "properties": {
        "backend": {
          "type": "string",
          "enum": ["xcap", "screenpipe"]
        },
        "state": {
          "type": "string",
          "enum": ["disabled", "permission_missing", "degraded", "enabled"]
        },
        "mode": {
          "type": "string",
          "enum": ["off", "snapshot", "continuous"]
        },
        "ocr_engine": {
          "type": "string",
          "enum": ["vision", "winrt", "tesseract", "off"]
        },
        "permission": {
          "type": "string",
          "enum": ["not_requested", "granted", "denied", "revoked"]
        },
        "degradation": {
          "type": "object",
          "additionalProperties": false,
          "required": ["code", "message"],
          "properties": {
            "code": {
              "type": "string",
              "enum": [
                "screen.disabled",
                "screen.permission_missing",
                "screen.backend_unavailable",
                "screen.degraded"
              ]
            },
            "message": { "type": "string", "minLength": 1 }
          }
        }
      }
    }
  }
}
```

- [ ] **Step 4: Regenerate code**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: generated Rust and copied MCP schemas update.

- [ ] **Step 5: Inspect generated type names**

Run:

```bash
rg -n "StatusResponseSensors|CairnSensorV1Screen" crates/cairn-core/src/generated
```

Expected: output includes `StatusResponseSensors`, `StatusResponseSensorsScreen`, `StatusResponseSensorsScreenDegradation`, and generated capability enum variants for the new screen capabilities.

- [ ] **Step 6: Run schema and generated tests**

Run:

```bash
cargo test -p cairn-idl --test schema_files --locked
cargo test -p cairn-core --test generated_wire status --locked
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all commands pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-idl/schema/capabilities/capabilities.json crates/cairn-idl/schema/prelude/status.json crates/cairn-core/src/generated crates/cairn-mcp/src/generated
git commit -m "feat(idl): add screen sensor status contract"
```

---

### Task 3: Add Mockable Screen Runtime Module

**Files:**
- Modify: `crates/cairn-sensors-local/Cargo.toml`
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Create: `crates/cairn-sensors-local/src/screen.rs`

- [ ] **Step 1: Add dependency and feature flags**

In `crates/cairn-sensors-local/Cargo.toml`, add `thiserror` under `[dependencies]`:

```toml
thiserror = { workspace = true }
```

Add the feature block:

```toml
[features]
default = []
screenpipe-runtime = []
```

- [ ] **Step 2: Write failing screen module tests**

Create `crates/cairn-sensors-local/src/screen.rs` with only this test module first:

```rust
#[cfg(test)]
mod tests {
    use cairn_core::config::{ScreenBackend, ScreenSensorConfig};

    use super::*;

    #[derive(Clone)]
    struct FakeBackend {
        probe: ScreenProbe,
        observation: ScreenObservation,
    }

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

    #[test]
    fn disabled_sensor_emits_nothing() {
        let backend = FakeBackend {
            probe: fake_probe(),
            observation: fake_observation("ignored"),
        };
        let sensor = ScreenSensor::new(backend, NoopScreenPolicy);
        let result = sensor.capture_snapshot(&ScreenSensorConfig::default()).unwrap();
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

    #[test]
    fn screenpipe_without_feature_reports_backend_unavailable() {
        let config = ScreenSensorConfig {
            enabled: true,
            backend: ScreenBackend::Screenpipe,
            ..ScreenSensorConfig::default()
        };
        let probe = probe_config(&config);
        if cfg!(feature = "screenpipe-runtime") {
            assert_ne!(probe.degradation_code(), Some(ScreenDegradationCode::BackendUnavailable));
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
}
```

- [ ] **Step 3: Expose module and verify failure**

Add this to `crates/cairn-sensors-local/src/lib.rs`:

```rust
pub mod screen;
```

Run:

```bash
cargo test -p cairn-sensors-local screen::tests::disabled_sensor_emits_nothing --locked
```

Expected: failure due to missing screen types and functions.

- [ ] **Step 4: Implement screen module**

Replace the temporary `crates/cairn-sensors-local/src/screen.rs` contents with:

```rust
//! Screen sensor runtime boundary.

use cairn_core::config::{ScreenBackend, ScreenOcrEngine, ScreenSensorConfig};
use cairn_core::generated::common::Capabilities;
use thiserror::Error;

/// Sensor label used by the default xcap backend.
pub const XCAP_SENSOR_LABEL: &str = "snr:local:screen:xcap:v1";

/// Sensor label used by the optional screenpipe backend.
pub const SCREENPIPE_SENSOR_LABEL: &str = "snr:local:screen:screenpipe:v1";

/// Runtime state for screen capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    /// Config disables the sensor.
    Disabled,
    /// OS permission is missing.
    PermissionMissing,
    /// Backend is present but degraded.
    Degraded,
    /// Capture may run.
    Enabled,
}

/// Runtime capture mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    /// Capture is off.
    Off,
    /// Snapshot capture.
    Snapshot,
    /// Continuous capture.
    Continuous,
}

/// OS permission state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenPermission {
    /// Permission has not been requested.
    NotRequested,
    /// Permission has been granted.
    Granted,
    /// Permission was denied.
    Denied,
    /// Permission was revoked after a previous grant.
    Revoked,
}

/// OCR engine resolved after platform probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedScreenOcrEngine {
    /// Apple Vision OCR.
    Vision,
    /// Windows Runtime OCR.
    Winrt,
    /// Tesseract OCR.
    Tesseract,
    /// OCR disabled.
    Off,
}

impl ResolvedScreenOcrEngine {
    /// Resolve config `auto` to the platform default.
    #[must_use]
    pub fn from_config(engine: ScreenOcrEngine) -> Self {
        match engine {
            ScreenOcrEngine::Auto => platform_default_ocr(),
            ScreenOcrEngine::Vision => Self::Vision,
            ScreenOcrEngine::Winrt => Self::Winrt,
            ScreenOcrEngine::Tesseract => Self::Tesseract,
            ScreenOcrEngine::Off => Self::Off,
        }
    }
}

/// Stable screen degradation code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenDegradationCode {
    /// Screen sensor is disabled in config.
    Disabled,
    /// OS permission is missing.
    PermissionMissing,
    /// Requested backend is not compiled or available.
    BackendUnavailable,
    /// Platform capture support is degraded.
    Degraded,
}

impl ScreenDegradationCode {
    /// Wire code string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "screen.disabled",
            Self::PermissionMissing => "screen.permission_missing",
            Self::BackendUnavailable => "screen.backend_unavailable",
            Self::Degraded => "screen.degraded",
        }
    }

    /// Operator-facing message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Disabled => "screen sensor is disabled in config",
            Self::PermissionMissing => "screen recording permission is missing",
            Self::BackendUnavailable => "requested screen backend is unavailable",
            Self::Degraded => "screen capture is degraded on this platform",
        }
    }
}

/// Screen degradation detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDegradation {
    /// Stable code.
    pub code: ScreenDegradationCode,
    /// Human-readable message.
    pub message: String,
}

impl ScreenDegradation {
    /// Build degradation detail from a stable code.
    #[must_use]
    pub fn new(code: ScreenDegradationCode) -> Self {
        Self {
            code,
            message: code.message().to_owned(),
        }
    }
}

/// Screen runtime probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenProbe {
    /// Selected backend.
    pub backend: ScreenBackend,
    /// Runtime state.
    pub state: ScreenState,
    /// Capture mode.
    pub mode: ScreenMode,
    /// Permission state.
    pub permission: ScreenPermission,
    /// Resolved OCR engine.
    pub ocr_engine: ResolvedScreenOcrEngine,
    /// Degradation detail when unavailable.
    pub degradation: Option<ScreenDegradation>,
}

impl ScreenProbe {
    /// Return the degradation code when present.
    #[must_use]
    pub fn degradation_code(&self) -> Option<ScreenDegradationCode> {
        self.degradation.as_ref().map(|d| d.code)
    }
}

/// OCR bounding box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundingBox {
    /// Left coordinate.
    pub x: u32,
    /// Top coordinate.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Screen observation after OCR and metadata extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenObservation {
    /// OCR text after budget enforcement and policy.
    pub text: String,
    /// Focused app name.
    pub app: String,
    /// Active window title.
    pub window_title: String,
    /// Optional active URL.
    pub url: Option<String>,
    /// OCR bounding boxes.
    pub bounding_boxes: Vec<BoundingBox>,
    /// Capture timestamp.
    pub captured_at: String,
    /// Sensor identity label.
    pub sensor_label: String,
    /// Backend that captured the observation.
    pub backend: ScreenBackend,
    /// OCR engine used.
    pub ocr_engine: ResolvedScreenOcrEngine,
}

/// Screen runtime error.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScreenError {
    /// Capture is unavailable.
    #[error("{code:?}: {message}")]
    Unavailable {
        /// Stable degradation code.
        code: ScreenDegradationCode,
        /// Human-readable detail.
        message: String,
    },
}

impl ScreenError {
    /// Return the stable degradation code.
    #[must_use]
    pub const fn code(&self) -> ScreenDegradationCode {
        match self {
            Self::Unavailable { code, .. } => *code,
        }
    }
}

/// Screen backend runtime boundary.
pub trait ScreenBackendRuntime {
    /// Probe runtime availability.
    fn probe(&self, config: &ScreenSensorConfig) -> ScreenProbe;

    /// Capture one snapshot.
    fn capture_snapshot(
        &self,
        config: &ScreenSensorConfig,
    ) -> Result<ScreenObservation, ScreenError>;
}

/// Policy hook applied before observations leave the sensor.
pub trait ScreenPolicy {
    /// Redact or reject an observation.
    fn redact(&self, observation: ScreenObservation) -> Result<ScreenObservation, ScreenError>;
}

/// No-op policy used by tests and safe scaffolds.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopScreenPolicy;

impl ScreenPolicy for NoopScreenPolicy {
    fn redact(&self, observation: ScreenObservation) -> Result<ScreenObservation, ScreenError> {
        Ok(observation)
    }
}

/// Minimal policy hook for secrets-shaped fixture text.
#[derive(Debug, Clone, Copy, Default)]
pub struct BasicScreenPolicy;

impl ScreenPolicy for BasicScreenPolicy {
    fn redact(&self, mut observation: ScreenObservation) -> Result<ScreenObservation, ScreenError> {
        if observation.text.to_ascii_lowercase().contains("password=") {
            observation.text = "[redacted]".to_owned();
        }
        Ok(observation)
    }
}

/// Screen sensor composed from a backend and policy.
#[derive(Debug, Clone)]
pub struct ScreenSensor<B, P> {
    backend: B,
    policy: P,
}

impl<B, P> ScreenSensor<B, P>
where
    B: ScreenBackendRuntime,
    P: ScreenPolicy,
{
    /// Create a screen sensor.
    #[must_use]
    pub const fn new(backend: B, policy: P) -> Self {
        Self { backend, policy }
    }

    /// Capture one snapshot when enabled and available.
    pub fn capture_snapshot(
        &self,
        config: &ScreenSensorConfig,
    ) -> Result<Option<ScreenObservation>, ScreenError> {
        if !config.enabled {
            return Ok(None);
        }
        let probe = self.backend.probe(config);
        ensure_available(&probe)?;
        let mut observation = self.backend.capture_snapshot(config)?;
        truncate_text(&mut observation.text, config.budget.max_text_bytes_per_event);
        self.policy.redact(observation).map(Some)
    }
}

/// Capabilities compiled into this binary.
#[must_use]
pub fn compiled_capabilities() -> Vec<Capabilities> {
    let mut caps = vec![Capabilities::CairnSensorV1ScreenXcap, platform_ocr_capability()];
    if cfg!(feature = "screenpipe-runtime") {
        caps.push(Capabilities::CairnSensorV1ScreenScreenpipe);
    }
    caps
}

/// Probe screen config without touching desktop APIs.
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
        };
    }
    if config.backend == ScreenBackend::Screenpipe && !cfg!(feature = "screenpipe-runtime") {
        return ScreenProbe {
            backend: config.backend,
            state: ScreenState::Degraded,
            mode: ScreenMode::Off,
            permission: ScreenPermission::NotRequested,
            ocr_engine,
            degradation: Some(ScreenDegradation::new(
                ScreenDegradationCode::BackendUnavailable,
            )),
        };
    }
    ScreenProbe {
        backend: config.backend,
        state: ScreenState::PermissionMissing,
        mode: ScreenMode::Off,
        permission: ScreenPermission::NotRequested,
        ocr_engine,
        degradation: Some(ScreenDegradation::new(
            ScreenDegradationCode::PermissionMissing,
        )),
    }
}

fn ensure_available(probe: &ScreenProbe) -> Result<(), ScreenError> {
    if probe.state == ScreenState::Enabled {
        return Ok(());
    }
    let degradation = probe
        .degradation
        .clone()
        .unwrap_or_else(|| ScreenDegradation::new(ScreenDegradationCode::Degraded));
    Err(ScreenError::Unavailable {
        code: degradation.code,
        message: degradation.message,
    })
}

fn truncate_text(text: &mut String, max_bytes: u32) {
    let limit = max_bytes as usize;
    if text.len() <= limit {
        return;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

fn platform_default_ocr() -> ResolvedScreenOcrEngine {
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
```

Keep the tests from Step 2 at the bottom of the file.

- [ ] **Step 5: Run module tests**

Run:

```bash
cargo test -p cairn-sensors-local screen::tests --locked
```

Expected: all screen module tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-sensors-local/Cargo.toml crates/cairn-sensors-local/src/lib.rs crates/cairn-sensors-local/src/screen.rs
git commit -m "feat(sensors): add screen runtime boundary"
```

---

### Task 4: Wire Screen State Into `cairn status`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Modify: `crates/cairn-cli/tests/status_snapshot.rs`

- [ ] **Step 1: Write failing status tests**

Add these tests to `crates/cairn-cli/tests/status_snapshot.rs`:

```rust
#[test]
fn status_json_reports_screen_default_disabled() {
    let out = cli()
        .args(["status", "--json"])
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["sensors"]["screen"]["backend"], "xcap");
    assert_eq!(v["sensors"]["screen"]["state"], "disabled");
    assert_eq!(v["sensors"]["screen"]["mode"], "off");
    assert_eq!(v["sensors"]["screen"]["permission"], "not_requested");
    assert_eq!(
        v["sensors"]["screen"]["degradation"]["code"],
        "screen.disabled"
    );
    let caps = v["capabilities"].as_array().expect("capabilities array");
    assert!(
        caps.iter().any(|c| c == "cairn.sensor.v1.screen.xcap"),
        "screen xcap capability missing: {caps:?}"
    );
}

#[test]
fn status_json_reports_unavailable_screenpipe_from_env() {
    let out = cli()
        .args(["status", "--json"])
        .env("CAIRN_SENSORS__SCREEN__ENABLED", "true")
        .env("CAIRN_SENSORS__SCREEN__BACKEND", "screenpipe")
        .output()
        .expect("cairn status --json");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(v["sensors"]["screen"]["backend"], "screenpipe");
    assert_eq!(v["sensors"]["screen"]["state"], "degraded");
    assert_eq!(
        v["sensors"]["screen"]["degradation"]["code"],
        "screen.backend_unavailable"
    );
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p cairn-cli --test status_snapshot status_json_reports_screen_default_disabled --locked
```

Expected: failure because `status` does not include `sensors.screen` or screen capabilities.

- [ ] **Step 3: Implement status response builder**

In `crates/cairn-cli/src/verbs/status.rs`, add imports:

```rust
use cairn_core::config::{CairnConfig, ScreenBackend};
use cairn_core::generated::status::{
    StatusResponse, StatusResponseSensors, StatusResponseSensorsScreen,
    StatusResponseSensorsScreenBackend, StatusResponseSensorsScreenDegradation,
    StatusResponseSensorsScreenDegradationCode, StatusResponseSensorsScreenMode,
    StatusResponseSensorsScreenOcrEngine, StatusResponseSensorsScreenPermission,
    StatusResponseSensorsScreenState, StatusResponseServerInfo,
};
use cairn_sensors_local::screen::{
    self, ResolvedScreenOcrEngine, ScreenDegradationCode, ScreenMode, ScreenPermission,
    ScreenProbe, ScreenState,
};
```

Add a pure response builder:

```rust
fn build_response(config: &CairnConfig, started_at: String, incarnation: cairn_core::generated::common::Ulid) -> StatusResponse {
    let mut capabilities = p0_capabilities();
    capabilities.extend(screen::compiled_capabilities());
    capabilities.sort_by_key(|cap| serde_json::to_string(cap).unwrap_or_default());
    capabilities.dedup();
    let probe = screen::probe_config(&config.sensors.screen);
    StatusResponse {
        contract: "cairn.mcp.v1".to_owned(),
        server_info: StatusResponseServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build: build_profile(),
            started_at,
            incarnation,
        },
        capabilities,
        extensions: vec![],
        sensors: StatusResponseSensors {
            screen: map_screen_probe(&probe),
        },
    }
}
```

Replace the existing inline `StatusResponse` construction in `run()` with:

```rust
let config = crate::config::load(
    &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    &crate::config::CliOverrides::default(),
)
.unwrap_or_else(|_| CairnConfig::default());
let resp = build_response(&config, started_at.clone(), incarnation.clone());
```

Add mapping helpers:

```rust
fn map_screen_probe(probe: &ScreenProbe) -> StatusResponseSensorsScreen {
    StatusResponseSensorsScreen {
        backend: map_backend(probe.backend),
        state: map_screen_state(probe.state),
        mode: map_screen_mode(probe.mode),
        ocr_engine: map_ocr_engine(probe.ocr_engine),
        permission: map_permission(probe.permission),
        degradation: probe.degradation.as_ref().map(|d| StatusResponseSensorsScreenDegradation {
            code: map_degradation_code(d.code),
            message: d.message.clone(),
        }),
    }
}

fn map_backend(backend: ScreenBackend) -> StatusResponseSensorsScreenBackend {
    match backend {
        ScreenBackend::Xcap => StatusResponseSensorsScreenBackend::Xcap,
        ScreenBackend::Screenpipe => StatusResponseSensorsScreenBackend::Screenpipe,
    }
}

fn map_screen_state(state: ScreenState) -> StatusResponseSensorsScreenState {
    match state {
        ScreenState::Disabled => StatusResponseSensorsScreenState::Disabled,
        ScreenState::PermissionMissing => StatusResponseSensorsScreenState::PermissionMissing,
        ScreenState::Degraded => StatusResponseSensorsScreenState::Degraded,
        ScreenState::Enabled => StatusResponseSensorsScreenState::Enabled,
    }
}

fn map_screen_mode(mode: ScreenMode) -> StatusResponseSensorsScreenMode {
    match mode {
        ScreenMode::Off => StatusResponseSensorsScreenMode::Off,
        ScreenMode::Snapshot => StatusResponseSensorsScreenMode::Snapshot,
        ScreenMode::Continuous => StatusResponseSensorsScreenMode::Continuous,
    }
}

fn map_permission(permission: ScreenPermission) -> StatusResponseSensorsScreenPermission {
    match permission {
        ScreenPermission::NotRequested => StatusResponseSensorsScreenPermission::NotRequested,
        ScreenPermission::Granted => StatusResponseSensorsScreenPermission::Granted,
        ScreenPermission::Denied => StatusResponseSensorsScreenPermission::Denied,
        ScreenPermission::Revoked => StatusResponseSensorsScreenPermission::Revoked,
    }
}

fn map_ocr_engine(engine: ResolvedScreenOcrEngine) -> StatusResponseSensorsScreenOcrEngine {
    match engine {
        ResolvedScreenOcrEngine::Vision => StatusResponseSensorsScreenOcrEngine::Vision,
        ResolvedScreenOcrEngine::Winrt => StatusResponseSensorsScreenOcrEngine::Winrt,
        ResolvedScreenOcrEngine::Tesseract => StatusResponseSensorsScreenOcrEngine::Tesseract,
        ResolvedScreenOcrEngine::Off => StatusResponseSensorsScreenOcrEngine::Off,
    }
}

fn map_degradation_code(code: ScreenDegradationCode) -> StatusResponseSensorsScreenDegradationCode {
    match code {
        ScreenDegradationCode::Disabled => {
            StatusResponseSensorsScreenDegradationCode::ScreenDisabled
        }
        ScreenDegradationCode::PermissionMissing => {
            StatusResponseSensorsScreenDegradationCode::ScreenPermissionMissing
        }
        ScreenDegradationCode::BackendUnavailable => {
            StatusResponseSensorsScreenDegradationCode::ScreenBackendUnavailable
        }
        ScreenDegradationCode::Degraded => {
            StatusResponseSensorsScreenDegradationCode::ScreenDegraded
        }
    }
}
```

Update human output in `run()` after capabilities:

```rust
println!(
    "screen:      {:?} {:?}",
    resp.sensors.screen.backend, resp.sensors.screen.state
);
```

- [ ] **Step 4: Run status tests**

Run:

```bash
cargo test -p cairn-cli --test status_snapshot --locked
```

Expected: all status integration tests pass.

- [ ] **Step 5: Run clippy for exact generated names**

Run:

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

Expected: pass. If a generated enum variant name differs, inspect `crates/cairn-core/src/generated/status.rs`, update the mapping helper to use the generated name, and rerun this command.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/status.rs crates/cairn-cli/tests/status_snapshot.rs
git commit -m "feat(cli): report screen sensor status"
```

---

### Task 5: Final Verification

**Files:**
- Review all modified files from Tasks 1-4.

- [ ] **Step 1: Run formatting**

```bash
cargo fmt --all --check
```

Expected: pass. If it fails, run `cargo fmt --all`, inspect the diff, then rerun the check.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: pass.

- [ ] **Step 3: Run workspace check**

```bash
cargo check --workspace --all-targets --locked
```

Expected: pass.

- [ ] **Step 4: Run tests**

```bash
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
```

Expected: both commands pass.

- [ ] **Step 5: Run boundary and codegen checks**

```bash
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: both commands pass.

- [ ] **Step 6: Inspect final diff**

```bash
git status --short
git diff --stat
git diff --check
```

Expected: only issue #86 files are modified, and `git diff --check` reports no whitespace errors.

- [ ] **Step 7: Commit verification fixes if any**

If Steps 1-6 required changes, commit them:

```bash
git add crates docs
git commit -m "fix: align screen sensor verification"
```

If Steps 1-6 required no changes, do not create an empty commit.
