//! `MetricsSink` contract — emits `MetricEvent` lines to a sink.
//!
//! Implementations:
//! - `cairn-cli::metrics::JsonlMetricsSink` (writes
//!   `<vault>/.cairn/metrics.jsonl`)
//! - [`NoopMetricsSink`] for tests / verbs that don't observe metrics
//! - [`CapturingMetricsSink`] for tests that need to assert emissions

use crate::domain::metrics::MetricEvent;

/// Errors a metrics sink may surface. Callers MUST NOT abort the verb on
/// emit failures — they SHOULD log at `warn` and proceed (brief §15: a
/// missing line is preferable to a broken `assemble_hot`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetricsError {
    /// Filesystem / I/O failure.
    #[error("metrics sink io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialisation failure.
    #[error("metrics sink serialize: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Append-only, fail-soft metrics sink.
#[async_trait::async_trait]
pub trait MetricsSink: Send + Sync {
    /// Persist one event. Sink failures are surfaced as `MetricsError`
    /// but the caller decides whether to ignore or escalate.
    async fn emit(&self, event: MetricEvent) -> Result<(), MetricsError>;
}

/// In-memory sink for tests; records every emit.
#[derive(Debug, Default)]
pub struct CapturingMetricsSink {
    events: tokio::sync::Mutex<Vec<MetricEvent>>,
}

impl CapturingMetricsSink {
    /// Construct an empty capturing sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot every event emitted so far.
    pub async fn snapshot(&self) -> Vec<MetricEvent> {
        self.events.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl MetricsSink for CapturingMetricsSink {
    async fn emit(&self, event: MetricEvent) -> Result<(), MetricsError> {
        self.events.lock().await.push(event);
        Ok(())
    }
}

/// Discard sink — useful when the caller is verb-only.
#[derive(Debug, Default)]
pub struct NoopMetricsSink;

#[async_trait::async_trait]
impl MetricsSink for NoopMetricsSink {
    async fn emit(&self, _event: MetricEvent) -> Result<(), MetricsError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::hot_prefix::SourceWatermarks;

    fn sample_event() -> MetricEvent {
        MetricEvent::HotPrefixAssembled {
            ts_ms: 1,
            vault_id: "v".into(),
            agent_id: "a".into(),
            recipe_hash: "h".into(),
            latency_ms: 1,
            bytes: 1,
            budget_bytes: 10,
            budget_used_ratio: 0.1,
            cache_hit: false,
            watermarks: SourceWatermarks::default(),
        }
    }

    #[tokio::test]
    async fn capturing_sink_records_emits_in_order() {
        let sink = CapturingMetricsSink::new();
        sink.emit(sample_event()).await.expect("emit");
        sink.emit(sample_event()).await.expect("emit");
        let evs = sink.snapshot().await;
        assert_eq!(evs.len(), 2);
    }

    #[tokio::test]
    async fn noop_sink_returns_ok() {
        let sink = NoopMetricsSink;
        sink.emit(sample_event()).await.expect("noop emit");
    }
}
