//! `JsonlMetricsSink` — appends one JSON line per `MetricEvent` to
//! `<vault_root>/.cairn/metrics.jsonl`. Append-only, line-buffered.
//!
//! Failures are surfaced as `MetricsError` and the caller (the
//! `cached_assemble` verb) swallows them with a `tracing::warn!`. Brief
//! §15: a missing line is preferable to a broken `assemble_hot`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::contract::metrics::{MetricsError, MetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// JSONL metrics sink. Opens (or creates) the file in append mode.
pub struct JsonlMetricsSink {
    path: PathBuf,
    file: Arc<Mutex<tokio::fs::File>>,
}

impl JsonlMetricsSink {
    /// Open or create `<vault_root>/.cairn/metrics.jsonl`. Creates
    /// `<vault_root>/.cairn/` if absent.
    ///
    /// # Errors
    /// Returns [`MetricsError::Io`] if the directory cannot be created
    /// or the file cannot be opened in append mode.
    pub async fn open(vault_root: &Path) -> Result<Self, MetricsError> {
        let path = vault_root.join(".cairn").join("metrics.jsonl");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            file: Arc::new(Mutex::new(file)),
        })
    }

    /// Path of the JSONL file (informational; useful for tests).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait::async_trait]
impl MetricsSink for JsonlMetricsSink {
    async fn emit(&self, event: MetricEvent) -> Result<(), MetricsError> {
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        let mut guard = self.file.lock().await;
        guard.write_all(&line).await?;
        guard.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::hot_prefix::SourceWatermarks;

    fn sample() -> MetricEvent {
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
    async fn emit_writes_one_jsonl_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = JsonlMetricsSink::open(dir.path()).await.expect("open sink");
        sink.emit(sample()).await.expect("emit");
        let content = tokio::fs::read_to_string(sink.path()).await.expect("read");
        assert_eq!(content.lines().count(), 1);
        assert!(content.contains("\"event\":\"hot_prefix_assembled\""));
    }

    #[tokio::test]
    async fn two_emits_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sink = JsonlMetricsSink::open(dir.path()).await.expect("open sink");
        for _ in 0..2 {
            sink.emit(sample()).await.expect("emit");
        }
        let content = tokio::fs::read_to_string(sink.path()).await.expect("read");
        assert_eq!(content.lines().count(), 2);
    }
}
