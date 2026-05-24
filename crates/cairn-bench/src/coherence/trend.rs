//! Append-only JSONL trend file with per-line schema migration.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::score::CategoryScore;

/// One trend entry — one line in `coherence-trend.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendEntry {
    /// Schema version stamped on the line. Currently `1`.
    pub schema_version: u32,
    /// Stable run identifier.
    pub run_id: String,
    /// RFC-3339 timestamp of the run.
    pub ts: String,
    /// Cairn version that produced the entry.
    pub cairn_version: String,
    /// Git SHA of the commit at run time.
    pub git_sha: String,
    /// Gate mode string (`"beta"`, `"rc"`, or `"none"`).
    pub gate: String,
    /// Overall outcome (`"pass"` or `"fail"`).
    pub outcome: String,
    /// Wire-string names of categories that failed (empty on pass).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    /// Per-category scores, keyed by wire-string name.
    pub metrics: BTreeMap<String, CategoryScore>,
}

/// Trend load/append errors.
#[derive(Debug, thiserror::Error)]
pub enum TrendError {
    /// Filesystem error during read or write.
    #[error("read {path}: {source}")]
    Io {
        /// Path that failed.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// JSON parse failed on a specific line.
    #[error("parse line {line} of {path}: {source}")]
    Json {
        /// Path that failed.
        path: String,
        /// 1-based line number.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Line had no `schema_version` field.
    #[error("missing schema_version on line {line} of {path}")]
    MissingSchemaVersion {
        /// Path that failed.
        path: String,
        /// 1-based line number.
        line: usize,
    },
    /// Line declared a `schema_version` this build does not understand.
    #[error("unknown schema_version {version} on line {line} of {path}")]
    UnknownSchemaVersion {
        /// Path that failed.
        path: String,
        /// 1-based line number.
        line: usize,
        /// The unknown version.
        version: u64,
    },
}

/// Load every trend entry from disk, dispatching per line on `schema_version`.
///
/// # Errors
/// - `Io` for filesystem failures (except `NotFound`, which returns an empty vec).
/// - `Json` for malformed JSON.
/// - `MissingSchemaVersion` / `UnknownSchemaVersion` if a line cannot be classified.
pub fn load(path: &Path) -> Result<Vec<TrendEntry>, TrendError> {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(TrendError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let raw = line.map_err(|source| TrendError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if raw.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&raw).map_err(|source| TrendError::Json {
            path: path.display().to_string(),
            line: idx + 1,
            source,
        })?;
        let version =
            value
                .get("schema_version")
                .and_then(Value::as_u64)
                .ok_or_else(|| TrendError::MissingSchemaVersion {
                    path: path.display().to_string(),
                    line: idx + 1,
                })?;
        let entry = match version {
            1 => from_v1(value).map_err(|source| TrendError::Json {
                path: path.display().to_string(),
                line: idx + 1,
                source,
            })?,
            other => {
                return Err(TrendError::UnknownSchemaVersion {
                    path: path.display().to_string(),
                    line: idx + 1,
                    version: other,
                });
            }
        };
        out.push(entry);
    }
    Ok(out)
}

fn from_v1(value: Value) -> Result<TrendEntry, serde_json::Error> {
    serde_json::from_value(value)
}

/// Append a single trend entry to disk. The write is a single `write_all`
/// call on a file opened with `append`, so concurrent appends from sibling
/// processes interleave at line boundaries (POSIX guarantees up to
/// `PIPE_BUF`; the rendered lines are well under it).
///
/// # Errors
/// `TrendError::Io` on filesystem failure or write error.
/// `TrendError::Json` if the entry cannot be serialised.
pub fn append(path: &Path, entry: &TrendEntry) -> Result<(), TrendError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| TrendError::Io {
            path: path.display().to_string(),
            source,
        })?;
    let mut line = serde_json::to_string(entry).map_err(|source| TrendError::Json {
        path: path.display().to_string(),
        line: 0,
        source,
    })?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .map_err(|source| TrendError::Io {
            path: path.display().to_string(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn fixture_entry(version: u32, run_id: &str) -> TrendEntry {
        let mut metrics = BTreeMap::new();
        metrics.insert(
            "recall_precision".to_owned(),
            CategoryScore {
                passed: 5,
                total: 5,
                score: 1.0,
            },
        );
        TrendEntry {
            schema_version: version,
            run_id: run_id.to_owned(),
            ts: "2026-05-24T12:00:00Z".to_owned(),
            cairn_version: "0.0.0".to_owned(),
            git_sha: "deadbeef".to_owned(),
            gate: "beta".to_owned(),
            outcome: "pass".to_owned(),
            failures: vec![],
            metrics,
        }
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        assert!(load(&path).unwrap().is_empty());
    }

    #[test]
    fn append_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trend.jsonl");
        let entry = fixture_entry(1, "run-a");
        append(&path, &entry).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, vec![entry]);
    }

    #[test]
    fn load_missing_schema_version_fails_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trend.jsonl");
        std::fs::write(&path, b"{\"run_id\":\"x\"}\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, TrendError::MissingSchemaVersion { .. }));
    }

    #[test]
    fn load_unknown_schema_version_fails_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trend.jsonl");
        std::fs::write(&path, b"{\"schema_version\":99}\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(
            err,
            TrendError::UnknownSchemaVersion { version: 99, .. }
        ));
    }

    proptest! {
        #[test]
        fn append_atomic_under_concurrent_writes(
            ids in proptest::collection::vec("[a-z0-9]{8}", 4..8)
        ) {
            let dir = tempdir().unwrap();
            let path = Arc::new(dir.path().join("trend.jsonl"));
            let mut handles = Vec::new();
            for id in &ids {
                let p = Arc::clone(&path);
                let entry = fixture_entry(1, id);
                handles.push(std::thread::spawn(move || append(&p, &entry).unwrap()));
            }
            for h in handles {
                h.join().unwrap();
            }
            let loaded = load(&path).unwrap();
            prop_assert_eq!(loaded.len(), ids.len());
            for entry in loaded {
                prop_assert_eq!(entry.schema_version, 1);
            }
        }
    }
}
