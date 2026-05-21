# Issue #88 - Sensor consent, budgets, and privacy gates

| | |
|---|---|
| **Issue** | https://github.com/windoliver/cairn/issues/88 |
| **Phase** | v0.1 P0 |
| **Status** | Design approved, implementation plan follows |
| **Branch** | `codex/issue-88-sensor-consent-privacy-gates` |

---

## 1. Problem

Local sensors can currently produce capture artifacts or capture events from config alone, and some paths do not consult config at all before writing body-bearing artifacts. Screen capture writes a PNG before any consent-journal check. Recording ingest stages derived payload files before capture import. The hook command writes trace artifacts without loading vault config or consent state. `capture_trace` resolves bodies and extracts text before any source-family consent gate.

Issue #88 closes that gap for every local sensor family Cairn already models:

- `hook`
- `ide`
- `terminal`
- `clipboard`
- `voice`
- `screen`
- `recording`

Explicit `cli`, `mcp`, and `proactive` writes are not local sensors for this issue; they remain governed by existing signed intent, scope, filter, and consent-timeline checks.

## 2. Acceptance Criteria

1. No local sensor capture occurs unless the sensor is enabled in config and has an active consent-journal enablement row.
2. Budget exhaustion drops the capture before body resolution or artifact writes and logs a body-free reason.
3. Sensor enablement changes are visible in both `consent_journal` and `.cairn/consent.log`.
4. Status output reports sensor enablement, latest consent state, budgets, retention defaults, and recent drop state.
5. Privacy denial is a first-class discard reason, appears in policy trace / audit metadata, and is visible through lint.

## 3. Architecture

```
                cairn sensor CLI
                      |
                      v
        .cairn/config.yaml + consent_journal
                      |
                      v
              SensorPolicySnapshot
                      |
      +---------------+----------------+
      |               |                |
      v               v                v
 screen capture   hook artifacts   capture_trace / recording
 pre-write gate   pre-write gate    pre-extraction gate
      |               |                |
      +---------------+----------------+
                      |
                      v
          .cairn/metrics.jsonl sensor_drop rows
                      |
                      v
                 cairn lint findings
```

### 3.1 Boundaries

- `cairn-core` owns pure sensor policy types: sensor names, budget checks, retention defaults, drop reason vocabulary, config mapping, and lint kind/schema additions.
- `cairn-store-sqlite` owns consent-journal reads and writes.
- `cairn-workflows` already owns `.cairn/consent.log`; the sensor command uses `ConsentLogMaterializer::tick` after appending journal rows.
- `cairn-cli` owns vault config mutation, metrics JSONL emission, command routing, and pre-I/O gates for `hook`, `screen`, `recording`, and `capture_trace`.
- `cairn-sensors-local` keeps source-side adapter drops, but receives config through a shared mapper rather than ad hoc test-only `LocalSensorConfig` construction.

## 4. Data Model

### 4.1 Config

Add a reusable local sensor setting:

```rust
pub struct LocalSensorRuntimeConfig {
    pub enabled: bool,
    pub budget: SensorCaptureBudget,
    pub retention: SensorRetentionConfig,
}

pub struct SensorCaptureBudget {
    pub max_items: Option<u64>,
    pub max_bytes: Option<u64>,
}

pub struct SensorRetentionConfig {
    pub max_days: Option<u32>,
}
```

Extend `SensorsConfig`:

```rust
pub struct SensorsConfig {
    pub hooks: LocalSensorRuntimeConfig,
    pub ide: LocalSensorRuntimeConfig,
    pub terminal: LocalSensorRuntimeConfig,
    pub clipboard: LocalSensorRuntimeConfig,
    pub voice: LocalSensorRuntimeConfig,
    pub screen: ScreenSensorConfig,
    pub recording: LocalSensorRuntimeConfig,
    pub slack: SlackSensorConfig,
}
```

`ScreenSensorConfig` keeps its existing backend/OCR fields and gains the shared retention field. Its existing `ScreenCaptureBudget` remains the screen-specific continuous-frame budget.

Defaults preserve the current operational posture where possible:

- `hooks.enabled = true`
- `ide.enabled = true`
- `terminal`, `clipboard`, `voice`, `screen`, `recording` default disabled
- all shared budgets default unlimited
- all retention defaults are `None`

