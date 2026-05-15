//! CLI-side local sensor gate adapters.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use cairn_core::config::{CairnConfig, LocalSensorRuntimeConfig, SensorCaptureBudget};
use cairn_core::domain::{
    BudgetObservation, ConsentKind, LocalSensorName, SensorGateReason, SensorLabel, SourceFamily,
};
use cairn_store_sqlite::SqliteMemoryStore;
use serde::{Deserialize, Serialize};

/// Stage where a local sensor capture was denied or dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorGateStage {
    /// Before an OS/backend capture is attempted.
    PreCapture,
    /// Before a body-bearing hook artifact is written.
    PreArtifact,
    /// Before capture_trace resolves body bytes or dispatches extraction.
    PreExtraction,
}

/// Budget details attached to a body-free sensor drop metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorDropBudgetMetric {
    /// Configured item limit, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    /// Configured byte limit, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Observed item count.
    pub observed_items: u64,
    /// Observed byte count.
    pub observed_bytes: u64,
}

/// Body-free JSONL metric emitted when a local sensor capture is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorDropMetric {
    /// Constant event discriminator.
    pub event: &'static str,
    /// Local sensor family.
    pub sensor: LocalSensorName,
    /// Capture source family, when there is an event envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_family: Option<SourceFamily>,
    /// Stable body-free drop reason.
    pub reason: SensorGateReason,
    /// Gate stage that produced the drop.
    pub stage: SensorGateStage,
    /// Operation id, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Capture session id, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Capture turn id, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Budget details for budget drops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<SensorDropBudgetMetric>,
}

impl<'de> Deserialize<'de> for SensorDropMetric {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            event: String,
            sensor: LocalSensorName,
            #[serde(default)]
            source_family: Option<SourceFamily>,
            reason: SensorGateReason,
            stage: SensorGateStage,
            #[serde(default)]
            operation_id: Option<String>,
            #[serde(default)]
            session_id: Option<String>,
            #[serde(default)]
            turn_id: Option<String>,
            #[serde(default)]
            budget: Option<SensorDropBudgetMetric>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.event != SENSOR_DROP_EVENT {
            return Err(serde::de::Error::custom(format!(
                "expected event {SENSOR_DROP_EVENT}, got {}",
                raw.event
            )));
        }
        Ok(Self {
            event: SENSOR_DROP_EVENT,
            sensor: raw.sensor,
            source_family: raw.source_family,
            reason: raw.reason,
            stage: raw.stage,
            operation_id: raw.operation_id,
            session_id: raw.session_id,
            turn_id: raw.turn_id,
            budget: raw.budget,
        })
    }
}

/// Constant discriminator for sensor drop metrics.
pub const SENSOR_DROP_EVENT: &str = "sensor_drop";
const MAX_SENSOR_METRIC_REF_LEN: usize = 128;

/// Latest consent state for a sensor family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorConsentState {
    /// Latest journal row enables the sensor.
    Enabled,
    /// Latest journal row disables the sensor.
    Disabled,
    /// No sensor journal row exists.
    Missing,
}

