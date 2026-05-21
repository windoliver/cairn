# Issue #88 Sensor Consent Privacy Gates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-local-sensor consent controls, budgets, retention defaults, pre-capture/pre-extraction privacy gates, audit metrics, status output, and lint visibility for issue #88.

**Architecture:** Keep policy decisions pure in `cairn-core`, persist consent in the existing SQLite `consent_journal`, mirror it through the existing `.cairn/consent.log` materializer, and enforce gates at every current local capture entry point: `hook`, `screen`, `recording`, and `capture_trace`.

**Tech Stack:** Rust 1.95.0 edition 2024, clap, serde, yaml_serde, rusqlite/tokio-rusqlite, tempfile, assert_cmd where existing tests use it, cairn-idl codegen.

**Spec:** `docs/superpowers/specs/2026-05-14-issue-88-sensor-consent-privacy-gates-design.md`

**Branch:** `codex/issue-88-sensor-consent-privacy-gates`

---

## File Structure

### Created files

| Path | Responsibility |
|---|---|
| `crates/cairn-core/src/domain/sensor_policy.rs` | Pure sensor names, family labels, gate input/output, budget checks, config mapping helpers. |
| `crates/cairn-cli/src/verbs/sensor.rs` | `cairn sensor` command, config mutation, journal append, consent-log tick, status rendering. |
| `crates/cairn-cli/src/sensor_gate.rs` | CLI-side I/O adapter: load latest consent, read/write sensor drop metrics, enforce gates before local capture I/O. |
| `crates/cairn-cli/tests/sensor_cli.rs` | E2E tests for enable/disable/status and journal/log visibility. |
| `crates/cairn-cli/tests/sensor_gate_cli.rs` | E2E tests for hook, screen, recording, capture_trace denial/allowance. |
| `crates/cairn-cli/tests/sensor_lint.rs` | E2E tests for lint findings from `sensor_drop` metrics rows. |

### Modified files

| Path | Why |
|---|---|
| `crates/cairn-core/src/config/mod.rs` | Add shared local sensor config, budgets, retention, validation, tests. |
| `crates/cairn-core/src/domain/mod.rs` | Export `sensor_policy`. |
| `crates/cairn-core/src/pipeline/filter/decision.rs` | Add `DiscardReason::PrivacyDenied`. |
| `crates/cairn-core/src/pipeline/filter/mod.rs` | Update discard reason docs. |
| `crates/cairn-core/src/policy_trace/gate.rs` | Add `PolicyGate::SensorConsent`. |
| `crates/cairn-core/src/policy_trace/detail.rs` | Support `privacy_denied` detail through existing discard reason path. |
| `crates/cairn-idl/schema/verbs/lint.json` | Add `sensor_privacy_denied`, `sensor_budget_exceeded`. |
| `crates/cairn-core/src/generated/verbs/lint.rs` | Regenerate from IDL. |
| `crates/cairn-core/src/generated/schemas/verbs/lint.json` | Regenerate from IDL. |
| `crates/cairn-core/src/verbs/lint/mod.rs` | Add `kind_key` entries for new lint kinds. |
| `crates/cairn-cli/src/verbs/lint.rs` | Read `sensor_drop` rows from metrics and append lint findings. |
| `crates/cairn-cli/src/command.rs` | Register `verbs::sensor::command()`. |
| `crates/cairn-cli/src/main.rs` | Dispatch `sensor`; pass vault root to `screen`; wire config-aware gates. |
| `crates/cairn-cli/src/verbs/mod.rs` | Export `sensor` module. |
| `crates/cairn-cli/src/verbs/status.rs` | Include generic sensor consent/status in status output. |
| `crates/cairn-idl/schema/prelude/status.json` | Add optional generic sensor status list. |
| `crates/cairn-core/src/generated/status.rs` | Regenerate from IDL. |
| `crates/cairn-core/src/generated/schemas/prelude/status.json` | Regenerate from IDL. |
| `crates/cairn-cli/src/hooks/mod.rs` | Gate body-bearing hook artifacts before writing traces/queue. |
| `crates/cairn-cli/src/verbs/screen.rs` | Gate screen capture before writing PNG and emit metrics on denied/drop. |
| `crates/cairn-cli/src/verbs/ingest/recording.rs` | Gate recording before planning/staging derived payloads. |
| `crates/cairn-cli/src/verbs/capture_trace.rs` | Gate local sensor events before source-body resolution/extraction. |
| `crates/cairn-sensors-local/src/config.rs` | Accept mapped budgets from core config. |
| `crates/cairn-sensors-local/src/outcome.rs` | Map drops to stable reason strings for metrics. |
| Existing tests under `crates/cairn-cli/tests` and `crates/cairn-core/src/config/mod.rs` | Update expectations for explicit consent where local sensors are involved. |