Consent is still required even when config defaults to enabled. A missing consent row denies capture.

### 4.2 Sensor Labels

Policy gating is family-level at P0. The consent journal row for a command such as `cairn sensor enable hook` uses a deterministic family sensor label:

| Sensor command name | Consent sensor label |
|---|---|
| `hook`, `hooks` | `local:hook:default:v1` |
| `ide` | `local:ide:default:v1` |
| `terminal` | `local:terminal:default:v1` |
| `clipboard` | `local:clipboard:default:v1` |
| `voice` | `local:voice:default:v1` |
| `screen` | `local:screen:default:v1` |
| `recording` | `local:recording:default:v1` |

The gate maps incoming `SourceFamily` to this family label, then resolves the latest journal event for that label. Future per-instance sensor consent can extend the resolver without changing the P0 command surface.

### 4.3 Metrics

Sensor drops append one body-free JSON row to `.cairn/metrics.jsonl`:

```json
{
  "event": "sensor_drop",
  "sensor": "screen",
  "source_family": "screen",
  "reason": "privacy_denied",
  "stage": "pre_capture",
  "operation_id": "01...",
  "session_id": "optional",
  "turn_id": "optional",
  "budget": {
    "max_items": null,
    "max_bytes": null,
    "observed_items": 1,
    "observed_bytes": 24576
  }
}
```

No raw text, URL, title, file path, OCR body, command, prompt, or clipboard value appears in this row.

## 5. Command Surface

```
cairn sensor status [SENSOR] [--json]
cairn sensor enable SENSOR --reason REASON_CODE [--json]
cairn sensor disable SENSOR --reason REASON_CODE [--json]
```

Rules:

- `SENSOR` accepts `hook`, `hooks`, `ide`, `terminal`, `clipboard`, `voice`, `screen`, `recording`.
- `--reason` must match the existing consent reason-code grammar.
- Enable/disable updates `.cairn/config.yaml` atomically, appends a `sensor_enable` or `sensor_disable` row, then ticks `.cairn/consent.log`.
- JSON status emits the config state, latest journal state, effective gate result, budgets, retention, and last drop reason from metrics.
- Human status prints one line per sensor.

## 6. Gate Semantics

The policy decision is:

1. If the sensor is not a local sensor family, allow and leave existing gates to decide.
2. If config is disabled, deny with `disabled`.
3. If there is no latest `sensor_enable` row after the latest `sensor_disable` row, deny with `privacy_denied`.
4. If the configured budget rejects the observation, deny with `budget_exceeded`.
5. Otherwise allow.

Denied local sensor events do not resolve body bytes, call extractors, write derived payloads, or write body-bearing hook/screen artifacts.

## 7. Privacy Trace And Lint

Add first-class pipeline discard reason:

```rust
DiscardReason::PrivacyDenied -> "privacy_denied"
```

Add a policy gate:

```rust
PolicyGate::SensorConsent -> "sensor_consent"
```

Add two lint kinds through the IDL/codegen path:

- `sensor_privacy_denied`
- `sensor_budget_exceeded`

`cairn lint` reads `.cairn/metrics.jsonl` and emits those findings for recent `sensor_drop` rows. Findings include only sensor name, reason code, operation id if present, and path target `.cairn/metrics.jsonl`.

## 8. Test Strategy

TDD is required for each implementation slice.

- Config tests cover defaults, YAML round-trip, validation of zero budgets and zero retention days, and mapping to `LocalSensorConfig`.
- Store/policy tests cover latest enable/disable resolution.
- CLI tests cover `cairn sensor enable`, `disable`, and `status` writing config plus journal plus consent log.
- Hook tests prove trace artifacts are not written before consent and are written after enablement.
- Screen tests prove disabled or unconsented screen capture returns before the PNG path is created.
- Recording tests prove unconsented recording does not stage derived payloads.
- Capture-trace tests prove denied local sensor events do not resolve source bodies and produce `privacy_denied`.
- Lint tests prove `sensor_privacy_denied` and `sensor_budget_exceeded` findings are emitted from metrics rows.

## 9. Non-Goals

- No OS permission prompt UX beyond the existing screen permission probe.
- No new background continuous capture service.
- No retention purge worker changes. This issue records per-sensor retention defaults in config/status so the policy surface exists; actual purge enforcement remains in the existing vault retention machinery.
