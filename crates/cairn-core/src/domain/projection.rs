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
    /// Memory kind — immutable after first write.
    pub kind: MemoryKind,
    /// Memory class — immutable after first write.
    pub class: MemoryClass,
    /// Visibility tier — immutable except via `promote`/`forget`.
    pub visibility: MemoryVisibility,
    /// Markdown body (everything after the closing `---`).
    pub body: String,
    /// Free-form tags — mutable in resync.
    pub tags: Vec<String>,
    /// All frontmatter key/value pairs, including those not in the fixed set.
    pub raw_frontmatter: BTreeMap<String, serde_yaml::Value>,
}

/// Result of the optimistic-concurrency conflict check.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConflictOutcome {
    /// Parsed edit has no version conflict and all immutable fields are unchanged.
    Clean,
    /// Parsed edit conflicts with the current store state.
    Conflict {
        /// Human-readable description written to `.cairn/quarantine/`.
        marker: String,
        /// Version the file claims to have been based on.
        file_version: u32,
        /// Version currently held in the store.
        store_version: u32,
    },
}

/// Errors from parsing or conflict detection in the resync path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResyncError {
    /// Frontmatter YAML could not be parsed.
    #[error("failed to parse frontmatter: {0}")]
    ParseFailed(String),
    /// Frontmatter is missing the required `id` field.
    #[error("frontmatter missing required field `id`")]
    MissingId,
    /// Optimistic-concurrency or immutable-field conflict detected.
    #[error("version conflict (file={file_version}, store={store_version}): {reason}")]
    Conflict {
        /// Version the file claims.
        file_version: u32,
        /// Version currently in the store.
        store_version: u32,
        /// Human-readable description of the conflict reason.
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
    #[must_use]
    pub fn project(&self, stored: &StoredRecord) -> ProjectedFile {
        let r = &stored.record;
        let doc = FrontmatterDoc {
            id: r.id.as_str(),
            version: stored.version,
            kind: r.kind.as_str(),
            class: r.class.as_str(),
            visibility: r.visibility.as_str(),
            scope: &r.scope,
            confidence: r.confidence,
            salience: r.salience,
            tags: &r.tags,
            created: r.provenance.created_at.as_str(),
            updated: r.updated_at.as_str(),
        };
        // pure struct, no Rc or custom Serialize — infallible
        #[allow(clippy::expect_used)]
        let yaml = serde_yaml::to_string(&doc).expect("FrontmatterDoc serializes infallibly");
        // serde_yaml 0.9.34 does NOT prepend a "---\n" document-start marker for
        // plain structs; debug_assert guards this so a version upgrade that adds
        // the marker back fails fast rather than silently double-fencing.
        debug_assert!(
            !yaml.starts_with("---\n"),
            "serde_yaml now prepends document-start marker; strip_prefix logic needs revisiting: {:?}",
            &yaml[..yaml.len().min(60)]
        );
        let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
        let content = format!("---\n{yaml}---\n\n{}", r.body);
        let path = PathBuf::from(format!("raw/{}_{}.md", r.kind.as_str(), r.id.as_str()));
        ProjectedFile { path, content }
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
    #[must_use]
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
    use crate::contract::memory_store::StoredRecord;
    use crate::domain::record::tests::sample_record;

    fn stored(version: u32) -> StoredRecord {
        StoredRecord { record: sample_record(), version }
    }

    #[test]
    fn types_exist() {
        let _: ResyncError = ResyncError::MissingId;
        let _: ConflictOutcome = ConflictOutcome::Clean;
    }

    #[test]
    fn project_starts_with_yaml_fence() {
        let pf = MarkdownProjector.project(&stored(1));
        assert!(pf.content.starts_with("---\n"), "content: {:?}", &pf.content[..40.min(pf.content.len())]);
    }

    #[test]
    fn project_contains_id() {
        let stored = stored(1);
        let pf = MarkdownProjector.project(&stored);
        assert!(pf.content.contains(stored.record.id.as_str()));
    }

    #[test]
    fn project_contains_version() {
        let pf = MarkdownProjector.project(&stored(7));
        assert!(pf.content.contains("version: 7"));
    }

    #[test]
    fn project_body_follows_closing_fence() {
        let stored = stored(1);
        let pf = MarkdownProjector.project(&stored);
        let parts: Vec<&str> = pf.content.splitn(3, "---\n").collect();
        // parts[0] = "", parts[1] = yaml, parts[2] = "\nbody..."
        assert_eq!(parts.len(), 3, "expected three ---\\n-delimited sections");
        let body_section = parts[2].trim_start_matches('\n');
        assert_eq!(body_section, stored.record.body);
    }

    #[test]
    fn project_path_contains_kind_and_id() {
        let stored = stored(1);
        let pf = MarkdownProjector.project(&stored);
        let path_str = pf.path.to_string_lossy();
        assert!(path_str.contains(stored.record.kind.as_str()));
        assert!(path_str.contains(stored.record.id.as_str()));
    }
}
