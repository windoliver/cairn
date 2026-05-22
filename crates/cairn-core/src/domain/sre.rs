//! Body-free SRE report DTOs and pure classifiers.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Roll-up health state for SRE dashboard sections and gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SreStatus {
    /// The measured state is within its expected bound.
    Ok,
    /// The measured state needs attention but is not failing.
    Warning,
    /// The measured state breached a hard threshold.
    Fail,
    /// The state cannot be computed from the available inputs.
    Unknown,
}

/// Sanitized body-free detail class for SRE gate output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SreDetail(String);

impl SreDetail {
    /// Builds a detail class from raw text by applying SRE scrubbing.
    #[must_use]
    pub fn from_raw(raw: &str) -> Self {
        Self(scrub_detail(raw).to_owned())
    }

    /// Builds a detail from an already stable class if it is body-free.
    #[must_use]
    pub fn stable(class: impl AsRef<str>) -> Option<Self> {
        let class = class.as_ref();
        if class.is_empty()
            || !class
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
        {
            return None;
        }

        let lower = class.to_ascii_lowercase();
        if lower.contains("secret")
            || lower.contains("private")
            || lower.contains("body")
            || lower.contains("query")
            || lower.contains('/')
            || lower.contains('\\')
        {
            None
        } else {
            Some(Self(class.to_owned()))
        }
    }

    /// Returns the stable detail class.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SreDetail {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::stable(&raw).unwrap_or_else(|| Self::from_raw(&raw)))
    }
}

/// Finite numeric measurement serialized as a JSON number.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct SreMeasurement(f64);

impl SreMeasurement {
    /// Builds a measurement when the value is finite.
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then_some(Self(value))
    }

    /// Returns the numeric measurement.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for SreMeasurement {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for SreMeasurement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| {
            de::Error::invalid_value(de::Unexpected::Float(value), &"a finite SRE measurement")
        })
    }
}

/// Body-free vault identity for an SRE report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SreVaultSummary {
    /// Stable hash of the vault identifier.
    pub id_hash: String,
    /// Human-readable vault name.
    pub name: String,
}

/// Top-level body-free SRE report payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Capture timestamp in Unix epoch milliseconds.
    pub captured_at_ms: i64,
    /// Vault identity summary.
    pub vault: SreVaultSummary,
    /// Workflow queue and worker health summary.
    pub workflow: SreWorkflowSummary,
    /// Rehydration latency health summary.
    pub rehydration: SreRehydrationSummary,
    /// Projection target health summary.
    pub projection: SreProjectionSummary,
    /// Search mode health summary.
    pub search: SreSearchSummary,
    /// SLO gate summary.
    pub gates: SreGateSummary,
    /// Privacy scrub summary.
    pub privacy: SrePrivacySummary,
}

/// Workflow subsystem health summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SreWorkflowSummary {
    /// Roll-up workflow status.
    pub status: SreStatus,
    /// Age of the oldest queued workflow item in milliseconds.
    pub oldest_queued_age_ms: Option<i64>,
    /// Longest currently held lease age in milliseconds.
    pub longest_held_lease_ms: Option<i64>,
    /// Count of dead-lettered workflow items.
    pub dead_letter_count: usize,
    /// Per-workflow-kind summaries.
    pub kinds: Vec<SreWorkflowKindSummary>,
}

/// Health counters for one workflow kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SreWorkflowKindSummary {
    /// Workflow kind identifier.
    pub kind: String,
    /// Number of queued items.
    pub queued: u64,
    /// Number of leased items.
    pub leased: u64,
    /// Recently completed item count.
    pub done_recent: u64,
    /// Recently failed item count.
    pub failed_recent: u64,
    /// Age of the oldest queued item in milliseconds.
    pub oldest_queued_age_ms: Option<i64>,
    /// Age since the last successful item in milliseconds.
    pub last_success_age_ms: Option<i64>,
    /// Backlog warning threshold in milliseconds.
    pub backlog_threshold_ms: i64,
    /// Health status for this workflow kind.
    pub status: SreStatus,
}