---

## Task 1: Core Config And Policy Types

**Files:**
- Create: `crates/cairn-core/src/domain/sensor_policy.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Modify: `crates/cairn-core/src/config/mod.rs`

- [ ] **Step 1: Write failing config tests**

Add tests in `crates/cairn-core/src/config/mod.rs`:

```rust
#[test]
fn local_sensor_defaults_require_consent_but_preserve_existing_enablement() {
    let config = CairnConfig::default();
    assert!(config.sensors.hooks.enabled);
    assert!(config.sensors.ide.enabled);
    assert!(!config.sensors.terminal.enabled);
    assert!(!config.sensors.clipboard.enabled);
    assert!(!config.sensors.voice.enabled);
    assert!(!config.sensors.screen.enabled);
    assert!(!config.sensors.recording.enabled);
    assert_eq!(config.sensors.hooks.budget.max_items, None);
    assert_eq!(config.sensors.hooks.budget.max_bytes, None);
    assert_eq!(config.sensors.hooks.retention.max_days, None);
}

#[test]
fn rejects_zero_local_sensor_budget_and_retention() {
    let mut config = CairnConfig::default();
    config.sensors.terminal.budget.max_items = Some(0);
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidBudget {
            field: "sensors.terminal.budget.max_items",
            value: 0
        })
    ));

    let mut config = CairnConfig::default();
    config.sensors.recording.retention.max_days = Some(0);
    assert!(matches!(
        config.validate(),
        Err(ConfigError::InvalidBudget {
            field: "sensors.recording.retention.max_days",
            value: 0
        })
    ));
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p cairn-core config::tests::local_sensor_defaults_require_consent_but_preserve_existing_enablement config::tests::rejects_zero_local_sensor_budget_and_retention
```

Expected: compile/test failure because `terminal`, `clipboard`, `voice`, `recording`, `budget`, and `retention` fields do not exist.

- [ ] **Step 3: Add core policy/config implementation**

Create `crates/cairn-core/src/domain/sensor_policy.rs` with:

```rust
use crate::domain::{SensorLabel, SourceFamily};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalSensorName {
    Hook,
    Ide,
    Terminal,
    Clipboard,
    Voice,
    Screen,
    Recording,
}

