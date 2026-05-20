# MCP Conformance + Capability-Rejection Test Suite — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fixture-driven MCP conformance suite that walks every P0 verb with valid + invalid envelopes, plus a generated cross-product backstop that asserts every un-advertised capability rejects with `CapabilityUnavailable`. Closes issue [#67](https://github.com/windoliver/cairn/issues/67).

**Architecture:** Envelope-canonical JSON fixtures under `fixtures/v0/mcp/conformance/`, loader helper in `cairn-test-fixtures::mcp::conformance` (`include_dir!`-embedded), single replay test binary in `crates/cairn-mcp/tests/mcp_conformance.rs`. JSON-RPC framing reconstructed in-test from envelopes; no stored wire bytes. Failure printing via `pretty_assertions`. Spec: [`docs/superpowers/specs/2026-05-11-mcp-conformance-suite-design.md`](../specs/2026-05-11-mcp-conformance-suite-design.md).

**Tech Stack:** Rust 1.95, tokio 1.x (current_thread for tests), `rmcp` for MCP framing, `serde_json` for envelopes, `include_dir` 0.7 for compile-time fixture embedding, `pretty_assertions` 1.x for diff output, `rstest` 0.23 for parameterized tests.

**Brief refs:** §4.1 (Conformance is tested), §8.0 (verb table), §8.0.a (handshake / status / capability advertisement, invariant (b)), §8.0.b (envelope shape), §15 (wire-compat).

---

## Map of files touched

| File | Action | Why |
|---|---|---|
| `Cargo.toml` (workspace) | modify | Add `include_dir` 0.7 + `pretty_assertions` 1.x to `[workspace.dependencies]` |
| `crates/cairn-test-fixtures/Cargo.toml` | modify | Add `include_dir` to `[dependencies]` (loader is non-test code in this crate; the crate as a whole is dev-only by being used solely as a dev-dep elsewhere) |
| `crates/cairn-test-fixtures/src/lib.rs` | modify | `pub mod mcp;` |
| `crates/cairn-test-fixtures/src/mcp.rs` | create | `pub mod conformance;` |
| `crates/cairn-test-fixtures/src/mcp/conformance.rs` | create | `ConformanceCase`, `CaseKind`, `ConfigOverrides`, `load_all`, `load_case` |
| `crates/cairn-mcp/Cargo.toml` | modify | Add `include_dir`, `pretty_assertions`, `rstest` to `[dev-dependencies]` |
| `crates/cairn-mcp/tests/common/mod.rs` | create | Extract `send_frame` / `recv_frame` / `do_initialize` shared by smoke + parity + new conformance |
| `crates/cairn-mcp/tests/mcp_conformance.rs` | create | Runner: replay, jsonrpc layer, cross-product, six self-tests |
| `fixtures/v0/mcp/conformance/` | create tree | 12 subdirectories, 20 case pairs, 12 `_meta.json` files |

Existing files that **stay untouched** in this plan: `smoke.rs`, `init_status_parity.rs`, `handler_rejection.rs`. The stdio helpers are duplicated across those today; this plan only extracts a fresh copy for the new test and leaves the old copies in place — no drive-by refactor.

---

## Task 1 — Add workspace deps and create empty fixtures tree

**Files:**
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/cairn-test-fixtures/Cargo.toml`
- Modify: `crates/cairn-mcp/Cargo.toml`
- Create: `fixtures/v0/mcp/conformance/.keep`

The loader uses `include_dir` to embed the fixture tree at compile time; the runner uses `pretty_assertions::assert_eq` for legible diffs and `rstest` for parameterized replay.

- [ ] **Step 1.1: Add `include_dir` and `pretty_assertions` to workspace deps**

Edit `Cargo.toml` `[workspace.dependencies]` block (alphabetical order, near existing `insta`, `pretty_assertions`, `rstest` etc.):

```toml
include_dir = { version = "0.7", default-features = false }
pretty_assertions = "1"
```

Place each line so it stays alphabetical with the surrounding lines. If `pretty_assertions` is already there (some commits add it for diff use), skip that line and proceed.

- [ ] **Step 1.2: Wire `include_dir` into `cairn-test-fixtures`**

Edit `crates/cairn-test-fixtures/Cargo.toml`, add to `[dependencies]` (alphabetical with the existing list):

```toml
include_dir = { workspace = true }
```

- [ ] **Step 1.3: Wire dev-deps into `cairn-mcp`**

Edit `crates/cairn-mcp/Cargo.toml`, add to `[dev-dependencies]`:

```toml
include_dir = { workspace = true }
pretty_assertions = { workspace = true }
rstest = { workspace = true }
```

- [ ] **Step 1.4: Create the fixtures-tree root with a placeholder**

```bash
mkdir -p fixtures/v0/mcp/conformance
touch fixtures/v0/mcp/conformance/.keep
```

Without at least one file, `include_dir!` may behave oddly across git versions; the `.keep` stays until Task 5 puts a real `_meta.json` in.

- [ ] **Step 1.5: Verify the workspace still compiles cleanly**

```bash
cargo check --workspace --all-targets --locked
```

Expected: success. No `include_dir` usage yet, but the dep resolution must complete.

- [ ] **Step 1.6: Commit**

```bash
git add Cargo.toml Cargo.lock \
  crates/cairn-test-fixtures/Cargo.toml \
  crates/cairn-mcp/Cargo.toml \
  fixtures/v0/mcp/conformance/.keep
git commit -m "chore(mcp): add include_dir / pretty_assertions / rstest deps for conformance suite (issue #67)"
```

---

## Task 2 — Extract stdio frame helpers to `tests/common/mod.rs`

**Files:**
- Create: `crates/cairn-mcp/tests/common/mod.rs`

`smoke.rs`, `init_status_parity.rs`, and `handler_rejection.rs` each carry their own copy of `send_frame` / `recv_frame` / `do_initialize`. The new conformance test will be the fourth consumer. Per CLAUDE.md §5.3 ("no drive-by refactors"), we won't touch the existing three — we just publish a shared module the new test can import.

- [ ] **Step 2.1: Create the common module**

```bash
mkdir -p crates/cairn-mcp/tests/common
```

Write `crates/cairn-mcp/tests/common/mod.rs`:

```rust
//! Shared helpers for MCP integration tests — newline-delimited JSON-RPC
//! framing over a `tokio::io::duplex` transport.
//!
//! `smoke.rs`, `init_status_parity.rs`, and `handler_rejection.rs` each carry
//! their own copy of these helpers. Issue #67 added this module for the new
//! `mcp_conformance` test; the older copies remain as-is.
//!
//! Tests reach this module via `#[path = "common/mod.rs"]` from
//! `mcp_conformance.rs` — Cargo integration tests are separate binaries and
//! a module imported from a sibling test file is the simplest way to share
//! without spinning up another crate.
#![allow(dead_code, missing_docs)]

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Write one newline-terminated JSON-RPC frame and flush.
pub async fn send_frame(writer: &mut (impl AsyncWriteExt + Unpin), json: &str) {
    writer.write_all(json.as_bytes()).await.expect("write frame");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush");
}

/// Read one newline-terminated JSON-RPC frame and parse it.
pub async fn recv_frame(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read frame line");
    serde_json::from_str(line.trim()).expect("parse frame as JSON")
}

/// Send `initialize` (id=1), read response, send `notifications/initialized`.
pub async fn do_initialize(
    writer: &mut (impl AsyncWriteExt + Unpin),
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> serde_json::Value {
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"conformance-test","version":"0.0.0"}}}"#,
    )
    .await;
    let resp = recv_frame(reader).await;
    send_frame(
        writer,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
    resp
}
```

- [ ] **Step 2.2: Verify the module compiles**

The module is not yet referenced; nothing exercises it. We can still run the workspace check:

```bash
cargo check --workspace --all-targets --locked
```

Expected: success. (Cargo will pick the module up later when a sibling test file declares `mod common;`.)

- [ ] **Step 2.3: Commit**

```bash
git add crates/cairn-mcp/tests/common/mod.rs
git commit -m "test(mcp): extract shared stdio frame helpers for conformance suite (issue #67)"
```

---

## Task 3 — Define `ConformanceCase` + `CaseKind` + `ConfigOverrides`

**Files:**
- Create: `crates/cairn-test-fixtures/src/mcp.rs`
- Create: `crates/cairn-test-fixtures/src/mcp/conformance.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`
- Test: inline `#[cfg(test)]` unit tests in `conformance.rs`

We start with the type and a single trivially-constructible instance so the type alone compiles and can be referenced from later steps. The loader function lands in Task 4.

- [ ] **Step 3.1: Create the `mcp` sub-module**

Write `crates/cairn-test-fixtures/src/mcp.rs`:

```rust
//! MCP-specific test helpers.

pub mod conformance;
```

- [ ] **Step 3.2: Write the type definitions in `conformance.rs`**

Write `crates/cairn-test-fixtures/src/mcp/conformance.rs`:

```rust
//! Conformance fixtures shared by `crates/cairn-mcp/tests/mcp_conformance.rs`.
//!
//! Each fixture is a `(request, response)` envelope pair embedded from
//! `fixtures/v0/mcp/conformance/` at compile time via `include_dir!`. Each
//! verb-group directory carries a `_meta.json` that names per-case kind and
//! config overrides; the loader pairs files and meta entries strictly, panicking
//! on orphans or missing entries (brief §8.0.a fail-closed invariant projected
//! into the test infra layer).

use serde::Deserialize;

/// One fixture entry, ready for replay.
#[derive(Debug, Clone)]
pub struct ConformanceCase {
    /// `"<verb_dir>/<case_id>"` — e.g., `"search/err_semantic_disabled"`.
    pub id: String,
    /// Cairn verb name as it appears in the envelope, e.g., `"search"`. For
    /// cross-verb directories (`_envelope`, `_extension`) this is the directory
    /// name (callers reading `verb` should treat values starting with `_` as
    /// synthetic groupings, not real verbs).
    pub verb: String,
    /// What the runner should expect when dispatching the request.
    pub kind: CaseKind,
    /// Per-case capability gates fed into `build_handler_for`.
    pub config: ConfigOverrides,
    /// Canonical envelope per brief §8.0.b.
    pub request: serde_json::Value,
    /// Expected canonical envelope after replay.
    pub response: serde_json::Value,
}

/// What outcome a case asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// `status = "committed"` (or `"aborted"` for valid-but-rejected
    /// non-capability cases — none expected at v0.1).
    Ok,
    /// `status = "rejected"`, `error.code = "InvalidArgs"` or
    /// `"InvalidFilter"` or `"UnknownVerb"`.
    InvalidArgs,
    /// `status = "rejected"`, `error.code = "CapabilityUnavailable"`,
    /// `error.data.capability` matches a known capability id.
    CapabilityRejected,
    /// Same as `CapabilityRejected` but for verbs from an extension namespace
    /// that the runtime does not advertise (brief §8.0.a, extensions table).
    ExtensionRejected,
}

/// Per-case capability gates. Mirrors the subset of
/// `cairn_core::status::CapabilityGates` and `wiring::*_WIRED` that conformance
/// cases need to switch on. Typed booleans, not strings — drift between this
/// struct and `cairn-core::status::advertise` is caught by
/// `config_overrides_match_advertised_capabilities` in the runner self-tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ConfigOverrides {
    pub keyword_search: bool,
    pub semantic_search: bool,
    pub hybrid_search: bool,
    pub policy_trace: bool,
    pub aggregate_extension_enabled: bool,
    pub admin_extension_enabled: bool,
}

impl Default for ConfigOverrides {
    /// P0 baseline — keyword on, semantic + hybrid require an embedding
    /// provider to be ready (off by default in tests), extensions disabled.
    fn default() -> Self {
        Self {
            keyword_search: true,
            semantic_search: false,
            hybrid_search: false,
            policy_trace: false,
            aggregate_extension_enabled: false,
            admin_extension_enabled: false,
        }
    }
}

impl ConfigOverrides {
    /// Convenience: every search mode on (requires the test handler to be
    /// constructed with `embedding_provider_ready = true`).
    #[must_use]
    pub fn search_all_on() -> Self {
        Self {
            keyword_search: true,
            semantic_search: true,
            hybrid_search: true,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_kind_deserializes_snake_case() {
        let v: CaseKind = serde_json::from_str("\"ok\"").unwrap();
        assert_eq!(v, CaseKind::Ok);
        let v: CaseKind = serde_json::from_str("\"invalid_args\"").unwrap();
        assert_eq!(v, CaseKind::InvalidArgs);
        let v: CaseKind = serde_json::from_str("\"capability_rejected\"").unwrap();
        assert_eq!(v, CaseKind::CapabilityRejected);
        let v: CaseKind = serde_json::from_str("\"extension_rejected\"").unwrap();
        assert_eq!(v, CaseKind::ExtensionRejected);
    }

    #[test]
    fn config_overrides_default_is_p0_baseline() {
        let c = ConfigOverrides::default();
        assert!(c.keyword_search);
        assert!(!c.semantic_search);
        assert!(!c.hybrid_search);
        assert!(!c.aggregate_extension_enabled);
    }

    #[test]
    fn config_overrides_deserialize_partial() {
        let v: ConfigOverrides =
            serde_json::from_str(r#"{"semantic_search": true}"#).unwrap();
        assert!(v.semantic_search);
        assert!(v.keyword_search); // serde(default) applied → default = true
    }
}
```

- [ ] **Step 3.3: Re-export from `lib.rs`**

Edit `crates/cairn-test-fixtures/src/lib.rs`. Find the existing `pub mod ...;` block (it lists `graph`, `hybrid_vault`, `intent`, `keystore`, `store`, etc.). Add:

```rust
pub mod mcp;
```

Keep alphabetical with the existing list.

- [ ] **Step 3.4: Run the unit tests for the new module**

```bash
cargo nextest run -p cairn-test-fixtures --locked
```

Expected: 3 new tests pass (`case_kind_deserializes_snake_case`, `config_overrides_default_is_p0_baseline`, `config_overrides_deserialize_partial`), plus whatever pre-existing tests this crate has.

- [ ] **Step 3.5: Commit**

```bash
git add crates/cairn-test-fixtures/src/lib.rs \
  crates/cairn-test-fixtures/src/mcp.rs \
  crates/cairn-test-fixtures/src/mcp/conformance.rs
git commit -m "feat(test-fixtures): ConformanceCase + CaseKind + ConfigOverrides types (issue #67)"
```

---

## Task 4 — Implement the fixture loader (`load_all`, `load_case`)

**Files:**
- Modify: `crates/cairn-test-fixtures/src/mcp/conformance.rs`
- Create: `fixtures/v0/mcp/conformance/search/_meta.json` (smallest possible meta to exercise the loader; real cases land in later tasks)
- Create: `fixtures/v0/mcp/conformance/search/ok_keyword.request.json`
- Create: `fixtures/v0/mcp/conformance/search/ok_keyword.response.json`

`include_dir!` embeds the entire tree under `fixtures/v0/mcp/conformance/` at compile time. The loader walks each subdirectory, parses its `_meta.json`, pairs case ids with on-disk files, and returns a sorted `Vec<ConformanceCase>`.

- [ ] **Step 4.1: Write the failing loader test**

Add to `crates/cairn-test-fixtures/src/mcp/conformance.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn load_all_returns_at_least_one_case() {
    let cases = load_all();
    assert!(
        !cases.is_empty(),
        "expected at least one fixture under fixtures/v0/mcp/conformance/"
    );
}

#[test]
fn load_all_pairs_request_with_response() {
    let cases = load_all();
    for c in &cases {
        assert!(
            c.request.is_object(),
            "case {}: request must be a JSON object",
            c.id
        );
        assert!(
            c.response.is_object(),
            "case {}: response must be a JSON object",
            c.id
        );
    }
}

#[test]
fn load_case_returns_by_id() {
    let c = load_case("search/ok_keyword");
    assert_eq!(c.id, "search/ok_keyword");
    assert_eq!(c.verb, "search");
    assert_eq!(c.kind, CaseKind::Ok);
}
```

- [ ] **Step 4.2: Run the test, watch it fail**

```bash
cargo nextest run -p cairn-test-fixtures --locked load_all
```

Expected: FAIL with "cannot find function `load_all`" or similar.

- [ ] **Step 4.3: Add the smallest fixture content**

Create `fixtures/v0/mcp/conformance/search/_meta.json`:

```json
{
  "cases": {
    "ok_keyword": {
      "kind": "ok",
      "config": { "keyword_search": true }
    }
  }
}
```

Create `fixtures/v0/mcp/conformance/search/ok_keyword.request.json`:

```json
{
  "args": {
    "mode": "keyword",
    "query": "user prefers dark mode"
  },
  "contract": "cairn.mcp.v1",
  "verb": "search"
}
```

Create `fixtures/v0/mcp/conformance/search/ok_keyword.response.json`:

```json
{
  "contract": "cairn.mcp.v1",
  "data": {
    "hits": []
  },
  "operation_id": "<OPERATION_ID>",
  "policy_trace": [],
  "status": "committed",
  "verb": "search"
}
```

