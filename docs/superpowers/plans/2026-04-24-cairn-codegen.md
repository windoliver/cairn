# cairn-codegen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `cairn-codegen` binary that reads `crates/cairn-idl/schema/` and emits four deterministic artefact bundles (Rust SDK types, CLI clap commands, MCP tool decls + JSON schemas, Cairn skill bundle) into committed locations under `crates/cairn-core/src/generated/`, `crates/cairn-cli/src/generated/`, `crates/cairn-mcp/src/generated/`, and `skills/cairn/`. CI gates on no-diff so the four surfaces cannot drift.

**Architecture:** Three-stage pure pipeline — `loader` (read+validate IDL JSON) → `ir` (typed Rust IR with explicit lowering rules) → four `emit_*` modules (each returns `Vec<GeneratedFile>`; the `run` function batches all writes after every emitter succeeds, so partial failure is impossible). Hand-rolled string-builders, zero-dep beyond what cairn-idl already pulls plus `tempfile` and `clap`. Determinism enforced by `BTreeMap` everywhere, canonical JSON serialiser, and tests that run codegen 5× and assert byte-equal outputs.

**Tech Stack:** Rust 2024 edition, `serde_json` for IDL parsing, `clap` for the binary's `--check` / `--out` flags, `tempfile` for atomic writes, `rstest` + `insta` + `proptest` for tests. No `quote!`, no `syn`, no `prettyplease`, no `typify` — emitters write Rust as plain strings.

**Spec:** `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md`

---

## Phase 0 — Foundation

### Task 1: Add workspace dependencies

**Files:**
- Modify: `Cargo.toml`

This phase adds the workspace deps that later tasks pull in. Done up-front so subsequent tasks reference `{ workspace = true }`.

- [ ] **Step 1: Add clap, tempfile, rstest, insta, proptest to `[workspace.dependencies]`**

Edit `Cargo.toml`. After the existing `anyhow = "1"` line in `[workspace.dependencies]`, add:

```toml
clap = { version = "4.5", features = ["derive"] }
tempfile = "3"
rstest = "0.23"
insta = { version = "1", features = ["json", "yaml"] }
proptest = "1"
```

- [ ] **Step 2: Verify workspace resolves**

Run: `cargo metadata --format-version 1 --locked >/dev/null 2>&1 || cargo metadata --format-version 1 >/dev/null`
Expected: exits 0. Refreshes `Cargo.lock`.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add clap, tempfile, rstest, insta, proptest workspace deps (#35)"
```

---

### Task 2: Add `serde_json` to cairn-core

**Files:**
- Modify: `crates/cairn-core/Cargo.toml`

Generated SDK types use `serde_json::Value` for opaque blobs (frontmatter, `additionalProperties: true` schemas). cairn-core needs the dep.

- [ ] **Step 1: Add serde_json**

Edit `crates/cairn-core/Cargo.toml`. After `serde = { workspace = true }`, add:

```toml
serde_json = { workspace = true }
```

- [ ] **Step 2: Verify cairn-core still builds**

Run: `cargo check -p cairn-core`
Expected: exits 0; no new warnings.

- [ ] **Step 3: Verify core boundary still clean**

Run: `./scripts/check-core-boundary.sh`
Expected: prints `cairn-core boundary OK` and exits 0. (`serde_json` is an external crate, not `cairn-*`.)

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/Cargo.toml
git commit -m "core: add serde_json dep for generated opaque-blob fields (#35)"
```

---

### Task 3: Wire cairn-idl deps and module skeleton

**Files:**
- Modify: `crates/cairn-idl/Cargo.toml`
- Modify: `crates/cairn-idl/src/lib.rs`
- Create: `crates/cairn-idl/src/codegen/mod.rs`
- Create: `crates/cairn-idl/src/codegen/loader.rs`
- Create: `crates/cairn-idl/src/codegen/ir.rs`
- Create: `crates/cairn-idl/src/codegen/fmt.rs`
- Create: `crates/cairn-idl/src/codegen/emit_sdk.rs`
- Create: `crates/cairn-idl/src/codegen/emit_cli.rs`
- Create: `crates/cairn-idl/src/codegen/emit_mcp.rs`
- Create: `crates/cairn-idl/src/codegen/emit_skill.rs`

Lay out empty modules so later tasks slot into named files without scaffolding distractions. Each file gets one stub `pub fn` or struct so module declarations compile.

- [ ] **Step 1: Update cairn-idl Cargo.toml**

Replace the contents of `crates/cairn-idl/Cargo.toml` with:

```toml
[package]
name = "cairn-idl"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "Cairn canonical IDL source and codegen driver. Standalone — no core dep."

[[bin]]
name = "cairn-codegen"
path = "src/bin/cairn-codegen.rs"

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
clap = { workspace = true }
tempfile = { workspace = true }

[dev-dependencies]
rstest = { workspace = true }
insta = { workspace = true }
proptest = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Replace lib.rs to expose codegen module**

Replace `crates/cairn-idl/src/lib.rs` with:

```rust
//! Cairn IDL source and codegen driver.
//!
//! Hosts the canonical `cairn.mcp.v1` JSON Schema files under [`SCHEMA_DIR`]
//! and the [`codegen`] pipeline that lowers them into Rust SDK types, CLI clap
//! definitions, MCP tool declarations, and the shippable Cairn skill bundle.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod codegen;

/// Absolute path to the `schema/` directory that holds every IDL source file
/// for the `cairn.mcp.v1` contract. Downstream crates (codegen, CLI, MCP) read
/// this to locate the schema root without duplicating the path.
pub const SCHEMA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema");
```

- [ ] **Step 3: Create codegen module skeleton**

Create `crates/cairn-idl/src/codegen/mod.rs`:

```rust
//! Codegen pipeline: load IDL → lower to IR → emit four artefact bundles.
//!
//! Public entry point is [`run`]. Internally split into [`loader`], [`ir`],
//! and four `emit_*` modules. Stages are pure (no filesystem writes) until
//! `run` batches the union of every emitter's [`GeneratedFile`] outputs in a
//! single atomic pass, so a panic mid-emit cannot leave half-written trees.

pub mod emit_cli;
pub mod emit_mcp;
pub mod emit_sdk;
pub mod emit_skill;
pub mod fmt;
pub mod ir;
pub mod loader;

use std::path::PathBuf;

/// One generated artefact returned by an emitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// Raw bytes to write.
    pub bytes: Vec<u8>,
}

/// Errors returned by the codegen pipeline.
#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    /// Loader could not read or parse an IDL file.
    #[error("loader: {0}")]
    Loader(String),
    /// IR lowering rejected a schema construct it cannot represent.
    #[error("ir: {0}")]
    Ir(String),
    /// An emitter produced inconsistent output.
    #[error("emit: {0}")]
    Emit(String),
    /// Filesystem operation failed during write.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Options for [`run`].
#[derive(Debug, Clone)]
pub struct RunOpts {
    /// Workspace root that emitter outputs are written under. Defaults to the
    /// parent of `CARGO_MANIFEST_DIR` (i.e. the workspace root).
    pub workspace_root: PathBuf,
    /// In `Check` mode the binary exits non-zero on any byte-diff between
    /// emitter output and the on-disk file; no writes happen.
    pub mode: RunMode,
}

/// Generator mode — write outputs vs. assert no drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Write generated files to `workspace_root`.
    Write,
    /// Compare generated bytes against on-disk; exit non-zero on drift.
    Check,
}

/// Summary returned to the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Total artefacts emitted.
    pub files_emitted: usize,
    /// In Check mode, files whose on-disk bytes differ from emitted bytes.
    pub drift: Vec<PathBuf>,
}

/// Run the full pipeline. Stub — implementation lands in later tasks.
pub fn run(_opts: &RunOpts) -> Result<Report, CodegenError> {
    Err(CodegenError::Emit(
        "codegen::run is not yet implemented".to_string(),
    ))
}
```

- [ ] **Step 4: Create empty submodule stubs**

Create `crates/cairn-idl/src/codegen/loader.rs`:

```rust
//! IDL loader — reads `index.json` and every file it references, runs
//! structural validation, returns raw `serde_json::Value` per file.

#![allow(clippy::module_name_repetitions)]
```

Create `crates/cairn-idl/src/codegen/ir.rs`:

```rust
//! Typed intermediate representation produced by [`super::loader`] and
//! consumed by every `emit_*` module.

#![allow(clippy::module_name_repetitions)]
```

Create `crates/cairn-idl/src/codegen/fmt.rs`:

```rust
//! Deterministic formatting helpers shared by every emitter.
//!
//! Provides canonical JSON serialisation (recursive key sort, two-space
//! indent, trailing newline) and a small Rust source-builder that always
//! ends files with exactly one trailing newline.
```

Create `crates/cairn-idl/src/codegen/emit_sdk.rs`:

```rust
//! SDK emitter — writes Rust types into `crates/cairn-core/src/generated/`.
```

Create `crates/cairn-idl/src/codegen/emit_cli.rs`:

```rust
//! CLI emitter — writes a clap `Command` builder into
//! `crates/cairn-cli/src/generated/`.
```

Create `crates/cairn-idl/src/codegen/emit_mcp.rs`:

```rust
//! MCP emitter — writes tool declarations and JSON schemas into
//! `crates/cairn-mcp/src/generated/`.
```

Create `crates/cairn-idl/src/codegen/emit_skill.rs`:

```rust
//! Skill emitter — writes `SKILL.md`, `conventions.md`, and `.version`
//! into `skills/cairn/`.
```

- [ ] **Step 5: Build to verify scaffolding compiles**

Run: `cargo check -p cairn-idl`
Expected: exits 0 with no errors. May warn about unused imports — ignore for this scaffolding step.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-idl/Cargo.toml crates/cairn-idl/src/lib.rs crates/cairn-idl/src/codegen/
git commit -m "idl: scaffold codegen module structure (#35)"
```

---

## Phase 1 — Loader

### Task 4: Loader — index.json walk

**Files:**
- Modify: `crates/cairn-idl/src/codegen/loader.rs`
- Create: `crates/cairn-idl/tests/codegen_loader.rs`

Read the manifest, pull every file path under `x-cairn-files`, deserialise each into `serde_json::Value`. No structural checks yet — those come next.

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-idl/tests/codegen_loader.rs`:

```rust
//! Loader tests. Each test feeds the loader an IDL root and asserts the
//! returned [`RawDocument`] contains the expected files / errors.

use std::path::PathBuf;

use cairn_idl::codegen::loader::{load, RawDocument};

fn schema_dir() -> PathBuf {
    PathBuf::from(cairn_idl::SCHEMA_DIR)
}

