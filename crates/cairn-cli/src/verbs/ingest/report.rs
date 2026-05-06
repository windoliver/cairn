//! Human and structured summaries for folder ingest runs.

use serde::Serialize;

/// Summary counters emitted after scanning and ingesting a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderIngestSummary {
    /// Files discovered during scanning.
    pub scanned: u64,
    /// Files skipped because cached content was unchanged.
    pub cached: u64,
    /// Files processed by the ingest pipeline.
    pub processed: u64,
    /// Files skipped by filters or unsupported handling.
    pub skipped: u64,
    /// Non-fatal warnings observed during the run.
    pub warnings: u64,
    /// New entities extracted from processed files.
    pub entities_new: u64,
    /// Extracted entities merged into existing records.
    pub entities_merged: u64,
    /// New edges extracted from processed files.
    pub edges_new: u64,
    /// Existing contradictions resolved during ingestion.
    pub contradictions_resolved: u64,
    /// Records written to the store.
    pub records_written: u64,
    /// Wall-clock runtime in milliseconds.
    pub elapsed_ms: u64,
    /// Whether the run avoided writing changes.
    pub dry_run: bool,
    /// Extraction mode used for the run.
    pub mode: String,
}

/// Render a stable human-readable folder ingest summary.
#[must_use]
pub fn render_human(folder: &str, summary: &FolderIngestSummary) -> String {
    let dry_run_suffix = if summary.dry_run { " (dry-run)" } else { "" };
    let elapsed_tenths = summary.elapsed_ms.saturating_add(50) / 100;
    let elapsed_seconds = elapsed_tenths / 10;
    let elapsed_tenth = elapsed_tenths % 10;

    format!(
        "Scanning {folder} ({} files)...\n  Cached  {} (no changes detected)\n  Processed {} files\n    Entities: {} new · {} merged\n    Edges:    {} new · {} contradictions resolved\n    Records:  {} written to store{}\nElapsed: {elapsed_seconds}.{elapsed_tenth}s\n",
        summary.scanned,
        summary.cached,
        summary.processed,
        summary.entities_new,
        summary.entities_merged,
        summary.edges_new,
        summary.contradictions_resolved,
        summary.records_written,
        dry_run_suffix,
    )
}

#[cfg(test)]
mod tests {
    use super::{FolderIngestSummary, render_human};

    #[test]
    fn human_output_has_stable_shape() {
        let summary = FolderIngestSummary {
            scanned: 3,
            cached: 1,
            processed: 2,
            skipped: 4,
            warnings: 0,
            entities_new: 5,
            entities_merged: 0,
            edges_new: 1,
            contradictions_resolved: 0,
            records_written: 0,
            elapsed_ms: 2300,
            dry_run: true,
            mode: "keyword".to_string(),
        };

        let output = render_human("./docs", &summary);

        assert!(output.contains("Scanning ./docs (3 files)..."));
        assert!(output.contains("  Cached  1 (no changes detected)"));
        assert!(output.contains("  Processed 2 files"));
        assert!(output.contains("    Entities: 5 new"));
        assert!(output.contains("    Edges:    1 new"));
        assert!(output.contains("    Records:  0 written to store (dry-run)"));
        assert!(output.contains("Elapsed: 2.3s"));
        assert!(output.ends_with("Elapsed: 2.3s\n"));
    }
}
