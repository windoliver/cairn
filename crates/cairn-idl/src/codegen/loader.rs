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

    let doc = RawDocument {
        schema_root: schema_root.to_path_buf(),
        index,
        envelope,
        errors,
        capabilities,
        extensions,
        common,
        preludes,
        verbs,
    };
    validate(&doc)?;
    Ok(doc)
}

/// Apply the structural invariants the spec relies on. Mirrors the
/// assertions already covered by `tests/schema_files.rs`, but consumed by the
/// codegen pipeline so a malformed IDL fails the generator before any file is
/// written.
pub fn validate(doc: &RawDocument) -> Result<(), CodegenError> {
    let contract = "cairn.mcp.v1";

    // (1) x-cairn-contract matches on every file (and the index).
    check_contract(&doc.index, "index.json", contract)?;
    for files in [
        &doc.envelope,
        &doc.errors,
        &doc.capabilities,
        &doc.extensions,
        &doc.common,
        &doc.preludes,
    ] {
        for (key, file) in files {
            check_contract(&file.value, &format!("{key}.json"), contract)?;
        }
    }
    for file in &doc.verbs {
        check_contract(
            &file.value,
            file.rel_path.to_str().unwrap_or("<verb>"),
            contract,
        )?;
    }

    // (2) Every verb declares the expected fields.
    for file in &doc.verbs {
        let path = file.rel_path.to_str().unwrap_or("<verb>");
        for required in [
            "x-cairn-verb-id",
            "x-cairn-cli",
            "x-cairn-skill-triggers",
            "x-cairn-auth",
        ] {
            if file.value.get(required).is_none() {
                return Err(CodegenError::Loader(format!(
                    "{path}: missing required key {required}"
                )));
            }
        }
        let defs = file
            .value
            .get("$defs")
            .and_then(Value::as_object)
            .ok_or_else(|| CodegenError::Loader(format!("{path}: $defs must be an object")))?;
        for required in ["Args", "Data"] {
            if !defs.contains_key(required) {
                return Err(CodegenError::Loader(format!(
                    "{path}: $defs.{required} is required"
                )));
            }
        }
    }

    // (3) Every x-cairn-capability resolves against capabilities.json.
    let capability_set = capability_universe(doc)?;
    walk_capabilities(&doc.index, "index.json", &capability_set)?;
    for file in &doc.verbs {
        walk_capabilities(
            &file.value,
            file.rel_path.to_str().unwrap_or("<verb>"),
            &capability_set,
        )?;
    }

    // (4) Every cross-file $ref resolves.
    let target_index = build_ref_index(doc);
    walk_refs(
        &doc.index,
        "index.json",
        &target_index,
        &doc.schema_root,
    )?;
    for file in &doc.verbs {
        walk_refs(
            &file.value,
            file.rel_path.to_str().unwrap_or("<verb>"),
            &target_index,
            &doc.schema_root,
        )?;
    }
    for files in [
        &doc.envelope,
        &doc.errors,
        &doc.preludes,
        &doc.common,
        &doc.extensions,
    ] {
        for file in files.values() {
            walk_refs(
                &file.value,
                file.rel_path.to_str().unwrap_or("<file>"),
                &target_index,
                &doc.schema_root,
            )?;
        }
    }

    Ok(())
}

fn check_contract(value: &Value, where_: &str, expected: &str) -> Result<(), CodegenError> {
    let actual = value.get("x-cairn-contract").and_then(Value::as_str);
    if actual != Some(expected) {
        return Err(CodegenError::Loader(format!(
            "{where_}: x-cairn-contract = {actual:?}, expected {expected:?}"
        )));
    }
    Ok(())
}

