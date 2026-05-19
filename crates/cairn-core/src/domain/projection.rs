//! Derived projection domain types for Nexus sandbox indexes (§3.0, §19).
//!
//! These types describe rebuildable sidecar state. They never own record
//! authority; `.cairn/cairn.db` remains the source of truth.

use serde::{Deserialize, Serialize};

use crate::domain::record::RecordId;

/// Parser projection kinds produced by the Nexus sandbox sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ParserProjectionKind {
    /// Text extracted from a PDF source.
    PdfText,
    /// Text extracted from a DOCX source.
    DocxText,
    /// OCR or metadata text extracted from a video frame.
    VideoFrameText,
    /// Vision model caption for an image or video frame.
    VisionCaption,
}

impl ParserProjectionKind {
    /// Stable ledger suffix for this parser kind.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::PdfText => "pdf_text",
            Self::DocxText => "docx_text",
            Self::VideoFrameText => "video_frame_text",
            Self::VisionCaption => "vision_caption",
        }
    }
}

/// Rebuildable Nexus projection target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectionTarget {
    /// BM25S lexical index projection.
    Bm25sLexical,
    /// Parser-derived text projection.
    Parser(ParserProjectionKind),
}

impl ProjectionTarget {
    /// Stable key used in ledger rows and JSON diagnostics.
    #[must_use]
    pub fn as_key(&self) -> String {
        match self {
            Self::Bm25sLexical => "bm25s_lexical".to_owned(),
            Self::Parser(kind) => format!("parser_{}", kind.as_key()),
        }
    }
}

/// Authoritative cursor used to decide whether a projection item is current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionCursor {
    /// Authoritative record id.
    pub record_id: RecordId,
    /// Monotonic WAL sequence observed for the record.
    pub wal_sequence: u64,
    /// Hash of the authoritative record body/frontmatter used by the projection.
    pub record_hash: String,
    /// Optional source hash for parser projections.
    pub source_hash: Option<String>,
}

/// Projection item state for one target and one authoritative cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProjectionItemState {
    /// Sidecar projection is current for the cursor hashes.
    Current,
    /// Ledger row exists but does not match the current authoritative hash.
    Stale,
    /// Sidecar projection failed for this item.
    Failed {
        /// Failure reason safe for status/lint output.
        reason: String,
    },
    /// No projection row exists for this item.
    Missing,
}

/// Projection ledger row persisted by the authoritative store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionLedgerRow {
    /// Projection target.
    pub target: ProjectionTarget,
    /// Authoritative cursor this row was computed from.
    pub cursor: ProjectionCursor,
    /// Per-item projection state.
    pub state: ProjectionItemState,
    /// RFC3339 timestamp of the last projection attempt.
    pub updated_at: String,
}

impl ProjectionLedgerRow {
    /// Whether this row is current for an authoritative cursor.
    #[must_use]
    pub fn is_current_for(&self, cursor: &ProjectionCursor) -> bool {
        self.cursor.record_id == cursor.record_id
            && self.cursor.record_hash == cursor.record_hash
            && self.cursor.source_hash == cursor.source_hash
            && matches!(self.state, ProjectionItemState::Current)
    }
}

/// Aggregated lag summary for one projection target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionSummary {
    /// Projection target.
    pub target: ProjectionTarget,
    /// Count of authoritative records or sources in scope.
    pub total_authoritative_items: usize,
    /// Count of current projection rows.
    pub current_items: usize,
    /// Count of missing or stale projection rows.
    pub lagging_items: usize,
    /// Count of failed projection rows.
    pub failed_items: usize,
    /// Latest successful rebuild timestamp.
    pub last_successful_rebuild_at: Option<String>,
}

impl ProjectionSummary {
    /// Build a summary from per-item states.
    #[must_use]
    pub fn from_rows<I>(
        target: ProjectionTarget,
        total_authoritative_items: usize,
        states: I,
        last_successful_rebuild_at: Option<String>,
    ) -> Self
    where
        I: IntoIterator<Item = ProjectionItemState>,
    {
        let mut current_items = 0usize;
        let mut lagging_items = 0usize;
        let mut failed_items = 0usize;
        for state in states {
            match state {
                ProjectionItemState::Current => current_items += 1,
                ProjectionItemState::Stale | ProjectionItemState::Missing => lagging_items += 1,
                ProjectionItemState::Failed { .. } => {
                    lagging_items += 1;
                    failed_items += 1;
                }
            }
        }
        Self {
            target,
            total_authoritative_items,
            current_items,
            lagging_items,
            failed_items,
            last_successful_rebuild_at,
        }
    }
}
