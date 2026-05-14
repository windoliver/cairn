# Screen Sensor Design — Issue #86

**Date:** 2026-05-12
**Issue:** [#86 — Implement screen capture via screenpipe primary and xcap/tesseract fallback](https://github.com/windoliver/cairn/issues/86)
**Brief sections:** §8.0.a Status · §9.1 Source families · §14 Privacy & consent · §19 v0.1 sequencing
**ADR:** [ADR 0003 — Screen sensor packaging and opt-in model for P0](../../design/decisions/0003-screen-sensor-packaging.md)
**Status:** Approved

---

## 1. Scope

Implement the P0 screen sensor contract as a safe, testable slice:

- screen sensor config with default-off enablement;
- xcap as the default always-compiled backend, with screenpipe as an optional heavy backend behind `screenpipe-runtime`;
- status reporting for backend, OCR engine, permission, mode, availability, and degradation state;
- a mockable backend boundary for OCR text, app focus, window metadata, timestamps, sensor labels, and capture budgets;
- degradation paths for disabled config, missing permissions, unavailable dependencies, and degraded platform support.

The issue wording says "screenpipe primary and xcap/tesseract fallback." ADR 0003 supersedes that wording for P0: xcap + OS OCR is the default path, and screenpipe is opt-in.

Out of scope: desktop overlays, retained screenshots, real OS capture in CI, GUI toggles, and the recording-to-text batch pipeline.

---

## 2. Architecture

The work stays behind the existing `SensorIngress` boundary.

| Layer | Crate | Responsibility |
|---|---|---|
| Config types | `cairn-core::config` | Add typed screen config, enums, validation, and derived capability flags. Pure data only. |
| Runtime sensor | `cairn-sensors-local` | Add screen module, backend trait, mock backend, xcap slot, optional screenpipe slot, redaction/policy hook, budget enforcement. |
| Status surface | `cairn-cli` + generated IDL | Report installed capabilities and `sensors.screen` runtime state. |
| Tests | `cairn-sensors-local` + `cairn-cli` | Mocked OCR fixtures, dependency-unavailable cases, and status capability/state assertions. |

No screen capture code enters `cairn-core`. Core owns only serializable config and status types so the no-I/O invariant remains intact.

---

## 3. Config

Replace the current `SensorToggle`-only screen field with a typed screen config:

```rust
pub struct SensorsConfig {
    pub hooks: SensorToggle,
    pub ide: SensorToggle,
    pub screen: ScreenSensorConfig,
    pub slack: SlackSensorConfig,
}

pub struct ScreenSensorConfig {
    pub enabled: bool,                 // default false
    pub backend: ScreenBackend,        // default Xcap
    pub ocr: ScreenOcrConfig,          // default Auto
    pub allow_apps: Vec<String>,       // empty = no app restriction
    pub blur_password_fields: bool,    // default true
    pub budget: ScreenCaptureBudget,
}

pub enum ScreenBackend { Xcap, Screenpipe }
pub enum ScreenOcrEngine { Auto, Vision, Winrt, Tesseract, Off }

pub struct ScreenOcrConfig {
    pub engine: ScreenOcrEngine,
}

pub struct ScreenCaptureBudget {
    pub max_frames_per_minute: u32,    // validate > 0
    pub max_text_bytes_per_event: u32, // validate > 0
}
```

Environment override names follow the existing config loader convention:

- `CAIRN_SENSORS__SCREEN__ENABLED`
- `CAIRN_SENSORS__SCREEN__BACKEND`
- `CAIRN_SENSORS__SCREEN__OCR__ENGINE`

Runtime default remains `enabled: false`, so the sensor never captures without explicit operator action.

---

## 4. Status And Capabilities

Add sensor capability constants to the IDL capability list:

- `cairn.sensor.v1.screen.xcap`
- `cairn.sensor.v1.screen.screenpipe`
- `cairn.sensor.v1.screen.ocr.vision`
- `cairn.sensor.v1.screen.ocr.winrt`
- `cairn.sensor.v1.screen.ocr.tesseract`

Add a structured status block:

```json
{
  "sensors": {
    "screen": {
      "backend": "xcap",
      "state": "disabled",
      "mode": "off",
      "ocr_engine": "vision",
      "permission": "not_requested",
      "degradation": {
        "code": "screen.disabled",
        "message": "screen sensor is disabled in config"
      }
    }
  }
}
```

Closed sets:

- `state`: `disabled`, `permission_missing`, `degraded`, `enabled`
- `mode`: `off`, `snapshot`, `continuous`
- `permission`: `not_requested`, `granted`, `denied`, `revoked`
- `backend`: `xcap`, `screenpipe`
- `ocr_engine`: `vision`, `winrt`, `tesseract`, `off`

Capability strings report what the binary can do. `sensors.screen.state` reports whether capture can run right now. Config may use `ocr.engine: auto`; status reports the resolved engine after platform probing.

---

## 5. Runtime Sensor Boundary

`cairn-sensors-local::screen` introduces two internal traits:

```rust
pub trait ScreenBackendRuntime {
    fn probe(&self, config: &ScreenSensorConfig) -> ScreenProbe;
    fn capture_snapshot(&self, config: &ScreenSensorConfig) -> Result<ScreenObservation, ScreenError>;
}

pub trait ScreenPolicy {
    fn redact(&self, observation: ScreenObservation) -> Result<ScreenObservation, ScreenError>;
}
```

`ScreenObservation` carries the issue-required fields:

- OCR text;
- app name;
- active window title;
- optional URL;
- bounding boxes when the backend provides them;
- captured timestamp;
- sensor label, such as `snr:local:screen:xcap:v1`;
- backend and OCR engine metadata.

Budget enforcement happens before observation emission:

- disabled sensor emits nothing;
- frame/text budgets truncate or reject observations before policy;
- redaction/policy runs before any observation is converted into a `SensorObservation` record;
- raw frame bytes are not retained.

The first implementation ships a mock backend for tests and runtime probes. The xcap and screenpipe modules expose feature-gated slots so real capture can be added without changing config or status contracts.

---

## 6. Degradation And Errors

Use stable machine-readable codes from ADR 0003:

| Code | Meaning |
|---|---|
| `screen.disabled` | Config has `sensors.screen.enabled: false`. |
| `screen.permission_missing` | OS permission is denied, revoked, or not granted. |
| `screen.backend_unavailable` | Requested backend is not compiled or cannot start. |
| `screen.degraded` | Platform support is partial, such as Wayland portal failure. |

Calls that require screen capture fail closed with `CapabilityUnavailable` rather than falling back silently. Status still includes the selected backend and degradation detail so operators can fix config or permissions.

---

## 7. Testing

Tests are written before implementation and avoid real desktop APIs:

- mocked OCR fixture test: a fake backend returns text/window/app metadata and the sensor emits the expected observation fields;
- disabled-sensor test: default config emits nothing and reports `state: disabled`;
- dependency-unavailable test: `backend: screenpipe` without `screenpipe-runtime` reports `screen.backend_unavailable`;
- permission-denied test: probe result maps to `permission_missing`;
- status tests: `cairn status --json` includes sensor capability strings and `sensors.screen` state;
- config tests: screen defaults round-trip, env overrides work, and zero budgets are rejected.

Full verification target for the implementation PR:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

---

## 8. Implementation Notes

- Preserve backward-compatible config deserialization where possible: `{ screen: { enabled: false } }` should still parse.
- Do not add platform OCR dependencies until the trait and mocked test matrix are stable.
- Do not log OCR text above `debug`; status reports metadata only.
- Keep screenpipe behind `screenpipe-runtime`; the default feature set must not pull subprocess or model dependencies.
- Update generated status types through `cairn-codegen` if IDL schemas change.