fn capability_universe(
    doc: &RawDocument,
) -> Result<std::collections::BTreeSet<String>, CodegenError> {
    let cap_file = doc
        .capabilities
        .get("capabilities")
        .ok_or_else(|| CodegenError::Loader("capabilities/capabilities.json missing".to_string()))?;
    let one_of = cap_file
        .value
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CodegenError::Loader("capabilities.json must have oneOf array".to_string())
        })?;
    let mut out = std::collections::BTreeSet::new();
    for entry in one_of {
        let c = entry
            .get("const")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodegenError::Loader(
                    "capabilities.oneOf[*].const must be string".to_string(),
                )
            })?;
        out.insert(c.to_string());
    }
    Ok(out)
}

fn walk_capabilities(
    value: &Value,
    where_: &str,
    universe: &std::collections::BTreeSet<String>,
) -> Result<(), CodegenError> {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(cap)) = map.get("x-cairn-capability") && !universe.contains(cap) {
                return Err(CodegenError::Loader(format!(
                    "{where_}: x-cairn-capability {cap:?} not declared in capabilities.json"
                )));
            }
            for v in map.values() {
                walk_capabilities(v, where_, universe)?;
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_capabilities(v, where_, universe)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Build a set of `(rel_path, json_pointer)` targets that any `$ref` may
/// resolve to.
fn build_ref_index(doc: &RawDocument) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut all: Vec<&RawFile> = Vec::new();
    all.extend(doc.envelope.values());
    all.extend(doc.errors.values());
    all.extend(doc.capabilities.values());
    all.extend(doc.extensions.values());
    all.extend(doc.common.values());
    all.extend(doc.preludes.values());
    all.extend(doc.verbs.iter());
    for file in all {
        let rel = file.rel_path.to_str().unwrap_or("");
        out.insert(format!("{rel}#"));
        collect_pointers(&file.value, &mut String::new(), rel, &mut out);
    }
    out
}

fn collect_pointers(
    value: &Value,
    prefix: &mut String,
    file: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
    out.insert(format!("{file}#{prefix}"));
    if let Value::Object(map) = value {
        for (k, v) in map {
            let saved = prefix.len();
            prefix.push('/');
            for c in k.chars() {
                match c {
                    '~' => prefix.push_str("~0"),
                    '/' => prefix.push_str("~1"),
                    other => prefix.push(other),
                }
            }
            collect_pointers(v, prefix, file, out);
            prefix.truncate(saved);
        }
    }
}

fn walk_refs(
    value: &Value,
    where_: &str,
    targets: &std::collections::BTreeSet<String>,
    schema_root: &Path,
) -> Result<(), CodegenError> {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                resolve_ref(reference, where_, targets, schema_root)?;
            }
            for v in map.values() {
                walk_refs(v, where_, targets, schema_root)?;
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_refs(v, where_, targets, schema_root)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_ref(
    reference: &str,
    where_: &str,
    targets: &std::collections::BTreeSet<String>,
    schema_root: &Path,
) -> Result<(), CodegenError> {
    // Local pointer (e.g. "#/$defs/Filter"): can't validate without the
    // origin file; the structural test suite already covers in-file pointers,
    // so the codegen loader trusts them.
    if reference.starts_with('#') {
        return Ok(());
    }
    let (file_part, pointer) = reference.split_once('#').unwrap_or((reference, ""));
    let abs = schema_root
        .join(Path::new(where_).parent().unwrap_or(Path::new("")))
        .join(file_part);
    let Ok(normalised) = abs.canonicalize() else {
        return Err(CodegenError::Loader(format!(
            "{where_}: $ref {reference:?} -> file {} does not exist",
            abs.display()
        )));
    };
    let rel = normalised
        .strip_prefix(
            schema_root
                .canonicalize()
                .map_err(|e| CodegenError::Loader(format!("canonicalize schema_root: {e}")))?,
        )
        .map_err(|_| {
            CodegenError::Loader(format!(
                "{where_}: $ref {reference:?} resolves outside schema root"
            ))
        })?;
    let needle = format!("{}#{pointer}", rel.to_string_lossy());
    if !targets.contains(&needle) {
        return Err(CodegenError::Loader(format!(
            "{where_}: $ref {reference:?} -> {needle} not found"
        )));
    }
    Ok(())
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