impl LocalSensorName {
    pub const ALL: [Self; 7] = [
        Self::Hook,
        Self::Ide,
        Self::Terminal,
        Self::Clipboard,
        Self::Voice,
        Self::Screen,
        Self::Recording,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Ide => "ide",
            Self::Terminal => "terminal",
            Self::Clipboard => "clipboard",
            Self::Voice => "voice",
            Self::Screen => "screen",
            Self::Recording => "recording",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "hook" | "hooks" => Some(Self::Hook),
            "ide" => Some(Self::Ide),
            "terminal" => Some(Self::Terminal),
            "clipboard" => Some(Self::Clipboard),
            "voice" => Some(Self::Voice),
            "screen" => Some(Self::Screen),
            "recording" => Some(Self::Recording),
            _ => None,
        }
    }

    pub const fn source_family(self) -> SourceFamily {
        match self {
            Self::Hook => SourceFamily::Hook,
            Self::Ide => SourceFamily::Ide,
            Self::Terminal => SourceFamily::Terminal,
            Self::Clipboard => SourceFamily::Clipboard,
            Self::Voice => SourceFamily::Voice,
            Self::Screen => SourceFamily::Screen,
            Self::Recording => SourceFamily::RecordingBatch,
        }
    }

    pub fn from_source_family(family: SourceFamily) -> Option<Self> {
        match family {
            SourceFamily::Hook => Some(Self::Hook),
            SourceFamily::Ide => Some(Self::Ide),
            SourceFamily::Terminal => Some(Self::Terminal),
            SourceFamily::Clipboard => Some(Self::Clipboard),
            SourceFamily::Voice => Some(Self::Voice),
            SourceFamily::Screen => Some(Self::Screen),
            SourceFamily::RecordingBatch => Some(Self::Recording),
            _ => None,
        }
    }

    pub fn family_label(self) -> SensorLabel {
        let label = match self {
            Self::Hook => "local:hook:default:v1",
            Self::Ide => "local:ide:default:v1",
            Self::Terminal => "local:terminal:default:v1",
            Self::Clipboard => "local:clipboard:default:v1",
            Self::Voice => "local:voice:default:v1",
            Self::Screen => "local:screen:default:v1",
            Self::Recording => "local:recording:default:v1",
        };
        SensorLabel::parse(label).expect("invariant: P0 sensor family labels are valid")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorGateReason {
    Disabled,
    PrivacyDenied,
    BudgetExceeded,
}

impl SensorGateReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::PrivacyDenied => "privacy_denied",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetObservation {
    pub items: u64,
    pub bytes: u64,
}
```

In `crates/cairn-core/src/domain/mod.rs`, export `pub mod sensor_policy;`.

In `crates/cairn-core/src/config/mod.rs`, add `SensorCaptureBudget`, `SensorRetentionConfig`, and `LocalSensorRuntimeConfig`, replace `SensorToggle` usage for `hooks` and `ide`, add `terminal`, `clipboard`, `voice`, and `recording`, and validate all optional budget/retention values are non-zero.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-core config::tests::local_sensor_defaults_require_consent_but_preserve_existing_enablement config::tests::rejects_zero_local_sensor_budget_and_retention
cargo test -p cairn-core config::tests::default_config_validates
```

Expected: all listed tests pass.

---

## Task 2: IDL Additions For Lint And Status

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/lint.json`
- Modify: `crates/cairn-idl/schema/prelude/status.json`
- Regenerate: `crates/cairn-core/src/generated/verbs/lint.rs`
- Regenerate: `crates/cairn-core/src/generated/status.rs`

- [ ] **Step 1: Add failing assertions against generated types**

Add a small compile test in `crates/cairn-core/src/verbs/lint/mod.rs` tests:

```rust
#[test]
fn sensor_lint_kinds_have_stable_keys() {
    assert_eq!(kind_key(Kind::SensorPrivacyDenied), "sensor_privacy_denied");
    assert_eq!(kind_key(Kind::SensorBudgetExceeded), "sensor_budget_exceeded");
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-core verbs::lint::tests::sensor_lint_kinds_have_stable_keys
```

Expected: compile failure because the generated enum variants do not exist.

- [ ] **Step 3: Update IDL and regenerate**

Add `sensor_budget_exceeded` and `sensor_privacy_denied` to `crates/cairn-idl/schema/verbs/lint.json` `Kind` enum.

Add optional `local` array under `sensors` in `crates/cairn-idl/schema/prelude/status.json`:

```json
"local": {
  "type": "array",
  "items": {
    "type": "object",
    "additionalProperties": false,
    "required": ["sensor", "enabled", "consent", "gate", "budget", "retention"],
    "properties": {
      "sensor": { "type": "string", "enum": ["hook", "ide", "terminal", "clipboard", "voice", "screen", "recording"] },
      "enabled": { "type": "boolean" },
      "consent": { "type": "string", "enum": ["enabled", "disabled", "missing"] },
      "gate": { "type": "string", "enum": ["allowed", "disabled", "privacy_denied", "budget_exceeded"] },
      "budget": { "type": "object" },
      "retention": { "type": "object" },
      "last_drop_reason": { "type": "string", "enum": ["disabled", "privacy_denied", "budget_exceeded", "policy_rejected", "malformed_observation"] }
    }
  }
}
```

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: generated Rust/schema files update.

- [ ] **Step 4: Implement `kind_key` mappings and verify**

Add to `kind_key` in `crates/cairn-core/src/verbs/lint/mod.rs`:

```rust
Kind::SensorBudgetExceeded => "sensor_budget_exceeded",
Kind::SensorPrivacyDenied => "sensor_privacy_denied",
```

Run:

```bash
cargo test -p cairn-core verbs::lint::tests::sensor_lint_kinds_have_stable_keys
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: both commands exit 0.

---

## Task 3: Consent Resolution And Metrics Adapter

**Files:**
- Create: `crates/cairn-cli/src/sensor_gate.rs`
- Modify: `crates/cairn-cli/src/lib.rs`

- [ ] **Step 1: Write failing unit tests for pure metric parsing**

Add tests in `crates/cairn-cli/src/sensor_gate.rs`:

```rust
#[test]
fn sensor_drop_metric_is_body_free_and_round_trips() {
    let row = SensorDropMetric {
        event: "sensor_drop",
        sensor: LocalSensorName::Screen,
        source_family: Some(SourceFamily::Screen),
        reason: SensorGateReason::PrivacyDenied,
        stage: SensorGateStage::PreCapture,
        operation_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
        session_id: None,
        turn_id: None,
        budget: None,
    };
    let json = serde_json::to_string(&row).expect("serialize metric");
    for banned in ["body", "text", "content", "raw", "snippet", "command", "url", "title", "file_path", "input"] {
        assert!(!json.contains(banned), "metric leaked banned field {banned}: {json}");
    }
    let decoded: SensorDropMetric = serde_json::from_str(&json).expect("decode metric");
    assert_eq!(decoded.reason, SensorGateReason::PrivacyDenied);
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli sensor_gate::tests::sensor_drop_metric_is_body_free_and_round_trips
```

Expected: compile failure because `sensor_gate` module and metric types do not exist.

- [ ] **Step 3: Implement metrics and consent readers**

Create `sensor_gate.rs` with:

- `SensorGateStage::{PreCapture, PreArtifact, PreExtraction}`
- `SensorConsentState::{Enabled, Disabled, Missing}`
- `SensorDropMetric`
- `append_sensor_drop_metric(vault_root, &SensorDropMetric)`
- `read_sensor_drop_metrics(vault_root)`
- `latest_sensor_consent(store, LocalSensorName)`, using `store.raw_conn_for_admin().call(...)` and `cairn_store_sqlite::consent::query_by_sensor`
- `evaluate_sensor_gate(config, consent_state, sensor, BudgetObservation) -> Result<(), SensorGateReason>`

Export `pub mod sensor_gate;` from `crates/cairn-cli/src/lib.rs`.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli sensor_gate::tests::sensor_drop_metric_is_body_free_and_round_trips
```

Expected: test passes.

---

## Task 4: Sensor Control CLI

**Files:**
- Create: `crates/cairn-cli/src/verbs/sensor.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Test: `crates/cairn-cli/tests/sensor_cli.rs`

- [ ] **Step 1: Write failing E2E test for enable/status**

Create `crates/cairn-cli/tests/sensor_cli.rs` with a test that bootstraps a temp vault, runs:

```bash
cairn sensor enable screen --reason operator_on --json
cairn sensor status screen --json
```

and asserts:

```rust
assert_eq!(enable["status"], "enabled");
assert_eq!(status["sensor"], "screen");
assert_eq!(status["enabled"], true);
assert_eq!(status["consent"], "enabled");
assert!(vault.path().join(".cairn/consent.log").exists());
assert!(std::fs::read_to_string(vault.path().join(".cairn/consent.log")).unwrap().contains("sensor_enable"));
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_cli sensor_enable_updates_config_journal_and_log
```

Expected: clap error because `sensor` command does not exist.

- [ ] **Step 3: Implement command**

Implement:

```rust
pub fn command() -> clap::Command
pub fn run(sub: &ArgMatches, vault_root: &Path, config: CairnConfig) -> ExitCode
```

The command:

- parses `LocalSensorName::parse`
- loads `<vault>/.cairn/config.yaml`
- toggles the matching config field
- writes YAML through a temp file in `.cairn` then `rename`
- opens `cairn_store_sqlite::open(<vault>/.cairn/cairn.db)`
- appends `ConsentEvent { kind: SensorEnable | SensorDisable, actor: hmn:local-operator, subject: format!("snr:{}", label.as_str()), sensor_id: Some(label), payload: ConsentPayload::SensorToggle { sensor_label: label, reason_code }, scope: "tenant=local", op_id: None, decided_at: now, expires_at: None }`
- ticks `ConsentLogMaterializer::open(<vault>/.cairn)?.tick(conn)`
- prints JSON/human status

Wire `.subcommand(verbs::sensor::command())` in `command.rs` and dispatch it in `main.rs` through `resolve_vault_and_config`.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_cli sensor_enable_updates_config_journal_and_log
```

Expected: test passes.

---

## Task 5: Status Output Includes Consent State

**Files:**
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Test: `crates/cairn-cli/tests/sensor_cli.rs`

- [ ] **Step 1: Add failing status test**

Add a test that enables `recording`, runs `cairn status --json`, and asserts:

```rust
let sensors = response["sensors"]["local"].as_array().expect("local sensors");
let recording = sensors.iter().find(|row| row["sensor"] == "recording").expect("recording row");
assert_eq!(recording["enabled"], true);
assert_eq!(recording["consent"], "enabled");
assert_eq!(recording["gate"], "allowed");
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_cli status_json_reports_sensor_consent_state
```

Expected: test fails because `sensors.local` is missing.

- [ ] **Step 3: Implement status mapping**

Add a helper in `status.rs` that builds one local status row per `LocalSensorName::ALL`, using config and `sensor_gate::latest_sensor_consent` when a bound vault/store is available. For no vault, report consent `missing` and gate `privacy_denied` for enabled local sensors.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_cli status_json_reports_sensor_consent_state
```

Expected: test passes.

---

## Task 6: Gate Hook Artifacts

**Files:**
- Modify: `crates/cairn-cli/src/hooks/mod.rs`
- Test: `crates/cairn-cli/tests/sensor_gate_cli.rs`
- Update: `crates/cairn-cli/tests/hook_cli.rs`

- [ ] **Step 1: Write failing hook denial test**

Create a test that bootstraps a vault, runs `cairn hook UserPromptSubmit --vault-path <vault> --payload <json> --json` without enabling `hook`, and asserts:

```rust
assert_eq!(out.status.code(), Some(77));
assert!(!vault.path().join(".cairn/hooks/traces").exists());
let metrics = std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).unwrap();
assert!(metrics.contains("\"event\":\"sensor_drop\""));
assert!(metrics.contains("\"reason\":\"privacy_denied\""));
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli hook_without_consent_writes_no_trace_artifact
```

Expected: test fails because hook currently writes trace artifacts.

- [ ] **Step 3: Implement hook gate**

In `hooks::run`, before loading or writing body-bearing artifacts for the hook command:

- resolve config from `<vault-path>/.cairn/config.yaml`
- open store
- evaluate `LocalSensorName::Hook`
- if denied, append metric with `stage = PreArtifact`, emit JSON failure with code `PermissionDenied`, and exit 77

Update existing hook tests to enable hook in their bootstrap helper by running the new sensor command or appending equivalent config/journal through test helper.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli hook_without_consent_writes_no_trace_artifact
cargo test -p cairn-cli --test hook_cli
```

Expected: all listed tests pass.

---

## Task 7: Gate Screen Capture Before PNG Write

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`
- Modify: `crates/cairn-cli/src/verbs/screen.rs`
- Test: `crates/cairn-cli/tests/sensor_gate_cli.rs`

- [ ] **Step 1: Write failing screen denial test**

Add a test that bootstraps a vault, runs `cairn screen capture --output <tmp>/screen.png --json` without enabling `screen`, and asserts exit 77 or 78, no PNG file, and a `sensor_drop` metric with `privacy_denied` when config is enabled but consent missing.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli screen_without_consent_writes_no_png
```

Expected: test fails because screen only checks config and does not log consent denial.

- [ ] **Step 3: Implement screen gate**

Change `verbs::screen::run(sub, &config)` to `verbs::screen::run(sub, vault_root, config)`. Before calling `capture_png_snapshot_configured`, evaluate `LocalSensorName::Screen`; on denial, append metric and return without touching the output path.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli screen_without_consent_writes_no_png
```

Expected: test passes.

---

## Task 8: Gate Recording Before Planning And Staging

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: `crates/cairn-cli/tests/sensor_gate_cli.rs`
- Update: `crates/cairn-cli/tests/recording_ingest.rs`

- [ ] **Step 1: Write failing recording denial test**

Add a test that bootstraps a vault, points at a recording fixture, runs `cairn ingest --recording <fixture> --json` without enabling `recording`, and asserts:

```rust
assert_ne!(out.status.code(), Some(0));
assert!(recording_payload_files(vault.path()).is_empty());
let metrics = std::fs::read_to_string(vault.path().join(".cairn/metrics.jsonl")).unwrap();
assert!(metrics.contains("\"sensor\":\"recording\""));
assert!(metrics.contains("\"reason\":\"privacy_denied\""));
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli recording_without_consent_stages_no_payloads
```

Expected: test fails because recording currently plans/stages without a sensor gate.

- [ ] **Step 3: Implement recording gate**

At the start of `recording::run`, after path/extension validation and before `build_recording_plan`, evaluate `LocalSensorName::Recording` with `items = 1` and `bytes = std::fs::metadata(recording_path).len()`. On denial, append metric and emit a JSON error with public reason `privacy_denied` or `budget_exceeded`.

Update existing recording ingest success tests to call `cairn sensor enable recording --reason test_enable --json` before ingest.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli recording_without_consent_stages_no_payloads
cargo test -p cairn-cli --test recording_ingest
```

Expected: all listed tests pass.

---

## Task 9: Gate Capture Trace Before Body Resolution

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Test: existing unit tests in `capture_trace.rs` plus `crates/cairn-cli/tests/sensor_gate_cli.rs`

- [ ] **Step 1: Write failing pre-extraction denial test**

Add a test that writes a capture_trace JSONL event with `source_family = "terminal"` and a source body file containing sentinel text. Run `cairn capture_trace --from <jsonl> --json` without enabling `terminal`. Assert:

```rust
assert_eq!(response["status"], "committed");
assert_eq!(response["data"]["failed_turns"][0]["reason"], "sensor_gate:privacy_denied");
assert!(!vault_contains_exact_bytes(vault.path(), b"SENTINEL_TERMINAL_BODY"));
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli capture_trace_denies_local_sensor_before_body_resolution
```

Expected: test fails because `capture_trace` currently resolves source bodies before any sensor consent gate.

- [ ] **Step 3: Implement capture_trace gate**

In `run_events_handler_inner_no_guard`, before `resolve_body_bytes`, map `event.source_family` to `LocalSensorName`. If local:

- evaluate config + latest consent + budget
- append metric with `stage = PreExtraction`
- push `PolicyTraceEntry::error(PolicyGate::SensorConsent, PolicyErrorCode::from_static(reason.as_str()))`
- add failed turn reason `sensor_gate:<reason>`
- skip body resolution and extraction

Thread `&CairnConfig` and `&SqliteMemoryStore` into the helper paths that currently call `run_events_handler_with_scope`.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_gate_cli capture_trace_denies_local_sensor_before_body_resolution
cargo test -p cairn-cli capture_trace
```

Expected: tests pass.

---

## Task 10: First-Class Privacy Denial And Budget Lint

**Files:**
- Modify: `crates/cairn-core/src/pipeline/filter/decision.rs`
- Modify: `crates/cairn-core/src/policy_trace/gate.rs`
- Modify: `crates/cairn-cli/src/verbs/lint.rs`
- Test: `crates/cairn-cli/tests/sensor_lint.rs`

- [ ] **Step 1: Write failing lint tests**

Create tests that write metrics rows directly:

```json
{"event":"sensor_drop","sensor":"clipboard","reason":"privacy_denied","stage":"pre_extraction"}
{"event":"sensor_drop","sensor":"screen","reason":"budget_exceeded","stage":"pre_capture"}
```

Run `cairn lint --json` and assert finding kinds contain:

```rust
Kind::SensorPrivacyDenied
Kind::SensorBudgetExceeded
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-cli --test sensor_lint
```

Expected: tests fail because lint does not read `sensor_drop` metrics.

- [ ] **Step 3: Implement lint bridge**

Add `DiscardReason::PrivacyDenied` and `as_str() -> "privacy_denied"`. Add `PolicyGate::SensorConsent`.

In `lint.rs`, parse `.cairn/metrics.jsonl`, ignore malformed non-`sensor_drop` rows, fail closed on malformed `sensor_drop` rows, and append:

- `Kind::SensorPrivacyDenied`, severity `Warning`, for `privacy_denied`
- `Kind::SensorBudgetExceeded`, severity `Warning`, for `budget_exceeded`

Use target path `.cairn/metrics.jsonl` and messages that name only sensor/reason/stage.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test sensor_lint
cargo test -p cairn-core pipeline::filter::decision
```

Expected: tests pass.

---

## Task 11: Map Core Config To Local Sensor Adapters

**Files:**
- Modify: `crates/cairn-sensors-local/src/config.rs`
- Modify: sensor adapter tests in `crates/cairn-sensors-local/src/*.rs`

- [ ] **Step 1: Write failing mapper test**

Add a test in `crates/cairn-sensors-local/src/config.rs`:

```rust
#[test]
fn maps_core_sensor_config_to_local_adapter_config() {
    let mut config = cairn_core::config::CairnConfig::default();
    config.sensors.clipboard.enabled = true;
    config.sensors.clipboard.budget.max_bytes = Some(128);
    let local = LocalSensorConfig::from_core(&config.sensors);
    assert!(local.clipboard.enabled);
    assert_eq!(local.clipboard.budget.max_bytes, Some(128));
    assert!(!local.screen.enabled);
}
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
cargo test -p cairn-sensors-local config::tests::maps_core_sensor_config_to_local_adapter_config
```

Expected: compile failure because `from_core` does not exist and core config lacks fields until Task 1 is complete.

- [ ] **Step 3: Implement mapper**

Add `LocalSensorConfig::from_core(&cairn_core::config::SensorsConfig) -> Self`, converting `u64` budgets to `usize` with saturation at `usize::MAX`.

- [ ] **Step 4: Verify GREEN**

Run:

```bash
cargo test -p cairn-sensors-local config::tests::maps_core_sensor_config_to_local_adapter_config
cargo test -p cairn-sensors-local
```

Expected: tests pass.

---

## Task 12: Final Verification And PR Prep

**Files:**
- All changed files.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all
```

Expected: exit 0.

- [ ] **Step 2: Run targeted test suite**

Run:

```bash
cargo test -p cairn-core config::tests::local_sensor_defaults_require_consent_but_preserve_existing_enablement
cargo test -p cairn-core verbs::lint::tests::sensor_lint_kinds_have_stable_keys
cargo test -p cairn-cli --test sensor_cli
cargo test -p cairn-cli --test sensor_gate_cli
cargo test -p cairn-cli --test sensor_lint
cargo test -p cairn-cli --test recording_ingest
cargo test -p cairn-cli --test hook_cli
cargo test -p cairn-sensors-local
```

Expected: all commands exit 0.

- [ ] **Step 3: Run workspace verification**

Run:

```bash
cargo build --workspace
cargo test --workspace
```

Expected: both commands exit 0.

- [ ] **Step 4: Review diff for body-free guarantees**

Run:

```bash
rg -n "\"body\"|\"text\"|\"content\"|\"raw\"|\"snippet\"|\"command\"|\"url\"|\"title\"|\"file_path\"|\"input\"" crates/cairn-cli/src/sensor_gate.rs crates/cairn-cli/src/verbs/sensor.rs
```

Expected: no banned metric/journal field names except in explicit test arrays asserting they are absent.

- [ ] **Step 5: Commit**

Run:

```bash
git status --short
git add docs/superpowers/specs/2026-05-14-issue-88-sensor-consent-privacy-gates-design.md \
        docs/superpowers/plans/2026-05-14-issue-88-sensor-consent-privacy-gates.md \
        crates/cairn-core \
        crates/cairn-idl \
        crates/cairn-cli \
        crates/cairn-sensors-local
git commit -m "feat: gate local sensors on consent and budgets (#88)"
```

Expected: commit succeeds.

---

## Self-Review

- Spec coverage: Every issue criterion maps to at least one task: config/consent command in Tasks 1 and 4, consent/status in Task 5, pre-capture/pre-extraction gates in Tasks 6 through 9, first-class privacy/lint in Task 10, adapter config in Task 11, verification in Task 12.
- Placeholder scan: The plan contains concrete file paths, commands, expected failures, and expected passing states. It avoids deferred implementation language.
- Type consistency: `LocalSensorName`, `SensorGateReason`, `SensorDropMetric`, and `PolicyGate::SensorConsent` are introduced before subsequent tasks depend on them.