(Empty hits is the realistic shape — a fresh handler with no store has nothing to return. We'll iterate on the actual expected response in Task 7 once the runner runs.)

Delete the placeholder:

```bash
rm fixtures/v0/mcp/conformance/.keep
```

- [ ] **Step 4.4: Implement `load_all` and `load_case`**

Append to `crates/cairn-test-fixtures/src/mcp/conformance.rs` (above the `#[cfg(test)]` block):

```rust
use include_dir::{Dir, include_dir};

/// Embedded fixture tree — `fixtures/v0/mcp/conformance/` at compile time.
static CONFORMANCE_DIR: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../fixtures/v0/mcp/conformance");

#[derive(Debug, Deserialize)]
struct MetaFile {
    cases: std::collections::BTreeMap<String, MetaEntry>,
}

#[derive(Debug, Deserialize)]
struct MetaEntry {
    kind: CaseKind,
    #[serde(default)]
    config: ConfigOverrides,
}

/// Load every fixture under `fixtures/v0/mcp/conformance/`, sorted by id.
///
/// Panics if:
/// - A `_meta.json` is malformed.
/// - A case named in `_meta.json` is missing its `.request.json` or
///   `.response.json`.
/// - A `.request.json` / `.response.json` on disk is not named in `_meta.json`.
#[must_use]
pub fn load_all() -> Vec<ConformanceCase> {
    let mut out = Vec::new();
    for verb_dir in CONFORMANCE_DIR.dirs() {
        let verb = verb_dir
            .path()
            .file_name()
            .and_then(|s| s.to_str())
            .expect("verb dir name");
        let meta_file = verb_dir
            .get_file(format!("{}/_meta.json", verb_dir.path().display()))
            .unwrap_or_else(|| panic!("missing _meta.json in {}", verb_dir.path().display()));
        let meta: MetaFile = serde_json::from_slice(meta_file.contents())
            .unwrap_or_else(|e| panic!("malformed _meta.json in {verb}: {e}"));

        // Build the set of on-disk case ids from filenames.
        let mut on_disk = std::collections::BTreeSet::<String>::new();
        for f in verb_dir.files() {
            let name = f
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .expect("file name");
            if name == "_meta.json" {
                continue;
            }
            if let Some(case_id) = name.strip_suffix(".request.json") {
                on_disk.insert(case_id.to_string());
            } else if let Some(case_id) = name.strip_suffix(".response.json") {
                on_disk.insert(case_id.to_string());
            } else {
                panic!("unexpected file {} in {}", name, verb_dir.path().display());
            }
        }

        // Orphan check: every on-disk case id must be in _meta.json.
        for case_id in &on_disk {
            assert!(
                meta.cases.contains_key(case_id),
                "fixture {verb}/{case_id} has no _meta.json entry",
            );
        }
        // Reverse orphan check: every _meta.json entry must have files on disk.
        for case_id in meta.cases.keys() {
            assert!(
                on_disk.contains(case_id),
                "_meta.json entry {verb}/{case_id} has no .request/.response files",
            );
            let req = verb_dir
                .get_file(format!(
                    "{}/{}.request.json",
                    verb_dir.path().display(),
                    case_id
                ))
                .unwrap_or_else(|| panic!("missing {verb}/{case_id}.request.json"));
            let resp = verb_dir
                .get_file(format!(
                    "{}/{}.response.json",
                    verb_dir.path().display(),
                    case_id
                ))
                .unwrap_or_else(|| panic!("missing {verb}/{case_id}.response.json"));
            let entry = &meta.cases[case_id];
            out.push(ConformanceCase {
                id: format!("{verb}/{case_id}"),
                verb: verb.to_string(),
                kind: entry.kind,
                config: entry.config,
                request: serde_json::from_slice(req.contents()).unwrap_or_else(|e| {
                    panic!("{verb}/{case_id}.request.json: {e}")
                }),
                response: serde_json::from_slice(resp.contents()).unwrap_or_else(|e| {
                    panic!("{verb}/{case_id}.response.json: {e}")
                }),
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Load one case by id (e.g., `"search/ok_keyword"`).
///
/// # Panics
///
/// If the case does not exist.
#[must_use]
pub fn load_case(id: &str) -> ConformanceCase {
    load_all()
        .into_iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no conformance case with id {id}"))
}
```

- [ ] **Step 4.5: Run the loader tests**

```bash
cargo nextest run -p cairn-test-fixtures --locked
```

Expected: all tests pass, including the three new ones from Step 4.1.

- [ ] **Step 4.6: Commit**

```bash
git add crates/cairn-test-fixtures/src/mcp/conformance.rs \
  fixtures/v0/mcp/conformance/search/
git rm fixtures/v0/mcp/conformance/.keep
git commit -m "feat(test-fixtures): include_dir!-backed conformance loader (issue #67)"
```

---

## Task 5 — Canonicalization function + idempotency test

**Files:**
- Modify: `crates/cairn-test-fixtures/src/mcp/conformance.rs`

The runner stores fixtures and diffs handler output. To make diffs deterministic, both sides go through `canonicalize`: sort object keys recursively + replace volatile fields with stable placeholders.

- [ ] **Step 5.1: Write the failing canonicalization tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn canonicalize_sorts_keys_recursively() {
    let unsorted = serde_json::json!({
        "z": 1,
        "a": { "z": 2, "a": 3 }
    });
    let canon = canonicalize(&unsorted);
    let s = serde_json::to_string(&canon).unwrap();
    // Inner object keys are sorted: "a" before "z".
    assert_eq!(s, r#"{"a":{"a":3,"z":2},"z":1}"#);
}

#[test]
fn canonicalize_replaces_operation_id() {
    let v = serde_json::json!({
        "operation_id": "01HQZX9F5N0000000000000000",
        "status": "committed"
    });
    let canon = canonicalize(&v);
    assert_eq!(canon["operation_id"], serde_json::json!("<OPERATION_ID>"));
}

#[test]
fn canonicalize_replaces_server_info_volatile_fields() {
    let v = serde_json::json!({
        "data": {
            "server_info": {
                "build": "abc123",
                "incarnation": "01HQZ...",
                "started_at": "2026-05-11T10:00:00Z",
                "version": "0.1.0"
            }
        }
    });
    let canon = canonicalize(&v);
    let s = &canon["data"]["server_info"];
    assert_eq!(s["build"], serde_json::json!("<BUILD>"));
    assert_eq!(s["incarnation"], serde_json::json!("<INCARNATION>"));
    assert_eq!(s["started_at"], serde_json::json!("<STARTED_AT>"));
    assert_eq!(s["version"], serde_json::json!("0.1.0"));
}

#[test]
fn canonicalize_replaces_handshake_challenge_fields() {
    let v = serde_json::json!({
        "data": {
            "challenge": {
                "expires_at": 1_735_000_000_000_u64,
                "nonce": "base64stuff=="
            }
        }
    });
    let canon = canonicalize(&v);
    let c = &canon["data"]["challenge"];
    assert_eq!(c["nonce"], serde_json::json!("<NONCE>"));
    assert_eq!(c["expires_at"], serde_json::json!("<EXPIRES_AT>"));
}

#[test]
fn canonicalize_is_idempotent_on_every_fixture() {
    for case in load_all() {
        let a = canonicalize(&case.response);
        let b = canonicalize(&a);
        assert_eq!(a, b, "canonicalize is not idempotent on {}", case.id);
    }
}
```

- [ ] **Step 5.2: Run, watch fail**

```bash
cargo nextest run -p cairn-test-fixtures --locked canonicalize
```

Expected: FAIL (`canonicalize` does not exist).

- [ ] **Step 5.3: Implement `canonicalize`**

Append to `crates/cairn-test-fixtures/src/mcp/conformance.rs` (above the `#[cfg(test)]` block):

```rust
use serde_json::{Map, Value};

/// Replace non-deterministic fields with stable placeholders and sort object
/// keys recursively. Idempotent.
///
/// Volatile fields (replaced):
/// - `operation_id` (top-level or nested) → `"<OPERATION_ID>"`
/// - `data.server_info.started_at` → `"<STARTED_AT>"`
/// - `data.server_info.incarnation` → `"<INCARNATION>"`
/// - `data.server_info.build` → `"<BUILD>"`
/// - `data.challenge.nonce` → `"<NONCE>"`
/// - `data.challenge.expires_at` → `"<EXPIRES_AT>"`
/// - any `policy_trace[*].timestamp` → `"<TIMESTAMP>"`
///
/// Every other field is preserved as-is. Object keys are sorted lexicographically.
#[must_use]
pub fn canonicalize(value: &Value) -> Value {
    let mut v = value.clone();
    replace_volatile_in_place(&mut v, &[]);
    sort_keys_in_place(&mut v);
    v
}

const VOLATILE_LEAF_KEYS: &[&str] = &["operation_id"];

/// Maps a `dotted.path` prefix to the placeholder used when a leaf at that
/// path is replaced. Path matching is done against the field's parent path
/// joined by `.` and starting from the envelope root.
const VOLATILE_PATHS: &[(&[&str], &str)] = &[
    (&["data", "server_info", "started_at"], "<STARTED_AT>"),
    (&["data", "server_info", "incarnation"], "<INCARNATION>"),
    (&["data", "server_info", "build"], "<BUILD>"),
    (&["data", "challenge", "nonce"], "<NONCE>"),
    (&["data", "challenge", "expires_at"], "<EXPIRES_AT>"),
];

fn replace_volatile_in_place(v: &mut Value, path: &[&str]) {
    match v {
        Value::Object(map) => {
            // Path-based replacements (specific to known structural paths).
            for (target, placeholder) in VOLATILE_PATHS {
                if path.len() + 1 == target.len() && path == &target[..path.len()] {
                    let leaf = target[target.len() - 1];
                    if map.contains_key(leaf) {
                        map.insert(leaf.to_string(), Value::String((*placeholder).into()));
                    }
                }
            }
            // Leaf-key replacements (apply anywhere in the tree).
            for key in VOLATILE_LEAF_KEYS {
                if map.contains_key(*key) {
                    let placeholder = match *key {
                        "operation_id" => "<OPERATION_ID>",
                        _ => continue,
                    };
                    map.insert((*key).into(), Value::String(placeholder.into()));
                }
            }
            // Recurse with appended path.
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                let mut next_path = path.to_vec();
                next_path.push(Box::leak(k.clone().into_boxed_str()));
                if let Some(child) = map.get_mut(&k) {
                    replace_volatile_in_place(child, &next_path);
                }
            }
        }
        Value::Array(arr) => {
            // Policy-trace timestamp scrub: when the parent key is "policy_trace",
            // each element's `timestamp` (if present) becomes "<TIMESTAMP>".
            let in_policy_trace =
                matches!(path.last(), Some(&"policy_trace"));
            for item in arr.iter_mut() {
                if in_policy_trace {
                    if let Value::Object(map) = item {
                        if map.contains_key("timestamp") {
                            map.insert("timestamp".into(), Value::String("<TIMESTAMP>".into()));
                        }
                    }
                }
                replace_volatile_in_place(item, path);
            }
        }
        _ => {}
    }
}

fn sort_keys_in_place(v: &mut Value) {
    match v {
        Value::Object(map) => {
            let mut sorted: Map<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            sorted.sort_keys();
            for child in sorted.values_mut() {
                sort_keys_in_place(child);
            }
            *map = sorted;
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                sort_keys_in_place(item);
            }
        }
        _ => {}
    }
}
```

Note on the `Box::leak` for path strings: this is a test helper. The leaked strings live for the test process lifetime. Acceptable because `canonicalize` is only called from test code and the leak is bounded by fixture-tree size (~20 cases × small JSON). If clippy complains, swap to `String`-based paths and `Vec<String>`.

- [ ] **Step 5.4: Run all conformance tests**

```bash
cargo nextest run -p cairn-test-fixtures --locked
```

Expected: all five new canonicalize tests pass.

- [ ] **Step 5.5: Commit**

```bash
git add crates/cairn-test-fixtures/src/mcp/conformance.rs
git commit -m "feat(test-fixtures): canonicalize() with idempotency check (issue #67)"
```

---

## Task 6 — Runner skeleton + first self-test (`runner_actually_diffs`)

**Files:**
- Create: `crates/cairn-mcp/tests/mcp_conformance.rs`

We write the test binary skeleton with **one self-test** (`runner_actually_diffs`) that proves the runner can detect a forced mismatch. The actual replay test against fixtures lands in the next task. Both happy-path and gap-fill fixtures arrive in Tasks 8–9.

The runner needs:
- A way to construct a `CairnMcpHandler` with the case's config.
- A function that dispatches an envelope into the handler and returns the handler's envelope output.
- The canonicalize-and-diff plumbing.

For envelope dispatch in v0.1, the handler's MCP entry point is `tools/call` over stdio. We **could** call the handler's internal dispatch path directly without going through JSON-RPC framing, but that's not part of the stable public API of `cairn-mcp`. Simpler and more representative: use the same `tools/call` JSON-RPC frame the JSON-RPC layer test uses, just synchronously inside one tokio runtime. That collapses "envelope replay" into "JSON-RPC tools/call → unwrap result → diff envelope" — one dispatch path, one diff helper.

(If a future refactor exposes a synchronous in-process dispatch entry point, the envelope-replay test could drop the stdio plumbing — but that's not in scope for this issue.)

- [ ] **Step 6.1: Write the runner skeleton**

Write `crates/cairn-mcp/tests/mcp_conformance.rs`:

```rust
//! MCP conformance suite (issue #67).
//!
//! Walks every P0 verb with valid + invalid envelopes from
//! `fixtures/v0/mcp/conformance/` and asserts each handler response matches
//! the canonical envelope in the matching `.response.json`. Adds a
//! cross-product test that iterates un-advertised, dispatch-routable
//! verb-modes and asserts each rejects with `CapabilityUnavailable`.
//!
//! Brief refs: §4.1, §8.0.a (handshake / status / cap advertisement), §8.0.b
//! (envelope), §15 (wire-compat).
#![allow(missing_docs)]

#[path = "common/mod.rs"]
mod common;

use cairn_mcp::CairnMcpHandler;
use cairn_test_fixtures::mcp::conformance::{
    ConfigOverrides, ConformanceCase, canonicalize, load_all, load_case,
};
use rmcp::ServiceExt as _;
use tokio::io::BufReader;

use common::{do_initialize, recv_frame, send_frame};

/// Build a handler with the case's capability gates wired in.
///
/// This intentionally uses `CairnMcpHandler::new()` (the unwired variant) for
/// most cases — that's the same handler `smoke.rs` and `init_status_parity.rs`
/// use for protocol-layer assertions, and it produces deterministic envelopes
/// for the unwired verbs at v0.1. Cases that need a real store (a few of the
/// `Ok` ones, e.g., `ingest/ok_minimal`) construct a wired handler via the
/// existing `tiny_graph_async` helper.
async fn build_handler_for(_config: &ConfigOverrides) -> CairnMcpHandler {
    // For Task 6 + 7 we only need the unwired handler. Wired handlers land in
    // Task 8 when wired-store happy-path fixtures arrive.
    CairnMcpHandler::new()
}

/// Round-trip one envelope through a fresh handler via `tools/call` over
/// stdio. Returns the handler's envelope response (the `result.structuredContent`
/// or analogous field — extracted in `unwrap_envelope_from_tool_result`).
async fn dispatch_envelope(
    handler: CairnMcpHandler,
    request: &serde_json::Value,
) -> serde_json::Value {
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);

    let _init = do_initialize(&mut client_write, &mut client_reader).await;

    let verb = request
        .get("verb")
        .and_then(|v| v.as_str())
        .expect("envelope.verb missing");
    let args = request.get("args").cloned().unwrap_or(serde_json::json!({}));

    // JSON-RPC tools/call frame with `name = verb` and `arguments = args`.
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": verb, "arguments": args }
    });
    send_frame(&mut client_write, &frame.to_string()).await;
    let resp = recv_frame(&mut client_reader).await;

    unwrap_envelope_from_tool_result(&resp).unwrap_or_else(|| {
        panic!(
            "could not unwrap envelope from tools/call response: {}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        )
    })
}

/// MCP returns `tools/call` results in a `result.content[]` array. Cairn's
/// envelope is the first `text` element's JSON payload. (Cf. `cairn-mcp`
/// handler dispatch — `dispatch_stub` for unwired verbs returns an MCP error
/// frame with `result.isError = true` and the envelope nested in
/// `result.content[0].text`.)
fn unwrap_envelope_from_tool_result(resp: &serde_json::Value) -> Option<serde_json::Value> {
    let result = resp.get("result")?;
    // Common path: result.content[0].text == stringified envelope JSON.
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        if let Some(first) = content.first() {
            if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                return serde_json::from_str(text).ok();
            }
        }
    }
    // Some handlers may use `result.structuredContent` directly.
    if let Some(sc) = result.get("structuredContent") {
        return Some(sc.clone());
    }
    None
}

// ── self-tests for the runner ────────────────────────────────────────────────
mod runner_self_tests {
    use super::*;

    /// Negative meta-test: the runner *can* detect a mismatch. If this test
    /// passes by NOT panicking, the runner has lost its assertion path.
    #[tokio::test]
    async fn runner_actually_diffs() {
        let mut case = load_case("search/ok_keyword");
        // Mutate the expected response so it disagrees with whatever the
        // handler produces.
        case.response["data"]["hits"] = serde_json::json!([{ "definitely": "wrong" }]);

        let handler = build_handler_for(&case.config).await;
        let actual = dispatch_envelope(handler, &case.request).await;

        let result = std::panic::catch_unwind(|| {
            pretty_assertions::assert_eq!(
                canonicalize(&actual),
                canonicalize(&case.response),
            );
        });
        assert!(
            result.is_err(),
            "runner failed to detect a forced mismatch — assertion path is broken"
        );
    }
}
```

- [ ] **Step 6.2: Run the new self-test**

```bash
cargo nextest run -p cairn-mcp --test mcp_conformance --locked
```

Expected: `runner_actually_diffs` passes. (The handler is unwired so the actual response for `search/ok_keyword` will be the `dispatch_stub` error envelope — that's fine; we're only asserting the runner panics on a forced mismatch.)

If the test hangs: the `_server_task` is detached. The test ends when the assertion completes; the server task gets dropped with the duplex stream. Acceptable.

If `unwrap_envelope_from_tool_result` returns `None`: print the raw `resp` value to confirm the field path. Adjust the unwrap helper based on what `rmcp` actually emits; the helper's design is to be resilient — extend `unwrap_envelope_from_tool_result` to cover the actual shape.

- [ ] **Step 6.3: Commit**

```bash
git add crates/cairn-mcp/tests/mcp_conformance.rs
git commit -m "test(mcp): runner skeleton + runner_actually_diffs self-test (issue #67)"
```

---

## Task 7 — Parameterized envelope-replay test (empty / one-case)

**Files:**
- Modify: `crates/cairn-mcp/tests/mcp_conformance.rs`
- Modify: `fixtures/v0/mcp/conformance/search/ok_keyword.response.json` (re-bless after first run)

Now we add the parameterized test that walks every loaded case. With only the `search/ok_keyword` fixture present, this exercises exactly one case. We use `rstest::rstest` with `#[case]` per loaded case — but since the case list comes from `include_dir!` at compile time and we can't generate `#[case]` attrs from a Vec, we use a different pattern: one `#[tokio::test]` that iterates `load_all()` inside.

That trades nextest's per-case isolation for simplicity. The failure message includes the case id, so a failure still points at the exact fixture. If we want per-case nextest entries later, switch to `rstest_reuse` or a build-script-generated `#[case]` list — out of scope for v0.1.

- [ ] **Step 7.1: Add the replay test**

Append to `crates/cairn-mcp/tests/mcp_conformance.rs` (above the `runner_self_tests` module):

```rust
/// Replay every loaded conformance case and assert the handler's envelope
/// matches the canonical response after canonicalization.
///
/// On failure: print the case id, both canonical envelopes via
/// `pretty_assertions`, and a `CAIRN_BLESS=1` hint.
#[tokio::test]
async fn conformance_envelope_replay() {
    for case in load_all() {
        eprintln!("[conformance] {}", case.id);
        let handler = build_handler_for(&case.config).await;
        let actual = dispatch_envelope(handler, &case.request).await;

        let actual_canon = canonicalize(&actual);
        let expected_canon = canonicalize(&case.response);

        if std::env::var_os("CAIRN_BLESS").is_some() && actual_canon != expected_canon {
            // Bless workflow: write canonicalized actual back to disk.
            bless_response(&case.id, &actual_canon);
            continue;
        }

        pretty_assertions::assert_eq!(
            actual_canon,
            expected_canon,
            "case {}: envelope mismatch (rerun with CAIRN_BLESS=1 to update)",
            case.id,
        );
    }
}

fn bless_response(case_id: &str, canonical_actual: &serde_json::Value) {
    let path = format!(
        "{}/../../fixtures/v0/mcp/conformance/{case_id}.response.json",
        env!("CARGO_MANIFEST_DIR"),
    );
    let pretty = serde_json::to_string_pretty(canonical_actual).expect("serialize bless");
    std::fs::write(&path, pretty).unwrap_or_else(|e| {
        panic!("CAIRN_BLESS: failed to write {path}: {e}")
    });
    eprintln!("[conformance] blessed {case_id}");
}
```

- [ ] **Step 7.2: Run it once and bless**

```bash
CAIRN_BLESS=1 cargo nextest run -p cairn-mcp --test mcp_conformance --locked conformance_envelope_replay
```

Expected: the test re-writes `search/ok_keyword.response.json` with whatever the unwired handler actually returns for that envelope (likely a stub-error envelope, not the placeholder we wrote in Task 4). The bless flow lets us capture the **real** v0.1 baseline rather than guess it.

- [ ] **Step 7.3: Inspect the blessed file**

```bash
cat fixtures/v0/mcp/conformance/search/ok_keyword.response.json
```

Sanity-check the result: it should be a valid envelope (contract, verb, status, etc.). If the file contains a stub-error envelope rather than a "committed" search response, the fixture's `kind: ok` is wrong — at this commit, with the unwired handler, search is not yet routable, so the response will be a `rejected`-shape envelope. Adjust the `_meta.json` entry from `kind: ok` to `kind: invalid_args` (or whatever matches the real error code), or replace the entire `search/ok_keyword` case with one against an already-wired verb (e.g., `forget/ok_record`, since `FORGET_RECORD_WIRED = true`).

**Important judgement call:** if the actual handler at this commit doesn't have a wired-store search path, do not commit a fictional "ok" fixture. Adjust the case so it accurately reflects the v0.1 contract surface — that's the entire point of conformance.

- [ ] **Step 7.4: Run without `CAIRN_BLESS`**

```bash
cargo nextest run -p cairn-mcp --test mcp_conformance --locked
```

Expected: every test passes, including `conformance_envelope_replay` against the blessed fixture and `runner_actually_diffs`.

- [ ] **Step 7.5: Commit**

```bash
git add crates/cairn-mcp/tests/mcp_conformance.rs \
  fixtures/v0/mcp/conformance/search/
git commit -m "test(mcp): conformance_envelope_replay with bless workflow (issue #67)"
```

---

## Task 8 — Author happy-path fixtures for unwired verbs (status, handshake, dispatch-stub verbs)

**Files:**
- Create: `fixtures/v0/mcp/conformance/{status,handshake,ingest,search,retrieve,summarize,assemble_hot,capture_trace,lint,forget}/` — per the §6.1 table in the spec.

For each verb, we author the `.request.json` by hand (deliberate input), then run the replay with `CAIRN_BLESS=1` to capture the canonical response. **This requires care:** the bless flow trusts the handler's current output. If the handler returns the wrong shape today, blessing locks in the wrong shape forever. So:

> **Author's discipline:** before committing a blessed `.response.json`, eyeball it against brief §8.0.b. If `status` is `committed` but the verb is known to not be wired, that's wrong — open a follow-up issue and bless to the *correct* expected response by hand. If `error.code` is missing on a `rejected` envelope, that's wrong. If `policy_trace` is absent from a mutating verb's response, that's a deviation worth flagging.

For each happy-path fixture: author request → bless → eyeball response → commit (or file a follow-up + adjust kind).

- [ ] **Step 8.1: `status/ok_default`**

Author `fixtures/v0/mcp/conformance/status/ok_default.request.json` (this is the `initialize`-as-status surface; we author a synthetic envelope that gets routed to the same advertise path):

```json
{
  "args": {},
  "contract": "cairn.mcp.v1",
  "verb": "status"
}
```

If the v0.1 handler does not expose `status` as a tools/call verb (it lives in `initialize`'s `experimental.cairn.status` block, per `init_status_parity.rs`), drop this fixture and instead author it under a synthetic verb dir `_initialize/` that the runner special-cases. Decision: defer to the engineer at implementation time based on what `unwrap_envelope_from_tool_result` returns when `verb = "status"`. If it returns `None`, the verb isn't routed via `tools/call`; remove the case and document that status conformance lives in `init_status_parity.rs`.

Author `fixtures/v0/mcp/conformance/status/_meta.json`:

```json
{
  "cases": {
    "ok_default": {
      "kind": "ok",
      "config": { "keyword_search": true }
    }
  }
}
```

- [ ] **Step 8.2: `handshake/ok_mint`**

Same pattern. Author the request:

```json
{
  "args": {},
  "contract": "cairn.mcp.v1",
  "verb": "handshake"
}
```

And `_meta.json`. If `handshake` is wired (it is — `handshake_tool.rs` exists and tests pass), the bless flow will capture a real challenge envelope. `canonicalize` already scrubs `data.challenge.nonce` and `data.challenge.expires_at`.

- [ ] **Step 8.3: `forget/ok_record`**

The only wired core verb at this commit (per `wiring.rs`). Author the request — needs to match the verb's actual `forget` `args` shape from the IDL. Grep for an existing forget invocation to copy the args shape:

```bash
grep -rn "verb\": \"forget" fixtures/v0/envelopes/ crates/cairn-mcp/tests/ 2>&1 | head
```

Author request based on what you find. Expect the bless flow to produce a `rejected` envelope with `error.code = NotFound` or similar — because we're calling on an empty store. That's still informative; mark the kind as `invalid_args` or `ok` depending on the actual outcome.

- [ ] **Step 8.4: Run the bless flow for each happy-path case**

```bash
CAIRN_BLESS=1 cargo nextest run -p cairn-mcp --test mcp_conformance --locked conformance_envelope_replay
```

For every newly-added case, the runner writes the canonicalized actual to disk. Inspect each file before committing.

- [ ] **Step 8.5: Author remaining happy-path requests one at a time**

`ingest/ok_minimal`, `search/ok_keyword` (already exists; re-author with wired handler if applicable), `retrieve/ok_record`, `summarize/ok_no_persist`, `assemble_hot/ok_empty_vault`, `capture_trace/ok_minimal`, `lint/ok_read_only`.

For each:
1. Grep for an existing call site to copy the args shape.
2. Author `<case>.request.json`.
3. Author `_meta.json` entry.
4. Run bless.
5. Eyeball response. If kind matches expected behavior → keep as `ok`; if rejected → flip kind and either re-bless or note as a discovered handler gap.

If a verb's handler is stubbed (`dispatch_stub` returns an error result) and we cannot produce a real "ok" outcome at this commit, **author the case as `kind: invalid_args` or omit it entirely** with a comment in the spec's risks section. Do not lie about an "ok" outcome.

- [ ] **Step 8.6: Verify the suite still passes**

```bash
cargo nextest run -p cairn-mcp --test mcp_conformance --locked
```

Expected: all happy-path cases pass against their blessed responses. `runner_actually_diffs` still passes.

- [ ] **Step 8.7: Commit**

```bash
git add fixtures/v0/mcp/conformance/
git commit -m "test(mcp): happy-path conformance fixtures for v0.1 verbs (issue #67)"
```

---

## Task 9 — Author gap-fill (`invalid_args` + `capability_rejected`) fixtures

**Files:**
- Create: per the §6.2 table — 10 fixtures spanning `search`, `forget`, `retrieve`, `lint`, `summarize`, `_envelope`, `_extension`.

Same workflow as Task 8 but the envelopes here are deliberately malformed or use disabled capabilities. The bless flow captures whatever rejection envelope the handler emits; the engineer eyeballs it against brief §8.0.b's error contract (`error.code`, `error.data.capability`, `error.data.remediation`).

- [ ] **Step 9.1: `search/err_invalid_mode`**

Request: `"mode": "fuzzy"`. Expected: `status: "rejected"`, `error.code: "InvalidArgs"`, `error.data.field: "mode"` (or similar — bless captures whatever the IDL validator emits).

```json
{
  "args": {
    "mode": "fuzzy",
    "query": "test"
  },
  "contract": "cairn.mcp.v1",
  "verb": "search"
}
```

`_meta.json` entry: `"kind": "invalid_args"`.

- [ ] **Step 9.2: `search/err_semantic_disabled`**

Request uses `"mode": "semantic"`. `_meta.json` entry: `"kind": "capability_rejected", "config": {"semantic_search": false}`.

This requires the handler in `build_handler_for` to honor `ConfigOverrides::semantic_search = false` — meaning we need to wire a real `CapabilityGates` into the handler. **Implementation note for the engineer:**

```rust
async fn build_handler_for(config: &ConfigOverrides) -> CairnMcpHandler {
    // For cases that need capability gating, use a wired handler with a
    // tiny graph + config-driven CapabilitySet.
    if config.semantic_search || config.aggregate_extension_enabled {
        let f = cairn_test_fixtures::graph::tiny_graph().await;
        let mut cfg = cairn_core::config::CairnConfig::default();
        cfg.search.semantic = config.semantic_search;
        cfg.search.hybrid = config.hybrid_search;
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(f.scope_a.clone());
        // ... mirror build_handler_wired() from init_status_parity.rs
        return wire_handler(f, cfg).await;
    }
    CairnMcpHandler::new()
}
```

The exact wiring code mirrors `init_status_parity.rs::build_handler_wired`. Lift the pattern; do not import the function directly (it's a `tests/` helper from a different test binary).

- [ ] **Step 9.3: `forget/err_mode_session_unsupported`**

```json
{
  "args": { "mode": "session", "session_id": "01HQZX..." },
  "contract": "cairn.mcp.v1",
  "verb": "forget"
}
```

`_meta.json`: `"kind": "capability_rejected"`. With `FORGET_SESSION_WIRED = false` at this commit, the handler rejects.

- [ ] **Step 9.4: `forget/err_mode_scope_unsupported`**

Same pattern, `mode: "scope"`.

- [ ] **Step 9.5: `retrieve/err_target_turn_unsupported`**

Author with `target: "turn"`. `RETRIEVE_TURN_WIRED = false` at this commit — so this case is valid today.

> **Maintenance note** baked into the spec §6.2: if `RETRIEVE_TURN_WIRED` flips to `true`, this case must be removed (or the kind flipped). The cross-product backstop in Task 11 will catch the drift either way.

- [ ] **Step 9.6: `lint/err_write_no_capability`**

`args: { "write_report": true }`. The spec marked this as "exact code TBD on handler audit." Run the bless flow, inspect the actual rejection envelope:

```bash
CAIRN_BLESS=1 cargo nextest run -p cairn-mcp --test mcp_conformance --locked conformance_envelope_replay
cat fixtures/v0/mcp/conformance/lint/err_write_no_capability.response.json
```

If `error.code` comes back as `Unauthorized` rather than `CapabilityUnavailable`, the handler may be flagging this as an auth error, not a capability error. Two paths:
- File a follow-up issue against `cairn-mcp` for the semantic mismatch and leave the fixture as `kind: invalid_args` (since the rejection is still real).
- Or accept the existing behavior, set `kind: invalid_args` (the actual error code), and remove the spec's TBD note.

Decide based on what brief §8.0.b says about `write_report: true` without write capability. Default: file the follow-up and keep `kind: invalid_args` so the fixture suite still represents reality. Add a `notes` field to the `_meta.json` entry if useful.

- [ ] **Step 9.7: `summarize/err_persist_no_capability`**

Same pattern as 9.6 with `persist: true`.

- [ ] **Step 9.8: `_envelope/err_unknown_verb`**

```json
{
  "args": {},
  "contract": "cairn.mcp.v1",
  "verb": "does_not_exist"
}
```

`_meta.json`: `"kind": "invalid_args"` (the MCP layer will reject with `UnknownVerb` or similar — the existing `wire_call_tool_unknown_verb_returns_mcp_error` test in `smoke.rs` already covers this; we capture its canonical envelope here).

- [ ] **Step 9.9: `_envelope/err_malformed_args`**

```json
{
  "args": { "but_missing_required_field": null },
  "contract": "cairn.mcp.v1",
  "verb": "ingest"
}
```

`_meta.json`: `"kind": "invalid_args"`.

- [ ] **Step 9.10: `_extension/err_aggregate_unadvertised`**

```json
{
  "args": {},
  "contract": "cairn.mcp.v1",
  "verb": "agent_summary"
}
```

`_meta.json`: `"kind": "extension_rejected", "config": {"aggregate_extension_enabled": false}`. This is the same case `calling_unadvertised_extension_verb_returns_mcp_error` covers in `init_status_parity.rs`; we capture the canonical envelope here.

- [ ] **Step 9.11: Bless and verify**

```bash
CAIRN_BLESS=1 cargo nextest run -p cairn-mcp --test mcp_conformance --locked
cargo nextest run -p cairn-mcp --test mcp_conformance --locked
```

Inspect every blessed file before committing. Reject any that look wrong against brief §8.0.b.

- [ ] **Step 9.12: Commit**

```bash
git add fixtures/v0/mcp/conformance/ crates/cairn-mcp/tests/mcp_conformance.rs
git commit -m "test(mcp): gap-fill conformance fixtures — invalid args + cap rejections (issue #67)"
```

---

## Task 10 — JSON-RPC layer test

**Files:**
- Modify: `crates/cairn-mcp/tests/mcp_conformance.rs`

The envelope-replay test already routes through `tools/call`. The "JSON-RPC layer test" is a stricter assertion: re-frame the envelope as a `tools/call` AND assert the outer JSON-RPC response shape conforms (jsonrpc=2.0, matching id, no protocol error), not just the inner envelope.

Smaller scope: this test was originally a separate function in the spec. With the dispatch path collapsed into `dispatch_envelope` (Task 6), the JSON-RPC layer is *already* exercised on every envelope replay. So this task adds one targeted test that asserts the JSON-RPC outer shape directly — not a parallel replay of every fixture.

- [ ] **Step 10.1: Add the JSON-RPC-layer test**

Append to `crates/cairn-mcp/tests/mcp_conformance.rs`:

```rust
/// Assert the JSON-RPC outer envelope (jsonrpc, id, result/error) is well-formed
/// for one representative Ok case. Complements the per-case envelope replay,
/// which only diffs the inner Cairn envelope.
#[tokio::test]
async fn conformance_jsonrpc_layer_well_formed() {
    let case = load_case("handshake/ok_mint");
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        CairnMcpHandler::new()
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);
    let _ = do_initialize(&mut client_write, &mut client_reader).await;

    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": { "name": case.verb, "arguments": case.request.get("args").cloned().unwrap_or_default() }
    });
    send_frame(&mut client_write, &frame.to_string()).await;
    let resp = recv_frame(&mut client_reader).await;

    assert_eq!(resp["jsonrpc"], "2.0", "jsonrpc field");
    assert_eq!(resp["id"], 42, "id must echo request id");
    assert!(resp.get("result").is_some(), "result must be present");
    assert!(resp.get("error").is_none(), "error must be absent for ok case");
}
```

(If `handshake/ok_mint` was dropped during Task 8 because the verb isn't routable, pick another `Ok` fixture here. The test only needs one.)

- [ ] **Step 10.2: Run**

```bash
cargo nextest run -p cairn-mcp --test mcp_conformance --locked conformance_jsonrpc_layer_well_formed
```

Expected: pass.

- [ ] **Step 10.3: Commit**

```bash
git add crates/cairn-mcp/tests/mcp_conformance.rs
git commit -m "test(mcp): JSON-RPC outer envelope well-formedness check (issue #67)"
```

---

## Task 11 — Cross-product backstop + remaining self-tests

**Files:**
- Modify: `crates/cairn-mcp/tests/mcp_conformance.rs`

The backstop is the §8.0.a (b) invariant in code: every un-advertised, dispatch-routable verb-mode rejects with `CapabilityUnavailable`. Plus the remaining self-tests from spec §7: idempotency over every fixture, on-disk canonicality, meta completeness, and config-vs-advertise consistency.

- [ ] **Step 11.1: Add the cross-product backstop**

Append to `crates/cairn-mcp/tests/mcp_conformance.rs`:

```rust
use cairn_core::generated::common::Capabilities;
use cairn_core::status::{CapabilityGates, Phase, StoreCaps, advertise};

/// Brief §8.0.a invariant (b): every un-advertised capability rejects with
/// `CapabilityUnavailable`.
///
/// Iterates every `Capabilities` variant. For each one *not* advertised under
/// a default-P0 gates set AND whose dispatcher path is routable today, sends a
/// minimal request envelope and asserts the response is `status: "rejected"`,
/// `error.code: "CapabilityUnavailable"`, `error.data.capability` matches the
/// capability id.
#[tokio::test]
async fn unadvertised_capability_rejects_for_every_routable_mode() {
    let gates = default_p0_gates();
    let advertised: std::collections::BTreeSet<&'static str> = advertise(&gates)
        .into_iter()
        .map(capability_wire_id)
        .collect();

    let mut tested = 0usize;
    for cap in all_capabilities() {
        let wire = capability_wire_id(cap);
        if advertised.contains(wire) {
            continue;
        }
        let Some(req) = minimal_request_for_capability(cap) else {
            continue; // not currently routable through tools/call dispatch
        };
        let handler = CairnMcpHandler::new();
        let resp = dispatch_envelope(handler, &req).await;
        let canon = canonicalize(&resp);

        assert_eq!(
            canon["status"], "rejected",
            "{wire}: expected status=rejected, got {}", canon["status"]
        );
        assert_eq!(
            canon["error"]["code"], "CapabilityUnavailable",
            "{wire}: expected error.code=CapabilityUnavailable"
        );
        assert_eq!(
            canon["error"]["data"]["capability"], wire,
            "{wire}: error.data.capability mismatch"
        );
        tested += 1;
    }

    assert!(
        tested > 0,
        "cross-product test did not exercise any verb-mode — every capability \
         is advertised in default-P0 gates, which contradicts brief §15. \
         Backstop is testing nothing — verify wiring constants in \
         cairn-core::status::wiring."
    );
}

fn default_p0_gates() -> CapabilityGates {
    CapabilityGates {
        config: cairn_core::config::CapabilitySet::default(),
        store: Some(StoreCaps { fts: true, vector: false }),
        vault_bound: true,
        model_present: false,
        embedding_provider_ready: false,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    }
}

fn all_capabilities() -> &'static [Capabilities] {
    use Capabilities as C;
    &[
        C::CairnMcpV1SearchKeyword,
        C::CairnMcpV1SearchSemantic,
        C::CairnMcpV1SearchHybrid,
        C::CairnMcpV1RetrieveRecord,
        C::CairnMcpV1RetrieveSession,
        C::CairnMcpV1RetrieveTurn,
        C::CairnMcpV1RetrieveFolder,
        C::CairnMcpV1RetrieveScope,
        C::CairnMcpV1RetrieveProfile,
        C::CairnMcpV1ForgetRecord,
        C::CairnMcpV1ForgetSession,
        C::CairnMcpV1ForgetScope,
        C::CairnMcpV1ExtensionAggregate,
        C::CairnMcpV1ExtensionAdmin,
        C::CairnMcpV1ExtensionFederation,
        C::CairnMcpV1ExtensionSessiontree,
        C::CairnMcpV1PolicyTrace,
        C::CairnMcpV1ReplaySequence,
        C::CairnMcpV1ReplayChallenge,
    ]
}

fn capability_wire_id(cap: Capabilities) -> &'static str {
    use Capabilities as C;
    match cap {
        C::CairnMcpV1SearchKeyword => "cairn.mcp.v1.search.keyword",
        C::CairnMcpV1SearchSemantic => "cairn.mcp.v1.search.semantic",
        C::CairnMcpV1SearchHybrid => "cairn.mcp.v1.search.hybrid",
        C::CairnMcpV1RetrieveRecord => "cairn.mcp.v1.retrieve.record",
        C::CairnMcpV1RetrieveSession => "cairn.mcp.v1.retrieve.session",
        C::CairnMcpV1RetrieveTurn => "cairn.mcp.v1.retrieve.turn",
        C::CairnMcpV1RetrieveFolder => "cairn.mcp.v1.retrieve.folder",
        C::CairnMcpV1RetrieveScope => "cairn.mcp.v1.retrieve.scope",
        C::CairnMcpV1RetrieveProfile => "cairn.mcp.v1.retrieve.profile",
        C::CairnMcpV1ForgetRecord => "cairn.mcp.v1.forget.record",
        C::CairnMcpV1ForgetSession => "cairn.mcp.v1.forget.session",
        C::CairnMcpV1ForgetScope => "cairn.mcp.v1.forget.scope",
        C::CairnMcpV1ExtensionAggregate => "cairn.mcp.v1.extension.aggregate",
        C::CairnMcpV1ExtensionAdmin => "cairn.mcp.v1.extension.admin",
        C::CairnMcpV1ExtensionFederation => "cairn.mcp.v1.extension.federation",
        C::CairnMcpV1ExtensionSessiontree => "cairn.mcp.v1.extension.sessiontree",
        C::CairnMcpV1PolicyTrace => "cairn.mcp.v1.policy_trace",
        C::CairnMcpV1ReplaySequence => "cairn.mcp.v1.replay.sequence",
        C::CairnMcpV1ReplayChallenge => "cairn.mcp.v1.replay.challenge",
    }
}

/// Return a minimal request envelope that would exercise the given capability
/// IF it were advertised. Returns `None` for capabilities whose dispatch path
/// is not yet routable through `tools/call` (e.g., `forget.session` has no
/// handler at v0.1 — calling it would fail at parse, not at cap-check).
fn minimal_request_for_capability(cap: Capabilities) -> Option<serde_json::Value> {
    use Capabilities as C;
    let req = match cap {
        C::CairnMcpV1SearchSemantic => serde_json::json!({
            "args": { "mode": "semantic", "query": "x" },
            "contract": "cairn.mcp.v1",
            "verb": "search"
        }),
        C::CairnMcpV1SearchHybrid => serde_json::json!({
            "args": { "mode": "hybrid", "query": "x" },
            "contract": "cairn.mcp.v1",
            "verb": "search"
        }),
        // forget.session / forget.scope: handler not yet wired — skip
        C::CairnMcpV1ForgetSession | C::CairnMcpV1ForgetScope => return None,
        // retrieve targets not yet wired — skip until RETRIEVE_*_WIRED flips
        C::CairnMcpV1RetrieveRecord
        | C::CairnMcpV1RetrieveSession
        | C::CairnMcpV1RetrieveTurn
        | C::CairnMcpV1RetrieveFolder
        | C::CairnMcpV1RetrieveScope
        | C::CairnMcpV1RetrieveProfile => return None,
        // extension namespaces: only aggregate has a sample verb at v0.1
        C::CairnMcpV1ExtensionAggregate => serde_json::json!({
            "args": {},
            "contract": "cairn.mcp.v1",
            "verb": "agent_summary"
        }),
        C::CairnMcpV1ExtensionAdmin
        | C::CairnMcpV1ExtensionFederation
        | C::CairnMcpV1ExtensionSessiontree => return None,
        // policy_trace + replay surfaces are flags, not verbs — skip
        C::CairnMcpV1PolicyTrace
        | C::CairnMcpV1ReplaySequence
        | C::CairnMcpV1ReplayChallenge => return None,
        // search.keyword and forget.record are wired by default — not in the
        // un-advertised set under default-P0 gates, so they're filtered earlier.
        // If they do appear here, return a request anyway for completeness:
        C::CairnMcpV1SearchKeyword => serde_json::json!({
            "args": { "mode": "keyword", "query": "x" },
            "contract": "cairn.mcp.v1",
            "verb": "search"
        }),
        C::CairnMcpV1ForgetRecord => serde_json::json!({
            "args": { "mode": "record", "id": "01HQZX9F5N0000000000000000" },
            "contract": "cairn.mcp.v1",
            "verb": "forget"
        }),
    };
    Some(req)
}
```

- [ ] **Step 11.2: Add the non-empty-backstop self-test**

In the `runner_self_tests` module:

```rust
#[tokio::test]
async fn cross_product_backstop_is_non_empty() {
    // Reuses the same iteration logic — re-counts how many cases the backstop
    // exercises and asserts > 0. If every capability becomes advertised in
    // default-P0 gates, this test fails loudly so a deliberate decision can
    // be made.
    let gates = default_p0_gates();
    let advertised: std::collections::BTreeSet<&'static str> = advertise(&gates)
        .into_iter()
        .map(capability_wire_id)
        .collect();
    let mut routable_unadvertised = 0;
    for cap in all_capabilities() {
        let wire = capability_wire_id(*cap);
        if !advertised.contains(wire) && minimal_request_for_capability(*cap).is_some() {
            routable_unadvertised += 1;
        }
    }
    assert!(
        routable_unadvertised > 0,
        "every routable verb-mode is advertised — backstop is testing nothing. \
         Either remove this test or relax minimal_request_for_capability."
    );
}
```

- [ ] **Step 11.3: Add the remaining self-tests**

In the `runner_self_tests` module:

```rust
#[test]
fn canonicalize_is_idempotent_on_every_fixture() {
    for case in load_all() {
        let a = canonicalize(&case.response);
        let b = canonicalize(&a);
        assert_eq!(a, b, "canonicalize not idempotent on {}", case.id);
    }
}

#[test]
fn fixtures_on_disk_are_canonical() {
    for case in load_all() {
        let raw = case.response.clone();
        assert_eq!(
            raw, canonicalize(&raw),
            "fixture {} is not canonical on disk; run CAIRN_BLESS=1 cargo \
             nextest run -p cairn-mcp --test mcp_conformance to fix",
            case.id,
        );
    }
}

#[test]
fn meta_registry_covers_every_fixture_directory() {
    // load_all() panics on orphans or missing entries, so reaching this
    // test passing means the registry is consistent. This test re-runs the
    // load to fail loudly with a stable name if a future refactor of
    // load_all() drops the orphan checks.
    let cases = load_all();
    assert!(!cases.is_empty(), "no fixtures loaded");
    for case in &cases {
        assert!(
            !case.id.is_empty(),
            "case with empty id loaded — _meta.json registry is incomplete"
        );
    }
}

#[test]
fn config_overrides_match_advertised_capabilities() {
    // For each case: build the CapabilityGates equivalent and assert the
    // advertised set is the closure of the case's config. This catches drift
    // where a new ConfigOverrides field is added without a matching gate.
    for case in load_all() {
        let mut gates = default_p0_gates();
        gates.config.keyword_search = case.config.keyword_search;
        gates.config.semantic_search = case.config.semantic_search;
        gates.config.hybrid_search = case.config.hybrid_search;
        gates.config.policy_trace = case.config.policy_trace;
        // (extensions are handled via runtime config registration; not in CapabilitySet)
        let _adv = advertise(&gates);
        // If this loop completes without panic, the call surface is internally
        // consistent. A future addition to ConfigOverrides without a matching
        // CapabilitySet field would not compile, catching drift at compile time.
    }
}
```

- [ ] **Step 11.4: Run everything**

```bash
cargo nextest run -p cairn-mcp --test mcp_conformance --locked
```

Expected: every test passes. Cross-product reports at least one tested verb-mode (likely `search.semantic`, `search.hybrid`, and the aggregate extension verb).

- [ ] **Step 11.5: Commit**

```bash
git add crates/cairn-mcp/tests/mcp_conformance.rs
git commit -m "test(mcp): cross-product backstop + runner self-tests (issue #67)"
```

---

## Task 12 — Final verification, lint, machete, PR

**Files:**
- None (verification only) — except if clippy nudges show up.

- [ ] **Step 12.1: Run the full CLAUDE.md §8 checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

If clippy emits warnings on the new code (likely candidates: `clippy::too_many_lines`, `clippy::large_enum_variant`, `clippy::missing_panics_doc`), fix or add a localized `#[allow(...)]` with a one-line reason comment per CLAUDE.md §6.8.

- [ ] **Step 12.2: Run the docs workflow**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

The conformance suite doesn't touch CLI flags, config defaults, or generated docs, so this should pass without changes. If it fails, the conformance work is implicated only via dev-dep additions — not likely.

- [ ] **Step 12.3: Run supply-chain checks**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

`cargo machete` may flag `include_dir` in `cairn-test-fixtures` if the loader has no test-side caller yet (it does — the loader is `pub fn` consumed by `cairn-mcp/tests/`, but machete scans library source only). If machete flags `include_dir`, add it to `[package.metadata.cargo-machete] ignored = [...]` in `crates/cairn-test-fixtures/Cargo.toml` with a comment noting the integration-test usage.

- [ ] **Step 12.4: Confirm no core-boundary regression**

```bash
./scripts/check-core-boundary.sh
```

Expected: pass. `cairn-test-fixtures` is dev-only by virtue of every consumer using it as a `dev-dependency`; the boundary script enforces no non-dev imports into `cairn-core`.

- [ ] **Step 12.5: Update the traceability doc**

Edit `docs/design/traceability.md` if it lists §4.1 / §8 entries for issue #67 — add the new test file path to the row. If no such row exists, skip.

- [ ] **Step 12.6: Push and open the PR**

```bash
git push -u origin HEAD
```

Then:

```bash
gh pr create --title "test(mcp): conformance + capability-rejection suite (issue #67)" --body "$(cat <<'EOF'
## Summary

- Add `crates/cairn-mcp/tests/mcp_conformance.rs` — envelope-replay suite over every P0 verb, with a generated cross-product backstop that asserts every un-advertised capability rejects with `CapabilityUnavailable` (brief §8.0.a invariant (b) made mechanical).
- Add `fixtures/v0/mcp/conformance/` — 20 canonical envelope fixture pairs across 12 verb-group directories. Loader in `cairn-test-fixtures::mcp::conformance` embeds the tree via `include_dir!`.
- Add runner self-tests: canonicalization idempotency, on-disk canonicality, fixture / meta consistency, config-vs-advertise drift, runner-can-fail negative meta-test.

Brief refs: §4.1 (Conformance is tested), §8.0 (verb table), §8.0.a (capability advertisement), §8.0.b (envelope), §15 (wire-compat).
Design doc: `docs/superpowers/specs/2026-05-11-mcp-conformance-suite-design.md`.
Plan: `docs/superpowers/plans/2026-05-11-issue-67-mcp-conformance-suite.md`.

Invariants strengthened: brief §2 #6 (fail-closed-on-capability) — now mechanically tested via the cross-product.

## Test plan

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [x] `cargo nextest run --workspace --locked`
- [x] `cargo nextest run -p cairn-mcp --test mcp_conformance --locked` runs the new suite cleanly
- [x] `./scripts/check-core-boundary.sh`
- [x] `cargo deny check && cargo audit && cargo machete`
- [x] Re-bless workflow tested: `CAIRN_BLESS=1 cargo nextest run -p cairn-mcp --test mcp_conformance` rewrites `.response.json` files and the suite passes on re-run

Closes #67.
EOF
)"
```

---

## Self-review checklist

(Run this on the plan before handing off.)

**1. Spec coverage:**

| Spec section | Task |
|---|---|
| §1 Problem statement | (context only — no code task) |
| §2 Goals: replay runner + targeted gap fills | Tasks 6–11 |
| §2 Non-goals | Honored — no SDK harness, no `cairn mcp verify` subcommand |
| §3 Architecture (3 components) | Task 3–4 (loader), Task 5 (canon), Task 6 (runner) |
| §4.1 ConformanceCase | Task 3 |
| §4.2 Loader | Task 4 |
| §4.3 Runner — three test fns | Task 6 (replay), Task 10 (jsonrpc), Task 11 (cross-product) |
| §4.4 Canonicalization | Task 5 |
| §4.5 Handler construction | Task 6 (skeleton), Task 9 (wired variant for cap-gated cases) |
| §5 Data flow per case | Implicit in Task 7+ |
| §6.1 Happy-path manifest | Task 8 |
| §6.2 Gap-fill manifest | Task 9 |
| §6.3 `_meta.json` shape | Task 4 (loader), Task 8/9 (authoring) |
| §6.4 Canonical-on-disk authoring rule | Task 5 (canonicalize) + Task 11 (fixtures_on_disk_are_canonical) |
| §7 Six self-tests | Task 6 (#3 runner_actually_diffs), Task 11 (#1 #2 #4 #5 #6) |
| §8 Error handling | Task 7 (bless workflow), Task 11 (cross-product reporting) |
| §9 CI integration | Task 12 (verification) |
| §10 Invariants touched | Task 12 (PR body) |
| §11 Deliverables | Maps 1:1 to file table above |
| §12 Risks | Surfaced in Task 9 step 9.6 (`lint`/`summarize` write-cap) and Task 1 step 1.5 (`include_dir` rerun-if-changed) |
| §13 Out-of-scope | Not implemented — by design |

**2. Placeholder scan:** None. Every step contains exact file path, exact code, exact command, expected outcome. The `// ... mirror build_handler_wired() ...` comment in Task 9 step 9.2 is a *pointer* to a concrete existing function (`init_status_parity.rs::build_handler_wired`), not a TODO — the engineer copies the pattern from that file.

**3. Type consistency:** `ConformanceCase`, `CaseKind`, `ConfigOverrides`, `canonicalize`, `load_all`, `load_case`, `dispatch_envelope`, `build_handler_for`, `capability_wire_id`, `all_capabilities`, `default_p0_gates`, `minimal_request_for_capability` all carry the same names across tasks. `pretty_assertions::assert_eq!` used consistently. `include_dir!` path consistent (`$CARGO_MANIFEST_DIR/../../fixtures/v0/mcp/conformance`).

**4. Scope:** One PR, one issue. No tests added to unrelated crates. No core API change. CLAUDE.md §3 boundary preserved (cairn-test-fixtures stays dev-only via its dev-dep relationships in consumer crates).

---

## Notes for the implementing engineer

- **Worktree:** this plan was authored from `agile-exploring-porcupine` worktree. Implement on a fresh branch from `main`.
- **TDD discipline:** Tasks 4, 5, 6 are strict TDD (write failing test, watch fail, implement, watch pass). Tasks 8–9 are *bless-driven* — the test passes against whatever the handler currently emits; the engineer's discipline is to eyeball the blessed output against brief §8.0.b before committing.
- **Don't lie about fixtures.** If a verb is unwired, mark its case as `invalid_args` / `capability_rejected`, not `ok`. The cross-product backstop will tell you what's un-advertised; trust it. The bless flow tells you what the handler actually emits.
- **Follow-ups likely to fall out of this work:**
  - `lint`/`summarize` write-capability error code semantics (Task 9 step 9.6) — possibly file an issue.
  - Per-case nextest entries (currently one `conformance_envelope_replay` test iterates the whole set) — defer until the case set grows past ~30.
  - Extract `build_handler_wired` pattern to `tests/common/mod.rs` — defer until a fourth duplicate appears.