/// Append one body-free sensor drop metric to `<vault>/.cairn/metrics.jsonl`.
///
/// # Errors
/// Returns I/O or serialization failures.
pub fn append_sensor_drop_metric(vault_root: &Path, row: &SensorDropMetric) -> anyhow::Result<()> {
    let cairn_dir = vault_root.join(".cairn");
    fs::create_dir_all(&cairn_dir)
        .with_context(|| format!("create metrics dir {}", cairn_dir.display()))?;
    let metrics_path = cairn_dir.join("metrics.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&metrics_path)
        .with_context(|| format!("open {}", metrics_path.display()))?;
    serde_json::to_writer(&mut file, row).context("serialize sensor drop metric")?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", metrics_path.display()))?;
    Ok(())
}

/// Read all valid `sensor_drop` rows from `<vault>/.cairn/metrics.jsonl`.
///
/// Malformed `sensor_drop` rows are returned as errors; malformed unrelated
/// metric rows are ignored to preserve existing best-effort metrics behavior.
///
/// # Errors
/// Returns I/O or malformed sensor metric failures.
pub fn read_sensor_drop_metrics(vault_root: &Path) -> anyhow::Result<Vec<SensorDropMetric>> {
    let metrics_path = vault_root.join(".cairn").join("metrics.jsonl");
    if !metrics_path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(&metrics_path)
        .with_context(|| format!("read {}", metrics_path.display()))?;
    let mut rows = Vec::new();
    for (line_no, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(raw) => raw,
            Err(error) if metrics_line_sets_event(line, SENSOR_DROP_EVENT) => {
                anyhow::bail!(
                    "parsing {} line {}: {error}",
                    metrics_path.display(),
                    line_no + 1
                );
            }
            Err(_) => continue,
        };
        if raw.get("event").and_then(serde_json::Value::as_str) != Some(SENSOR_DROP_EVENT) {
            continue;
        }
        let row: SensorDropMetric = serde_json::from_value(raw).with_context(|| {
            format!(
                "invalid sensor_drop row in {} line {}",
                metrics_path.display(),
                line_no + 1
            )
        })?;
        rows.push(row);
    }
    Ok(rows)
}

/// Keep optional metric references to machine-generated identifier shapes.
#[must_use]
pub fn safe_metric_ref(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| is_safe_metric_ref(value))
        .map(ToOwned::to_owned)
}

/// Resolve the latest consent-journal state for a local sensor family.
///
/// # Errors
/// Returns store access or journal decode failures.
pub async fn latest_sensor_consent(
    store: &SqliteMemoryStore,
    sensor: LocalSensorName,
) -> anyhow::Result<SensorConsentState> {
    let label = SensorLabel::parse(sensor.family_label().to_owned())
        .with_context(|| format!("parse sensor label {}", sensor.family_label()))?;
    let conn = store
        .raw_conn_for_admin()
        .cloned()
        .context("sqlite store has no connection")?;
    let events = conn
        .call(move |c| {
            cairn_store_sqlite::consent::query_by_sensor(c, &label)
                .map_err(|error| tokio_rusqlite::Error::Other(Box::new(error)))
        })
        .await
        .context("query sensor consent")?;
    let Some(event) = events.last() else {
        return Ok(SensorConsentState::Missing);
    };
    match event.kind {
        ConsentKind::SensorEnable => Ok(SensorConsentState::Enabled),
        ConsentKind::SensorDisable => Ok(SensorConsentState::Disabled),
        _ => Ok(SensorConsentState::Missing),
    }
}

/// Resolve latest consent for a local sensor in a vault-backed SQLite store.
///
/// Missing stores are treated as missing consent rather than creating a new
/// database from a read/gate path.
///
/// # Errors
/// Returns store access or journal decode failures when a store exists.
pub async fn latest_sensor_consent_for_vault(
    vault_root: &Path,
    sensor: LocalSensorName,
) -> anyhow::Result<SensorConsentState> {
    let db_path = vault_root.join(".cairn").join("cairn.db");
    if !db_path.exists() {
        return Ok(SensorConsentState::Missing);
    }
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .with_context(|| format!("open {}", db_path.display()))?;
    latest_sensor_consent(&store, sensor).await
}

/// Evaluate config, consent, and source-side budget for one sensor capture.
#[must_use]
pub fn evaluate_sensor_gate(
    config: &CairnConfig,
    consent: SensorConsentState,
    sensor: LocalSensorName,
    observation: BudgetObservation,
) -> Result<(), SensorGateReason> {
    if !sensor_enabled(config, sensor) {
        return Err(SensorGateReason::Disabled);
    }
    if consent != SensorConsentState::Enabled {
        return Err(SensorGateReason::PrivacyDenied);
    }
    if let Some(budget) = sensor_budget(config, sensor)
        && !budget_allows(budget, observation)
    {
        return Err(SensorGateReason::BudgetExceeded);
    }
    Ok(())
}