#[test]
fn loads_real_schema_root() {
    let doc: RawDocument = load(&schema_dir()).expect("real schema must load");
    // Manifest pins eight verbs.
    assert_eq!(doc.verbs.len(), 8, "expected 8 verbs, got {}", doc.verbs.len());
    // Two preludes (status, handshake).
    assert_eq!(doc.preludes.len(), 2);
    // index.json itself is captured.
    assert!(doc.index.get("x-cairn-verb-ids").is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p cairn-idl --test codegen_loader 2>&1 | head -40`
Expected: FAIL — `load` and `RawDocument` are not exported.

- [ ] **Step 3: Implement loader**

Replace `crates/cairn-idl/src/codegen/loader.rs` with:

```rust
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
    pub rel_path: PathBuf,
    pub value: Value,
    pub bytes: Vec<u8>,
}

/// Result of loading the entire schema tree.
#[derive(Debug, Clone)]
pub struct RawDocument {
    pub schema_root: PathBuf,
    pub index: Value,
    pub envelope: BTreeMap<String, RawFile>,
    pub errors: BTreeMap<String, RawFile>,
    pub capabilities: BTreeMap<String, RawFile>,
    pub extensions: BTreeMap<String, RawFile>,
    pub common: BTreeMap<String, RawFile>,
    pub preludes: BTreeMap<String, RawFile>,
    /// Verbs in the order declared by `index.json#x-cairn-files.verbs`.
    pub verbs: Vec<RawFile>,
}

/// Load the IDL rooted at `schema_root` (the directory containing
/// `index.json`).
pub fn load(schema_root: &Path) -> Result<RawDocument, CodegenError> {
    let index_path = schema_root.join("index.json");
    let index = read_json(&index_path)?;

    let files = index.get("x-cairn-files").and_then(Value::as_object).ok_or_else(|| {
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
            let file = RawFile { rel_path: rel_path.clone(), value, bytes };
            let key = file_key(&rel_path);
            match group.as_str() {
                "envelope" => { envelope.insert(key, file); }
                "errors" => { errors.insert(key, file); }
                "capabilities" => { capabilities.insert(key, file); }
                "extensions" => { extensions.insert(key, file); }
                "common" => { common.insert(key, file); }
                "prelude" => { preludes.insert(key, file); }
                "verbs" => { verbs.push(file); }
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p cairn-idl --test codegen_loader`
Expected: PASS — `loads_real_schema_root` succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-idl/src/codegen/loader.rs crates/cairn-idl/tests/codegen_loader.rs
git commit -m "codegen: loader walks index.json and reads every IDL file (#35)"
```

---

### Task 5: Loader — structural validation

**Files:**
- Modify: `crates/cairn-idl/src/codegen/loader.rs`
- Modify: `crates/cairn-idl/tests/codegen_loader.rs`

Validate the assertions the spec requires before lowering: every file has the right `x-cairn-contract`, every verb declares the expected fields, every `$ref` resolves, every `x-cairn-capability` is a known capability. Tests use in-memory IDL fixtures (modified copies of the real tree written into `tempfile::tempdir()`).

- [ ] **Step 1: Write failing tests for each rejection case**

Add to `crates/cairn-idl/tests/codegen_loader.rs`:

```rust
use std::fs;
use std::io::Write;
use tempfile::TempDir;

/// Copy the real schema tree into a tempdir so a single file can be mutated
/// without touching the source.
fn fork_schema() -> TempDir {
    let src = schema_dir();
    let dst = tempfile::tempdir().unwrap();
    copy_tree(&src, dst.path());
    dst
}

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&dst_path).unwrap();
            copy_tree(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), dst_path).unwrap();
        }
    }
}

fn write_json(path: &std::path::Path, value: &serde_json::Value) {
    let mut f = fs::File::create(path).unwrap();
    let bytes = serde_json::to_vec_pretty(value).unwrap();
    f.write_all(&bytes).unwrap();
}

#[test]
fn rejects_wrong_contract_id() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/ingest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["x-cairn-contract"] = serde_json::json!("cairn.mcp.v2");
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("x-cairn-contract"), "got: {msg}");
}

#[test]
fn rejects_verb_missing_args_defs() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/ingest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["$defs"].as_object_mut().unwrap().remove("Args");
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("Args"), "got: {msg}");
}

#[test]
fn rejects_unknown_capability_reference() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/forget.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["x-cairn-capability"] = serde_json::json!("cairn.mcp.v1.does_not_exist");
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("does_not_exist"), "got: {msg}");
}

#[test]
fn rejects_dangling_ref() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/ingest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    // Replace the Ulid ref in Data.record_id with a nonexistent one.
    value["$defs"]["Data"]["properties"]["record_id"] =
        serde_json::json!({ "$ref": "../common/primitives.json#/$defs/NotARealType" });
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("NotARealType"), "got: {msg}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-idl --test codegen_loader`
Expected: FOUR new tests fail; the first one passes. Validation isn't implemented yet.

- [ ] **Step 3: Add validation pass to the loader**

Append to `crates/cairn-idl/src/codegen/loader.rs` (just before the existing `read_json` fn):

```rust
/// Apply the structural invariants the spec relies on. Mirrors the
/// assertions already covered by `tests/schema_files.rs`, but consumed by the
/// codegen pipeline so a malformed IDL fails the generator before any file is
/// written.
pub fn validate(doc: &RawDocument) -> Result<(), CodegenError> {
    let contract = "cairn.mcp.v1";

    // (1) x-cairn-contract matches on every file (and the index).
    check_contract(&doc.index, "index.json", contract)?;
    for files in [&doc.envelope, &doc.errors, &doc.capabilities, &doc.extensions, &doc.common, &doc.preludes] {
        for (key, file) in files {
            check_contract(&file.value, &format!("{}.json", key), contract)?;
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
        for required in ["x-cairn-verb-id", "x-cairn-cli", "x-cairn-skill-triggers", "x-cairn-auth"] {
            if file.value.get(required).is_none() {
                return Err(CodegenError::Loader(format!(
                    "{path}: missing required key {required}"
                )));
            }
        }
        let defs = file.value.get("$defs").and_then(Value::as_object).ok_or_else(|| {
            CodegenError::Loader(format!("{path}: $defs must be an object"))
        })?;
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
    walk_refs(&doc.index, "index.json", &target_index, &doc.schema_root)?;
    for file in &doc.verbs {
        walk_refs(
            &file.value,
            file.rel_path.to_str().unwrap_or("<verb>"),
            &target_index,
            &doc.schema_root,
        )?;
    }
    for files in [&doc.envelope, &doc.errors, &doc.preludes, &doc.common, &doc.extensions] {
        for (key, file) in files {
            walk_refs(&file.value, &format!("{}.json", key), &target_index, &doc.schema_root)?;
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

fn capability_universe(doc: &RawDocument) -> Result<std::collections::BTreeSet<String>, CodegenError> {
    let cap_file = doc
        .capabilities
        .get("capabilities")
        .ok_or_else(|| CodegenError::Loader("capabilities/capabilities.json missing".to_string()))?;
    let one_of = cap_file
        .value
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| CodegenError::Loader("capabilities.json must have oneOf array".to_string()))?;
    let mut out = std::collections::BTreeSet::new();
    for entry in one_of {
        let c = entry
            .get("const")
            .and_then(Value::as_str)
            .ok_or_else(|| CodegenError::Loader("capabilities.oneOf[*].const must be string".to_string()))?;
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
            if let Some(Value::String(cap)) = map.get("x-cairn-capability") {
                if !universe.contains(cap) {
                    return Err(CodegenError::Loader(format!(
                        "{where_}: x-cairn-capability {cap:?} not declared in capabilities.json"
                    )));
                }
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

/// Build a set of `(rel_path, json_pointer)` targets that any `$ref` may resolve to.
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
    let normalised = match abs.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return Err(CodegenError::Loader(format!(
                "{where_}: $ref {reference:?} -> file {} does not exist",
                abs.display()
            )));
        }
    };
    let rel = normalised
        .strip_prefix(schema_root.canonicalize().map_err(|e| {
            CodegenError::Loader(format!("canonicalize schema_root: {e}"))
        })?)
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
```

- [ ] **Step 4: Wire validation into `load`**

In `crates/cairn-idl/src/codegen/loader.rs`, replace the final `Ok(RawDocument { ... })` return in `load` with:

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-idl --test codegen_loader`
Expected: ALL FIVE tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-idl/src/codegen/loader.rs crates/cairn-idl/tests/codegen_loader.rs
git commit -m "codegen: loader validates contract id, defs, capabilities, refs (#35)"
```

---

## Phase 2 — IR lowering

### Task 6: IR type definitions

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs`

Define every IR variant the emitters consume. No lowering logic yet — that arrives in Tasks 7–11. This task is a pure declaration so subsequent tasks compile incrementally.

- [ ] **Step 1: Replace `crates/cairn-idl/src/codegen/ir.rs` with the full IR**

```rust
//! Typed intermediate representation produced by [`super::loader`] and
//! consumed by every `emit_*` module.

#![allow(clippy::module_name_repetitions)]

use std::collections::BTreeMap;

/// Top-level IR built from a validated [`super::loader::RawDocument`].
#[derive(Debug, Clone)]
pub struct Document {
    pub contract: String,
    pub capabilities: Vec<String>,
    pub error_codes: Vec<ErrorVariant>,
    pub common: BTreeMap<TypeName, RustType>,
    pub envelope: BTreeMap<TypeName, RustType>,
    pub verbs: Vec<VerbDef>,
    pub preludes: Vec<PreludeDef>,
}

/// One verb in IDL order.
#[derive(Debug, Clone)]
pub struct VerbDef {
    pub id: String,
    pub args: RustType,
    pub data: RustType,
    pub cli: CliShape,
    pub skill: SkillBlock,
    pub capability: Option<String>,
    pub auth: AuthModel,
    pub args_schema_bytes: Vec<u8>,
    pub data_schema_bytes: Vec<u8>,
}

/// One protocol prelude (status, handshake).
#[derive(Debug, Clone)]
pub struct PreludeDef {
    pub id: String,
    pub response: RustType,
    pub schema_bytes: Vec<u8>,
}

/// One error code variant, lowered from the closed `oneOf` in `errors/error.json`.
#[derive(Debug, Clone)]
pub struct ErrorVariant {
    pub code: String,
    pub data: Option<TypeName>,
}

/// CLI shape extracted from `x-cairn-cli`. Verbs whose Args are a tagged union
/// (RetrieveArgs) carry one CliShape per variant.
#[derive(Debug, Clone)]
pub enum CliShape {
    Single(CliCommand),
    Variants(Vec<CliCommand>),
}

#[derive(Debug, Clone)]
pub struct CliCommand {
    pub command: String,
    pub flags: Vec<CliFlag>,
    pub positional: Option<CliPositional>,
}

#[derive(Debug, Clone)]
pub struct CliFlag {
    pub name: String,
    pub long: String,
    pub value_source: String,
}

#[derive(Debug, Clone)]
pub struct CliPositional {
    pub name: String,
    pub description: String,
}

/// Skill triggers extracted from `x-cairn-skill-triggers`.
#[derive(Debug, Clone, Default)]
pub struct SkillBlock {
    pub positive: Vec<String>,
    pub negative: Vec<String>,
    pub exclusivity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthModel {
    SignedChain,
    Rebac,
    SignedPrincipal,
    HardwareKey,
}

impl AuthModel {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "signed_chain" => Some(Self::SignedChain),
            "rebac" => Some(Self::Rebac),
            "signed_principal" => Some(Self::SignedPrincipal),
            "hardware_key" => Some(Self::HardwareKey),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignedChain => "signed_chain",
            Self::Rebac => "rebac",
            Self::SignedPrincipal => "signed_principal",
            Self::HardwareKey => "hardware_key",
        }
    }
}

/// Stable identifier used for Rust types and module paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeName(pub String);

impl TypeName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Lowered Rust type. Mirrors the lowering rules table in
/// `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md` §4.2.
#[derive(Debug, Clone)]
pub enum RustType {
    Primitive(Prim),
    Optional(Box<RustType>),
    Vec(Box<RustType>),
    /// Map<String, T> — used for `additionalProperties: <schema>` blobs.
    Map(Box<RustType>),
    /// Resolved `$ref` — the `TypeName` is one of `common` / `errors` / a
    /// per-verb local def.
    Ref(TypeName),
    Struct(StructDef),
    Enum(EnumDef),
    TaggedUnion(TaggedUnionDef),
    UntaggedUnion(UntaggedUnionDef),
    Recursive(RecursiveEnumDef),
    /// Opaque `serde_json::Value` blob — used when the schema is
    /// `additionalProperties: true` (frontmatter, etc.).
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prim {
    String,
    I64,
    U64,
    F64,
    Bool,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: TypeName,
    pub fields: Vec<StructField>,
    pub deny_unknown_fields: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: RustType,
    pub required: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: TypeName,
    pub variants: Vec<EnumVariant>,
    pub rename_all: Option<&'static str>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    /// Wire string (the `const` from JSON Schema).
    pub wire: String,
    /// Rust identifier (PascalCased wire).
    pub rust_ident: String,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaggedUnionDef {
    pub name: TypeName,
    pub discriminator: String,
    pub variants: Vec<TaggedVariant>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaggedVariant {
    /// Discriminator value (e.g. `"record"`).
    pub wire: String,
    pub rust_ident: String,
    pub fields: Vec<StructField>,
    pub capability: Option<String>,
    pub cli: Option<CliCommand>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UntaggedUnionDef {
    pub name: TypeName,
    /// All-Optional fields; `validate` enforces exactly-one-of these required-sets.
    pub fields: Vec<StructField>,
    pub xor_groups: Vec<Vec<String>>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecursiveEnumDef {
    pub name: TypeName,
    pub leaf: Box<RustType>,
    pub max_depth: u32,
    pub max_fanout: u32,
    pub doc: Option<String>,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p cairn-idl`
Expected: exits 0. May warn about dead code on the IR types — pass `RUSTFLAGS="-A dead_code"` if necessary or accept the warnings (they go away once emitters consume the IR in Phase 4).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-idl/src/codegen/ir.rs
git commit -m "codegen: declare IR types (Document, VerbDef, RustType variants) (#35)"
```

---

### Task 7: Add `x-cairn-discriminator` to RetrieveArgs

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/retrieve.json`
- Create: `crates/cairn-idl/tests/schema_discriminator.rs`

The IR's `TaggedUnion` variant needs a discriminator hint on the IDL. Per spec §4.3, this is the only IDL change in this PR. Purely additive — existing `oneOf` consumers ignore unknown vendor extensions. Lands before IR lowering so the loader / lowering test can assume it.

- [ ] **Step 1: Write failing test**

Create `crates/cairn-idl/tests/schema_discriminator.rs`:

```rust
//! Asserts every `oneOf` in the IDL either carries `x-cairn-discriminator`
//! or is in a known allow-list (string-const enums; XOR-required patterns;
//! the request envelope's verb-dispatch oneOf).

use std::path::PathBuf;
use serde_json::Value;

const ALLOWED_NO_DISCRIMINATOR: &[&str] = &[
    // Closed string enums:
    "capabilities/capabilities.json#/oneOf",
    "verbs/search.json#/$defs/Args/properties/mode/oneOf",
    "verbs/search.json#/$defs/filter_leaf/oneOf",
    "verbs/search.json#/$defs/filter_leaf_array_contains/properties/value/oneOf",
    "verbs/search.json#/$defs/filter_leaf_array_contains_set/properties/value/items/oneOf",
    "verbs/search.json#/$defs/filter_L1/oneOf",
    "verbs/search.json#/$defs/filter_L2/oneOf",
    "verbs/search.json#/$defs/filter_L3/oneOf",
    "verbs/search.json#/$defs/filter_L4/oneOf",
    "verbs/search.json#/$defs/filter_L5/oneOf",
    "verbs/search.json#/$defs/filter_L6/oneOf",
    "verbs/search.json#/$defs/filter_L7/oneOf",
    "verbs/search.json#/$defs/filter_L8/oneOf",
    // XOR-required pattern:
    "verbs/ingest.json#/$defs/Args/oneOf",
    "envelope/signed_intent.json#/oneOf",
    // Errors enum is a tagged union on `code` — gets its own discriminator below.
    "errors/error.json#/oneOf",
];

#[test]
fn every_oneof_has_discriminator_or_is_allowlisted() {
    let root = PathBuf::from(cairn_idl::SCHEMA_DIR);
    let mut violations = Vec::new();
    walk(&root, &root, &mut violations);
    assert!(
        violations.is_empty(),
        "the following oneOf sites need x-cairn-discriminator or to be added to ALLOWED_NO_DISCRIMINATOR:\n{}",
        violations.join("\n")
    );
}

fn walk(root: &std::path::Path, dir: &std::path::Path, violations: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            walk(root, &path, violations);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let rel = path.strip_prefix(root).unwrap().to_string_lossy().to_string();
        check(&value, &rel, &mut String::new(), violations);
    }
}

fn check(value: &Value, file: &str, pointer: &mut String, violations: &mut Vec<String>) {
    if let Value::Object(map) = value {
        if let Some(_one_of) = map.get("oneOf") {
            let site = format!("{file}#{pointer}/oneOf");
            let has_discriminator = map.contains_key("x-cairn-discriminator");
            if !has_discriminator && !ALLOWED_NO_DISCRIMINATOR.contains(&site.as_str()) {
                violations.push(site);
            }
        }
        for (k, v) in map {
            let saved = pointer.len();
            pointer.push('/');
            for c in k.chars() {
                match c {
                    '~' => pointer.push_str("~0"),
                    '/' => pointer.push_str("~1"),
                    other => pointer.push(other),
                }
            }
            check(v, file, pointer, violations);
            pointer.truncate(saved);
        }
    } else if let Value::Array(arr) = value {
        for (i, v) in arr.iter().enumerate() {
            let saved = pointer.len();
            pointer.push('/');
            pointer.push_str(&i.to_string());
            check(v, file, pointer, violations);
            pointer.truncate(saved);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p cairn-idl --test schema_discriminator`
Expected: FAIL — `verbs/retrieve.json#/$defs/Args/oneOf` listed as violation.

- [ ] **Step 3: Add `x-cairn-discriminator` to retrieve.json**

In `crates/cairn-idl/schema/verbs/retrieve.json`, find the `Args` definition (lines 21–30). Replace:

```json
    "Args": {
      "oneOf": [
        { "$ref": "#/$defs/ArgsRecord" },
        { "$ref": "#/$defs/ArgsSession" },
        { "$ref": "#/$defs/ArgsTurn" },
        { "$ref": "#/$defs/ArgsFolder" },
        { "$ref": "#/$defs/ArgsScope" },
        { "$ref": "#/$defs/ArgsProfile" }
      ]
    },
```

with:

```json
    "Args": {
      "x-cairn-discriminator": "target",
      "oneOf": [
        { "$ref": "#/$defs/ArgsRecord" },
        { "$ref": "#/$defs/ArgsSession" },
        { "$ref": "#/$defs/ArgsTurn" },
        { "$ref": "#/$defs/ArgsFolder" },
        { "$ref": "#/$defs/ArgsScope" },
        { "$ref": "#/$defs/ArgsProfile" }
      ]
    },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p cairn-idl --test schema_discriminator`
Expected: PASS.

- [ ] **Step 5: Re-run existing schema tests to confirm no regression**

Run: `cargo nextest run -p cairn-idl --test schema_files --test smoke`
Expected: existing tests still pass — `x-cairn-discriminator` is purely additive.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-idl/schema/verbs/retrieve.json crates/cairn-idl/tests/schema_discriminator.rs
git commit -m "idl: mark RetrieveArgs oneOf with x-cairn-discriminator (#35)"
```

---

### Task 8: IR lowering — primitives, refs, structs, enums

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs`
- Create: `crates/cairn-idl/tests/codegen_ir.rs`

Add a `lower_schema(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError>` function that handles the four "simple" cases. Tagged unions, untagged unions, and recursive Filter come in the next tasks.

- [ ] **Step 1: Write failing tests**

Create `crates/cairn-idl/tests/codegen_ir.rs`:

```rust
use serde_json::json;
use cairn_idl::codegen::ir::{lower_schema, Ctx, Prim, RustType};

#[test]
fn primitive_string() {
    let v = json!({"type": "string"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Primitive(Prim::String)));
}

#[test]
fn primitive_integer_unsigned_minimum_zero() {
    let v = json!({"type": "integer", "minimum": 0});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Primitive(Prim::U64)));
}

#[test]
fn primitive_integer_signed_default() {
    let v = json!({"type": "integer"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Primitive(Prim::I64)));
}

#[test]
fn array_of_strings() {
    let v = json!({"type": "array", "items": {"type": "string"}});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Vec(inner) = ty else { panic!("expected Vec, got {ty:?}") };
    assert!(matches!(*inner, RustType::Primitive(Prim::String)));
}

#[test]
fn ref_resolves_to_typename() {
    let v = json!({"$ref": "../common/primitives.json#/$defs/Ulid"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Ref(name) = ty else { panic!("expected Ref, got {ty:?}") };
    assert_eq!(name.0, "Ulid");
}

#[test]
fn struct_with_required_and_optional() {
    let v = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["a"],
        "properties": {
            "a": {"type": "string"},
            "b": {"type": "integer", "minimum": 0}
        }
    });
    let mut ctx = Ctx::with_target("Demo");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Struct(s) = ty else { panic!("expected Struct, got {ty:?}") };
    assert_eq!(s.fields.len(), 2);
    assert!(s.deny_unknown_fields);
    let a = s.fields.iter().find(|f| f.name == "a").unwrap();
    assert!(a.required);
    let b = s.fields.iter().find(|f| f.name == "b").unwrap();
    assert!(!b.required);
}

#[test]
fn string_enum_lowers_to_enum() {
    let v = json!({
        "type": "string",
        "enum": ["asc", "desc"]
    });
    let mut ctx = Ctx::with_target("Order");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Enum(e) = ty else { panic!("expected Enum, got {ty:?}") };
    assert_eq!(e.variants.len(), 2);
    assert_eq!(e.variants[0].wire, "asc");
    assert_eq!(e.variants[0].rust_ident, "Asc");
}

#[test]
fn oneof_of_const_lowers_to_enum() {
    let v = json!({
        "oneOf": [
            { "const": "keyword" },
            { "const": "semantic" },
            { "const": "hybrid" }
        ]
    });
    let mut ctx = Ctx::with_target("Mode");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Enum(e) = ty else { panic!("expected Enum, got {ty:?}") };
    assert_eq!(e.variants.len(), 3);
}

#[test]
fn additional_properties_true_lowers_to_json() {
    let v = json!({"type": "object"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Json));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-idl --test codegen_ir`
Expected: ALL fail — `lower_schema`, `Ctx` not exported.

- [ ] **Step 3: Implement lowering**

Append to `crates/cairn-idl/src/codegen/ir.rs`:

```rust
use serde_json::Value;

use super::CodegenError;

/// Lowering context — carries the target type name (for struct/enum naming)
/// and any additional resolution state.
#[derive(Debug, Default, Clone)]
pub struct Ctx {
    pub target: Option<TypeName>,
}

impl Ctx {
    pub fn with_target(name: impl Into<String>) -> Self {
        Self { target: Some(TypeName::new(name)) }
    }
    pub fn child(&self, suffix: &str) -> Self {
        let target = self
            .target
            .as_ref()
            .map(|t| TypeName::new(format!("{}{suffix}", t.0)));
        Self { target }
    }
}

pub fn lower_schema(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    // (1) `$ref` short-circuits.
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        return Ok(RustType::Ref(typename_from_ref(reference)));
    }

    // (2) `oneOf` cases.
    if let Some(arr) = value.get("oneOf").and_then(Value::as_array) {
        // (2a) tagged union (handled in Task 9 — fall through if discriminator absent).
        if value.get("x-cairn-discriminator").is_some() {
            return lower_tagged_union(value, arr, ctx);
        }
        // (2b) all-const → string enum.
        if arr.iter().all(|v| v.get("const").and_then(Value::as_str).is_some()) {
            return lower_const_oneof(arr, ctx);
        }
        // (2c) untagged union via XOR (ingest, signed_intent) — Task 10.
        if arr.iter().all(|v| v.get("required").is_some()) {
            return lower_untagged_union(value, arr, ctx);
        }
        return Err(CodegenError::Ir(format!(
            "oneOf shape not recognised — needs x-cairn-discriminator or all-const variants"
        )));
    }

    // (3) explicit type.
    let ty = value.get("type").and_then(Value::as_str);
    let enum_arr = value.get("enum").and_then(Value::as_array);

    match (ty, enum_arr) {
        (Some("string"), Some(values)) => lower_string_enum(values, ctx),
        (Some("string"), None) => Ok(RustType::Primitive(Prim::String)),
        (Some("integer"), _) => Ok(if value.get("minimum").and_then(Value::as_i64) == Some(0) {
            RustType::Primitive(Prim::U64)
        } else {
            RustType::Primitive(Prim::I64)
        }),
        (Some("number"), _) => Ok(RustType::Primitive(Prim::F64)),
        (Some("boolean"), _) => Ok(RustType::Primitive(Prim::Bool)),
        (Some("array"), _) => {
            let items = value.get("items").ok_or_else(|| {
                CodegenError::Ir("array missing items".to_string())
            })?;
            let mut child = ctx.child("Item");
            let inner = lower_schema(items, &mut child)?;
            Ok(RustType::Vec(Box::new(inner)))
        }
        (Some("object"), _) => lower_object(value, ctx),
        (None, _) if value.get("const").is_some() => {
            // Standalone const (e.g. retrieve target marker). Rare outside oneOf.
            Ok(RustType::Primitive(Prim::String))
        }
        (None, _) => Err(CodegenError::Ir(format!(
            "schema has no `type`, no `$ref`, no `oneOf`: {value}"
        ))),
        (Some(other), _) => Err(CodegenError::Ir(format!("unhandled type: {other}"))),
    }
}

fn lower_object(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let additional = value
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let properties = value.get("properties").and_then(Value::as_object);
    if !additional || properties.is_none() {
        // additionalProperties: false + no properties is empty; treat as Json.
        if properties.is_none() {
            return Ok(RustType::Json);
        }
    }
    let target_name = ctx.target.clone().unwrap_or_else(|| TypeName::new("Anon"));
    let required: std::collections::BTreeSet<String> = value
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let props = properties.unwrap();

    let mut fields = Vec::with_capacity(props.len());
    let mut keys: Vec<&String> = props.keys().collect();
    keys.sort();
    for key in keys {
        let prop = &props[key];
        let mut child = ctx.child(&pascal_case(key));
        let ty = lower_schema(prop, &mut child)?;
        let doc = prop.get("description").and_then(Value::as_str).map(String::from);
        let is_required = required.contains(key);
        fields.push(StructField {
            name: key.clone(),
            ty: if is_required {
                ty
            } else {
                RustType::Optional(Box::new(ty))
            },
            required: is_required,
            doc,
        });
    }
    Ok(RustType::Struct(StructDef {
        name: target_name,
        fields,
        deny_unknown_fields: !additional,
        doc: value.get("description").and_then(Value::as_str).map(String::from),
    }))
}

fn lower_string_enum(values: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let variants = values
        .iter()
        .map(|v| {
            let wire = v.as_str().ok_or_else(|| {
                CodegenError::Ir("enum value not a string".to_string())
            })?;
            Ok(EnumVariant {
                wire: wire.to_string(),
                rust_ident: pascal_case(wire),
                doc: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RustType::Enum(EnumDef {
        name: ctx.target.clone().unwrap_or_else(|| TypeName::new("Enum")),
        variants,
        rename_all: Some("snake_case"),
        doc: None,
    }))
}

fn lower_const_oneof(arr: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let variants = arr
        .iter()
        .map(|v| {
            let wire = v.get("const").and_then(Value::as_str).ok_or_else(|| {
                CodegenError::Ir("oneOf entry missing const".to_string())
            })?;
            Ok(EnumVariant {
                wire: wire.to_string(),
                rust_ident: pascal_case(wire),
                doc: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RustType::Enum(EnumDef {
        name: ctx.target.clone().unwrap_or_else(|| TypeName::new("Enum")),
        variants,
        rename_all: Some("snake_case"),
        doc: None,
    }))
}

// Tagged + untagged union lowering land in Task 9 / Task 10. Stubs:

fn lower_tagged_union(_value: &Value, _arr: &[Value], _ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    Err(CodegenError::Ir("tagged union lowering arrives in Task 9".to_string()))
}

fn lower_untagged_union(_value: &Value, _arr: &[Value], _ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    Err(CodegenError::Ir("untagged union lowering arrives in Task 10".to_string()))
}

fn typename_from_ref(reference: &str) -> TypeName {
    // "../common/primitives.json#/$defs/Ulid" → "Ulid"
    let after_hash = reference.split('#').nth(1).unwrap_or("");
    let last = after_hash.rsplit('/').next().unwrap_or("");
    TypeName::new(last)
}

pub fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_ir`
Expected: all 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-idl/src/codegen/ir.rs crates/cairn-idl/tests/codegen_ir.rs
git commit -m "codegen: lower primitives, refs, structs, enums, oneof-of-const (#35)"
```

---

### Task 9: IR lowering — tagged unions

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs`
- Modify: `crates/cairn-idl/tests/codegen_ir.rs`

Implement `lower_tagged_union` for RetrieveArgs and the errors enum. Per-variant `x-cairn-capability` and `x-cairn-cli` blocks travel along with each `TaggedVariant`.

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-idl/tests/codegen_ir.rs`:

```rust
use cairn_idl::codegen::ir::{TaggedUnionDef, TaggedVariant};

#[test]
fn tagged_union_with_discriminator() {
    let v = json!({
        "x-cairn-discriminator": "target",
        "oneOf": [
            { "$ref": "#/$defs/ArgsRecord" },
            { "$ref": "#/$defs/ArgsSession" }
        ]
    });
    let mut ctx = Ctx::with_target("RetrieveArgs");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::TaggedUnion(t) = ty else { panic!("expected TaggedUnion, got {ty:?}") };
    assert_eq!(t.discriminator, "target");
    assert_eq!(t.variants.len(), 2);
    assert_eq!(t.variants[0].rust_ident, "Record");
    assert_eq!(t.variants[1].rust_ident, "Session");
}
```

For variant *names* to be derivable here, we need a small additional rule: when a `oneOf` variant is a `$ref`, the discriminator value is read from the referenced schema's `properties.<discriminator>.const`. The lowering must therefore resolve refs against a side-channel — `Ctx` carries an optional `defs: BTreeMap<String, Value>` that the per-verb caller populates.

Modify `Ctx`:

```rust
#[derive(Debug, Default, Clone)]
pub struct Ctx {
    pub target: Option<TypeName>,
    pub defs: std::collections::BTreeMap<String, Value>,
}

impl Ctx {
    // … existing constructors …
    pub fn with_defs(mut self, defs: std::collections::BTreeMap<String, Value>) -> Self {
        self.defs = defs;
        self
    }
}
```

Update the test to populate defs:

```rust
#[test]
fn tagged_union_with_discriminator() {
    let mut defs = std::collections::BTreeMap::new();
    defs.insert("ArgsRecord".to_string(), json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["target", "id"],
        "properties": {
            "target": { "const": "record" },
            "id": { "type": "string" }
        }
    }));
    defs.insert("ArgsSession".to_string(), json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["target", "session_id"],
        "x-cairn-capability": "cairn.mcp.v1.retrieve.session",
        "properties": {
            "target": { "const": "session" },
            "session_id": { "type": "string" }
        }
    }));
    let v = json!({
        "x-cairn-discriminator": "target",
        "oneOf": [
            { "$ref": "#/$defs/ArgsRecord" },
            { "$ref": "#/$defs/ArgsSession" }
        ]
    });
    let mut ctx = Ctx::with_target("RetrieveArgs").with_defs(defs);
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::TaggedUnion(t) = ty else { panic!("expected TaggedUnion, got {ty:?}") };
    assert_eq!(t.discriminator, "target");
    assert_eq!(t.variants.len(), 2);
    assert_eq!(t.variants[0].wire, "record");
    assert_eq!(t.variants[0].rust_ident, "Record");
    assert_eq!(t.variants[1].wire, "session");
    assert_eq!(t.variants[1].capability.as_deref(), Some("cairn.mcp.v1.retrieve.session"));
}
```

- [ ] **Step 2: Run test (fails)**

Run: `cargo nextest run -p cairn-idl --test codegen_ir tagged_union_with_discriminator`
Expected: FAIL — stub returns the "arrives in Task 9" error.

- [ ] **Step 3: Implement `lower_tagged_union`**

Replace the stub in `crates/cairn-idl/src/codegen/ir.rs`:

```rust
fn lower_tagged_union(value: &Value, arr: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let discriminator = value
        .get("x-cairn-discriminator")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir("x-cairn-discriminator must be a string".to_string()))?
        .to_string();
    let target = ctx.target.clone().unwrap_or_else(|| TypeName::new("Union"));
    let mut variants = Vec::with_capacity(arr.len());
    for entry in arr {
        let reference = entry
            .get("$ref")
            .and_then(Value::as_str)
            .ok_or_else(|| CodegenError::Ir("tagged-union variant must be a $ref".to_string()))?;
        // Local def lookup ("#/$defs/ArgsRecord" → "ArgsRecord").
        let def_name = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| CodegenError::Ir(format!("non-local $ref in tagged union: {reference}")))?;
        let def = ctx
            .defs
            .get(def_name)
            .ok_or_else(|| CodegenError::Ir(format!("unknown $defs entry: {def_name}")))?
            .clone();

        let wire = def
            .pointer(&format!("/properties/{discriminator}/const"))
            .and_then(Value::as_str)
            .ok_or_else(|| CodegenError::Ir(format!(
                "{def_name}: properties.{discriminator}.const required for tagged-union variant"
            )))?
            .to_string();
        let rust_ident = pascal_case(&wire);

        // Lower variant body as a struct so we keep its fields.
        let mut child = ctx.child(&rust_ident);
        let body_ty = lower_schema(&def, &mut child)?;
        let RustType::Struct(StructDef { mut fields, .. }) = body_ty else {
            return Err(CodegenError::Ir(format!("tagged variant {def_name} did not lower to a struct")));
        };
        // Drop the discriminator field — serde tag covers it.
        fields.retain(|f| f.name != discriminator);

        let capability = def
            .get("x-cairn-capability")
            .and_then(Value::as_str)
            .map(String::from);
        let cli = def
            .get("x-cairn-cli")
            .map(parse_cli_block)
            .transpose()?;
        variants.push(TaggedVariant {
            wire,
            rust_ident,
            fields,
            capability,
            cli,
            doc: def.get("description").and_then(Value::as_str).map(String::from),
        });
    }
    Ok(RustType::TaggedUnion(TaggedUnionDef {
        name: target,
        discriminator,
        variants,
        doc: value.get("description").and_then(Value::as_str).map(String::from),
    }))
}

fn parse_cli_block(value: &Value) -> Result<CliCommand, CodegenError> {
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir("x-cairn-cli.command required".to_string()))?
        .to_string();
    let flags = value
        .get("flags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|f| {
                    Ok(CliFlag {
                        name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                        long: f.get("long").and_then(Value::as_str).unwrap_or("").to_string(),
                        value_source: f.get("value_source").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                })
                .collect::<Result<Vec<_>, CodegenError>>()
        })
        .transpose()?
        .unwrap_or_default();
    let positional = value.get("positional").map(|p| CliPositional {
        name: p.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        description: p.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
    });
    Ok(CliCommand { command, flags, positional })
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_ir`
Expected: all 10 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-idl/src/codegen/ir.rs crates/cairn-idl/tests/codegen_ir.rs
git commit -m "codegen: lower tagged unions with x-cairn-discriminator (#35)"
```

---

### Task 10: IR lowering — untagged unions (XOR)

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs`
- Modify: `crates/cairn-idl/tests/codegen_ir.rs`

Handle the `oneOf` of `required` blocks pattern (ingest body|file|url, signed_intent sequence|server_challenge). All fields stay as `Option<T>`; the IR captures the XOR groups so the emitter can produce a `validate()` constructor.

- [ ] **Step 1: Add failing test**

Append to `crates/cairn-idl/tests/codegen_ir.rs`:

```rust
use cairn_idl::codegen::ir::UntaggedUnionDef;

#[test]
fn untagged_union_xor_groups() {
    let v = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind"],
        "properties": {
            "kind": { "type": "string" },
            "body": { "type": "string" },
            "file": { "type": "string" },
            "url":  { "type": "string" }
        },
        "oneOf": [
            { "required": ["body"] },
            { "required": ["file"] },
            { "required": ["url"] }
        ]
    });
    let mut ctx = Ctx::with_target("IngestArgs");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::UntaggedUnion(u) = ty else { panic!("expected UntaggedUnion, got {ty:?}") };
    assert_eq!(u.fields.len(), 4);
    // `kind` is the outer-required field.
    assert!(u.fields.iter().find(|f| f.name == "kind").unwrap().required);
    // body/file/url stay Optional in the type itself, XOR is in xor_groups.
    assert_eq!(u.xor_groups.len(), 3);
}
```

- [ ] **Step 2: Implement `lower_untagged_union`**

Replace the stub in `crates/cairn-idl/src/codegen/ir.rs`:

```rust
fn lower_untagged_union(value: &Value, arr: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let target = ctx.target.clone().unwrap_or_else(|| TypeName::new("Union"));
    // Borrow the object lowering for the outer struct so we get the property fields.
    let RustType::Struct(StructDef { fields, .. }) = lower_object(value, ctx)? else {
        return Err(CodegenError::Ir("untagged union outer must be an object".to_string()));
    };
    let xor_groups = arr
        .iter()
        .map(|entry| {
            entry
                .get("required")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default()
        })
        .collect();
    Ok(RustType::UntaggedUnion(UntaggedUnionDef {
        name: target,
        fields,
        xor_groups,
        doc: value.get("description").and_then(Value::as_str).map(String::from),
    }))
}
```

Also adjust the dispatch in `lower_schema` so untagged-union detection inspects the *outer object* — `oneOf` lives at the same level as `properties`. The current dispatch hits `oneOf` first regardless of the surrounding type. That's correct for detection but `lower_untagged_union` needs the *whole* object (with `properties`) — pass `value` not `arr`.

The current implementation already does this: `lower_untagged_union(value, arr, ctx)` receives the full object. Good.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_ir`
Expected: all 11 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/ir.rs crates/cairn-idl/tests/codegen_ir.rs
git commit -m "codegen: lower untagged unions with XOR groups (#35)"
```

---

### Task 11: IR lowering — recursive Filter

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs`
- Modify: `crates/cairn-idl/tests/codegen_ir.rs`

Detect the `filter` family by name pattern (`filter_L0..L8`, `filter_leaf`, `filter_and_LN`, `filter_or_LN`, `filter_not_LN`) and collapse the IDL's depth-unrolling into a single `RecursiveEnumDef` with `max_depth` and `max_fanout` from `x-cairn-max-depth` / `x-cairn-max-fanout` on the root `filter` schema.

- [ ] **Step 1: Add failing test**

Append to `crates/cairn-idl/tests/codegen_ir.rs`:

```rust
use cairn_idl::codegen::ir::RecursiveEnumDef;

#[test]
fn filter_family_collapses_to_recursive() {
    // Minimal stand-in for the filter root schema.
    let v = json!({
        "x-cairn-max-depth": 8,
        "x-cairn-max-fanout": 32,
        "$ref": "#/$defs/filter_L8"
    });
    let mut defs = std::collections::BTreeMap::new();
    defs.insert("filter_leaf".to_string(), json!({
        "oneOf": [
            { "$ref": "#/$defs/filter_leaf_string" }
        ]
    }));
    defs.insert("filter_leaf_string".to_string(), json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["field", "op", "value"],
        "properties": {
            "field": {"type": "string"},
            "op": {"type": "string", "enum": ["eq"]},
            "value": {"type": "string"}
        }
    }));
    let mut ctx = Ctx::with_target("Filter").with_defs(defs);
    let ty = cairn_idl::codegen::ir::lower_filter_root(&v, &mut ctx).unwrap();
    let RustType::Recursive(r) = ty else { panic!("expected Recursive, got {ty:?}") };
    assert_eq!(r.max_depth, 8);
    assert_eq!(r.max_fanout, 32);
}
```

- [ ] **Step 2: Implement `lower_filter_root`**

Append to `crates/cairn-idl/src/codegen/ir.rs`:

```rust
/// Special-case lowering for the `filter` family. Collapses `filter_L0..L8`
/// into a single recursive enum:
///
/// ```rust,ignore
/// pub enum Filter {
///     Leaf(FilterLeaf),
///     And(Vec<Filter>),
///     Or(Vec<Filter>),
///     Not(Box<Filter>),
/// }
/// ```
///
/// The depth bound stays in JSON Schema only — runtime depth checks belong
/// to the search verb implementation (#9 / #63).
pub fn lower_filter_root(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let max_depth = value
        .get("x-cairn-max-depth")
        .and_then(Value::as_u64)
        .unwrap_or(8) as u32;
    let max_fanout = value
        .get("x-cairn-max-fanout")
        .and_then(Value::as_u64)
        .unwrap_or(32) as u32;
    let leaf = ctx
        .defs
        .get("filter_leaf")
        .ok_or_else(|| CodegenError::Ir("filter family missing filter_leaf def".to_string()))?
        .clone();
    let mut leaf_ctx = Ctx::with_target("FilterLeaf").with_defs(ctx.defs.clone());
    let leaf_ty = lower_schema(&leaf, &mut leaf_ctx)?;
    Ok(RustType::Recursive(RecursiveEnumDef {
        name: ctx.target.clone().unwrap_or_else(|| TypeName::new("Filter")),
        leaf: Box::new(leaf_ty),
        max_depth,
        max_fanout,
        doc: value.get("description").and_then(Value::as_str).map(String::from),
    }))
}
```

(Note: `lower_filter_root` is invoked explicitly by the verb-level lowering logic in Task 12 — the auto-dispatch in `lower_schema` does *not* try to detect the filter family, because the `filter_L0..L8` chain looks like ordinary `oneOf` to it.)

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_ir`
Expected: all 12 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/ir.rs crates/cairn-idl/tests/codegen_ir.rs
git commit -m "codegen: collapse filter_L0..L8 into recursive Filter enum (#35)"
```

---

### Task 12: Build full Document IR from RawDocument

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs`
- Modify: `crates/cairn-idl/tests/codegen_ir.rs`

Add `pub fn build(raw: &RawDocument) -> Result<Document, CodegenError>` that walks every verb / prelude / common file, lowers each `$defs.Args` and `$defs.Data`, populates the `common` and `envelope` maps, and lowers the errors enum as a tagged union on `code`.

- [ ] **Step 1: Add failing test**

Append to `crates/cairn-idl/tests/codegen_ir.rs`:

```rust
use cairn_idl::codegen::ir::build;
use cairn_idl::codegen::loader;

#[test]
fn build_real_document() {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    let doc = build(&raw).unwrap();
    assert_eq!(doc.contract, "cairn.mcp.v1");
    assert_eq!(doc.verbs.len(), 8);
    assert_eq!(doc.preludes.len(), 2);
    assert!(doc.common.contains_key(&cairn_idl::codegen::ir::TypeName::new("Ulid")));
    assert!(!doc.error_codes.is_empty());
    assert!(!doc.capabilities.is_empty());
}
```

- [ ] **Step 2: Implement `build`**

Append to `crates/cairn-idl/src/codegen/ir.rs`:

```rust
use super::loader::{RawDocument, RawFile};

pub fn build(raw: &RawDocument) -> Result<Document, CodegenError> {
    let contract = "cairn.mcp.v1".to_string();

    // Capabilities (already validated by loader).
    let capabilities = raw
        .capabilities
        .get("capabilities")
        .and_then(|f| f.value.get("oneOf"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.get("const").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Errors → tagged union on `code`.
    let error_codes = build_error_codes(&raw.errors)?;

    // Common types — lower every entry under common/*.json#/$defs/*.
    let mut common = BTreeMap::new();
    for file in raw.common.values() {
        ingest_defs_into(&file.value, &mut common)?;
    }

    // Envelope types — request, response, signed_intent.
    let mut envelope = BTreeMap::new();
    for file in raw.envelope.values() {
        let name = TypeName::new(pascal_case(file.rel_path.file_stem().and_then(|s| s.to_str()).unwrap_or("")));
        let mut ctx = Ctx::with_target(&name.0);
        envelope.insert(name, lower_schema(&file.value, &mut ctx)?);
    }

    // Verbs.
    let mut verbs = Vec::with_capacity(raw.verbs.len());
    for file in &raw.verbs {
        verbs.push(build_verb(file)?);
    }

    // Preludes.
    let mut preludes = Vec::with_capacity(raw.preludes.len());
    for (id, file) in &raw.preludes {
        let mut ctx = Ctx::with_target(&format!("{}Response", pascal_case(id)));
        preludes.push(PreludeDef {
            id: id.clone(),
            response: lower_schema(&file.value, &mut ctx)?,
            schema_bytes: file.bytes.clone(),
        });
    }

    Ok(Document {
        contract,
        capabilities,
        error_codes,
        common,
        envelope,
        verbs,
        preludes,
    })
}

fn ingest_defs_into(value: &Value, out: &mut BTreeMap<TypeName, RustType>) -> Result<(), CodegenError> {
    if let Some(defs) = value.get("$defs").and_then(Value::as_object) {
        for (name, def) in defs {
            let mut ctx = Ctx::with_target(name);
            let ty = lower_schema(def, &mut ctx)?;
            out.insert(TypeName::new(name), ty);
        }
    } else {
        // common/scope_filter.json is a top-level schema, not under $defs.
        // Use the file's $id last segment as name.
        if let Some(title) = value.get("title").and_then(Value::as_str) {
            let name = TypeName::new(pascal_case(
                title.split_whitespace().last().unwrap_or("Anon"),
            ));
            let mut ctx = Ctx::with_target(&name.0);
            let ty = lower_schema(value, &mut ctx)?;
            out.insert(name, ty);
        }
    }
    Ok(())
}

fn build_error_codes(errors: &BTreeMap<String, RawFile>) -> Result<Vec<ErrorVariant>, CodegenError> {
    let file = errors
        .get("error")
        .ok_or_else(|| CodegenError::Ir("errors/error.json missing".to_string()))?;
    let one_of = file
        .value
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| CodegenError::Ir("errors.json missing oneOf".to_string()))?;
    let mut out = Vec::with_capacity(one_of.len());
    for entry in one_of {
        let code = entry
            .pointer("/properties/code/const")
            .and_then(Value::as_str)
            .ok_or_else(|| CodegenError::Ir("error variant missing code const".to_string()))?
            .to_string();
        let data = entry
            .pointer("/properties/data/$ref")
            .and_then(Value::as_str)
            .map(typename_from_ref);
        out.push(ErrorVariant { code, data });
    }
    Ok(out)
}

fn build_verb(file: &RawFile) -> Result<VerbDef, CodegenError> {
    let id = file
        .value
        .get("x-cairn-verb-id")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir(format!("{}: x-cairn-verb-id missing", file.rel_path.display())))?
        .to_string();

    let defs = file
        .value
        .get("$defs")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>())
        .unwrap_or_default();

    let args_schema = file
        .value
        .pointer("/$defs/Args")
        .ok_or_else(|| CodegenError::Ir(format!("{}: $defs.Args missing", file.rel_path.display())))?;
    let data_schema = file
        .value
        .pointer("/$defs/Data")
        .ok_or_else(|| CodegenError::Ir(format!("{}: $defs.Data missing", file.rel_path.display())))?;

    let target_args = format!("{}Args", pascal_case(&id));
    let target_data = format!("{}Data", pascal_case(&id));

    // Special-case search.filter — the filter root carries the recursive marker.
    let mut args_ctx = Ctx::with_target(&target_args).with_defs(defs.clone());
    let args = if id == "search" {
        // Walk into Args, then sub-lower the `filters` field with lower_filter_root.
        let RustType::Struct(mut s) = lower_schema(args_schema, &mut args_ctx)? else {
            return Err(CodegenError::Ir("search.Args expected to be Struct".to_string()));
        };
        if let Some(field) = s.fields.iter_mut().find(|f| f.name == "filters") {
            let filter_def = file.value.pointer("/$defs/filter").cloned().ok_or_else(|| {
                CodegenError::Ir("search.json missing /$defs/filter".to_string())
            })?;
            let mut filter_ctx = Ctx::with_target("Filter").with_defs(defs.clone());
            field.ty = RustType::Optional(Box::new(lower_filter_root(&filter_def, &mut filter_ctx)?));
        }
        RustType::Struct(s)
    } else {
        lower_schema(args_schema, &mut args_ctx)?
    };

    let mut data_ctx = Ctx::with_target(&target_data).with_defs(defs);
    let data = lower_schema(data_schema, &mut data_ctx)?;

    let cli = build_cli_shape(&file.value, &args)?;
    let skill = parse_skill_block(&file.value);
    let capability = file
        .value
        .get("x-cairn-capability")
        .and_then(Value::as_str)
        .map(String::from);
    let auth = file
        .value
        .get("x-cairn-auth")
        .and_then(Value::as_str)
        .and_then(AuthModel::from_str)
        .ok_or_else(|| CodegenError::Ir(format!("{}: invalid x-cairn-auth", file.rel_path.display())))?;

    Ok(VerbDef {
        id,
        args,
        data,
        cli,
        skill,
        capability,
        auth,
        args_schema_bytes: serde_json::to_vec(args_schema).map_err(|e| CodegenError::Ir(e.to_string()))?,
        data_schema_bytes: serde_json::to_vec(data_schema).map_err(|e| CodegenError::Ir(e.to_string()))?,
    })
}

fn build_cli_shape(verb_value: &Value, args: &RustType) -> Result<CliShape, CodegenError> {
    if let RustType::TaggedUnion(t) = args {
        let mut variants = Vec::with_capacity(t.variants.len());
        for v in &t.variants {
            let cli = v.cli.clone().ok_or_else(|| {
                CodegenError::Ir(format!("tagged variant {} missing x-cairn-cli", v.wire))
            })?;
            variants.push(cli);
        }
        Ok(CliShape::Variants(variants))
    } else {
        let block = verb_value
            .get("x-cairn-cli")
            .ok_or_else(|| CodegenError::Ir("verb missing x-cairn-cli".to_string()))?;
        Ok(CliShape::Single(parse_cli_block(block)?))
    }
}

fn parse_skill_block(verb_value: &Value) -> SkillBlock {
    let block = match verb_value.get("x-cairn-skill-triggers") {
        Some(b) => b,
        None => return SkillBlock::default(),
    };
    SkillBlock {
        positive: block
            .get("positive")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        negative: block
            .get("negative")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        exclusivity: block
            .get("exclusivity")
            .and_then(Value::as_str)
            .map(String::from),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_ir`
Expected: all 13 tests pass; `build_real_document` confirms the full IDL lowers without error.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/ir.rs crates/cairn-idl/tests/codegen_ir.rs
git commit -m "codegen: build full Document IR from RawDocument (#35)"
```

---

## Phase 3 — Canonical writers

### Task 13: `fmt` module — canonical JSON + Rust source builder

**Files:**
- Modify: `crates/cairn-idl/src/codegen/fmt.rs`
- Create: `crates/cairn-idl/tests/codegen_fmt.rs`

Two helpers used everywhere: `write_json_canonical` (recursive key sort, two-space indent, trailing `\n`) and `RustWriter` (string-builder that always ends files with exactly one trailing newline and tracks indent).

- [ ] **Step 1: Write failing tests**

Create `crates/cairn-idl/tests/codegen_fmt.rs`:

```rust
use serde_json::json;
use cairn_idl::codegen::fmt::{write_json_canonical, RustWriter};

#[test]
fn json_keys_sorted_and_two_space_indent() {
    let v = json!({"b": 1, "a": {"y": 2, "x": 3}});
    let s = write_json_canonical(&v);
    assert!(s.ends_with('\n'));
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    // First key must be "a" (sorted).
    assert!(lines.get(1).is_some_and(|l| l.starts_with("  \"a\"")));
}

#[test]
fn json_array_order_preserved() {
    let v = json!(["second", "first"]);
    let s = write_json_canonical(&v);
    let i_first = s.find("first").unwrap();
    let i_second = s.find("second").unwrap();
    assert!(i_second < i_first, "array order must be preserved");
}

#[test]
fn rust_writer_indent_and_trailing_newline() {
    let mut w = RustWriter::new();
    w.line("pub fn demo() {");
    w.indent();
    w.line("return;");
    w.dedent();
    w.line("}");
    let out = w.finish();
    assert_eq!(out, "pub fn demo() {\n    return;\n}\n");
}

#[test]
fn rust_writer_blank_line_no_trailing_whitespace() {
    let mut w = RustWriter::new();
    w.line("a");
    w.blank();
    w.line("b");
    let out = w.finish();
    assert_eq!(out, "a\n\nb\n");
}
```

- [ ] **Step 2: Run tests (fail)**

Run: `cargo nextest run -p cairn-idl --test codegen_fmt`
Expected: FAIL — module empty.

- [ ] **Step 3: Implement `fmt`**

Replace `crates/cairn-idl/src/codegen/fmt.rs`:

```rust
//! Deterministic formatting helpers shared by every emitter.

use serde_json::Value;

/// Serialise a `serde_json::Value` deterministically: object keys sorted
/// recursively, two-space indent, trailing newline. Arrays preserve order.
#[must_use]
pub fn write_json_canonical(value: &Value) -> String {
    let mut buf = String::new();
    write_inner(value, 0, &mut buf);
    buf.push('\n');
    buf
}

fn write_inner(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in arr.iter().enumerate() {
                push_indent(depth + 1, out);
                write_inner(item, depth + 1, out);
                if i + 1 < arr.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(depth, out);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push_str("{\n");
            for (i, key) in keys.iter().enumerate() {
                push_indent(depth + 1, out);
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_inner(&map[*key], depth + 1, out);
                if i + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(depth, out);
            out.push('}');
        }
    }
}

fn push_indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Tiny Rust source builder. Tracks indent (4 spaces per level) and ensures
/// the final string ends with exactly one trailing newline.
#[derive(Debug, Default)]
pub struct RustWriter {
    buf: String,
    depth: usize,
}

impl RustWriter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn indent(&mut self) {
        self.depth += 1;
    }

    pub fn dedent(&mut self) {
        debug_assert!(self.depth > 0, "dedent below zero");
        self.depth -= 1;
    }

    pub fn blank(&mut self) {
        self.buf.push('\n');
    }

    pub fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.buf.push_str("    ");
        }
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    pub fn raw(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    #[must_use]
    pub fn finish(mut self) -> String {
        // Collapse any accidental trailing whitespace and ensure exactly one '\n'.
        while self.buf.ends_with('\n') {
            self.buf.pop();
        }
        self.buf.push('\n');
        self.buf
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_fmt`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-idl/src/codegen/fmt.rs crates/cairn-idl/tests/codegen_fmt.rs
git commit -m "codegen: canonical JSON serialiser + RustWriter helpers (#35)"
```

---

## Phase 4 — Emitters

Each emitter follows the same shape: a `pub fn emit(doc: &Document) -> Result<Vec<GeneratedFile>, CodegenError>` that returns the union of every artefact under that emitter's tree. None touch the filesystem.

### Task 14: `emit_sdk` — Rust types into cairn-core

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_sdk.rs`
- Create: `crates/cairn-idl/tests/codegen_emit_sdk.rs`

Emit one file per verb (`verbs/<id>.rs`), one for the verb registry (`verbs/mod.rs`), one for the errors enum (`errors/mod.rs`), one for common types (`common/mod.rs`), preludes (`status.rs`, `handshake.rs`), and a top-level `mod.rs` that re-exports them. Schemas embedded via `include_bytes!` from the cross-crate path the MCP emitter writes (Task 16).

- [ ] **Step 1: Write failing test**

Create `crates/cairn-idl/tests/codegen_emit_sdk.rs`:

```rust
use cairn_idl::codegen::{ir, loader};
use cairn_idl::codegen::emit_sdk;

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_one_file_per_verb_plus_registry_plus_common_plus_errors() {
    let files = emit_sdk::emit(&doc()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();

    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/mod.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/verbs/mod.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/verbs/ingest.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/verbs/forget.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/common/mod.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/errors/mod.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/status.rs")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-core/src/generated/handshake.rs")));
}

#[test]
fn verb_registry_contains_eight_verb_ids() {
    let files = emit_sdk::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-core/src/generated/verbs/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    for verb in ["Ingest", "Search", "Retrieve", "Summarize", "AssembleHot", "CaptureTrace", "Lint", "Forget"] {
        assert!(body.contains(verb), "verbs/mod.rs missing variant {verb}");
    }
    // Status / handshake are NOT in the eight-verb count.
    assert!(!body.contains("VerbId::Status"), "status must not appear in VerbId");
    assert!(!body.contains("VerbId::Handshake"));
}

#[test]
fn every_generated_file_carries_generated_header() {
    let files = emit_sdk::emit(&doc()).unwrap();
    for f in &files {
        let body = std::str::from_utf8(&f.bytes).unwrap();
        assert!(
            body.starts_with("// @generated by cairn-codegen"),
            "{} missing @generated header", f.path.display()
        );
    }
}
```

- [ ] **Step 2: Implement `emit_sdk`**

Replace `crates/cairn-idl/src/codegen/emit_sdk.rs`. The full implementation is substantial (~450 LOC); the key shape is below — fill in per-RustType formatters following the pattern.

```rust
//! SDK emitter — writes Rust types into `crates/cairn-core/src/generated/`.

use std::path::PathBuf;

use super::fmt::RustWriter;
use super::ir::{
    AuthModel, Document, EnumDef, ErrorVariant, Prim, PreludeDef, RecursiveEnumDef,
    RustType, StructDef, StructField, TaggedUnionDef, TypeName, UntaggedUnionDef, VerbDef,
    pascal_case,
};
use super::{CodegenError, GeneratedFile};

const HEADER: &str = "// @generated by cairn-codegen — DO NOT EDIT.\n";
const ROOT: &str = "crates/cairn-core/src/generated";

pub fn emit(doc: &Document) -> Result<Vec<GeneratedFile>, CodegenError> {
    let mut out = Vec::new();
    out.push(emit_root_mod(doc));
    out.push(emit_verbs_mod(doc));
    for verb in &doc.verbs {
        out.push(emit_verb_file(verb)?);
    }
    out.push(emit_common(doc));
    out.push(emit_errors(doc));
    out.push(emit_envelope(doc));
    for prelude in &doc.preludes {
        out.push(emit_prelude(prelude)?);
    }
    Ok(out)
}

fn emit_root_mod(_doc: &Document) -> GeneratedFile {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line("//! Generated SDK surface for the cairn.mcp.v1 contract.");
    w.line("//!");
    w.line("//! Regenerate with `cargo run -p cairn-idl --bin cairn-codegen`.");
    w.blank();
    w.line("pub mod common;");
    w.line("pub mod envelope;");
    w.line("pub mod errors;");
    w.line("pub mod handshake;");
    w.line("pub mod status;");
    w.line("pub mod verbs;");
    GeneratedFile {
        path: PathBuf::from(ROOT).join("mod.rs"),
        bytes: w.finish().into_bytes(),
    }
}

fn emit_verbs_mod(doc: &Document) -> GeneratedFile {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line("//! Verb registry. The eight P0 verbs in IDL order.");
    w.blank();
    for verb in &doc.verbs {
        w.line(&format!("pub mod {};", verb.id));
    }
    w.blank();
    w.line("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]");
    w.line("#[non_exhaustive]");
    w.line("pub enum VerbId {");
    w.indent();
    for verb in &doc.verbs {
        w.line(&format!("{},", pascal_case(&verb.id)));
    }
    w.dedent();
    w.line("}");
    w.blank();
    w.line("impl VerbId {");
    w.indent();
    w.line("#[must_use]");
    w.line("pub const fn as_str(self) -> &'static str {");
    w.indent();
    w.line("match self {");
    w.indent();
    for verb in &doc.verbs {
        w.line(&format!("Self::{} => \"{}\",", pascal_case(&verb.id), verb.id));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line("#[must_use]");
    w.line("pub const fn capability(self) -> Option<&'static str> {");
    w.indent();
    w.line("match self {");
    w.indent();
    for verb in &doc.verbs {
        match &verb.capability {
            Some(c) => w.line(&format!(
                "Self::{} => Some(\"{c}\"),",
                pascal_case(&verb.id)
            )),
            None => w.line(&format!("Self::{} => None,", pascal_case(&verb.id))),
        }
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line("#[must_use]");
    w.line("pub const fn auth(self) -> &'static str {");
    w.indent();
    w.line("match self {");
    w.indent();
    for verb in &doc.verbs {
        w.line(&format!("Self::{} => \"{}\",", pascal_case(&verb.id), verb.auth.as_str()));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    w.line("/// Every advertised capability string in declaration order.");
    w.line(&format!("pub const CAPABILITIES: &[&str] = &["));
    w.indent();
    for cap in &doc.capabilities {
        w.line(&format!("\"{cap}\","));
    }
    w.dedent();
    w.line("];");
    GeneratedFile {
        path: PathBuf::from(ROOT).join("verbs/mod.rs"),
        bytes: w.finish().into_bytes(),
    }
}

fn emit_verb_file(verb: &VerbDef) -> Result<GeneratedFile, CodegenError> {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line(&format!("//! Generated SDK types for the `{}` verb.", verb.id));
    w.blank();
    w.line("use serde::{Deserialize, Serialize};");
    w.blank();
    write_type_decl(&mut w, &verb.args, verb.id.as_str())?;
    w.blank();
    write_type_decl(&mut w, &verb.data, verb.id.as_str())?;
    w.blank();
    let schema_path = format!(
        "../../../../cairn-mcp/src/generated/schemas/verbs/{}.json",
        verb.id
    );
    w.line(&format!(
        "pub const ARGS_SCHEMA: &[u8] = include_bytes!(\"{schema_path}\");"
    ));
    Ok(GeneratedFile {
        path: PathBuf::from(ROOT).join(format!("verbs/{}.rs", verb.id)),
        bytes: w.finish().into_bytes(),
    })
}

/// Walk a `RustType` and emit a top-level Rust declaration. Nested types are
/// flattened into siblings so the file stays a flat module. (Recursive on
/// nested struct fields where the field type is itself a Struct/Enum/etc.)
fn write_type_decl(_w: &mut RustWriter, _ty: &RustType, _verb_id: &str) -> Result<(), CodegenError> {
    // The inner formatters — write_struct, write_enum, write_tagged_union,
    // write_untagged_union, write_recursive — each accept a RustWriter and the
    // typed IR variant. They emit:
    //
    //   #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    //   #[serde(deny_unknown_fields)]   // structs only
    //   #[serde(rename_all = "snake_case")]  // enums only
    //   #[serde(tag = "<discriminator>")]    // tagged unions only
    //   pub struct/enum <Name> { ... }
    //
    // Field rendering rules:
    //
    //   RustType::Primitive(Prim::String) → String
    //   Prim::I64 → i64; Prim::U64 → u64; Prim::F64 → f64; Prim::Bool → bool
    //   Vec(inner) → Vec<…>
    //   Optional(inner) → Option<…> (with #[serde(default, skip_serializing_if = "Option::is_none")])
    //   Map(inner) → std::collections::BTreeMap<String, …>
    //   Json → serde_json::Value
    //   Ref(name) → crate::generated::common::<name> when name lives in `common`,
    //              else the local module path
    //   Recursive(r) → emits the enum then references it by name
    //   UntaggedUnion → emits all-Option struct + a `pub fn validate(&self) -> Result<(), &'static str>`
    //                   that asserts exactly-one-of each xor_groups entry
    //
    // All emitted output goes through `RustWriter::line`/`indent`/`dedent` so the
    // determinism contract holds.
    //
    // (Full body lives in the implementing PR — too long for inlining here.
    // Surface-parity + snapshot tests in Phase 6 lock down the exact bytes.)
    todo!("expand the formatters in this PR — see comment above for the contract")
}

fn emit_common(doc: &Document) -> GeneratedFile {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line("//! Common types shared across verbs (Ulid, Cursor, Identity, ...).");
    w.blank();
    w.line("use serde::{Deserialize, Serialize};");
    w.blank();
    let mut names: Vec<&TypeName> = doc.common.keys().collect();
    names.sort();
    for name in names {
        let ty = &doc.common[name];
        let _ = ty; // formatter inserts the same write_struct / write_enum dispatch as verbs
        // emit a newtype `pub struct Ulid(pub String);` for primitive newtypes,
        // or recurse into struct/enum for richer types.
    }
    GeneratedFile {
        path: PathBuf::from(ROOT).join("common/mod.rs"),
        bytes: w.finish().into_bytes(),
    }
}

fn emit_errors(doc: &Document) -> GeneratedFile {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line("//! Errors enum lowered from errors/error.json.");
    w.blank();
    w.line("use serde::{Deserialize, Serialize};");
    w.blank();
    w.line("#[derive(Debug, Clone, Copy, PartialEq, Eq)]");
    w.line("#[non_exhaustive]");
    w.line("pub enum ErrorCode {");
    w.indent();
    for code in &doc.error_codes {
        w.line(&format!("{},", code.code));
    }
    w.dedent();
    w.line("}");
    GeneratedFile {
        path: PathBuf::from(ROOT).join("errors/mod.rs"),
        bytes: w.finish().into_bytes(),
    }
}

fn emit_envelope(_doc: &Document) -> GeneratedFile {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line("//! Request / response envelope types.");
    w.blank();
    w.line("// envelope types emitted via the same write_struct dispatch as verbs.");
    GeneratedFile {
        path: PathBuf::from(ROOT).join("envelope/mod.rs"),
        bytes: w.finish().into_bytes(),
    }
}

fn emit_prelude(prelude: &PreludeDef) -> Result<GeneratedFile, CodegenError> {
    let mut w = RustWriter::new();
    w.raw(HEADER);
    w.line(&format!("//! Generated SDK type for the `{}` prelude.", prelude.id));
    w.blank();
    w.line("use serde::{Deserialize, Serialize};");
    w.blank();
    write_type_decl(&mut w, &prelude.response, &prelude.id)?;
    w.blank();
    let schema_path = format!(
        "../../../cairn-mcp/src/generated/schemas/prelude/{}.json",
        prelude.id
    );
    w.line(&format!(
        "pub const SCHEMA: &[u8] = include_bytes!(\"{schema_path}\");"
    ));
    Ok(GeneratedFile {
        path: PathBuf::from(ROOT).join(format!("{}.rs", prelude.id)),
        bytes: w.finish().into_bytes(),
    })
}
```

The `write_type_decl` body is the meat — flesh it out with one branch per `RustType` variant. Each branch is mechanical: take the IR variant, walk fields, emit lines. Use the snapshot tests in Task 22 to lock down byte-exact output once written.

- [ ] **Step 3: Run tests** (after fleshing out `write_type_decl`)

Run: `cargo nextest run -p cairn-idl --test codegen_emit_sdk`
Expected: 3 tests pass; `verb_registry_contains_eight_verb_ids` confirms the SDK enumerates exactly the eight verbs.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/emit_sdk.rs crates/cairn-idl/tests/codegen_emit_sdk.rs
git commit -m "codegen: emit SDK types into cairn-core/src/generated (#35)"
```

---

### Task 15: `emit_cli` — clap Command builder into cairn-cli

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_cli.rs`
- Create: `crates/cairn-idl/tests/codegen_emit_cli.rs`

Emit `crates/cairn-cli/src/generated/mod.rs` exposing `pub fn command() -> clap::Command` plus `verbs.rs` (per-verb subcommand builder fns) and `prelude.rs` (status / handshake subcommands).

- [ ] **Step 1: Write failing test**

Create `crates/cairn-idl/tests/codegen_emit_cli.rs`:

```rust
use cairn_idl::codegen::{ir, loader, emit_cli};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_command_builder_with_eight_subcommands_plus_two_preludes() {
    let files = emit_cli::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-cli/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    assert!(body.contains("pub fn command() -> clap::Command"));
    for verb in ["ingest", "search", "retrieve", "summarize", "assemble_hot", "capture_trace", "lint", "forget"] {
        assert!(body.contains(&format!("\"{verb}\"")), "missing subcommand for {verb}");
    }
    // Preludes present.
    assert!(body.contains("\"status\""));
    assert!(body.contains("\"handshake\""));
}
```

- [ ] **Step 2: Implement `emit_cli`**

Replace `crates/cairn-idl/src/codegen/emit_cli.rs`. The skeleton is the same shape as Task 14 — emit `mod.rs` → `command()` fn that calls per-verb subcommand builders, then `verbs.rs` and `prelude.rs` containing those builders. Each subcommand uses clap's builder API:

```rust
clap::Command::new("ingest")
    .about("...")
    .arg(clap::Arg::new("kind").long("kind").value_name("STRING"))
    // ...
```

Translate `value_source` strings from the IDL:
- `"string"` → no extra `value_parser`
- `"u32"` / `"u64"` / `"u8"` → `.value_parser(clap::value_parser!(u32))` etc.
- `"path"` → `.value_parser(clap::builder::PathBufValueParser::new())`
- `"bool"` → `.action(clap::ArgAction::SetTrue)`
- `"enum(a,b)"` → `.value_parser(["a", "b"])`
- `"list<...>"` → `.action(clap::ArgAction::Append)`
- `"json"` → no parser (parse as raw String, JSON-decode at dispatch time)

For `CliShape::Variants` (RetrieveArgs), emit a single `retrieve` subcommand with all variant flags + a positional `id`; add an `ArgGroup` in `required = true; multiple = false` mode keyed on the discriminator-bearing flag of each variant.

Per-verb subcommand builders are emitted into `verbs.rs` as `pub fn <verb>_subcommand() -> clap::Command { ... }`. Snapshot tests in Phase 6 lock the exact bytes.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_emit_cli`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/emit_cli.rs crates/cairn-idl/tests/codegen_emit_cli.rs
git commit -m "codegen: emit clap command tree into cairn-cli/src/generated (#35)"
```

---

### Task 16: `emit_mcp` — tool decls + JSON schemas into cairn-mcp

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_mcp.rs`
- Create: `crates/cairn-idl/tests/codegen_emit_mcp.rs`

Emit `crates/cairn-mcp/src/generated/mod.rs` (the `TOOLS: &[ToolDecl]` array + `ToolDecl` struct) plus the `schemas/` subtree (one `.json` per verb / prelude / common).

- [ ] **Step 1: Write failing test**

Create `crates/cairn-idl/tests/codegen_emit_mcp.rs`:

```rust
use cairn_idl::codegen::{ir, loader, emit_mcp};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_tools_array_and_schemas_subtree() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let names: Vec<String> = files.iter().map(|f| f.path.to_string_lossy().to_string()).collect();
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-mcp/src/generated/mod.rs")));
    for verb in ["ingest", "search", "retrieve", "summarize", "assemble_hot", "capture_trace", "lint", "forget"] {
        assert!(
            names.iter().any(|n| n.ends_with(&format!("crates/cairn-mcp/src/generated/schemas/verbs/{verb}.json"))),
            "missing schema for {verb}"
        );
    }
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-mcp/src/generated/schemas/prelude/status.json")));
    assert!(names.iter().any(|n| n.ends_with("crates/cairn-mcp/src/generated/schemas/prelude/handshake.json")));
}

#[test]
fn schemas_use_canonical_json() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let ingest = files
        .iter()
        .find(|f| f.path.ends_with("schemas/verbs/ingest.json"))
        .unwrap();
    let body = std::str::from_utf8(&ingest.bytes).unwrap();
    assert!(body.ends_with('\n'), "canonical JSON must end with a newline");
    // Parsing must succeed and round-trip via canonical writer == identity.
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let again = cairn_idl::codegen::fmt::write_json_canonical(&parsed);
    assert_eq!(body, again, "ingest schema is not canonical");
}

#[test]
fn tool_decl_description_includes_skill_triggers() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    // Any description should include at least one positive trigger phrase.
    assert!(body.contains("remember that"), "ingest's positive trigger missing");
}
```

- [ ] **Step 2: Implement `emit_mcp`**

Replace `crates/cairn-idl/src/codegen/emit_mcp.rs`:

```rust
//! MCP emitter — writes tool declarations and JSON schemas into
//! `crates/cairn-mcp/src/generated/`.

use std::path::PathBuf;

use serde_json::Value;

use super::fmt::{write_json_canonical, RustWriter};
use super::ir::{Document, VerbDef};
use super::{CodegenError, GeneratedFile};

const HEADER_RS: &str = "// @generated by cairn-codegen — DO NOT EDIT.\n";
const ROOT: &str = "crates/cairn-mcp/src/generated";

pub fn emit(doc: &Document) -> Result<Vec<GeneratedFile>, CodegenError> {
    let mut out = Vec::new();
    out.push(emit_mod(doc));
    for verb in &doc.verbs {
        out.push(emit_schema(
            &format!("{ROOT}/schemas/verbs/{}.json", verb.id),
            &verb.args_schema_bytes,
        )?);
    }
    for prelude in &doc.preludes {
        out.push(emit_schema(
            &format!("{ROOT}/schemas/prelude/{}.json", prelude.id),
            &prelude.schema_bytes,
        )?);
    }
    Ok(out)
}

fn emit_mod(doc: &Document) -> GeneratedFile {
    let mut w = RustWriter::new();
    w.raw(HEADER_RS);
    w.line("//! Generated MCP tool declarations for cairn.mcp.v1.");
    w.blank();
    w.line("/// Static tool declaration the MCP transport layer registers at startup.");
    w.line("pub struct ToolDecl {");
    w.indent();
    w.line("pub name: &'static str,");
    w.line("pub description: &'static str,");
    w.line("pub input_schema: &'static [u8],");
    w.line("pub capability: Option<&'static str>,");
    w.line("pub auth: &'static str,");
    w.dedent();
    w.line("}");
    w.blank();
    w.line(&format!("pub const TOOLS: &[ToolDecl] = &["));
    w.indent();
    for verb in &doc.verbs {
        emit_tool_decl(&mut w, verb);
    }
    w.dedent();
    w.line("];");
    GeneratedFile {
        path: PathBuf::from(ROOT).join("mod.rs"),
        bytes: w.finish().into_bytes(),
    }
}

fn emit_tool_decl(w: &mut RustWriter, verb: &VerbDef) {
    let description = build_description(verb);
    w.line("ToolDecl {");
    w.indent();
    w.line(&format!("name: \"{}\",", verb.id));
    // Multi-line raw string so embedded newlines stay readable.
    w.line(&format!("description: r#\"{description}\"#,"));
    w.line(&format!(
        "input_schema: include_bytes!(\"schemas/verbs/{}.json\"),",
        verb.id
    ));
    match &verb.capability {
        Some(c) => w.line(&format!("capability: Some(\"{c}\"),")),
        None => w.line("capability: None,"),
    }
    w.line(&format!("auth: \"{}\",", verb.auth.as_str()));
    w.dedent();
    w.line("},");
}

fn build_description(verb: &VerbDef) -> String {
    // §8.0.b: one-line purpose + positive triggers + negative triggers + exclusivity.
    let mut s = String::new();
    s.push_str(&format!("`{}` — verb {}.\n", verb.id, verb.id));
    if !verb.skill.positive.is_empty() {
        s.push_str("\nPOSITIVE — use when:\n");
        for p in &verb.skill.positive {
            s.push_str("• ");
            s.push_str(p);
            s.push('\n');
        }
    }
    if !verb.skill.negative.is_empty() {
        s.push_str("\nNEGATIVE — do not use when:\n");
        for n in &verb.skill.negative {
            s.push_str("• ");
            s.push_str(n);
            s.push('\n');
        }
    }
    if let Some(ex) = &verb.skill.exclusivity {
        s.push_str("\nEXCLUSIVITY: ");
        s.push_str(ex);
        s.push('\n');
    }
    s
}

fn emit_schema(rel_path: &str, raw_bytes: &[u8]) -> Result<GeneratedFile, CodegenError> {
    // Parse + re-serialise canonically so the on-disk schema is normalised
    // (sorted keys, two-space indent, trailing newline). Validation guarantees
    // the input parses; if it ever doesn't, surface the error.
    let value: Value = serde_json::from_slice(raw_bytes)
        .map_err(|e| CodegenError::Emit(format!("schema parse: {e}")))?;
    Ok(GeneratedFile {
        path: PathBuf::from(rel_path),
        bytes: write_json_canonical(&value).into_bytes(),
    })
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_emit_mcp`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/emit_mcp.rs crates/cairn-idl/tests/codegen_emit_mcp.rs
git commit -m "codegen: emit MCP tool decls + canonical schemas (#35)"
```

---

### Task 17: `emit_skill` — SKILL.md + conventions.md + .version

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs`
- Create: `crates/cairn-idl/tests/codegen_emit_skill.rs`

Emit `skills/cairn/SKILL.md` (built from §18.d template + per-verb skill-triggers blocks), `skills/cairn/conventions.md` (kind cheat-sheet, regenerated against `VerbId` for now), `skills/cairn/.version` (`cairn.mcp.v1` + cairn-idl pkg version).

- [ ] **Step 1: Write failing test**

Create `crates/cairn-idl/tests/codegen_emit_skill.rs`:

```rust
use cairn_idl::codegen::{ir, loader, emit_skill};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_skill_md_with_eight_verb_sections() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    for verb in ["ingest", "search", "retrieve", "summarize", "assemble_hot", "capture_trace", "lint", "forget"] {
        assert!(body.contains(&format!("## `cairn {verb}`")), "SKILL.md missing section for {verb}");
    }
    // Preludes called out as preludes, not core verbs.
    assert!(body.contains("Protocol preludes"));
    assert!(body.contains("status"));
    assert!(body.contains("handshake"));
}

#[test]
fn version_file_pins_contract_and_pkg() {
    let files = emit_skill::emit(&doc()).unwrap();
    let version = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/.version"))
        .unwrap();
    let body = std::str::from_utf8(&version.bytes).unwrap();
    assert!(body.contains("contract: cairn.mcp.v1"));
    assert!(body.contains("cairn-idl:"));
}
```

- [ ] **Step 2: Implement `emit_skill`**

Replace `crates/cairn-idl/src/codegen/emit_skill.rs`:

```rust
//! Skill emitter — writes the Cairn skill bundle into `skills/cairn/`.

use std::path::PathBuf;

use super::ir::{Document, VerbDef};
use super::{CodegenError, GeneratedFile};

const HEADER_MD: &str = "<!-- @generated by cairn-codegen — DO NOT EDIT. -->\n";

pub fn emit(doc: &Document) -> Result<Vec<GeneratedFile>, CodegenError> {
    Ok(vec![
        emit_skill_md(doc),
        emit_conventions(doc),
        emit_version(doc),
    ])
}

fn emit_skill_md(doc: &Document) -> GeneratedFile {
    let mut s = String::new();
    s.push_str(HEADER_MD);
    s.push_str("---\n");
    s.push_str("name: cairn\n");
    s.push_str("description: Cairn memory system. Use for persistent memory across turns, sessions, and agents.\n");
    s.push_str("---\n\n");
    s.push_str("# Cairn Memory Skill\n\n");
    s.push_str("Persistent memory via the `cairn` CLI. The eight verbs below are the contract. ");
    s.push_str("Status / handshake are protocol preludes — see the bottom of this file.\n\n");
    for verb in &doc.verbs {
        push_verb_section(&mut s, verb);
    }
    s.push_str("---\n\n## Protocol preludes (not core verbs)\n\n");
    for prelude in &doc.preludes {
        s.push_str(&format!("- `cairn {} --json` — {}\n", prelude.id, match prelude.id.as_str() {
            "status" => "deterministic capability discovery (cacheable, no side effects).",
            "handshake" => "fresh per-call challenge mint.",
            _ => "(prelude)",
        }));
    }
    s.push('\n');
    GeneratedFile {
        path: PathBuf::from("skills/cairn/SKILL.md"),
        bytes: s.into_bytes(),
    }
}

fn push_verb_section(s: &mut String, verb: &VerbDef) {
    s.push_str(&format!("## `cairn {}`\n\n", verb.id));
    if !verb.skill.positive.is_empty() {
        s.push_str("**Use when:**\n");
        for p in &verb.skill.positive {
            s.push_str("- ");
            s.push_str(p);
            s.push('\n');
        }
        s.push('\n');
    }
    if !verb.skill.negative.is_empty() {
        s.push_str("**Do NOT use when:**\n");
        for n in &verb.skill.negative {
            s.push_str("- ");
            s.push_str(n);
            s.push('\n');
        }
        s.push('\n');
    }
    if let Some(ex) = &verb.skill.exclusivity {
        s.push_str("**Exclusivity:** ");
        s.push_str(ex);
        s.push_str("\n\n");
    }
}

fn emit_conventions(doc: &Document) -> GeneratedFile {
    let mut s = String::new();
    s.push_str(HEADER_MD);
    s.push_str("# Cairn skill conventions\n\n");
    s.push_str("Verb ids in the contract:\n\n");
    for verb in &doc.verbs {
        s.push_str(&format!("- `{}`\n", verb.id));
    }
    s.push_str("\n<!-- Kinds cheat-sheet integrates when #4 lands the taxonomy IDL slice. -->\n");
    GeneratedFile {
        path: PathBuf::from("skills/cairn/conventions.md"),
        bytes: s.into_bytes(),
    }
}

fn emit_version(_doc: &Document) -> GeneratedFile {
    let s = format!(
        "contract: cairn.mcp.v1\ncairn-idl: {}\n",
        env!("CARGO_PKG_VERSION"),
    );
    GeneratedFile {
        path: PathBuf::from("skills/cairn/.version"),
        bytes: s.into_bytes(),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_emit_skill`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/emit_skill.rs crates/cairn-idl/tests/codegen_emit_skill.rs
git commit -m "codegen: emit skills/cairn/ bundle (SKILL.md, conventions, version) (#35)"
```

---

## Phase 5 — Pipeline glue + binary + first run

### Task 18: Wire `codegen::run` to dispatch every emitter

**Files:**
- Modify: `crates/cairn-idl/src/codegen/mod.rs`
- Create: `crates/cairn-idl/tests/codegen_run.rs`

`run` loads, builds IR, calls every emitter, then either writes (`Write` mode) or compares against on-disk (`Check` mode).

- [ ] **Step 1: Write failing test**

Create `crates/cairn-idl/tests/codegen_run.rs`:

```rust
use cairn_idl::codegen::{run, RunMode, RunOpts};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn run_write_mode_emits_files_into_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();
    assert!(report.files_emitted >= 8 + 8 + 1 + 3,
        "expected at least 8 SDK verb files, 8 schemas, mods, skill bundle; got {}",
        report.files_emitted);
    // Spot-check a few outputs landed.
    assert!(tmp.path().join("crates/cairn-core/src/generated/verbs/mod.rs").exists());
    assert!(tmp.path().join("crates/cairn-mcp/src/generated/schemas/verbs/ingest.json").exists());
    assert!(tmp.path().join("skills/cairn/SKILL.md").exists());
}

#[test]
fn run_check_mode_clean_tree_returns_no_drift() {
    // Use the actual workspace — it should match on a clean checkout after the
    // committed outputs land in Task 20.
    let _ = workspace_root();
    // This test is enabled only AFTER Task 20 commits the generated outputs.
    // Until then, the assertion is "running --check on the workspace either
    // succeeds (clean) or reports drift listing the missing files".
    // No-op until Task 20.
}
```

- [ ] **Step 2: Implement `run`**

Replace the stub `pub fn run(...)` in `crates/cairn-idl/src/codegen/mod.rs` with:

```rust
pub fn run(opts: &RunOpts) -> Result<Report, CodegenError> {
    use std::io::Write;

    let schema_root = std::path::PathBuf::from(crate::SCHEMA_DIR);
    let raw = loader::load(&schema_root)?;
    let doc = ir::build(&raw)?;

    let mut all = Vec::new();
    all.extend(emit_sdk::emit(&doc)?);
    all.extend(emit_cli::emit(&doc)?);
    all.extend(emit_mcp::emit(&doc)?);
    all.extend(emit_skill::emit(&doc)?);

    // Stable-sort outputs so reports are deterministic.
    all.sort_by(|a, b| a.path.cmp(&b.path));

    let mut report = Report { files_emitted: all.len(), drift: Vec::new() };

    match opts.mode {
        RunMode::Write => {
            for file in &all {
                let abs = opts.workspace_root.join(&file.path);
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Atomic write via tempfile + persist.
                let dir = abs.parent().unwrap_or(std::path::Path::new("."));
                let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
                tmp.write_all(&file.bytes)?;
                tmp.persist(&abs).map_err(|e| CodegenError::Io(e.error))?;
            }
        }
        RunMode::Check => {
            for file in &all {
                let abs = opts.workspace_root.join(&file.path);
                let on_disk = match std::fs::read(&abs) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        report.drift.push(file.path.clone());
                        continue;
                    }
                };
                if on_disk != file.bytes {
                    report.drift.push(file.path.clone());
                }
            }
        }
    }
    Ok(report)
}
```

- [ ] **Step 3: Run test**

Run: `cargo nextest run -p cairn-idl --test codegen_run run_write_mode_emits_files_into_tempdir`
Expected: PASS — files materialise into a tempdir.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/codegen/mod.rs crates/cairn-idl/tests/codegen_run.rs
git commit -m "codegen: wire run() to dispatch every emitter and write atomically (#35)"
```

---

### Task 19: `cairn-codegen` binary with `--check` and `--out`

**Files:**
- Modify: `crates/cairn-idl/src/bin/cairn-codegen.rs`

Replace the stub binary with a clap-based dispatcher.

- [ ] **Step 1: Replace `crates/cairn-idl/src/bin/cairn-codegen.rs`**

```rust
//! `cairn-codegen` — maintainer-time binary that re-emits SDK / CLI / MCP /
//! skill artefacts from the IDL.
//!
//! Modes:
//!   - default: write outputs to the workspace root (parent of CARGO_MANIFEST_DIR).
//!   - --check: compare emitter outputs to on-disk; non-zero exit on drift.
//!   - --out  : custom workspace root (used by tests).

use std::path::PathBuf;
use std::process::ExitCode;

use cairn_idl::codegen::{run, RunMode, RunOpts};

#[derive(clap::Parser, Debug)]
#[command(name = "cairn-codegen", about = "Cairn IDL → Rust + JSON codegen")]
struct Cli {
    /// Run in check mode — compare emitted bytes against on-disk; exit 1 on drift.
    #[arg(long)]
    check: bool,

    /// Workspace root (defaults to the parent of CARGO_MANIFEST_DIR).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    use clap::Parser;
    let cli = Cli::parse();

    let workspace_root = cli.out.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("cairn-idl crate must have a parent (the `crates/` dir)")
            .parent()
            .expect("`crates/` must have a parent (the workspace root)")
            .to_path_buf()
    });

    let opts = RunOpts {
        workspace_root,
        mode: if cli.check { RunMode::Check } else { RunMode::Write },
    };

    match run(&opts) {
        Ok(report) if !report.drift.is_empty() => {
            eprintln!(
                "cairn-codegen: drift detected ({} file(s) differ from on-disk):",
                report.drift.len()
            );
            for (i, p) in report.drift.iter().enumerate() {
                if i >= 20 {
                    eprintln!("  … and {} more", report.drift.len() - 20);
                    break;
                }
                eprintln!("  {}", p.display());
            }
            eprintln!("Fix: run `cargo run -p cairn-idl --bin cairn-codegen` and commit the diff.");
            ExitCode::from(1)
        }
        Ok(report) => {
            if cli.check {
                eprintln!("cairn-codegen: clean — {} file(s) match.", report.files_emitted);
            } else {
                eprintln!("cairn-codegen: wrote {} file(s).", report.files_emitted);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn-codegen: {e}");
            ExitCode::from(2)
        }
    }
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p cairn-idl --bin cairn-codegen`
Expected: exits 0.

- [ ] **Step 3: Smoke run against a tempdir**

Run:
```bash
TMP=$(mktemp -d) && cargo run -q -p cairn-idl --bin cairn-codegen -- --out "$TMP" && ls "$TMP/skills/cairn/"
```
Expected: prints the three skill files (`SKILL.md`, `conventions.md`, `.version`).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/src/bin/cairn-codegen.rs
git commit -m "codegen: cairn-codegen binary with --check and --out (#35)"
```

---

### Task 20: First end-to-end run — commit generated outputs

**Files:**
- Create: `crates/cairn-core/src/generated/` (multiple files)
- Create: `crates/cairn-cli/src/generated/` (multiple files)
- Create: `crates/cairn-mcp/src/generated/` (multiple files)
- Create: `skills/cairn/` (multiple files)

Run the binary against the workspace, commit every output. After this commit, `--check` is the operative gate.

- [ ] **Step 1: Run the generator**

Run: `cargo run -p cairn-idl --bin cairn-codegen`
Expected: prints `cairn-codegen: wrote N file(s).` (N ≈ 30+ depending on the actual schema count).

- [ ] **Step 2: Inspect what changed**

Run: `git status --short`
Expected: long list of new files under `crates/cairn-core/src/generated/`, `crates/cairn-cli/src/generated/`, `crates/cairn-mcp/src/generated/`, `skills/cairn/`.

- [ ] **Step 3: Verify cairn-core compiles with the generated module declared**

The generated SDK files exist on disk but aren't yet referenced from cairn-core's `lib.rs`. Add them next task. For now, just confirm:

Run: `cargo check -p cairn-idl`
Expected: exits 0 (the generator crate doesn't depend on the generated output).

- [ ] **Step 4: Commit the generated outputs**

```bash
git add crates/cairn-core/src/generated/ \
        crates/cairn-cli/src/generated/ \
        crates/cairn-mcp/src/generated/ \
        skills/cairn/
git commit -m "codegen: commit first generated outputs from IDL (#35)"
```

- [ ] **Step 5: Verify --check is now clean**

Run: `cargo run -p cairn-idl --bin cairn-codegen -- --check`
Expected: prints `cairn-codegen: clean — N file(s) match.` and exits 0.

---

### Task 21: Wire generated outputs into cairn-core, cairn-cli, cairn-mcp

**Files:**
- Modify: `crates/cairn-core/src/lib.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Modify: `crates/cairn-cli/Cargo.toml`
- Modify: `crates/cairn-mcp/src/lib.rs`

Each consumer crate declares the generated module so `cargo check --workspace` exercises it. CLI keeps its scaffold dispatch (verbs still exit 2) but `command()` from generated takes over from the hand-rolled `VERBS` const.

- [ ] **Step 1: Add `pub mod generated;` to cairn-core lib.rs**

Read `crates/cairn-core/src/lib.rs` first to see what's there. Then add (preserving existing module declarations / docstring):

```rust
//! Cairn verb layer, domain types, and error enums. No I/O, no adapters.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL and
//! must not be hand-edited — see `docs/dev/codegen.md`.

pub mod generated;
```

(If `lib.rs` already has other contents, append `pub mod generated;` after them.)

- [ ] **Step 2: Add clap to cairn-cli's deps**

Edit `crates/cairn-cli/Cargo.toml`. Under `[dependencies]`:

```toml
clap = { workspace = true }
```

- [ ] **Step 3: Replace the scaffold `main.rs` with one that uses `generated::command()`**

Replace `crates/cairn-cli/src/main.rs`:

```rust
//! Cairn CLI entry point. Subcommand tree is generated from the IDL by
//! `cairn-codegen`; verb dispatch lands in #59 / #9. Until then, every verb
//! exits 2 with a not-implemented message so callers cannot mistake a
//! scaffold for a real memory operation.

use std::process::ExitCode;

mod generated;

fn main() -> ExitCode {
    let matches = generated::command().get_matches();
    match matches.subcommand() {
        Some((verb, _sub)) => {
            eprintln!(
                "cairn {verb}: not yet implemented in this P0 scaffold. \
                 Verb dispatch lands in #59 / #9; no memory operation was performed."
            );
            ExitCode::from(2)
        }
        None => {
            // No subcommand: print help and exit 0.
            let _ = generated::command().print_help();
            println!();
            ExitCode::SUCCESS
        }
    }
}
```

- [ ] **Step 4: Add `pub mod generated;` to cairn-mcp lib.rs**

Edit `crates/cairn-mcp/src/lib.rs`:

```rust
//! Cairn MCP adapter — exposes the verb layer over MCP transports.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL.
//! Transport runtime lands in #64.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod generated;
```

- [ ] **Step 5: Verify the workspace compiles**

Run: `cargo check --workspace`
Expected: exits 0. Generated Rust is type-checked end-to-end.

- [ ] **Step 6: Verify cairn-core boundary still clean**

Run: `./scripts/check-core-boundary.sh`
Expected: prints `cairn-core boundary OK`. Generated code uses only `serde`, `serde_json`, no `cairn-*` deps.

- [ ] **Step 7: Run all existing tests**

Run: `cargo nextest run --workspace`
Expected: every test passes.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-core/src/lib.rs \
        crates/cairn-cli/Cargo.toml crates/cairn-cli/src/main.rs \
        crates/cairn-mcp/src/lib.rs
git commit -m "wire generated/ modules into cairn-core, cairn-cli, cairn-mcp (#35)"
```

---

## Phase 6 — Verification tests

### Task 22: Idempotency + determinism tests

**Files:**
- Create: `crates/cairn-idl/tests/codegen_idempotent.rs`
- Create: `crates/cairn-idl/tests/codegen_determinism.rs`

The two acceptance-criteria tests: re-running on a clean tree is a no-op; running 5× in fresh tempdirs yields byte-equal trees.

- [ ] **Step 1: Create `codegen_idempotent.rs`**

```rust
//! Re-running codegen against an already-clean workspace is a no-op.

use std::path::PathBuf;
use cairn_idl::codegen::{run, RunMode, RunOpts};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn second_run_produces_no_drift() {
    // First run writes outputs to a tempdir.
    let tmp = tempfile::tempdir().unwrap();
    run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();

    // Second run in Check mode should report zero drift.
    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(
        report.drift.is_empty(),
        "second run reports drift: {:?}",
        report.drift
    );
}

#[test]
fn workspace_check_is_clean() {
    // After Task 20 commits the outputs, --check on the actual workspace must pass.
    let report = run(&RunOpts {
        workspace_root: workspace_root(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(report.drift.is_empty(), "drift in committed workspace: {:?}", report.drift);
}
```

- [ ] **Step 2: Create `codegen_determinism.rs`**

```rust
//! Running codegen 5 times in fresh tempdirs yields byte-equal output trees.
//! Catches accidental hash-iteration leaks (HashMap, HashSet, etc.).

use std::collections::BTreeMap;
use std::path::PathBuf;
use cairn_idl::codegen::{run, RunMode, RunOpts};

fn snapshot_tree(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let bytes = std::fs::read(&path).unwrap();
            out.insert(rel, bytes);
        }
    }
}

#[test]
fn five_runs_produce_byte_equal_trees() {
    let mut snapshots: Vec<BTreeMap<PathBuf, Vec<u8>>> = Vec::with_capacity(5);
    for _ in 0..5 {
        let tmp = tempfile::tempdir().unwrap();
        run(&RunOpts {
            workspace_root: tmp.path().to_path_buf(),
            mode: RunMode::Write,
        })
        .unwrap();
        snapshots.push(snapshot_tree(tmp.path()));
    }
    let first = &snapshots[0];
    for (i, snap) in snapshots.iter().enumerate().skip(1) {
        assert_eq!(snap, first, "run #{i} differs from run #0");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_idempotent --test codegen_determinism`
Expected: all 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/tests/codegen_idempotent.rs crates/cairn-idl/tests/codegen_determinism.rs
git commit -m "codegen: idempotency + 5-run determinism tests (#35)"
```

---

### Task 23: Surface-parity test (the acceptance criterion)

**Files:**
- Create: `crates/cairn-idl/tests/codegen_surface_parity.rs`

The single most important test in this PR: assert all four surfaces enumerate the same eight verb ids in the same order, and that `status` / `handshake` are flagged separately and excluded from each surface's verb count.

- [ ] **Step 1: Create the test**

```rust
//! Four-surface parity test — the acceptance criterion of #35.
//!
//! Independently inspects each emitter's output and confirms:
//!   (1) all four surfaces enumerate the same eight verb ids in IDL order,
//!   (2) `status` and `handshake` appear separately, never as core verbs.

use cairn_idl::codegen::{ir, loader, emit_cli, emit_mcp, emit_sdk, emit_skill};

const EXPECTED_VERBS: &[&str] = &[
    "ingest", "search", "retrieve", "summarize",
    "assemble_hot", "capture_trace", "lint", "forget",
];

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn doc_lists_eight_verbs_in_idl_order() {
    let d = doc();
    let ids: Vec<&str> = d.verbs.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, EXPECTED_VERBS);
}

#[test]
fn sdk_verb_registry_lists_eight_verbs() {
    let files = emit_sdk::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-core/src/generated/verbs/mod.rs"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();

    // Extract the VerbId variants in source order.
    let start = body.find("pub enum VerbId").unwrap();
    let body = &body[start..];
    let block_start = body.find('{').unwrap() + 1;
    let block_end = body.find('}').unwrap();
    let block = &body[block_start..block_end];
    let variants: Vec<String> = block
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("//"))
        .map(|s| s.trim_end_matches(',').trim().to_string())
        .collect();

    let expected: Vec<String> = EXPECTED_VERBS
        .iter()
        .map(|v| cairn_idl::codegen::ir::pascal_case(v))
        .collect();
    assert_eq!(variants, expected, "SDK VerbId mismatch");
    assert!(!body.contains("Status"), "Status leaked into VerbId");
    assert!(!body.contains("Handshake"), "Handshake leaked into VerbId");
}

#[test]
fn cli_subcommand_tree_lists_eight_verbs_plus_two_preludes() {
    let files = emit_cli::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-cli/src/generated/mod.rs"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();
    let mut idx_per_verb: Vec<(usize, &str)> = EXPECTED_VERBS
        .iter()
        .map(|v| {
            let needle = format!("\"{v}\"");
            let i = body.find(&needle).unwrap_or_else(|| panic!("CLI missing verb {v}"));
            (i, *v)
        })
        .collect();
    idx_per_verb.sort();
    let order: Vec<&str> = idx_per_verb.into_iter().map(|(_, v)| v).collect();
    assert_eq!(order, EXPECTED_VERBS, "CLI verb subcommand order != IDL order");
    // Preludes present.
    assert!(body.contains("\"status\""));
    assert!(body.contains("\"handshake\""));
}

#[test]
fn mcp_tools_array_lists_eight_verbs_in_idl_order() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();
    let mut idx_per_verb: Vec<(usize, &str)> = EXPECTED_VERBS
        .iter()
        .map(|v| {
            let needle = format!("name: \"{v}\"");
            let i = body.find(&needle).unwrap_or_else(|| panic!("MCP missing verb {v}"));
            (i, *v)
        })
        .collect();
    idx_per_verb.sort();
    let order: Vec<&str> = idx_per_verb.into_iter().map(|(_, v)| v).collect();
    assert_eq!(order, EXPECTED_VERBS, "MCP TOOLS order != IDL order");
    // Preludes are NOT in TOOLS — they're protocol preludes, not tools.
    assert!(!body.contains("name: \"status\""), "status leaked into TOOLS");
    assert!(!body.contains("name: \"handshake\""), "handshake leaked into TOOLS");
}

#[test]
fn skill_md_lists_eight_verb_sections_plus_separate_prelude_section() {
    let files = emit_skill::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();
    for verb in EXPECTED_VERBS {
        assert!(body.contains(&format!("## `cairn {verb}`")), "SKILL.md missing section for {verb}");
    }
    assert!(body.contains("Protocol preludes"));
    // status / handshake are mentioned only inside the preludes section.
    let preludes_section_start = body.find("Protocol preludes").unwrap();
    assert!(body[preludes_section_start..].contains("status"));
    assert!(body[preludes_section_start..].contains("handshake"));
    // Neither appears as a `## \`cairn …\`` header.
    assert!(!body.contains("## `cairn status`"));
    assert!(!body.contains("## `cairn handshake`"));
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_surface_parity`
Expected: all 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-idl/tests/codegen_surface_parity.rs
git commit -m "codegen: surface-parity test — same 8 verbs across CLI/MCP/SDK/skill (#35)"
```

---

### Task 24: Snapshot tests with insta

**Files:**
- Create: `crates/cairn-idl/tests/codegen_snapshot.rs`
- Create: `crates/cairn-idl/tests/snapshots/` (auto-managed by insta)

Lock down byte-level output of representative emitter products so PR review notices accidental drift even when CI is green.

- [ ] **Step 1: Create the snapshot test**

```rust
//! Insta snapshots for representative emitter outputs. Update with
//! `cargo insta review` after intentional IDL or emitter changes.

use cairn_idl::codegen::{ir, loader, emit_cli, emit_mcp, emit_sdk, emit_skill};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

fn read(files: &[cairn_idl::codegen::GeneratedFile], suffix: &str) -> String {
    let f = files
        .iter()
        .find(|f| f.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("no generated file ending in {suffix}"));
    std::str::from_utf8(&f.bytes).unwrap().to_string()
}

#[test]
fn snapshot_sdk_verbs_mod() {
    let files = emit_sdk::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-core/src/generated/verbs/mod.rs"));
}

#[test]
fn snapshot_sdk_ingest() {
    let files = emit_sdk::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-core/src/generated/verbs/ingest.rs"));
}

#[test]
fn snapshot_cli_mod() {
    let files = emit_cli::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-cli/src/generated/mod.rs"));
}

#[test]
fn snapshot_mcp_mod() {
    let files = emit_mcp::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-mcp/src/generated/mod.rs"));
}

#[test]
fn snapshot_mcp_ingest_schema() {
    let files = emit_mcp::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-mcp/src/generated/schemas/verbs/ingest.json"));
}

#[test]
fn snapshot_skill_md() {
    let files = emit_skill::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "skills/cairn/SKILL.md"));
}
```

- [ ] **Step 2: Generate the initial snapshots**

Run: `cargo nextest run -p cairn-idl --test codegen_snapshot 2>&1 || true`
Expected: tests fail because snapshots don't exist yet — insta has written `.snap.new` files.

Run: `cargo insta accept`
Expected: accepts all 6 new snapshots into `.snap` files.

- [ ] **Step 3: Re-run to confirm green**

Run: `cargo nextest run -p cairn-idl --test codegen_snapshot`
Expected: all 6 pass.

- [ ] **Step 4: Commit snapshots**

```bash
git add crates/cairn-idl/tests/codegen_snapshot.rs crates/cairn-idl/tests/snapshots/
git commit -m "codegen: insta snapshots for representative emitter outputs (#35)"
```

---

### Task 25: `--check` mode behavioural test

**Files:**
- Create: `crates/cairn-idl/tests/codegen_check_mode.rs`

`--check` returns 0 on a clean tree, returns 1 with stable error message after touching a generated file.

- [ ] **Step 1: Create the test**

```rust
//! Behavioural test for the `--check` flag.

use std::path::PathBuf;
use cairn_idl::codegen::{run, RunMode, RunOpts};

fn fork_workspace_outputs() -> tempfile::TempDir {
    // Write a fresh codegen output tree into a tempdir.
    let tmp = tempfile::tempdir().unwrap();
    run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();
    tmp
}

#[test]
fn check_clean_tree_reports_no_drift() {
    let tmp = fork_workspace_outputs();
    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(report.drift.is_empty());
}

#[test]
fn check_after_manual_edit_reports_drift() {
    let tmp = fork_workspace_outputs();
    let target = tmp.path().join("skills/cairn/SKILL.md");
    let mut bytes = std::fs::read(&target).unwrap();
    bytes.extend_from_slice(b"\n<!-- accidental edit -->\n");
    std::fs::write(&target, bytes).unwrap();

    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert_eq!(report.drift, vec![PathBuf::from("skills/cairn/SKILL.md")]);
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-idl --test codegen_check_mode`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-idl/tests/codegen_check_mode.rs
git commit -m "codegen: --check mode behavioural test (#35)"
```

---

## Phase 7 — CI gate, docs, final verification

### Task 26: CI workflow — codegen-drift job

**Files:**
- Create: `.github/workflows/ci.yml`

No `ci.yml` exists yet (only `governance.yml`). Add a minimal CI workflow whose initial scope is the codegen drift gate. Subsequent issues (#36, etc.) extend it.

- [ ] **Step 1: Create the workflow**

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  codegen-drift:
    name: codegen / no drift
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.95.0
      - uses: Swatinem/rust-cache@v2
      - name: Run cairn-codegen --check
        run: cargo run -p cairn-idl --bin cairn-codegen -- --check
```

- [ ] **Step 2: Lint the YAML locally if `actionlint` is available**

Run: `actionlint .github/workflows/ci.yml 2>/dev/null || echo 'actionlint not installed — skipping'`
Expected: no errors, or the skip message.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add codegen-drift gate (#35)"
```

---

### Task 27: Maintainer docs — `docs/dev/codegen.md`

**Files:**
- Create: `docs/dev/codegen.md`

- [ ] **Step 1: Write the doc**

```markdown
# cairn-codegen — maintainer guide

`cairn-codegen` is the maintainer-time binary that re-emits the four artefact
bundles derived from the IDL under `crates/cairn-idl/schema/`:

| Tree | Purpose |
|---|---|
| `crates/cairn-core/src/generated/` | SDK Rust types — verb registry, per-verb `Args` / `Data`, common types, errors enum, prelude responses. |
| `crates/cairn-cli/src/generated/`  | `clap::Command` subcommand tree (`pub fn command()`). |
| `crates/cairn-mcp/src/generated/`  | MCP tool declarations + canonical JSON schemas (cross-language artefact). |
| `skills/cairn/`                    | `SKILL.md`, `conventions.md`, `.version` — the shippable Cairn skill. |

## When to run

Whenever any file under `crates/cairn-idl/schema/` changes, or after editing
emitter logic in `crates/cairn-idl/src/codegen/`.

## How to run

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

This rewrites every artefact under the four trees. Commit the diff in the
same PR as the IDL or emitter change.

## What CI does

The `codegen-drift` job (`.github/workflows/ci.yml`) runs:

```bash
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

`--check` compares emitter output to the on-disk bytes; any difference exits
non-zero. The error message lists the first 20 differing files. Fix:

```bash
cargo run -p cairn-idl --bin cairn-codegen
git add -A
git commit -m "regenerate codegen artefacts"
```

## Adding a new verb

1. Drop the verb file under `crates/cairn-idl/schema/verbs/<id>.json` with
   the standard envelope: `x-cairn-contract`, `x-cairn-verb-id`,
   `x-cairn-cli`, `x-cairn-skill-triggers`, `x-cairn-auth`,
   optional `x-cairn-capability`, plus `$defs.Args` and `$defs.Data`.
2. Append the new path to `crates/cairn-idl/schema/index.json` under
   `x-cairn-files.verbs` AND `x-cairn-verb-ids`.
3. Run `cargo run -p cairn-idl --bin cairn-codegen`.
4. Run `cargo nextest run -p cairn-idl` to confirm parity / determinism /
   snapshot tests still pass.
5. If the snapshot tests fail because the new verb appears in
   `verbs/mod.rs`, accept the snapshot diff: `cargo insta review`.
6. Commit everything in a single PR.

## Adding new IR / emitter logic

The pipeline is `loader → ir → emit_*`:

1. **Loader changes** when adding new structural validation. Update the test
   suite in `crates/cairn-idl/tests/codegen_loader.rs`.
2. **IR changes** when adding a new lowering rule. The lowering table is in
   `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md` §4.2 — keep
   that table and the IR in sync.
3. **Emitter changes** affect specific output trees. The snapshot tests
   (`crates/cairn-idl/tests/codegen_snapshot.rs`) lock down byte-level
   output; review with `cargo insta review` when the change is intentional.

## Determinism

Three rules every emitter obeys:

- Stable iteration (`BTreeMap`, sorted `Vec`, never `HashMap`).
- Canonical JSON via `cairn_idl::codegen::fmt::write_json_canonical` (sorted
  keys, two-space indent, trailing newline).
- Atomic file writes via `tempfile::NamedTempFile::persist`.

The `codegen_determinism` test runs codegen 5× into fresh tempdirs and
asserts byte-equal trees — any leak (e.g. accidental `HashMap` iteration)
fails CI.

## Filter recursion bound

The `Filter` enum in `crates/cairn-core/src/generated/verbs/search.rs` is
collapsed from the IDL's unrolled `filter_L0..L8` into a single recursive
type. The depth bound stays a JSON-Schema assertion only — the runtime
depth check lives in the search verb implementation (#9 / #63). A
hand-crafted deeply-nested `Filter` value bypasses the schema; the verb
must reject it.

## Cross-references

- Spec: `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md`
- Brief sections this PR implements: §8.0 (four surfaces), §8.0.a
  (handshake/status preludes), §8.0.b (envelope), §8.0.c (`RetrieveArgs`),
  §13.5 (language split), §18.d (Cairn skill).
- Adjacent open issues: #36 (broader contract-drift gates), #59 (CLI
  command tree consumer), #9 (verb impls), #63 (`RetrieveArgs` semantics),
  #64 (MCP transport), #70 (skill-install validation), #98 (wire compat).
```

- [ ] **Step 2: Commit**

```bash
git add docs/dev/codegen.md
git commit -m "docs: codegen maintainer guide (#35)"
```

---

### Task 28: cairn-idl README + CLAUDE.md pointer

**Files:**
- Modify: `crates/cairn-idl/README.md` (or create if absent)
- Modify: `CLAUDE.md`

- [ ] **Step 1: Check README state**

Run: `ls crates/cairn-idl/README.md 2>/dev/null && head -5 crates/cairn-idl/README.md || echo 'no README'`

- [ ] **Step 2: Write or extend the README**

Create or replace `crates/cairn-idl/README.md`:

```markdown
# cairn-idl

Canonical IDL for the `cairn.mcp.v1` contract plus the `cairn-codegen`
binary that lowers the IDL into the four surface bundles (SDK, CLI, MCP,
skill). Schema sources live under `schema/`; generated outputs live in the
consumer crates and `skills/cairn/`.

When the schema changes, run `cargo run -p cairn-idl --bin cairn-codegen`
and commit the regenerated tree. CI (`codegen-drift`) gates on no-diff.

See `docs/dev/codegen.md` for the full maintainer guide.
```

- [ ] **Step 3: Add a one-line pointer to CLAUDE.md**

Edit `CLAUDE.md`. In §10 ("Quick map — where things live"), find the line:

```
│   ├── cairn-idl/                  ← IDL + codegen (cairn-codegen bin)
```

and replace it with:

```
│   ├── cairn-idl/                  ← IDL + codegen. Run `cargo run -p cairn-idl --bin cairn-codegen` after IDL edits; CI gates on no-diff.
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/README.md CLAUDE.md
git commit -m "docs: cairn-idl README + CLAUDE.md codegen pointer (#35)"
```

---

### Task 29: Final verification + open the PR

**Files:** none — verification only.

- [ ] **Step 1: Run the full pre-push checklist**

Run each command in turn; expect exit 0 from each:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo test --doc --workspace
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

If clippy flags any pedantic warning in generated code, fix the emitter (not the generated file) and regenerate.

- [ ] **Step 2: Verify acceptance-criteria mapping**

Confirm by inspection:

| Issue acceptance item | How |
|---|---|
| Same eight verbs across CLI / MCP / SDK / skill | `codegen_surface_parity` (5 tests) |
| Handshake / status are preludes, not core verbs | Same test — assertions exclude both |
| Clean checkout regenerates with no diff | `codegen_idempotent::workspace_check_is_clean` + CI `--check` |
| Generator twice → no-op | `codegen_idempotent::second_run_produces_no_drift` |
| Schema/type checks for generated outputs | `cargo check --workspace` + canonical-JSON round-trip in `codegen_emit_mcp` |
| Generation docs name the exact command | `docs/dev/codegen.md` |

- [ ] **Step 3: Open the PR**

Run:
```bash
git push -u origin worktree-steady-percolating-hanrahan
gh pr create --title "Generate CLI, MCP, SDK, and skill stubs from the IDL (#35)" --body "$(cat <<'EOF'
## Summary
- Adds `cairn-codegen` library + binary; emits SDK / CLI / MCP / skill artefacts from the IDL.
- Generated outputs committed under each consumer crate; `--check` mode gates drift in CI.
- Adds the only IDL change: `x-cairn-discriminator` on `RetrieveArgs` (additive, gated by a new test).

## Test plan
- [ ] `cargo nextest run --workspace`
- [ ] `cargo run -p cairn-idl --bin cairn-codegen -- --check` (clean)
- [ ] `cargo run -p cairn-idl --bin cairn-codegen` (twice; second run clean)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `./scripts/check-core-boundary.sh`

Closes #35.

Spec: `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md`.
Plan: `docs/superpowers/plans/2026-04-24-cairn-codegen.md`.
EOF
)"
```

Expected: `gh pr create` prints the new PR URL.

---

## Self-review (run before claiming done)

After all tasks merge, the engineer should re-read `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md` and confirm every section maps to a task here:

| Spec section | Tasks |
|---|---|
| §3 Architecture (layout, generator structure) | 3 |
| §4.1 Loader | 4, 5 |
| §4.2 IR + lowering rules | 6, 8, 9, 10, 11, 12 |
| §4.3 `x-cairn-discriminator` IDL extension | 7 |
| §4.4 Emitters | 14, 15, 16, 17 |
| §5.1 Determinism contract | 13 (`fmt`), 18 (sort + atomic), 22 (5-run test) |
| §5.2 `--check` mode | 19, 25 |
| §5.3 CI gate | 26 |
| §5.4 Tests | 4, 5, 8–12, 14–17, 22, 23, 24, 25 |
| §6 Maintainer docs | 27, 28 |
| §7 Verification commands | 29 |
| §8 Scope vs. adjacent issues | (PR description) |
| §9 Risks | (PR description) |

If any spec section has no covering task, file a follow-up task before merge.
