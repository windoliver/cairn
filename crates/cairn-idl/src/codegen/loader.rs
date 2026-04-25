//! IDL loader — reads `index.json` and every file it references, runs
//! structural validation, returns raw `serde_json::Value` per file.

#![allow(clippy::module_name_repetitions)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::CodegenError;

/// One IDL file as returned by the loader: original on-disk path (relative to
/// the schema root) plus parsed JSON. Bytes are kept around so emitters that
/// republish a schema (the MCP `schemas/` subtree) can hash-pin the byte
/// representation rather than re-serialising.
#[derive(Debug, Clone)]
pub struct RawFile {
    /// Path of this file relative to the schema root.
    pub rel_path: PathBuf,
    /// Parsed JSON value of the file contents.
    pub value: Value,
    /// Raw bytes as read from disk.
    pub bytes: Vec<u8>,
}

/// Result of loading the entire schema tree.
#[derive(Debug, Clone)]
pub struct RawDocument {
    /// Absolute path to the schema directory that was loaded.
    pub schema_root: PathBuf,
    /// Parsed contents of `index.json`.
    pub index: Value,
    /// Files listed under `x-cairn-files.envelope`, keyed by file stem.
    pub envelope: BTreeMap<String, RawFile>,
    /// Files listed under `x-cairn-files.errors`, keyed by file stem.
    pub errors: BTreeMap<String, RawFile>,
    /// Files listed under `x-cairn-files.capabilities`, keyed by file stem.
    pub capabilities: BTreeMap<String, RawFile>,
    /// Files listed under `x-cairn-files.extensions`, keyed by file stem.
    pub extensions: BTreeMap<String, RawFile>,
    /// Files listed under `x-cairn-files.common`, keyed by file stem.
    pub common: BTreeMap<String, RawFile>,
    /// Files listed under `x-cairn-files.prelude`, keyed by file stem.
    pub preludes: BTreeMap<String, RawFile>,
    /// Verbs in the order declared by `index.json#x-cairn-files.verbs`.
    pub verbs: Vec<RawFile>,
}

/// Load the IDL rooted at `schema_root` (the directory containing
/// `index.json`).
pub fn load(schema_root: &Path) -> Result<RawDocument, CodegenError> {
    let index_path = schema_root.join("index.json");
    let index = read_json(&index_path)?;

    let files = index
        .get("x-cairn-files")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CodegenError::Loader("index.json missing object x-cairn-files".to_string())
        })?;

    let mut envelope = BTreeMap::new();
    let mut errors = BTreeMap::new();
    let mut capabilities = BTreeMap::new();
    let mut extensions = BTreeMap::new();
    let mut common = BTreeMap::new();
    let mut preludes = BTreeMap::new();
    let mut verbs = Vec::new();

    for (group, list) in files {
        let arr = list.as_array().ok_or_else(|| {
            CodegenError::Loader(format!("x-cairn-files.{group} must be an array"))
        })?;
        for entry in arr {
            let rel = entry.as_str().ok_or_else(|| {
                CodegenError::Loader(format!(
                    "x-cairn-files.{group}[*] must be string paths"
                ))
            })?;
            let rel_path = PathBuf::from(rel);
            let abs = schema_root.join(&rel_path);
            let bytes = std::fs::read(&abs)
                .map_err(|e| CodegenError::Loader(format!("read {}: {e}", abs.display())))?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
                CodegenError::Loader(format!("parse {}: {e}", abs.display()))
            })?;
            let file = RawFile {
                rel_path: rel_path.clone(),
                value,
                bytes,
            };
            let key = file_key(&rel_path);
            match group.as_str() {
                "envelope" => {
                    envelope.insert(key, file);
                }
                "errors" => {
                    errors.insert(key, file);
                }
                "capabilities" => {
                    capabilities.insert(key, file);
                }
                "extensions" => {
                    extensions.insert(key, file);
                }
                "common" => {
                    common.insert(key, file);
                }
                "prelude" => {
                    preludes.insert(key, file);
                }
                "verbs" => {
                    verbs.push(file);
                }
                other => {
                    return Err(CodegenError::Loader(format!(
                        "unknown x-cairn-files group: {other}"
                    )));
                }
            }
        }
    }

    Ok(RawDocument {
        schema_root: schema_root.to_path_buf(),
        index,
        envelope,
        errors,
        capabilities,
        extensions,
        common,
        preludes,
        verbs,
    })
}

fn read_json(path: &Path) -> Result<Value, CodegenError> {
    let bytes = std::fs::read(path)
        .map_err(|e| CodegenError::Loader(format!("read {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| CodegenError::Loader(format!("parse {}: {e}", path.display())))
}

/// Stable map key for a file: stem of the file name (e.g. `verbs/ingest.json`
/// → `ingest`). Used so emitters address files by logical name rather than
/// path.
fn file_key(rel_path: &Path) -> String {
    rel_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}