/// Return the configured budget for local sensors with shared runtime config.
#[must_use]
pub fn sensor_budget(
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> Option<&SensorCaptureBudget> {
    local_runtime_config(config, sensor).map(|cfg| &cfg.budget)
}

/// Return whether a local sensor is enabled in config.
#[must_use]
pub fn sensor_enabled(config: &CairnConfig, sensor: LocalSensorName) -> bool {
    match sensor {
        LocalSensorName::Screen => config.sensors.screen.enabled,
        _ => local_runtime_config(config, sensor).is_some_and(|cfg| cfg.enabled),
    }
}

fn local_runtime_config(
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> Option<&LocalSensorRuntimeConfig> {
    match sensor {
        LocalSensorName::Hook => Some(&config.sensors.hooks),
        LocalSensorName::Ide => Some(&config.sensors.ide),
        LocalSensorName::Terminal => Some(&config.sensors.terminal),
        LocalSensorName::Clipboard => Some(&config.sensors.clipboard),
        LocalSensorName::Voice => Some(&config.sensors.voice),
        LocalSensorName::Screen => None,
        LocalSensorName::Recording => Some(&config.sensors.recording),
    }
}

fn budget_allows(budget: &SensorCaptureBudget, observation: BudgetObservation) -> bool {
    budget
        .max_items
        .is_none_or(|limit| observation.items <= limit)
        && budget
            .max_bytes
            .is_none_or(|limit| observation.bytes <= limit)
}

fn metrics_line_sets_event(line: &str, event: &str) -> bool {
    let Some(after_key) = line.split_once("\"event\"").map(|(_, rest)| rest) else {
        return false;
    };
    let after_key = after_key.trim_start();
    let Some(after_colon) = after_key.strip_prefix(':') else {
        return false;
    };
    let after_colon = after_colon.trim_start();
    let Some(after_quote) = after_colon.strip_prefix('"') else {
        return false;
    };
    let Some(after_event) = after_quote.strip_prefix(event) else {
        return false;
    };
    after_event.is_empty() || after_event.starts_with('"')
}

fn is_safe_metric_ref(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_SENSOR_METRIC_REF_LEN {
        return false;
    }
    if !value.bytes().all(is_sensor_metric_ref_char) {
        return false;
    }
    is_crockford_ulid(value) || has_metric_ref_prefix(value)
}

fn has_metric_ref_prefix(value: &str) -> bool {
    ["sess-", "session-", "turn-", "tool-", "generic-"]
        .iter()
        .any(|prefix| {
            value
                .strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty())
        })
}

fn is_sensor_metric_ref_char(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-'
    )
}

fn is_crockford_ulid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 26
        && matches!(bytes[0], b'0'..=b'7')
        && bytes[1..].iter().copied().all(is_crockford_base32)
}

const fn is_crockford_base32(byte: u8) -> bool {
    matches!(
        byte,
        b'0'..=b'9'
            | b'A'..=b'H'
            | b'J'
            | b'K'
            | b'M'
            | b'N'
            | b'P'..=b'T'
            | b'V'..=b'Z'
    )
}

#[cfg(test)]
mod tests {
    use cairn_core::domain::{LocalSensorName, SensorGateReason, SourceFamily};

    use super::*;

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
        for banned in [
            "body",
            "text",
            "content",
            "raw",
            "snippet",
            "command",
            "url",
            "title",
            "file_path",
            "input",
        ] {
            assert!(
                !json.contains(banned),
                "metric leaked banned field {banned}: {json}"
            );
        }
        let decoded: SensorDropMetric = serde_json::from_str(&json).expect("decode metric");
        assert_eq!(decoded.reason, SensorGateReason::PrivacyDenied);
    }

    #[test]
    fn safe_metric_ref_keeps_only_machine_identifier_shapes() {
        assert_eq!(
            safe_metric_ref(Some("01ARZ3NDEKTSV4RRFFQ69G5FAV")).as_deref(),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert_eq!(
            safe_metric_ref(Some("turn-terminal-gate")).as_deref(),
            Some("turn-terminal-gate")
        );
        assert_eq!(safe_metric_ref(Some("SECRET USER TEXT")), None);
        assert_eq!(safe_metric_ref(Some("remember_this")), None);
        assert_eq!(safe_metric_ref(Some("")), None);
    }
}
