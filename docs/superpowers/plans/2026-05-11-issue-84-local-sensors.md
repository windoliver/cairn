# Issue 84 Local Sensors Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build deterministic hook, IDE, terminal, and clipboard adapters in `cairn-sensors-local` that emit validated `CaptureEvent`s or explicit drop reasons.

**Architecture:** Keep the core `SensorIngress` trait unchanged. Add focused adapter modules in `crates/cairn-sensors-local/src/` that gate observations by config and budget, sanitize terminal/clipboard text, build sensor-authored Mode A `CaptureEvent`s through `CaptureEvent::try_new`, and leave persistence to `capture_trace`.

**Tech Stack:** Rust 2024, `cairn-core` domain types, `sha2` for payload hashes, `regex` for local redaction, cargo integration tests.

---

## File Structure

- Modify `crates/cairn-sensors-local/Cargo.toml`: add `sha2` and `regex` workspace deps used by the adapter crate.
- Modify `crates/cairn-sensors-local/src/lib.rs`: expose modules and update `LocalSensorIngress` capabilities.
- Create `crates/cairn-sensors-local/src/config.rs`: local per-sensor settings and budgets.
- Create `crates/cairn-sensors-local/src/outcome.rs`: `EmitOutcome`, `DropReason`, and `SensorKind`.
- Create `crates/cairn-sensors-local/src/event.rs`: shared payload hash, sensor identity, actor-chain, and `CaptureEvent::try_new` helpers.
- Create `crates/cairn-sensors-local/src/policy.rs`: terminal/clipboard redaction and policy rejection helpers.
- Create `crates/cairn-sensors-local/src/hook.rs`: hook observation adapter.
- Create `crates/cairn-sensors-local/src/ide.rs`: IDE observation adapter.
- Create `crates/cairn-sensors-local/src/terminal.rs`: terminal observation adapter.
- Create `crates/cairn-sensors-local/src/clipboard.rs`: clipboard observation adapter.
- Create tests:
  - `crates/cairn-sensors-local/tests/local_sensor_capabilities.rs`
  - `crates/cairn-sensors-local/tests/local_sensor_hook_ide.rs`
  - `crates/cairn-sensors-local/tests/local_sensor_policy.rs`
  - `crates/cairn-sensors-local/tests/local_sensor_terminal_clipboard.rs`

## Task 1: Config, Outcome, And Capability Surface

**Files:**
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Create: `crates/cairn-sensors-local/src/config.rs`
- Create: `crates/cairn-sensors-local/src/outcome.rs`
- Test: `crates/cairn-sensors-local/tests/local_sensor_capabilities.rs`

- [ ] **Step 1: Write the failing capability/config test**

Create `crates/cairn-sensors-local/tests/local_sensor_capabilities.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::contract::sensor_ingress::SensorIngress;
use cairn_sensors_local::{
    CaptureBudget, DropReason, EmitOutcome, LocalSensorConfig, LocalSensorIngress, SensorKind,
    SensorSettings,
};

#[test]
fn local_sensor_ingress_advertises_batch_consent_capabilities() {
    let ingress = LocalSensorIngress;
    let caps = ingress.capabilities();

    assert!(caps.batches);
    assert!(!caps.streaming);
    assert!(caps.consent_aware);
}

#[test]
fn local_sensor_config_can_disable_every_source() {
    let config = LocalSensorConfig::all_disabled();

    assert!(!config.hooks.enabled);
    assert!(!config.ide.enabled);
    assert!(!config.terminal.enabled);
    assert!(!config.clipboard.enabled);
}

#[test]
fn emit_outcome_exposes_drop_reason_without_event() {
    let outcome = EmitOutcome::Dropped {
        sensor: SensorKind::Clipboard,
        reason: DropReason::Disabled,
    };

    assert!(outcome.event().is_none());
    assert_eq!(outcome.sensor(), Some(SensorKind::Clipboard));
    assert_eq!(outcome.drop_reason(), Some(&DropReason::Disabled));
}

#[test]
fn sensor_settings_default_budget_is_unbounded() {
    let settings = SensorSettings::enabled();

    assert!(settings.budget.allows(1, 1024));
    assert_eq!(settings.budget, CaptureBudget::default());
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_capabilities
```

Expected: compile failure with unresolved imports such as `CaptureBudget`, `EmitOutcome`, `LocalSensorConfig`, and `SensorKind`.

- [ ] **Step 3: Implement config and outcome types**

Create `crates/cairn-sensors-local/src/config.rs`:

```rust
//! Local sensor adapter configuration.

/// Per-sensor event budget enforced before payload hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureBudget {
    /// Maximum number of observations accepted by a single adapter call.
    pub max_items: Option<usize>,
    /// Maximum raw byte count accepted by a single adapter call.
    pub max_bytes: Option<usize>,
}

impl CaptureBudget {
    /// Return whether this budget accepts `items` and `bytes`.
    #[must_use]
    pub fn allows(self, items: usize, bytes: usize) -> bool {
        self.max_items.is_none_or(|limit| items <= limit)
            && self.max_bytes.is_none_or(|limit| bytes <= limit)
    }
}

/// Shared settings for one local sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorSettings {
    /// Whether this sensor emits events.
    pub enabled: bool,
    /// Source-side budget for one observation.
    pub budget: CaptureBudget,
}

impl SensorSettings {
    /// Enabled settings with no item or byte limit.
    #[must_use]
    pub const fn enabled() -> Self {
        Self {
            enabled: true,
            budget: CaptureBudget {
                max_items: None,
                max_bytes: None,
            },
        }
    }

    /// Disabled settings with no item or byte limit.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            budget: CaptureBudget {
                max_items: None,
                max_bytes: None,
            },
        }
    }
}

/// Configuration for deterministic local sensor adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSensorConfig {
    /// Hook sensor settings.
    pub hooks: SensorSettings,
    /// IDE sensor settings.
    pub ide: SensorSettings,
    /// Terminal sensor settings.
    pub terminal: SensorSettings,
    /// Clipboard sensor settings.
    pub clipboard: SensorSettings,
}

impl LocalSensorConfig {
    /// Disable every local sensor.
    #[must_use]
    pub const fn all_disabled() -> Self {
        Self {
            hooks: SensorSettings::disabled(),
            ide: SensorSettings::disabled(),
            terminal: SensorSettings::disabled(),
            clipboard: SensorSettings::disabled(),
        }
    }
}

impl Default for LocalSensorConfig {
    fn default() -> Self {
        Self {
            hooks: SensorSettings::enabled(),
            ide: SensorSettings::enabled(),
            terminal: SensorSettings::disabled(),
            clipboard: SensorSettings::disabled(),
        }
    }
}
```

Create `crates/cairn-sensors-local/src/outcome.rs`:

```rust
//! Local sensor emission outcomes.

use cairn_core::domain::CaptureEvent;

/// Local sensor family handled by this adapter crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorKind {
    /// Harness hook sensor.
    Hook,
    /// IDE event sensor.
    Ide,
    /// Terminal command/output sensor.
    Terminal,
    /// Clipboard snapshot sensor.
    Clipboard,
}

impl SensorKind {
    /// Convert a core source family emitted by this crate into its sensor kind.
    #[must_use]
    pub const fn from_source_family(family: cairn_core::domain::SourceFamily) -> Option<Self> {
        match family {
            cairn_core::domain::SourceFamily::Hook => Some(Self::Hook),
            cairn_core::domain::SourceFamily::Ide => Some(Self::Ide),
            cairn_core::domain::SourceFamily::Terminal => Some(Self::Terminal),
            cairn_core::domain::SourceFamily::Clipboard => Some(Self::Clipboard),
            _ => None,
        }
    }
}

/// Reason an observation produced no event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    /// Sensor was disabled at source.
    Disabled,
    /// Raw observation exceeded its configured source-side budget.
    BudgetExceeded,
    /// Local privacy policy rejected the observation.
    PolicyRejected(String),
    /// Observation was malformed or failed core capture validation.
    MalformedObservation(String),
}

/// Result of trying to emit one local sensor event.
#[derive(Debug, Clone, PartialEq)]
pub enum EmitOutcome {
    /// Observation became a validated capture event.
    Emitted(CaptureEvent),
    /// Observation was dropped before event emission.
    Dropped {
        /// Sensor that dropped the observation.
        sensor: SensorKind,
        /// Concrete drop reason.
        reason: DropReason,
    },
}

impl EmitOutcome {
    /// Borrow the emitted event when present.
    #[must_use]
    pub const fn event(&self) -> Option<&CaptureEvent> {
        match self {
            Self::Emitted(event) => Some(event),
            Self::Dropped { .. } => None,
        }
    }

    /// Sensor kind associated with this outcome.
    #[must_use]
    pub const fn sensor(&self) -> Option<SensorKind> {
        match self {
            Self::Emitted(event) => SensorKind::from_source_family(event.source_family),
            Self::Dropped { sensor, .. } => Some(*sensor),
        }
    }

    /// Borrow the drop reason when present.
    #[must_use]
    pub const fn drop_reason(&self) -> Option<&DropReason> {
        match self {
            Self::Emitted(_) => None,
            Self::Dropped { reason, .. } => Some(reason),
        }
    }
}
```

Modify `crates/cairn-sensors-local/src/lib.rs`:

```rust
//! Local sensors for Cairn — hook, IDE, terminal, clipboard, voice, screen.
//!
//! Deterministic P0 adapters for hook, IDE, terminal, and clipboard
//! observations live here. Runtime loops that collect OS or editor events
//! call these adapters and pass the resulting `CaptureEvent`s to the
//! ingestion pipeline.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod outcome;

use cairn_core::contract::sensor_ingress::{
    CONTRACT_VERSION, SensorIngress, SensorIngressCapabilities,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

pub use config::{CaptureBudget, LocalSensorConfig, SensorSettings};
pub use outcome::{DropReason, EmitOutcome, SensorKind};

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-sensors-local";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Accepted host contract version range. Single source of truth for both the
/// trait impl's `supported_contract_versions()` and the const-eval guard.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// Local sensor `SensorIngress` plugin registration type.
#[derive(Default)]
pub struct LocalSensorIngress;

#[async_trait::async_trait]
impl SensorIngress for LocalSensorIngress {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &SensorIngressCapabilities {
        static CAPS: SensorIngressCapabilities = SensorIngressCapabilities {
            batches: true,
            streaming: false,
            consent_aware: true,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }
}

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    SensorIngress,
    LocalSensorIngress,
    "cairn-sensors-local",
    MANIFEST_TOML
);
```

- [ ] **Step 4: Run the test and verify it passes**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_capabilities
```

Expected: all four tests in `local_sensor_capabilities` pass.

- [ ] **Step 5: Commit Task 1**

```bash
git add crates/cairn-sensors-local/src/lib.rs \
  crates/cairn-sensors-local/src/config.rs \
  crates/cairn-sensors-local/src/outcome.rs \
  crates/cairn-sensors-local/tests/local_sensor_capabilities.rs
git commit -m "feat(sensors): add local sensor config outcomes"
```

## Task 2: Shared CaptureEvent Construction

**Files:**
- Modify: `crates/cairn-sensors-local/Cargo.toml`
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Create: `crates/cairn-sensors-local/src/event.rs`
- Test: unit tests in `crates/cairn-sensors-local/src/event.rs`

- [ ] **Step 1: Add failing shared event tests**

Create `crates/cairn-sensors-local/src/event.rs` with tests first:

```rust
//! Shared local sensor event construction.

#[cfg(test)]
mod tests {
    use cairn_core::domain::{
        CaptureEventId, CaptureMode, CapturePayload, Rfc3339Timestamp, SourceFamily,
    };

    use super::{build_auto_event, payload_hash, payload_ref};

