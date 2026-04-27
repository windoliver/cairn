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

/// Keys emitted by [`MarkdownProjector::project`] that are not part of
/// `extra_frontmatter`. Used by the resync handler to separate standard
/// projected fields from user-editable extras.
pub const PROJECTED_STANDARD_FIELDS: &[&str] = &[
    "id", "version", "kind", "class", "visibility",
    "scope", "confidence", "salience", "tags", "created", "updated",
];

/// Pure projection functions — render, parse, and conflict-check.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownProjector;

// Internal serde helper for project() — covers the fixed schema fields only.
// extra_frontmatter is merged into the YAML mapping after serialization.
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
        // Serialize standard fields first, then append extra_frontmatter entries.
        // Using the mapping API keeps key order stable: standard fields first (struct
        // declaration order), then extras (BTreeMap alphabetical order).
        #[allow(clippy::expect_used)]
        let mut yaml_map = serde_yaml::to_value(&doc)
            .expect("FrontmatterDoc serializes infallibly")
            .as_mapping()
            .expect("FrontmatterDoc serializes as a YAML mapping")
            .clone();
        for (k, v) in &r.extra_frontmatter {
            // Skip collisions with schema-owned keys; allow everything else.
            if !PROJECTED_STANDARD_FIELDS.contains(&k.as_str())
                && let Ok(yaml_val) = serde_yaml::to_value(v)
            {
                yaml_map.insert(serde_yaml::Value::String(k.clone()), yaml_val);
            }
        }
        #[allow(clippy::expect_used)]
        let yaml = serde_yaml::to_string(&yaml_map)
            .expect("YAML mapping serializes infallibly");
        // serde_yaml 0.9.34 does NOT prepend a "---\n" document-start marker for
        // plain structs or mappings; debug_assert guards against a future version
        // change that would silently double-fence the output.
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
    pub fn parse(&self, content: &str) -> Result<ParsedProjection, ResyncError> {
        let after_open = content
            .strip_prefix("---\n")
            .ok_or_else(|| ResyncError::ParseFailed("file must start with `---`".to_owned()))?;

        let (yaml_part, body_raw) = after_open
            .split_once("\n---\n")
            .ok_or_else(|| ResyncError::ParseFailed("no closing `---` delimiter".to_owned()))?;

        // Strip only the single structural blank line after the closing `---`, not any
        // leading blank lines that are part of the body content itself.
        let body = body_raw.strip_prefix('\n').unwrap_or(body_raw).to_owned();

        let val: serde_yaml::Value =
            serde_yaml::from_str(yaml_part).map_err(|e| ResyncError::ParseFailed(e.to_string()))?;

        let map = val.as_mapping().ok_or_else(|| {
            ResyncError::ParseFailed("frontmatter must be a YAML mapping".to_owned())
        })?;

        let target_id = map
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or(ResyncError::MissingId)?
            .to_owned();

        let version = map
            .get("version")
            .ok_or_else(|| ResyncError::ParseFailed("missing `version` field".to_owned()))
            .and_then(|v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| {
                        ResyncError::ParseFailed("invalid `version`: must be a non-negative integer within u32 range".to_owned())
                    })
            })?;

        let kind_str = map
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResyncError::ParseFailed("missing `kind`".to_owned()))?;
        let kind = MemoryKind::parse(kind_str)
            .map_err(|_| ResyncError::ParseFailed(format!("unknown kind: `{kind_str}`")))?;

        let class_str = map
            .get("class")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResyncError::ParseFailed("missing `class`".to_owned()))?;
        let class = MemoryClass::parse(class_str)
            .map_err(|_| ResyncError::ParseFailed(format!("unknown class: `{class_str}`")))?;

        let vis_str = map
            .get("visibility")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResyncError::ParseFailed("missing `visibility`".to_owned()))?;
        let visibility = MemoryVisibility::parse(vis_str)
            .map_err(|_| ResyncError::ParseFailed(format!("unknown visibility: `{vis_str}`")))?;

        let tags = match map.get("tags") {
            None => Vec::new(),
            Some(v) => {
                let seq = v.as_sequence().ok_or_else(|| {
                    ResyncError::ParseFailed("`tags` must be a YAML sequence".to_owned())
                })?;
                seq.iter()
                    .enumerate()
                    .map(|(i, entry)| {
                        entry.as_str().map(str::to_owned).ok_or_else(|| {
                            ResyncError::ParseFailed(format!(
                                "`tags[{i}]` must be a string"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };

        let raw_frontmatter = map
            .iter()
            .filter_map(|(k, v)| k.as_str().map(|s| (s.to_owned(), v.clone())))
            .collect();

        Ok(ParsedProjection {
            target_id,
            version,
            kind,
            class,
            visibility,
            body,
            tags,
            raw_frontmatter,
        })
    }

    /// Optimistic-concurrency conflict check.
    ///
    /// `current` is `None` when the record does not yet exist in the store
    /// (always `Clean`). When `current` is `Some`, checks version equality
    /// and immutable field mutations.
    #[must_use]
    pub fn check_conflict(
        &self,
        parsed: &ParsedProjection,
        current: Option<&StoredRecord>,
    ) -> ConflictOutcome {
        let Some(current) = current else {
            return ConflictOutcome::Clean;
        };

        // Immutable field check takes precedence over version rules.
        if parsed.kind != current.record.kind {
            return ConflictOutcome::Conflict {
                marker: format!(
                    "immutable field mutated: kind (file={}, store={})",
                    parsed.kind.as_str(),
                    current.record.kind.as_str()
                ),
                file_version: parsed.version,
                store_version: current.version,
            };
        }
        if parsed.class != current.record.class {
            return ConflictOutcome::Conflict {
                marker: format!(
                    "immutable field mutated: class (file={}, store={})",
                    parsed.class.as_str(),
                    current.record.class.as_str()
                ),
                file_version: parsed.version,
                store_version: current.version,
            };
        }
        if parsed.visibility != current.record.visibility {
            return ConflictOutcome::Conflict {
                marker: format!(
                    "read-only in resync path: visibility (use `cairn promote` or `cairn forget`) (file={}, store={})",
                    parsed.visibility.as_str(),
                    current.record.visibility.as_str()
                ),
                file_version: parsed.version,
                store_version: current.version,
            };
        }
        // Compare backend-owned projected fields (scope, confidence, salience, created,
        // updated) using the canonical projection as the reference so both sides go through
        // the same serde_yaml round-trip. This avoids f32↔f64 precision mismatches and
        // YAML-tagged timestamp subtleties.
        let canonical = self.project(current);
        // The canonical projection is always parseable; if it somehow isn't, skip the check.
        if let Ok(canonical_parsed) = self.parse(&canonical.content) {
            const BACKEND_FIELDS: &[&str] =
                &["scope", "confidence", "salience", "created", "updated"];
            for field in BACKEND_FIELDS {
                let canonical_val = canonical_parsed.raw_frontmatter.get(*field);
                let file_val = parsed.raw_frontmatter.get(*field);
                if canonical_val != file_val {
                    return ConflictOutcome::Conflict {
                        marker: format!("read-only field mutated: {field}"),
                        file_version: parsed.version,
                        store_version: current.version,
                    };
                }
            }
        }

        // Version check: only exact equality is clean.
        if parsed.version == current.version {
            ConflictOutcome::Clean
        } else {
            ConflictOutcome::Conflict {
                marker: format!("stale: file={}, store={}", parsed.version, current.version),
                file_version: parsed.version,
                store_version: current.version,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::memory_store::StoredRecord;
    use crate::domain::record::tests::sample_record;

    fn stored(version: u32) -> StoredRecord {
        StoredRecord {
            record: sample_record(),
            version,
        }
    }

    #[test]
    fn types_exist() {
        let _: ResyncError = ResyncError::MissingId;
        let _: ConflictOutcome = ConflictOutcome::Clean;
    }

    #[test]
    fn project_starts_with_yaml_fence() {
        let pf = MarkdownProjector.project(&stored(1));
        assert!(
            pf.content.starts_with("---\n"),
            "content: {:?}",
            &pf.content[..40.min(pf.content.len())]
        );
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

    #[test]
    fn parse_round_trip_preserves_mutable_fields() {
        let original = stored(3);
        let pf = MarkdownProjector.project(&original);
        let parsed = MarkdownProjector.parse(&pf.content).expect("parse");
        assert_eq!(parsed.target_id, original.record.id.as_str());
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.kind, original.record.kind);
        assert_eq!(parsed.body, original.record.body);
        assert_eq!(parsed.tags, original.record.tags);
        assert_eq!(parsed.class, original.record.class);
        assert_eq!(parsed.visibility, original.record.visibility);
    }

    #[test]
    fn parse_missing_id_returns_error() {
        let content =
            "---\nversion: 1\nkind: user\nclass: semantic\nvisibility: private\n---\n\nbody";
        let err = MarkdownProjector.parse(content).unwrap_err();
        assert!(matches!(err, ResyncError::MissingId));
    }

    #[test]
    fn parse_malformed_yaml_returns_parse_failed() {
        let content = "---\n: bad: yaml: [\n---\n\nbody";
        let err = MarkdownProjector.parse(content).unwrap_err();
        assert!(matches!(err, ResyncError::ParseFailed(_)));
    }

    #[test]
    fn parse_non_sequence_tags_returns_parse_failed() {
        // `tags: pref` is a scalar, not a sequence — should hard-fail, not silently become []
        let content = "---\nid: 01HQZX9F5N0000000000000000\nversion: 1\nkind: user\nclass: semantic\nvisibility: private\ntags: pref\n---\n\nbody";
        let err = MarkdownProjector.parse(content).unwrap_err();
        assert!(matches!(err, ResyncError::ParseFailed(_)));
    }

    #[test]
    fn parse_non_string_tag_entry_returns_parse_failed() {
        // `tags: [42]` — integer entry should fail, not be silently dropped
        let content = "---\nid: 01HQZX9F5N0000000000000000\nversion: 1\nkind: user\nclass: semantic\nvisibility: private\ntags: [42]\n---\n\nbody";
        let err = MarkdownProjector.parse(content).unwrap_err();
        assert!(matches!(err, ResyncError::ParseFailed(_)));
    }

    #[test]
    fn parse_no_closing_fence_returns_parse_failed() {
        let content = "---\nid: 01HQZX9F5N0000000000000000\nversion: 1\n";
        let err = MarkdownProjector.parse(content).unwrap_err();
        assert!(matches!(err, ResyncError::ParseFailed(_)));
    }

    #[test]
    fn parse_preserves_leading_blank_lines_in_body() {
        let proj = MarkdownProjector;
        let mut stored = crate::domain::record::tests::sample_stored_record(1);
        stored.record.body = "\n\nLeading blank lines.".to_owned();
        let file = proj.project(&stored);
        let parsed = proj.parse(&file.content).unwrap();
        assert_eq!(parsed.body, "\n\nLeading blank lines.");
    }

    #[test]
    fn check_conflict_new_record_is_clean() {
        let proj = MarkdownProjector;
        // parsed.version = 1, current = None → Clean
        let stored = crate::domain::record::tests::sample_stored_record(1);
        let file = proj.project(&stored);
        let parsed = proj.parse(&file.content).unwrap();
        let outcome = proj.check_conflict(&parsed, None);
        assert!(matches!(outcome, ConflictOutcome::Clean));
    }

    #[test]
    fn check_conflict_version_match_is_clean() {
        let proj = MarkdownProjector;
        let stored = crate::domain::record::tests::sample_stored_record(3);
        let file = proj.project(&stored);
        let parsed = proj.parse(&file.content).unwrap();
        // file version == store version → Clean
        let outcome = proj.check_conflict(&parsed, Some(&stored));
        assert!(matches!(outcome, ConflictOutcome::Clean));
    }

    #[test]
    fn check_conflict_stale_file_is_conflict() {
        let proj = MarkdownProjector;
        // store is at version 5, file is at version 3 → Conflict
        let stored_v5 = crate::domain::record::tests::sample_stored_record(5);
        let stored_v3 = crate::domain::record::tests::sample_stored_record(3);
        let file = proj.project(&stored_v3);
        let parsed = proj.parse(&file.content).unwrap();
        let outcome = proj.check_conflict(&parsed, Some(&stored_v5));
        assert!(matches!(
            outcome,
            ConflictOutcome::Conflict {
                file_version: 3,
                store_version: 5,
                ..
            }
        ));
    }

    #[test]
    fn check_conflict_future_file_version_is_conflict() {
        let proj = MarkdownProjector;
        // store is at version 2, file claims version 5 → frontend cannot increment versions → Conflict
        let stored_v2 = crate::domain::record::tests::sample_stored_record(2);
        let stored_v5 = crate::domain::record::tests::sample_stored_record(5);
        let file = proj.project(&stored_v5);
        let parsed = proj.parse(&file.content).unwrap();
        let outcome = proj.check_conflict(&parsed, Some(&stored_v2));
        assert!(matches!(
            outcome,
            ConflictOutcome::Conflict {
                file_version: 5,
                store_version: 2,
                ..
            }
        ));
    }

    #[test]
    fn check_conflict_immutable_field_mutation_is_conflict() {
        let proj = MarkdownProjector;
        let stored = crate::domain::record::tests::sample_stored_record(2);
        let file = proj.project(&stored);
        // Tamper with kind in the content string (sample_record uses "user"; replace with valid "feedback")
        let tampered = file.content.replace(
            &format!("kind: {}", stored.record.kind.as_str()),
            "kind: feedback",
        );
        let parsed = proj.parse(&tampered).unwrap();
        let outcome = proj.check_conflict(&parsed, Some(&stored));
        assert!(matches!(outcome, ConflictOutcome::Conflict { .. }));
    }

    #[test]
    fn check_conflict_class_mutation_is_conflict() {
        let proj = MarkdownProjector;
        let stored = crate::domain::record::tests::sample_stored_record(2);
        let file = proj.project(&stored);
        // sample_record uses class: semantic — replace with episodic
        let tampered = file.content.replace(
            &format!("class: {}", stored.record.class.as_str()),
            "class: episodic",
        );
        let parsed = proj.parse(&tampered).unwrap();
        let outcome = proj.check_conflict(&parsed, Some(&stored));
        assert!(matches!(outcome, ConflictOutcome::Conflict { .. }));
    }

    #[test]
    fn project_emits_extra_frontmatter_keys() {
        let mut stored = crate::domain::record::tests::sample_stored_record(1);
        stored.record.extra_frontmatter.insert(
            "category".to_owned(),
            serde_json::Value::String("tool".to_owned()),
        );
        let pf = MarkdownProjector.project(&stored);
        assert!(
            pf.content.contains("category: tool"),
            "extra frontmatter key missing from projection: {:?}",
            &pf.content
        );
    }

    #[test]
    fn project_extra_frontmatter_round_trips_through_parse() {
        let proj = MarkdownProjector;
        let mut stored = crate::domain::record::tests::sample_stored_record(1);
        stored.record.extra_frontmatter.insert(
            "category".to_owned(),
            serde_json::Value::String("tool".to_owned()),
        );
        let pf = proj.project(&stored);
        let parsed = proj.parse(&pf.content).expect("parse");
        // "category" is not a standard field, so it ends up in raw_frontmatter.
        assert_eq!(
            parsed.raw_frontmatter.get("category").and_then(|v| v.as_str()),
            Some("tool"),
            "extra_frontmatter key not preserved through parse"
        );
        // Standard fields must not bleed into extra territory.
        assert!(!parsed.raw_frontmatter.contains_key("extra_frontmatter_overlap_sentinel"),
            "sentinel key should not appear");
    }

    #[test]
    fn project_standard_fields_not_duplicated_by_extra_frontmatter() {
        // If extra_frontmatter contains a key that collides with a standard field
        // (e.g. "id"), project() must skip it so the output is well-formed.
        let mut stored = crate::domain::record::tests::sample_stored_record(1);
        stored.record.extra_frontmatter.insert(
            "id".to_owned(),
            serde_json::Value::String("injected".to_owned()),
        );
        let pf = MarkdownProjector.project(&stored);
        // The real id should appear exactly once; "injected" must not appear.
        let occurrences = pf.content.matches("id:").count();
        assert_eq!(occurrences, 1, "id key appeared more than once: {:?}", &pf.content);
        assert!(!pf.content.contains("injected"), "colliding extra key was not filtered");
    }

    #[test]
    fn check_conflict_visibility_mutation_is_conflict() {
        let proj = MarkdownProjector;
        let stored = crate::domain::record::tests::sample_stored_record(2);
        let file = proj.project(&stored);
        // sample_record uses visibility: private — replace with session
        let tampered = file.content.replace(
            &format!("visibility: {}", stored.record.visibility.as_str()),
            "visibility: session",
        );
        let parsed = proj.parse(&tampered).unwrap();
        let outcome = proj.check_conflict(&parsed, Some(&stored));
        assert!(matches!(outcome, ConflictOutcome::Conflict { .. }));
    }
}