/// Rehydration latency health summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreRehydrationSummary {
    /// Roll-up rehydration status.
    pub status: SreStatus,
    /// Latest observed latency in milliseconds.
    pub latest_latency_ms: Option<u64>,
    /// P95 latency in milliseconds.
    pub p95_latency_ms: Option<SreMeasurement>,
    /// Latency SLO in milliseconds.
    pub slo_ms: SreMeasurement,
    /// Number of samples used for latency statistics.
    pub sample_count: u64,
    /// Most recent rehydration gate result.
    pub last_gate: Option<SreGateResult>,
}

/// Projection subsystem health summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreProjectionSummary {
    /// Roll-up projection status.
    pub status: SreStatus,
    /// Current Nexus projection state.
    pub nexus_state: String,
    /// Optional stable reason for the Nexus state.
    pub nexus_reason: Option<String>,
    /// Per-target projection summaries.
    pub targets: Vec<SreProjectionTargetSummary>,
}

/// Health counters for one projection target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreProjectionTargetSummary {
    /// Projection target identifier.
    pub target: String,
    /// Current projected item count.
    pub current: u64,
    /// Stale projected item count.
    pub stale: u64,
    /// Failed projected item count.
    pub failed: u64,
    /// Missing projected item count.
    pub missing: u64,
    /// Maximum projection lag in milliseconds.
    pub max_lag_ms: Option<i64>,
    /// Latest rebuild latency in milliseconds.
    pub last_rebuild_latency_ms: Option<u64>,
    /// Health status for this projection target.
    pub status: SreStatus,
}

/// Search subsystem health summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreSearchSummary {
    /// Roll-up search status.
    pub status: SreStatus,
    /// Per-search-mode summaries.
    pub modes: Vec<SreSearchModeSummary>,
}

/// Health counters for one search mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreSearchModeSummary {
    /// Search mode identifier.
    pub mode: String,
    /// Whether the mode is advertised to callers.
    pub advertised: bool,
    /// Invocation count.
    pub invocations: u64,
    /// Degraded invocation count.
    pub degraded: u64,
    /// Failed invocation count.
    pub failed: u64,
    /// P95 latency in milliseconds.
    pub p95_latency_ms: Option<SreMeasurement>,
    /// Health status for this search mode.
    pub status: SreStatus,
}

/// Summary of SRE gates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreGateSummary {
    /// Roll-up gate status.
    pub status: SreStatus,
    /// Individual gate results.
    pub gates: Vec<SreGateResult>,
}

/// Result for one SRE gate evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreGateResult {
    /// Stable gate name.
    pub name: String,
    /// Gate status.
    pub status: SreStatus,
    /// Optional measured value.
    pub measured: Option<SreMeasurement>,
    /// Optional threshold value.
    pub threshold: Option<SreMeasurement>,
    /// Display unit for measured and threshold values.
    pub unit: String,
    /// Optional body-free detail class.
    pub detail: Option<SreDetail>,
}

/// Privacy summary for the SRE report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrePrivacySummary {
    /// Whether report details were scrubbed.
    pub scrubbed: bool,
    /// Number of forbidden fields detected in the report payload.
    pub forbidden_field_count: u64,
}

/// Classifies a count where zero is healthy and any positive count warns.
#[must_use]
pub fn classify_count_status(count: u64) -> SreStatus {
    if count == 0 {
        SreStatus::Ok
    } else {
        SreStatus::Warning
    }
}

/// Classifies a measured value against a threshold.
#[must_use]
pub fn classify_threshold(measured: Option<f64>, threshold: f64) -> SreStatus {
    if !threshold.is_finite() {
        return SreStatus::Unknown;
    }

    match measured {
        Some(value) if !value.is_finite() => SreStatus::Unknown,
        Some(value) if value <= threshold => SreStatus::Ok,
        Some(_) => SreStatus::Fail,
        None => SreStatus::Unknown,
    }
}

/// Maps raw detail text into a stable body-free detail class.
#[must_use]
pub fn scrub_detail(raw: &str) -> &'static str {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("secret")
        || lower.contains("private")
        || lower.contains('/')
        || lower.contains('\\')
    {
        "redacted"
    } else {
        "body_free"
    }
}