    fn event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid test ULID")
    }

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
    }

    #[test]
    fn payload_hash_uses_sha256_prefix() {
        let hash = payload_hash(b"hello").expect("hash parses");

        assert_eq!(
            hash.as_str(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn payload_ref_is_sources_family_event_json() {
        assert_eq!(
            payload_ref(SourceFamily::Hook, &event_id()),
            "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json"
        );
    }

    #[test]
    fn build_auto_event_binds_sensor_author_to_sensor_id() {
        let event = build_auto_event(
            event_id(),
            ts(),
            "local:hook:cc-session:v1",
            CapturePayload::Hook {
                hook_name: "UserPromptSubmit".to_owned(),
                tool_name: None,
            },
            SourceFamily::Hook,
            None,
            b"{\"prompt\":\"hi\"}",
        )
        .expect("event validates");

        assert_eq!(event.capture_mode, CaptureMode::Auto);
        assert_eq!(event.sensor_id.as_str(), "snr:local:hook:cc-session:v1");
        assert_eq!(event.actor_chain.len(), 1);
        assert_eq!(
            event.actor_chain[0].identity.as_str(),
            "snr:local:hook:cc-session:v1"
        );
        event.validate_for_capture().expect("fresh event is valid");
    }
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p cairn-sensors-local event::
```

Expected: compile failure for missing `build_auto_event`, `payload_hash`, and `payload_ref`.

- [ ] **Step 3: Implement shared event helpers**

Modify `crates/cairn-sensors-local/Cargo.toml`:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
tracing = { workspace = true }
sha2 = { workspace = true }
regex = { workspace = true }
```

Modify `crates/cairn-sensors-local/src/lib.rs` by adding the module:

```rust
mod event;
```

Replace `crates/cairn-sensors-local/src/event.rs` with:

```rust
//! Shared local sensor event construction.

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, DomainError, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use sha2::{Digest as _, Sha256};

/// Compute the canonical SHA-256 payload hash.
pub(crate) fn payload_hash(bytes: &[u8]) -> Result<PayloadHash, DomainError> {
    PayloadHash::parse(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Build the vault-relative source payload ref for an event.
pub(crate) fn payload_ref(family: SourceFamily, event_id: &CaptureEventId) -> String {
    format!("sources/{family}/{event_id}.json")
}

/// Build a validated Mode A event authored by the emitting sensor.
pub(crate) fn build_auto_event(
    event_id: CaptureEventId,
    captured_at: Rfc3339Timestamp,
    sensor_label: &'static str,
    payload: CapturePayload,
    source_family: SourceFamily,
    refs: Option<CaptureRefs>,
    sanitized_payload_bytes: &[u8],
) -> Result<CaptureEvent, DomainError> {
    let sensor_id = Identity::parse(format!("snr:{sensor_label}"))?;
    let actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: sensor_id.clone(),
        at: captured_at.clone(),
    }];
    let payload_hash = payload_hash(sanitized_payload_bytes)?;
    let payload_ref = payload_ref(source_family, &event_id);

    CaptureEvent::try_new(
        event_id,
        sensor_id,
        CaptureMode::Auto,
        actor_chain,
        refs,
        payload_hash,
        payload_ref,
        captured_at,
        payload,
        source_family,
    )
}

#[cfg(test)]
mod tests {
    use cairn_core::domain::{
        CaptureEventId, CaptureMode, CapturePayload, Rfc3339Timestamp, SourceFamily,
    };

    use super::{build_auto_event, payload_hash, payload_ref};

    fn event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid test ULID")
    }

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
    }

    #[test]
    fn payload_hash_uses_sha256_prefix() {
        let hash = payload_hash(b"hello").expect("hash parses");

        assert_eq!(
            hash.as_str(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn payload_ref_is_sources_family_event_json() {
        assert_eq!(
            payload_ref(SourceFamily::Hook, &event_id()),
            "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json"
        );
    }

    #[test]
    fn build_auto_event_binds_sensor_author_to_sensor_id() {
        let event = build_auto_event(
            event_id(),
            ts(),
            "local:hook:cc-session:v1",
            CapturePayload::Hook {
                hook_name: "UserPromptSubmit".to_owned(),
                tool_name: None,
            },
            SourceFamily::Hook,
            None,
            b"{\"prompt\":\"hi\"}",
        )
        .expect("event validates");

        assert_eq!(event.capture_mode, CaptureMode::Auto);
        assert_eq!(event.sensor_id.as_str(), "snr:local:hook:cc-session:v1");
        assert_eq!(event.actor_chain.len(), 1);
        assert_eq!(
            event.actor_chain[0].identity.as_str(),
            "snr:local:hook:cc-session:v1"
        );
        event.validate_for_capture().expect("fresh event is valid");
    }
}
```

- [ ] **Step 4: Run the test and verify it passes**

Run:

```bash
cargo test -p cairn-sensors-local event::
```

Expected: the three `event` module tests pass.

- [ ] **Step 5: Commit Task 2**

```bash
git add crates/cairn-sensors-local/Cargo.toml \
  crates/cairn-sensors-local/src/lib.rs \
  crates/cairn-sensors-local/src/event.rs
git commit -m "feat(sensors): build shared capture events"
```

## Task 3: Hook And IDE Adapters

**Files:**
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Create: `crates/cairn-sensors-local/src/hook.rs`
- Create: `crates/cairn-sensors-local/src/ide.rs`
- Test: `crates/cairn-sensors-local/tests/local_sensor_hook_ide.rs`

- [ ] **Step 1: Write failing hook and IDE adapter tests**

Create `crates/cairn-sensors-local/tests/local_sensor_hook_ide.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::domain::{
    CaptureEvent, CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};
use cairn_sensors_local::hook::{HookHarness, HookObservation};
use cairn_sensors_local::ide::IdeObservation;
use cairn_sensors_local::{
    DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings, hook, ide,
};

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
}

fn emitted(outcome: EmitOutcome) -> CaptureEvent {
    match outcome {
        EmitOutcome::Emitted(event) => event,
        EmitOutcome::Dropped { sensor, reason } => {
            panic!("expected emitted event, got drop from {sensor:?}: {reason:?}")
        }
    }
}

#[test]
fn enabled_hook_sensor_emits_valid_capture_event() {
    let observation = HookObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        captured_at: ts(),
        harness: HookHarness::ClaudeCode,
        hook_name: "UserPromptSubmit".to_owned(),
        tool_name: None,
        refs: Some(CaptureRefs {
            session_id: Some("session-1".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            tool_id: None,
        }),
        raw_payload: br#"{"prompt":"remember this"}"#.to_vec(),
    };

    let event = emitted(hook::emit(&LocalSensorConfig::default(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:hook:cc-session:v1");
    assert_eq!(event.source_family, SourceFamily::Hook);
    match event.payload {
        CapturePayload::Hook {
            hook_name,
            tool_name,
        } => {
            assert_eq!(hook_name, "UserPromptSubmit");
            assert_eq!(tool_name, None);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn disabled_hook_sensor_drops_without_event() {
    let mut config = LocalSensorConfig::default();
    config.hooks = SensorSettings::disabled();
    let observation = HookObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAW"),
        captured_at: ts(),
        harness: HookHarness::Codex,
        hook_name: "SessionStart".to_owned(),
        tool_name: None,
        refs: None,
        raw_payload: br#"{"session_id":"session-1"}"#.to_vec(),
    };

    let outcome = hook::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::Disabled
        }
    );
}

#[test]
fn enabled_ide_sensor_emits_valid_capture_event() {
    let observation = IdeObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAX"),
        captured_at: ts(),
        file_path: "crates/cairn-core/src/domain/capture.rs".to_owned(),
        event_kind: "diagnostic".to_owned(),
        refs: None,
        raw_payload: br#"{"diagnostics":1}"#.to_vec(),
    };

    let event = emitted(ide::emit(&LocalSensorConfig::default(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:ide:default:v1");
    assert_eq!(event.source_family, SourceFamily::Ide);
    match event.payload {
        CapturePayload::Ide {
            file_path,
            event_kind,
        } => {
            assert_eq!(file_path, "crates/cairn-core/src/domain/capture.rs");
            assert_eq!(event_kind, "diagnostic");
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_hook_ide
```

Expected: compile failure for missing `hook`, `ide`, `HookObservation`, `HookHarness`, and `IdeObservation`.

- [ ] **Step 3: Implement hook and IDE adapters**

Modify `crates/cairn-sensors-local/src/lib.rs`:

```rust
mod event;
pub mod hook;
pub mod ide;
```

Create `crates/cairn-sensors-local/src/hook.rs`:

```rust
//! Hook local sensor adapter.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};

use crate::config::LocalSensorConfig;
use crate::event::build_auto_event;
use crate::outcome::{DropReason, EmitOutcome, SensorKind};

/// Supported local hook harness labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookHarness {
    /// Claude Code reference hook surface.
    ClaudeCode,
    /// Codex hook surface.
    Codex,
    /// Gemini hook surface.
    Gemini,
}

impl HookHarness {
    const fn sensor_label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "local:hook:cc-session:v1",
            Self::Codex => "local:hook:codex-session:v1",
            Self::Gemini => "local:hook:gemini-session:v1",
        }
    }
}

/// Already-observed harness hook payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookObservation {
    /// Capture event id.
    pub event_id: CaptureEventId,
    /// Capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Harness that emitted this hook.
    pub harness: HookHarness,
    /// Harness hook name.
    pub hook_name: String,
    /// Optional tool name for tool hooks.
    pub tool_name: Option<String>,
    /// Optional session/turn/tool refs.
    pub refs: Option<CaptureRefs>,
    /// Raw hook payload bytes after harness parsing.
    pub raw_payload: Vec<u8>,
}

/// Emit a hook capture event or explicit drop reason.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: HookObservation) -> EmitOutcome {
    if !config.hooks.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::Disabled,
        };
    }
    if !config.hooks.budget.allows(1, observation.raw_payload.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::BudgetExceeded,
        };
    }

    let event = build_auto_event(
        observation.event_id,
        observation.captured_at,
        observation.harness.sensor_label(),
        CapturePayload::Hook {
            hook_name: observation.hook_name,
            tool_name: observation.tool_name,
        },
        SourceFamily::Hook,
        observation.refs,
        &observation.raw_payload,
    );

    event.map_or_else(
        |err| EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
        EmitOutcome::Emitted,
    )
}
```

Create `crates/cairn-sensors-local/src/ide.rs`:

```rust
//! IDE local sensor adapter.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};

use crate::config::LocalSensorConfig;
use crate::event::build_auto_event;
use crate::outcome::{DropReason, EmitOutcome, SensorKind};

const IDE_SENSOR_LABEL: &str = "local:ide:default:v1";

/// Already-observed IDE event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeObservation {
    /// Capture event id.
    pub event_id: CaptureEventId,
    /// Capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Workspace-relative path.
    pub file_path: String,
    /// IDE event subtype.
    pub event_kind: String,
    /// Optional session/turn/tool refs.
    pub refs: Option<CaptureRefs>,
    /// Raw IDE payload bytes after editor parsing.
    pub raw_payload: Vec<u8>,
}

/// Emit an IDE capture event or explicit drop reason.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: IdeObservation) -> EmitOutcome {
    if !config.ide.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::Disabled,
        };
    }
    if !config.ide.budget.allows(1, observation.raw_payload.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::BudgetExceeded,
        };
    }

    let event = build_auto_event(
        observation.event_id,
        observation.captured_at,
        IDE_SENSOR_LABEL,
        CapturePayload::Ide {
            file_path: observation.file_path,
            event_kind: observation.event_kind,
        },
        SourceFamily::Ide,
        observation.refs,
        &observation.raw_payload,
    );

    event.map_or_else(
        |err| EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
        EmitOutcome::Emitted,
    )
}
```

- [ ] **Step 4: Run the test and verify it passes**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_hook_ide
```

Expected: all three tests in `local_sensor_hook_ide` pass.

- [ ] **Step 5: Commit Task 3**

```bash
git add crates/cairn-sensors-local/src/lib.rs \
  crates/cairn-sensors-local/src/hook.rs \
  crates/cairn-sensors-local/src/ide.rs \
  crates/cairn-sensors-local/tests/local_sensor_hook_ide.rs
git commit -m "feat(sensors): emit hook and ide capture events"
```

## Task 4: Redaction And Policy Helpers

**Files:**
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Create: `crates/cairn-sensors-local/src/policy.rs`
- Test: `crates/cairn-sensors-local/tests/local_sensor_policy.rs`

- [ ] **Step 1: Write failing policy tests**

Create `crates/cairn-sensors-local/tests/local_sensor_policy.rs`:

```rust
#![allow(missing_docs)]

use cairn_sensors_local::policy::{PolicyAction, sanitize_text_payload};

#[test]
fn redacts_common_secret_assignments() {
    let action = sanitize_text_payload("run API_KEY=sk-test TOKEN=abc ok");

    assert_eq!(
        action,
        PolicyAction::Sanitized("run API_KEY=[REDACTED] TOKEN=[REDACTED] ok".to_owned())
    );
}

#[test]
fn redacts_authorization_bearer_header() {
    let action = sanitize_text_payload("Authorization: Bearer abc.def-123");

    assert_eq!(
        action,
        PolicyAction::Sanitized("Authorization: Bearer [REDACTED]".to_owned())
    );
}

#[test]
fn rejects_private_key_blocks() {
    let action = sanitize_text_payload("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----");

    assert_eq!(
        action,
        PolicyAction::Rejected("private key block".to_owned())
    );
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_policy
```

Expected: compile failure for missing `policy`, `PolicyAction`, and `sanitize_text_payload`.

- [ ] **Step 3: Implement policy helpers**

Modify `crates/cairn-sensors-local/src/lib.rs`:

```rust
pub mod policy;
```

Create `crates/cairn-sensors-local/src/policy.rs`:

```rust
//! Local redaction and source-side drop policy.

use regex::Regex;

/// Policy result for text-bearing sensor payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    /// Text was accepted after redaction.
    Sanitized(String),
    /// Text was rejected and must not be emitted.
    Rejected(String),
}

/// Sanitize text payloads before hashing or event construction.
#[must_use]
pub fn sanitize_text_payload(input: &str) -> PolicyAction {
    if contains_private_key_block(input) {
        return PolicyAction::Rejected("private key block".to_owned());
    }

    let mut text = input.to_owned();
    text = redact_regex(
        &text,
        r"(?i)\b([A-Z0-9_]*(TOKEN|API_KEY|SECRET|PASSWORD)[A-Z0-9_]*)=([^\s]+)",
        "$1=[REDACTED]",
    );
    text = redact_regex(
        &text,
        r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9._~+/=-]+",
        "Authorization: Bearer [REDACTED]",
    );

    PolicyAction::Sanitized(text)
}

fn redact_regex(input: &str, pattern: &str, replacement: &str) -> String {
    match Regex::new(pattern) {
        Ok(regex) => regex.replace_all(input, replacement).into_owned(),
        Err(_) => input.to_owned(),
    }
}

fn contains_private_key_block(input: &str) -> bool {
    let upper = input.to_ascii_uppercase();
    upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY-----")
}
```

- [ ] **Step 4: Run the test and verify it passes**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_policy
```

Expected: all three policy tests pass.

- [ ] **Step 5: Commit Task 4**

```bash
git add crates/cairn-sensors-local/src/lib.rs \
  crates/cairn-sensors-local/src/policy.rs \
  crates/cairn-sensors-local/tests/local_sensor_policy.rs
git commit -m "feat(sensors): redact local text payloads"
```

## Task 5: Terminal And Clipboard Adapters

**Files:**
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Create: `crates/cairn-sensors-local/src/terminal.rs`
- Create: `crates/cairn-sensors-local/src/clipboard.rs`
- Test: `crates/cairn-sensors-local/tests/local_sensor_terminal_clipboard.rs`

- [ ] **Step 1: Write failing terminal and clipboard tests**

Create `crates/cairn-sensors-local/tests/local_sensor_terminal_clipboard.rs`:

```rust
#![allow(missing_docs)]

use cairn_core::domain::{
    CaptureEvent, CaptureEventId, CapturePayload, Rfc3339Timestamp, SourceFamily, TerminalContext,
};
use cairn_sensors_local::clipboard::ClipboardObservation;
use cairn_sensors_local::terminal::TerminalObservation;
use cairn_sensors_local::{
    CaptureBudget, DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings,
    clipboard, terminal,
};
use sha2::{Digest as _, Sha256};

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
}

fn enabled_config() -> LocalSensorConfig {
    LocalSensorConfig {
        terminal: SensorSettings::enabled(),
        clipboard: SensorSettings::enabled(),
        ..LocalSensorConfig::default()
    }
}

fn emitted(outcome: EmitOutcome) -> CaptureEvent {
    match outcome {
        EmitOutcome::Emitted(event) => event,
        EmitOutcome::Dropped { sensor, reason } => {
            panic!("expected emitted event, got drop from {sensor:?}: {reason:?}")
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn terminal_sensor_redacts_before_hashing_and_emits_valid_event() {
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAY"),
        captured_at: ts(),
        command: "run TOKEN=secret".to_owned(),
        exit_code: Some(0),
        context: Some(TerminalContext::InteractiveTty),
        output: "PASSWORD=hunter2".to_owned(),
        refs: None,
    };

    let event = emitted(terminal::emit(&enabled_config(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:terminal:default:v1");
    assert_eq!(event.source_family, SourceFamily::Terminal);
    assert_eq!(
        event.payload_hash.as_str(),
        hash(b"run TOKEN=[REDACTED]\nPASSWORD=[REDACTED]")
    );
    match event.payload {
        CapturePayload::Terminal {
            command,
            exit_code,
            context,
        } => {
            assert_eq!(command, "run TOKEN=[REDACTED]");
            assert_eq!(exit_code, Some(0));
            assert_eq!(context, Some(TerminalContext::InteractiveTty));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn terminal_sensor_drops_missing_context() {
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAZ"),
        captured_at: ts(),
        command: "cargo test".to_owned(),
        exit_code: None,
        context: None,
        output: String::new(),
        refs: None,
    };

    let outcome = terminal::emit(&enabled_config(), observation);

    assert!(matches!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::MalformedObservation(_)
        }
    ));
}

#[test]
fn terminal_sensor_drops_private_key_output() {
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB0"),
        captured_at: ts(),
        command: "cat key.pem".to_owned(),
        exit_code: Some(0),
        context: Some(TerminalContext::InteractiveTty),
        output: "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----".to_owned(),
        refs: None,
    };

    let outcome = terminal::emit(&enabled_config(), observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::PolicyRejected("private key block".to_owned())
        }
    );
}

#[test]
fn terminal_budget_drops_before_event_creation() {
    let mut config = enabled_config();
    config.terminal.budget = CaptureBudget {
        max_items: Some(1),
        max_bytes: Some(4),
    };
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB1"),
        captured_at: ts(),
        command: "12345".to_owned(),
        exit_code: None,
        context: Some(TerminalContext::NonInteractiveOrStructured),
        output: String::new(),
        refs: None,
    };

    let outcome = terminal::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::BudgetExceeded
        }
    );
}

#[test]
fn clipboard_text_redacts_before_hashing_and_emits_valid_event() {
    let observation = ClipboardObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB2"),
        captured_at: ts(),
        mime_type: "text/plain".to_owned(),
        bytes: b"API_KEY=secret".to_vec(),
        metadata_only: false,
        refs: None,
    };

    let event = emitted(clipboard::emit(&enabled_config(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:clipboard:default:v1");
    assert_eq!(event.source_family, SourceFamily::Clipboard);
    assert_eq!(event.payload_hash.as_str(), hash(b"API_KEY=[REDACTED]"));
    match event.payload {
        CapturePayload::Clipboard {
            mime_type,
            byte_len,
        } => {
            assert_eq!(mime_type, "text/plain");
            assert_eq!(byte_len, 18);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn clipboard_drops_unsupported_mime_without_metadata_only() {
    let observation = ClipboardObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB3"),
        captured_at: ts(),
        mime_type: "application/octet-stream".to_owned(),
        bytes: vec![1, 2, 3],
        metadata_only: false,
        refs: None,
    };

    let outcome = clipboard::emit(&enabled_config(), observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::PolicyRejected("unsupported clipboard MIME type".to_owned())
        }
    );
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_terminal_clipboard
```

Expected: compile failure for missing `terminal`, `clipboard`, `TerminalObservation`, and `ClipboardObservation`.

- [ ] **Step 3: Implement terminal and clipboard adapters**

Modify `crates/cairn-sensors-local/src/lib.rs`:

```rust
pub mod clipboard;
pub mod terminal;
```

Create `crates/cairn-sensors-local/src/terminal.rs`:

```rust
//! Terminal local sensor adapter.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily, TerminalContext,
};

use crate::config::LocalSensorConfig;
use crate::event::build_auto_event;
use crate::outcome::{DropReason, EmitOutcome, SensorKind};
use crate::policy::{PolicyAction, sanitize_text_payload};

const TERMINAL_SENSOR_LABEL: &str = "local:terminal:default:v1";

/// Already-observed terminal command/output payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalObservation {
    /// Capture event id.
    pub event_id: CaptureEventId,
    /// Capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Command text.
    pub command: String,
    /// Exit code if known.
    pub exit_code: Option<i32>,
    /// Terminal routing context for fresh writes.
    pub context: Option<TerminalContext>,
    /// Captured output text.
    pub output: String,
    /// Optional session/turn/tool refs.
    pub refs: Option<CaptureRefs>,
}

/// Emit a terminal capture event or explicit drop reason.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: TerminalObservation) -> EmitOutcome {
    if !config.terminal.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::Disabled,
        };
    }
    let raw_len = observation.command.len() + observation.output.len();
    if !config.terminal.budget.allows(1, raw_len) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::BudgetExceeded,
        };
    }
    let Some(context) = observation.context else {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::MalformedObservation("terminal context is required".to_owned()),
        };
    };

    let command = match sanitize_text_payload(&observation.command) {
        PolicyAction::Sanitized(text) => text,
        PolicyAction::Rejected(reason) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Terminal,
                reason: DropReason::PolicyRejected(reason),
            };
        }
    };
    let output = match sanitize_text_payload(&observation.output) {
        PolicyAction::Sanitized(text) => text,
        PolicyAction::Rejected(reason) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Terminal,
                reason: DropReason::PolicyRejected(reason),
            };
        }
    };
    let sanitized = format!("{command}\n{output}");

    let event = build_auto_event(
        observation.event_id,
        observation.captured_at,
        TERMINAL_SENSOR_LABEL,
        CapturePayload::Terminal {
            command,
            exit_code: observation.exit_code,
            context: Some(context),
        },
        SourceFamily::Terminal,
        observation.refs,
        sanitized.as_bytes(),
    );

    event.map_or_else(
        |err| EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
        EmitOutcome::Emitted,
    )
}
```

Create `crates/cairn-sensors-local/src/clipboard.rs`:

```rust
//! Clipboard local sensor adapter.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};

use crate::config::LocalSensorConfig;
use crate::event::build_auto_event;
use crate::outcome::{DropReason, EmitOutcome, SensorKind};
use crate::policy::{PolicyAction, sanitize_text_payload};

const CLIPBOARD_SENSOR_LABEL: &str = "local:clipboard:default:v1";

/// Already-observed clipboard payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardObservation {
    /// Capture event id.
    pub event_id: CaptureEventId,
    /// Capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Clipboard MIME type.
    pub mime_type: String,
    /// Clipboard bytes.
    pub bytes: Vec<u8>,
    /// Whether non-text payloads may emit metadata only.
    pub metadata_only: bool,
    /// Optional session/turn/tool refs.
    pub refs: Option<CaptureRefs>,
}

/// Emit a clipboard capture event or explicit drop reason.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: ClipboardObservation) -> EmitOutcome {
    if !config.clipboard.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::Disabled,
        };
    }
    if !config.clipboard.budget.allows(1, observation.bytes.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::BudgetExceeded,
        };
    }

    let sanitized_bytes = if observation.mime_type == "text/plain" {
        let text = match String::from_utf8(observation.bytes) {
            Ok(text) => text,
            Err(_) => {
                return EmitOutcome::Dropped {
                    sensor: SensorKind::Clipboard,
                    reason: DropReason::PolicyRejected("clipboard text is not UTF-8".to_owned()),
                };
            }
        };
        match sanitize_text_payload(&text) {
            PolicyAction::Sanitized(text) => text.into_bytes(),
            PolicyAction::Rejected(reason) => {
                return EmitOutcome::Dropped {
                    sensor: SensorKind::Clipboard,
                    reason: DropReason::PolicyRejected(reason),
                };
            }
        }
    } else if observation.metadata_only {
        Vec::new()
    } else {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::PolicyRejected("unsupported clipboard MIME type".to_owned()),
        };
    };

    let byte_len = match u64::try_from(sanitized_bytes.len()) {
        Ok(len) => len,
        Err(_) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Clipboard,
                reason: DropReason::MalformedObservation(
                    "clipboard payload length exceeds u64".to_owned(),
                ),
            };
        }
    };

    let event = build_auto_event(
        observation.event_id,
        observation.captured_at,
        CLIPBOARD_SENSOR_LABEL,
        CapturePayload::Clipboard {
            mime_type: observation.mime_type,
            byte_len,
        },
        SourceFamily::Clipboard,
        observation.refs,
        &sanitized_bytes,
    );

    event.map_or_else(
        |err| EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
        EmitOutcome::Emitted,
    )
}
```

- [ ] **Step 4: Run the test and verify it passes**

Run:

```bash
cargo test -p cairn-sensors-local --test local_sensor_terminal_clipboard
```

Expected: all six tests in `local_sensor_terminal_clipboard` pass.

- [ ] **Step 5: Commit Task 5**

```bash
git add crates/cairn-sensors-local/src/lib.rs \
  crates/cairn-sensors-local/src/terminal.rs \
  crates/cairn-sensors-local/src/clipboard.rs \
  crates/cairn-sensors-local/tests/local_sensor_terminal_clipboard.rs
git commit -m "feat(sensors): emit terminal and clipboard events"
```

## Task 6: Full Verification And Cleanup

**Files:**
- Modify only files touched by previous tasks if formatting or clippy requires it.

- [ ] **Step 1: Run rustfmt**

Run:

```bash
cargo fmt --all
```

Expected: command exits `0`.

- [ ] **Step 2: Run focused crate tests**

Run:

```bash
cargo test -p cairn-sensors-local
```

Expected: all `cairn-sensors-local` unit, integration, and doc tests pass.

- [ ] **Step 3: Run core capture boundary tests**

Run:

```bash
cargo test -p cairn-core capture
```

Expected: all selected `cairn-core` capture tests pass. This confirms the adapters still satisfy the core `CaptureEvent` validation boundary without editing core.

- [ ] **Step 4: Run clippy for touched crates**

Run:

```bash
cargo clippy -p cairn-sensors-local -p cairn-core --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 5: Inspect final diff**

Run:

```bash
git status --short
git diff --stat origin/main..HEAD
```

Expected: only the issue #84 design/plan docs and `cairn-sensors-local` adapter files changed.

- [ ] **Step 6: Commit verification cleanup if needed**

If `cargo fmt` or clippy required edits after Task 5, commit those edits:

```bash
git add crates/cairn-sensors-local Cargo.toml
git commit -m "chore(sensors): format local sensor adapters"
```

If no files changed, do not create an empty commit.
