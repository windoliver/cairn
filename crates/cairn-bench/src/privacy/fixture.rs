//! Privacy-fixture YAML schema + loader.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A single record that is ingested during fixture setup.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct FixtureRecord {
    /// Stable identifier used across operations and assertions.
    pub id: String,
    /// Record kind (e.g. `semantic`, `verbatim`).
    pub kind: String,
    /// Body text to ingest.
    pub body: String,
    /// Optional visibility scope.
    #[serde(default)]
    pub visibility: Option<String>,
}

/// A privacy-affecting operation applied after setup records are ingested.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum FixtureOp {
    /// Forget a record or a session.
    Forget {
        /// Target identifier (record id or session id).
        target: String,
        /// Forget mode (`record`, `session`, `source`).
        mode: String,
    },
    /// Redact a span within a record.
    Redact {
        /// Target record identifier.
        target: String,
        /// The span kind to redact (e.g. `email`, `phone`).
        span_kind: String,
    },
    /// Revoke a consent receipt.
    Revoke {
        /// Consent receipt identifier.
        receipt_id: String,
    },
}

/// Assertion against the `search` verb after operations complete.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SearchAssertion {
    /// Search mode (e.g. `keyword`, `semantic`).
    pub mode: String,
    /// Query string.
    pub query: String,
    /// The result set must not contain this record id.
    pub must_not_contain_id: String,
}

/// Assertion against the `retrieve` verb after operations complete.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct RetrieveAssertion {
    /// Record id to retrieve.
    pub id: String,
    /// Expected outcome, e.g. `not_found`, `tombstoned`.
    pub expect: String,
}

/// Assertion about the markdown vault state after operations complete.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum MarkdownAssertion {
    /// The path must not exist or must be a tombstone.
    NotPresent {
        /// Vault-relative path that should be absent or tombstoned.
        path_must_not_exist_or_be_tombstoned: String,
    },
    /// The file at `path` must not contain any unmasked span text.
    NoUnmaskedSpan {
        /// Vault-relative path to inspect.
        path: String,
        /// Text that must not appear verbatim in the file.
        must_not_contain: String,
    },
}

/// Assertion about the `SQLite` index state after operations complete.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct IndexAssertion {
    /// Table to query.
    pub table: String,
    /// Column to search within.
    pub column: String,
    /// Value that must not appear in that column.
    pub must_not_contain: String,
}

/// Combined assertions for a fixture scenario.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
pub struct Assertions {
    /// Search verb assertions.
    #[serde(default)]
    pub search: Vec<SearchAssertion>,
    /// Retrieve verb assertions.
    #[serde(default)]
    pub retrieve: Vec<RetrieveAssertion>,
    /// Markdown vault assertions.
    #[serde(default)]
    pub markdown: Vec<MarkdownAssertion>,
    /// `SQLite` index assertions.
    #[serde(default)]
    pub indexes: Vec<IndexAssertion>,
}

/// Setup block: records to ingest before running operations.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SetupBlock {
    /// Records to ingest.
    pub records: Vec<FixtureRecord>,
}

/// Top-level privacy fixture document.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct PrivacyFixture {
    /// Short machine-readable scenario name.
    pub scenario: String,
    /// Human-readable description (brief section reference encouraged).
    pub description: String,
    /// Records to ingest as precondition.
    pub setup: SetupBlock,
    /// Privacy operations to apply after setup.
    #[serde(default)]
    pub operations: Vec<FixtureOp>,
    /// Postcondition assertions.
    pub assertions: Assertions,
}

/// Load all `*.yaml` fixtures from `dir`, sorted by filename.
///
/// # Errors
/// Returns an error if the directory cannot be read or any file is malformed.
pub fn load_dir(dir: &Path) -> anyhow::Result<Vec<(PathBuf, PrivacyFixture)>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        let bytes = std::fs::read_to_string(&path)?;
        let fixture: PrivacyFixture = yaml_serde::from_str(&bytes)
            .map_err(|e| anyhow::anyhow!("yaml parse error in {}: {e}", path.display()))?;
        out.push((path, fixture));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
scenario: forget_record_visibility
description: §5.6 Phase A forgotten record invisible
setup:
  records:
    - id: rec_001
      kind: semantic
      body: "x"
      visibility: project
operations:
  - verb: forget
    target: rec_001
    mode: record
assertions:
  search:
    - mode: keyword
      query: "x"
      must_not_contain_id: rec_001
  retrieve:
    - id: rec_001
      expect: not_found
  markdown:
    - path_must_not_exist_or_be_tombstoned: "raw/rec_001.md"
  indexes:
    - table: record_fts
      column: record_id
      must_not_contain: rec_001
"#;

    #[test]
    fn parses_minimal_fixture() {
        let f: PrivacyFixture = yaml_serde::from_str(SAMPLE).unwrap();
        assert_eq!(f.scenario, "forget_record_visibility");
        assert_eq!(f.setup.records.len(), 1);
        assert!(matches!(&f.operations[0], FixtureOp::Forget { mode, .. } if mode == "record"));
        assert_eq!(f.assertions.search.len(), 1);
        assert_eq!(f.assertions.indexes[0].table, "record_fts");
    }

    #[test]
    fn load_dir_sorts_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.yaml"), SAMPLE).unwrap();
        std::fs::write(dir.path().join("a.yaml"), SAMPLE).unwrap();
        let loaded = load_dir(dir.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].0.ends_with("a.yaml"));
        assert!(loaded[1].0.ends_with("b.yaml"));
    }

    #[test]
    fn load_dir_handles_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");
        let loaded = load_dir(&missing).unwrap();
        assert!(loaded.is_empty());
    }
}
