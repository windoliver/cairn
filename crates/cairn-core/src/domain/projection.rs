//! Markdown projection — pure render/parse/conflict functions (brief §3, §13.5.c).
//!
//! `MarkdownProjector` is a zero-field unit struct. All methods are pure:
//! no I/O, no async, no `MemoryStore` dependency.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::contract::memory_store::StoredRecord;
use crate::domain::{MemoryClass, MemoryKind, MemoryVisibility, ScopeTuple};

/// A markdown file ready to write: vault-relative path + full content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedFile {
    /// Vault-relative path, e.g. `raw/feedback_01J….md`.
    pub path: PathBuf,
    /// Full file content: YAML frontmatter block + blank line + markdown body.
    pub content: String,
}

/// Parsed content of a projected markdown file — the resync direction.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProjection {
    /// Stable record identity (`MemoryRecord.id`).
    pub target_id: String,
    /// Version of the store snapshot this file was projected from.
    pub version: u32,
    pub kind: MemoryKind,
    pub class: MemoryClass,
    pub visibility: MemoryVisibility,
    /// Markdown body (everything after the closing `---`).
    pub body: String,
    pub tags: Vec<String>,
    /// All frontmatter key/value pairs, including those not in the fixed set.
    pub raw_frontmatter: BTreeMap<String, serde_yaml::Value>,
}

/// Result of the optimistic-concurrency conflict check.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConflictOutcome {
    Clean,
    Conflict {
        /// Human-readable description for the quarantine file.
        marker: String,
        file_version: u32,
        store_version: u32,
    },
}

/// Errors from parsing or conflict detection in the resync path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResyncError {
    #[error("failed to parse frontmatter: {0}")]
    ParseFailed(String),
    #[error("frontmatter missing required field `id`")]
    MissingId,
    #[error("version conflict (file={file_version}, store={store_version}): {reason}")]
    Conflict {
        file_version: u32,
        store_version: u32,
        reason: String,
    },
}

/// Pure projection functions — render, parse, and conflict-check.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownProjector;

// Internal serde helper for project().
#[derive(Serialize)]
struct FrontmatterDoc<'a> {
    id: &'a str,
    version: u32,
    kind: &'a str,
    class: &'a str,
    visibility: &'a str,
    scope: &'a ScopeTuple,
    confidence: f32,
    salience: f32,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tags: &'a [String],
    created: &'a str,
    updated: &'a str,
}

impl MarkdownProjector {
    /// Render a `StoredRecord` to a markdown file.
    pub fn project(&self, _stored: &StoredRecord) -> ProjectedFile {
        todo!("Task 3")
    }

    /// Parse a projected markdown file's content.
    pub fn parse(&self, _content: &str) -> Result<ParsedProjection, ResyncError> {
        todo!("Task 4")
    }

    /// Optimistic-concurrency conflict check.
    ///
    /// `current` is `None` when the record does not yet exist in the store
    /// (always `Clean`). When `current` is `Some`, checks version equality
    /// and immutable field mutations.
    pub fn check_conflict(
        &self,
        _parsed: &ParsedProjection,
        _current: Option<&StoredRecord>,
    ) -> ConflictOutcome {
        todo!("Task 5")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_exist() {
        let _: ResyncError = ResyncError::MissingId;
        let _: ConflictOutcome = ConflictOutcome::Clean;
    }
}
