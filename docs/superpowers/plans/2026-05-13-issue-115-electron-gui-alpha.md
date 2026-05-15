# Issue 115 Electron GUI Alpha Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a narrow full vertical slice of the Cairn Electron GUI alpha backed by a Rust localhost API and deterministic fixture vault.

**Architecture:** Add `crates/cairn-desktop` as the GUI backend crate and `frontend/desktop-electron` as the Electron + React renderer. The renderer calls backend JSON endpoints only; the backend owns fixture loading, graph derivation, search/lint responses, and reconcile validation using the existing `FrontendFieldPolicy` boundary from `cairn-core`.

**Tech Stack:** Rust 2024, `axum`, `tokio`, `serde`, `serde_json`, Electron, React, Vite, TypeScript, Vitest, Testing Library.

---

## File Structure

- Create `crates/cairn-desktop/Cargo.toml`: desktop backend crate manifest.
- Create `crates/cairn-desktop/src/lib.rs`: crate module declarations and exports.
- Create `crates/cairn-desktop/src/error.rs`: backend error and JSON error response types.
- Create `crates/cairn-desktop/src/model.rs`: frontend-facing DTOs.
- Create `crates/cairn-desktop/src/fixture.rs`: fixture JSON loader and validation.
- Create `crates/cairn-desktop/src/repository.rs`: in-memory fixture repository, graph/search/lint/reconcile logic.
- Create `crates/cairn-desktop/src/server.rs`: axum router and handlers.
- Create `crates/cairn-desktop/src/bin/cairn-desktop-server.rs`: local server binary for development and Electron.
- Create `crates/cairn-desktop/tests/fixture_backend.rs`: backend behavior tests.
- Create `crates/cairn-desktop/tests/http_api.rs`: HTTP endpoint and smoke tests.
- Modify `Cargo.toml`: add workspace dependencies for `axum`, `tower`, and `tower-http` if needed.
- Create `fixtures/desktop-gui-alpha/vault.json`: deterministic desktop fixture.
- Create `frontend/desktop-electron/package.json`: frontend scripts and dependencies.
- Create `frontend/desktop-electron/index.html`: Vite HTML entry.
- Create `frontend/desktop-electron/electron/main.ts`: Electron main process.
- Create `frontend/desktop-electron/electron/preload.ts`: safe preload boundary.
- Create `frontend/desktop-electron/src/main.tsx`: React entry.
- Create `frontend/desktop-electron/src/App.tsx`: application shell.
- Create `frontend/desktop-electron/src/api/client.ts`: typed API client.
- Create `frontend/desktop-electron/src/api/types.ts`: TypeScript DTO mirrors.
- Create `frontend/desktop-electron/src/components/*.tsx`: focused UI panels.
- Create `frontend/desktop-electron/src/App.test.tsx`: UI state tests.
- Create `frontend/desktop-electron/src/api/client.test.ts`: API client tests.
- Create `frontend/desktop-electron/src/styles.css`: app styling.
- Create `frontend/desktop-electron/README.md`: desktop alpha notes and Tauri alternative.
- Create `frontend/desktop-electron/tsconfig.json`, `vite.config.ts`, `vitest.config.ts`: frontend tooling.

## Task 1: Backend Crate Skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cairn-desktop/Cargo.toml`
- Create: `crates/cairn-desktop/src/lib.rs`
- Create: `crates/cairn-desktop/src/error.rs`
- Test: `crates/cairn-desktop/src/lib.rs`

- [ ] **Step 1: Write the failing crate import test**

Create `crates/cairn-desktop/tests/skeleton.rs` with a minimal integration test that expects the crate to expose the public backend type:

```rust
use cairn_desktop::DesktopBackend;

#[test]
fn desktop_backend_type_is_exported() {
    let backend = DesktopBackend;
    assert_eq!(backend, DesktopBackend);
}
```

- [ ] **Step 2: Run the skeleton test to verify RED**

Run:

```bash
cargo test -p cairn-desktop desktop_backend_type_is_exported
```

Expected: FAIL because `cairn-desktop` does not exist yet.

- [ ] **Step 3: Add the crate manifest**

Create `crates/cairn-desktop/Cargo.toml`:

```toml
[package]
name = "cairn-desktop"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "Local backend for the Cairn desktop GUI alpha."

[dependencies]
cairn-core = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["rt", "macros", "net"] }
tracing = { workspace = true }

[dev-dependencies]
pretty_assertions = { workspace = true }
tempfile = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 4: Add the initial library and error module**

Create `crates/cairn-desktop/src/lib.rs`:

```rust
//! Desktop GUI backend for Cairn.

pub mod error;

/// Minimal exported backend marker used while the alpha backend is built out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopBackend;
```

Create `crates/cairn-desktop/src/error.rs`:

```rust
//! Error types for the desktop GUI backend.

/// Result alias for desktop backend operations.
pub type DesktopResult<T> = Result<T, DesktopError>;

/// Errors produced by the desktop GUI backend.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// Fixture data could not be loaded or parsed.
    #[error("desktop fixture error: {message}")]
    Fixture {
        /// Human-readable fixture failure.
        message: String,
    },
}
```

- [ ] **Step 5: Run the skeleton test to verify GREEN**

Run:

```bash
cargo test -p cairn-desktop desktop_backend_type_is_exported
```

Expected: PASS. If Cargo cannot find `cairn-desktop`, check that the crate is under `crates/` so the existing `members = ["crates/*"]` pattern picks it up.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/cairn-desktop
git commit -m "feat: add desktop backend crate skeleton"
```

## Task 2: Fixture Vault And DTOs

**Files:**
- Create: `fixtures/desktop-gui-alpha/vault.json`
- Create: `crates/cairn-desktop/src/model.rs`
- Create: `crates/cairn-desktop/src/fixture.rs`
- Modify: `crates/cairn-desktop/src/lib.rs`
- Test: `crates/cairn-desktop/tests/fixture_backend.rs`

- [ ] **Step 1: Write failing fixture loader tests**

Create `crates/cairn-desktop/tests/fixture_backend.rs`:

```rust
use cairn_desktop::fixture::DesktopFixture;

#[test]
fn fixture_loads_alpha_vault_records_and_folders() {
    let fixture = DesktopFixture::load_default().expect("fixture loads");

    assert_eq!(fixture.vault.id, "desktop-alpha");
    assert_eq!(fixture.folders.len(), 2);
    assert_eq!(fixture.records.len(), 3);
    assert!(
        fixture
            .records
            .iter()
            .any(|record| record.id == "rec-alpha-001" && record.links == ["rec-alpha-002"])
    );
}

#[test]
fn fixture_contains_lint_and_reconcile_examples() {
    let fixture = DesktopFixture::load_default().expect("fixture loads");

    assert_eq!(fixture.lint_findings.len(), 1);
    assert_eq!(fixture.reconcile_examples.mutable_record_id, "rec-alpha-001");
    assert_eq!(fixture.reconcile_examples.immutable_field, "confidence");
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p cairn-desktop fixture_loads_alpha_vault_records_and_folders fixture_contains_lint_and_reconcile_examples
```

Expected: FAIL because `fixture`, `model`, and the JSON fixture do not exist.

- [ ] **Step 3: Add DTO models**

Create `crates/cairn-desktop/src/model.rs`:

```rust
//! JSON DTOs used by the desktop GUI alpha.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Summary of the loaded desktop vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopVaultSummary {
    /// Stable vault id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Human-readable root path or fixture label.
    pub root: String,
    /// Number of records available to inspect.
    pub record_count: usize,
    /// Number of folders available to inspect.
    pub folder_count: usize,
}

/// Folder shown in the vault inspector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopFolder {
    /// Stable folder id.
    pub id: String,
    /// Folder display name.
    pub name: String,
    /// Parent folder id, when nested.
    pub parent_id: Option<String>,
}

/// Record summary shown in lists and graph nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRecordSummary {
    /// Stable record id.
    pub id: String,
    /// Record title.
    pub title: String,
    /// Owning folder id.
    pub folder_id: String,
    /// Record kind.
    pub kind: String,
    /// Tags projected for the GUI.
    pub tags: Vec<String>,
    /// Optimistic record version.
    pub version: u64,
    /// Confidence score displayed by the inspector.
    pub confidence: f64,
}

/// Full record detail shown in the editor pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRecordDetail {
    /// Stable record id.
    pub id: String,
    /// Record title.
    pub title: String,
    /// Owning folder id.
    pub folder_id: String,
    /// Markdown body.
    pub body: String,
    /// Record kind.
    pub kind: String,
    /// Tags projected for the GUI.
    pub tags: Vec<String>,
    /// Optimistic record version.
    pub version: u64,
    /// Backend projection hash.
    pub backend_hash: String,
    /// Confidence score displayed by the inspector.
    pub confidence: f64,
    /// Source hash displayed by the inspector.
    pub source_hash: String,
    /// Linked record ids.
    pub links: Vec<String>,
}

/// Derived graph response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraph {
    /// Graph nodes.
    pub nodes: Vec<DesktopGraphNode>,
    /// Graph edges.
    pub edges: Vec<DesktopGraphEdge>,
}

/// Derived graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraphNode {
    /// Node id.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Record kind.
    pub kind: String,
    /// Optional group or folder id.
    pub group: String,
}

/// Derived graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraphEdge {
    /// Edge id.
    pub id: String,
    /// Source record id.
    pub source: String,
    /// Target record id.
    pub target: String,
    /// Relationship label.
    pub label: String,
}

/// Search result shown in the search panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSearchResult {
    /// Matched record id.
    pub record_id: String,
    /// Matched title.
    pub title: String,
    /// Snippet with matching text.
    pub snippet: String,
    /// Deterministic fixture score.
    pub score: f64,
}

/// Lint finding shown in the lint panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopLintFinding {
    /// Stable finding id.
    pub id: String,
    /// Severity such as info, warning, or error.
    pub severity: String,
    /// Optional related record.
    pub record_id: Option<String>,
    /// Human-readable message.
    pub message: String,
}

/// Reconcile preview request from the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcilePreviewRequest {
    /// Target record id.
    pub target_id: String,
    /// Expected record version.
    pub expected_version: u64,
    /// Backend hash the edit was based on.
    pub backend_hash: String,
    /// Proposed field diff.
    pub field_diff: BTreeMap<String, serde_json::Value>,
}

/// Reconcile preview response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcilePreview {
    /// Whether the preview can be applied.
    pub accepted: bool,
    /// Target record id.
    pub target_id: String,
    /// Expected record version.
    pub expected_version: u64,
    /// Mutable fields that passed policy.
    pub mutable_diff: BTreeMap<String, serde_json::Value>,
    /// Rejected fields and reason codes.
    pub rejected_fields: Vec<DesktopRejectedField>,
}

/// Rejected reconcile field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRejectedField {
    /// Field name.
    pub field: String,
    /// Stable rejection code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

/// Reconcile apply request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcileApplyRequest {
    /// Preview request to apply.
    #[serde(flatten)]
    pub preview: DesktopReconcilePreviewRequest,
}

/// Reconcile apply result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcileApplyResult {
    /// Whether the apply succeeded.
    pub accepted: bool,
    /// Updated record when accepted.
    pub record: Option<DesktopRecordDetail>,
    /// Rejections when not accepted.
    pub rejected_fields: Vec<DesktopRejectedField>,
}
```

- [ ] **Step 4: Add fixture JSON**

Create `fixtures/desktop-gui-alpha/vault.json`:

```json
{
  "vault": {
    "id": "desktop-alpha",
    "name": "Desktop Alpha Fixture",
    "root": "fixtures/desktop-gui-alpha",
    "recordCount": 3,
    "folderCount": 2
  },
  "folders": [
    { "id": "folder-core", "name": "Core Memories", "parentId": null },
    { "id": "folder-ops", "name": "Operations", "parentId": null }
  ],
  "records": [
    {
      "id": "rec-alpha-001",
      "title": "Project memory scaffold",
      "folderId": "folder-core",
      "body": "Markdown body with [[Reconcile review]].",
      "kind": "skill",
      "tags": ["alpha", "frontend"],
      "version": 2,
      "backendHash": "sha256:fixture-alpha-001",
      "confidence": 0.86,
      "sourceHash": "sha256:source-alpha-001",
      "links": ["rec-alpha-002"]
    },
    {
      "id": "rec-alpha-002",
      "title": "Reconcile review",
      "folderId": "folder-core",
      "body": "Edits must pass through backend validation.",
      "kind": "procedural",
      "tags": ["reconcile", "safety"],
      "version": 1,
      "backendHash": "sha256:fixture-alpha-002",
      "confidence": 0.91,
      "sourceHash": "sha256:source-alpha-002",
      "links": ["rec-alpha-003"]
    },
    {
      "id": "rec-alpha-003",
      "title": "Lint follow-up",
      "folderId": "folder-ops",
      "body": "One stale source hash is intentionally present for lint.",
      "kind": "episodic",
      "tags": ["lint"],
      "version": 4,
      "backendHash": "sha256:fixture-alpha-003",
      "confidence": 0.62,
      "sourceHash": "sha256:source-alpha-stale",
      "links": []
    }
  ],
  "lintFindings": [
    {
      "id": "lint-alpha-001",
      "severity": "warning",
      "recordId": "rec-alpha-003",
      "message": "Source hash is stale relative to the fixture projection."
    }
  ],
  "reconcileExamples": {
    "mutableRecordId": "rec-alpha-001",
    "immutableField": "confidence"
  }
}
```

- [ ] **Step 5: Implement fixture loader**

Create `crates/cairn-desktop/src/fixture.rs`:

```rust
//! Fixture loading for the desktop GUI alpha.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    error::{DesktopError, DesktopResult},
    model::{DesktopFolder, DesktopLintFinding, DesktopRecordDetail, DesktopVaultSummary},
};

/// Built-in fixture path used by tests and local development.
pub const DEFAULT_FIXTURE_PATH: &str = "fixtures/desktop-gui-alpha/vault.json";

/// Complete desktop fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopFixture {
    /// Vault summary.
    pub vault: DesktopVaultSummary,
    /// Fixture folders.
    pub folders: Vec<DesktopFolder>,
    /// Fixture records.
    pub records: Vec<DesktopRecordDetail>,
    /// Fixture lint findings.
    pub lint_findings: Vec<DesktopLintFinding>,
    /// Fixture reconcile examples.
    pub reconcile_examples: DesktopReconcileExamples,
}

/// Fixture examples for reconcile tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcileExamples {
    /// Record id that accepts mutable edits.
    pub mutable_record_id: String,
    /// Field expected to be rejected as immutable.
    pub immutable_field: String,
}

impl DesktopFixture {
    /// Load the default repo fixture.
    pub fn load_default() -> DesktopResult<Self> {
        Self::load_from_path(DEFAULT_FIXTURE_PATH)
    }

    /// Load a fixture from a JSON path.
    pub fn load_from_path(path: impl AsRef<Path>) -> DesktopResult<Self> {
        let path = path.as_ref();
        let body = fs::read_to_string(path).map_err(|source| DesktopError::Fixture {
            message: format!("failed to read {}: {source}", path.display()),
        })?;
        serde_json::from_str(&body).map_err(|source| DesktopError::Fixture {
            message: format!("failed to parse {}: {source}", path.display()),
        })
    }
}
```

Modify `crates/cairn-desktop/src/lib.rs`:

```rust
//! Desktop GUI backend for Cairn.

pub mod error;
pub mod fixture;
pub mod model;
```

- [ ] **Step 6: Run tests to verify GREEN**

Run:

```bash
cargo test -p cairn-desktop fixture_loads_alpha_vault_records_and_folders fixture_contains_lint_and_reconcile_examples
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-desktop fixtures/desktop-gui-alpha
git commit -m "feat: add desktop alpha fixture models"
```

## Task 3: Backend Repository Behavior

**Files:**
- Create: `crates/cairn-desktop/src/repository.rs`
- Modify: `crates/cairn-desktop/src/error.rs`
- Modify: `crates/cairn-desktop/src/lib.rs`
- Modify: `crates/cairn-desktop/tests/fixture_backend.rs`

- [ ] **Step 1: Add failing repository behavior tests**

Append to `crates/cairn-desktop/tests/fixture_backend.rs`:

```rust
use std::collections::BTreeMap;

use cairn_desktop::{
    fixture::DesktopFixture,
    model::DesktopReconcilePreviewRequest,
    repository::DesktopRepository,
};
use serde_json::json;

#[test]
fn repository_derives_graph_edges_from_fixture_links() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let graph = repo.graph();

    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.edges.len(), 2);
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.source == "rec-alpha-001" && edge.target == "rec-alpha-002")
    );
}

#[test]
fn repository_searches_titles_tags_and_body() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let results = repo.search("reconcile");

    assert!(results.iter().any(|result| result.record_id == "rec-alpha-002"));
    assert!(results[0].score >= results.last().expect("result").score);
}

#[test]
fn repository_reconcile_accepts_body_edit() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("body".to_string(), json!("Updated fixture body"));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(preview.accepted);
    assert_eq!(preview.mutable_diff["body"], json!("Updated fixture body"));
    assert!(preview.rejected_fields.is_empty());
}

#[test]
fn repository_reconcile_rejects_immutable_field() {
    let repo = DesktopRepository::from_fixture(DesktopFixture::load_default().expect("fixture"));
    let mut field_diff = BTreeMap::new();
    field_diff.insert("confidence".to_string(), json!(0.99));

    let preview = repo.preview_reconcile(DesktopReconcilePreviewRequest {
        target_id: "rec-alpha-001".to_string(),
        expected_version: 2,
        backend_hash: "sha256:fixture-alpha-001".to_string(),
        field_diff,
    });

    assert!(!preview.accepted);
    assert_eq!(preview.rejected_fields[0].field, "confidence");
    assert_eq!(preview.rejected_fields[0].code, "immutable_field_changed");
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p cairn-desktop repository_
```

Expected: FAIL because `repository` does not exist.

- [ ] **Step 3: Implement repository**

Create `crates/cairn-desktop/src/repository.rs`:

```rust
//! In-memory fixture repository for the desktop GUI alpha.

use std::collections::BTreeMap;

use cairn_core::contract::frontend_adapter::FrontendFieldPolicy;

use crate::{
    fixture::DesktopFixture,
    model::{
        DesktopFolder, DesktopGraph, DesktopGraphEdge, DesktopGraphNode, DesktopLintFinding,
        DesktopRecordDetail, DesktopRecordSummary, DesktopReconcileApplyRequest,
        DesktopReconcileApplyResult, DesktopReconcilePreview, DesktopReconcilePreviewRequest,
        DesktopRejectedField, DesktopSearchResult, DesktopVaultSummary,
    },
};

/// Fixture-backed repository used by the desktop alpha.
#[derive(Debug, Clone)]
pub struct DesktopRepository {
    fixture: DesktopFixture,
}

impl DesktopRepository {
    /// Build a repository from a fixture.
    #[must_use]
    pub fn from_fixture(fixture: DesktopFixture) -> Self {
        Self { fixture }
    }

    /// Return the loaded vault summary.
    #[must_use]
    pub fn vault(&self) -> DesktopVaultSummary {
        self.fixture.vault.clone()
    }

    /// Return all folders.
    #[must_use]
    pub fn folders(&self) -> Vec<DesktopFolder> {
        self.fixture.folders.clone()
    }

    /// Return record summaries.
    #[must_use]
    pub fn records(&self) -> Vec<DesktopRecordSummary> {
        self.fixture
            .records
            .iter()
            .map(|record| DesktopRecordSummary {
                id: record.id.clone(),
                title: record.title.clone(),
                folder_id: record.folder_id.clone(),
                kind: record.kind.clone(),
                tags: record.tags.clone(),
                version: record.version,
                confidence: record.confidence,
            })
            .collect()
    }

    /// Return one record detail by id.
    #[must_use]
    pub fn record(&self, id: &str) -> Option<DesktopRecordDetail> {
        self.fixture
            .records
            .iter()
            .find(|record| record.id == id)
            .cloned()
    }

    /// Return derived graph data.
    #[must_use]
    pub fn graph(&self) -> DesktopGraph {
        let nodes = self
            .fixture
            .records
            .iter()
            .map(|record| DesktopGraphNode {
                id: record.id.clone(),
                label: record.title.clone(),
                kind: record.kind.clone(),
                group: record.folder_id.clone(),
            })
            .collect();

        let edges = self
            .fixture
            .records
            .iter()
            .flat_map(|record| {
                record.links.iter().map(|target| DesktopGraphEdge {
                    id: format!("{}--{}", record.id, target),
                    source: record.id.clone(),
                    target: target.clone(),
                    label: "wikilink".to_string(),
                })
            })
            .collect();

        DesktopGraph { nodes, edges }
    }

    /// Return deterministic fixture search results.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<DesktopSearchResult> {
        let query = query.to_lowercase();
        let mut results: Vec<_> = self
            .fixture
            .records
            .iter()
            .filter_map(|record| {
                let haystack = format!(
                    "{} {} {}",
                    record.title,
                    record.tags.join(" "),
                    record.body
                )
                .to_lowercase();
                if !haystack.contains(&query) {
                    return None;
                }
                let title_hit = record.title.to_lowercase().contains(&query);
                Some(DesktopSearchResult {
                    record_id: record.id.clone(),
                    title: record.title.clone(),
                    snippet: record.body.chars().take(96).collect(),
                    score: if title_hit { 1.0 } else { 0.7 },
                })
            })
            .collect();
        results.sort_by(|left, right| right.score.total_cmp(&left.score));
        results
    }

    /// Return fixture lint findings.
    #[must_use]
    pub fn lint_findings(&self) -> Vec<DesktopLintFinding> {
        self.fixture.lint_findings.clone()
    }

    /// Preview a reconcile request without mutating backend state.
    #[must_use]
    pub fn preview_reconcile(
        &self,
        request: DesktopReconcilePreviewRequest,
    ) -> DesktopReconcilePreview {
        let Some(record) = self.record(&request.target_id) else {
            return rejected_preview(request, "target", "record_not_found", "Record was not found");
        };
        if record.version != request.expected_version {
            return rejected_preview(
                request,
                "version",
                "version_conflict",
                "Record version does not match the projected version",
            );
        }
        if record.backend_hash != request.backend_hash {
            return rejected_preview(
                request,
                "backendHash",
                "target_hash_mismatch",
                "Backend hash does not match the projected record hash",
            );
        }

        let mut mutable_diff = BTreeMap::new();
        let mut rejected_fields = Vec::new();
        for (field, value) in request.field_diff {
            if FrontendFieldPolicy::is_mutable_from_frontend(&field) {
                mutable_diff.insert(field, value);
            } else {
                rejected_fields.push(DesktopRejectedField {
                    field,
                    code: "immutable_field_changed".to_string(),
                    message: "Field is owned by the backend and cannot be changed by the GUI"
                        .to_string(),
                });
            }
        }

        DesktopReconcilePreview {
            accepted: rejected_fields.is_empty(),
            target_id: request.target_id,
            expected_version: request.expected_version,
            mutable_diff,
            rejected_fields,
        }
    }

    /// Apply a reconcile request against the in-memory fixture model.
    #[must_use]
    pub fn apply_reconcile(
        &self,
        request: DesktopReconcileApplyRequest,
    ) -> DesktopReconcileApplyResult {
        let preview = self.preview_reconcile(request.preview);
        if !preview.accepted {
            return DesktopReconcileApplyResult {
                accepted: false,
                record: None,
                rejected_fields: preview.rejected_fields,
            };
        }

        let mut record = self.record(&preview.target_id);
        if let Some(record) = &mut record {
            if let Some(body) = preview.mutable_diff.get("body").and_then(serde_json::Value::as_str)
            {
                record.body = body.to_string();
                record.version += 1;
            }
        }

        DesktopReconcileApplyResult {
            accepted: record.is_some(),
            record,
            rejected_fields: Vec::new(),
        }
    }
}

fn rejected_preview(
    request: DesktopReconcilePreviewRequest,
    field: &str,
    code: &str,
    message: &str,
) -> DesktopReconcilePreview {
    DesktopReconcilePreview {
        accepted: false,
        target_id: request.target_id,
        expected_version: request.expected_version,
        mutable_diff: BTreeMap::new(),
        rejected_fields: vec![DesktopRejectedField {
            field: field.to_string(),
            code: code.to_string(),
            message: message.to_string(),
        }],
    }
}
```

Modify `crates/cairn-desktop/src/lib.rs`:

```rust
//! Desktop GUI backend for Cairn.

pub mod error;
pub mod fixture;
pub mod model;
pub mod repository;
```

- [ ] **Step 4: Run repository tests to verify GREEN**

Run:

```bash
cargo test -p cairn-desktop repository_
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-desktop
git commit -m "feat: add desktop fixture repository"
```

## Task 4: HTTP API And Server Binary

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/cairn-desktop/Cargo.toml`
- Create: `crates/cairn-desktop/src/server.rs`
- Create: `crates/cairn-desktop/src/bin/cairn-desktop-server.rs`
- Modify: `crates/cairn-desktop/src/lib.rs`
- Create: `crates/cairn-desktop/tests/http_api.rs`

- [ ] **Step 1: Add failing HTTP endpoint tests**

Create `crates/cairn-desktop/tests/http_api.rs`:

```rust
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use cairn_desktop::{fixture::DesktopFixture, repository::DesktopRepository, server::router};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
async fn health_and_records_endpoints_return_fixture_data() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    let health = app
        .clone()
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let records = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/records")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(records.status(), StatusCode::OK);
    let body = to_bytes(records.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn reconcile_preview_rejects_immutable_field_over_http() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/reconcile/preview")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "targetId": "rec-alpha-001",
                        "expectedVersion": 2,
                        "backendHash": "sha256:fixture-alpha-001",
                        "fieldDiff": { "confidence": 0.99 }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["accepted"], false);
    assert_eq!(json["rejectedFields"][0]["code"], "immutable_field_changed");
}
```

- [ ] **Step 2: Run HTTP tests to verify RED**

Run:

```bash
cargo test -p cairn-desktop --test http_api
```

Expected: FAIL because `axum`, `tower`, and `server` are not wired.

- [ ] **Step 3: Add workspace HTTP dependencies**

Modify root `Cargo.toml` `[workspace.dependencies]`:

```toml
axum = { version = "0.8", default-features = false, features = ["json", "tokio", "http1"] }
tower = { version = "0.5", default-features = false, features = ["util"] }
tower-http = { version = "0.6", default-features = false, features = ["cors"] }
```

Modify `crates/cairn-desktop/Cargo.toml` dependencies:

```toml
axum = { workspace = true }
tower-http = { workspace = true }
```

Modify `crates/cairn-desktop/Cargo.toml` dev-dependencies:

```toml
tower = { workspace = true }
```

- [ ] **Step 4: Implement the router**

Create `crates/cairn-desktop/src/server.rs`:

```rust
//! Local HTTP server for the desktop GUI alpha.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use tower_http::cors::CorsLayer;

use crate::{
    model::{DesktopReconcileApplyRequest, DesktopReconcilePreviewRequest},
    repository::DesktopRepository,
};

/// Shared server state.
#[derive(Debug, Clone)]
pub struct DesktopServerState {
    repo: Arc<DesktopRepository>,
}

/// Build the desktop alpha router.
#[must_use]
pub fn router(repo: DesktopRepository) -> Router {
    let state = DesktopServerState {
        repo: Arc::new(repo),
    };
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/vault", get(vault))
        .route("/api/v1/folders", get(folders))
        .route("/api/v1/records", get(records))
        .route("/api/v1/records/{id}", get(record))
        .route("/api/v1/graph", get(graph))
        .route("/api/v1/search", get(search))
        .route("/api/v1/lint", get(lint))
        .route("/api/v1/reconcile/preview", post(reconcile_preview))
        .route("/api/v1/reconcile/apply", post(reconcile_apply))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn vault(State(state): State<DesktopServerState>) -> Json<crate::model::DesktopVaultSummary> {
    Json(state.repo.vault())
}

async fn folders(State(state): State<DesktopServerState>) -> Json<Vec<crate::model::DesktopFolder>> {
    Json(state.repo.folders())
}

async fn records(
    State(state): State<DesktopServerState>,
) -> Json<Vec<crate::model::DesktopRecordSummary>> {
    Json(state.repo.records())
}

async fn record(
    State(state): State<DesktopServerState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.repo.record(&id) {
        Some(record) => (StatusCode::OK, Json(record)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "record_not_found",
                "message": "Record was not found"
            })),
        )
            .into_response(),
    }
}

async fn graph(State(state): State<DesktopServerState>) -> Json<crate::model::DesktopGraph> {
    Json(state.repo.graph())
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(
    State(state): State<DesktopServerState>,
    Query(query): Query<SearchQuery>,
) -> Json<Vec<crate::model::DesktopSearchResult>> {
    Json(state.repo.search(&query.q))
}

async fn lint(
    State(state): State<DesktopServerState>,
) -> Json<Vec<crate::model::DesktopLintFinding>> {
    Json(state.repo.lint_findings())
}

async fn reconcile_preview(
    State(state): State<DesktopServerState>,
    Json(request): Json<DesktopReconcilePreviewRequest>,
) -> Json<crate::model::DesktopReconcilePreview> {
    Json(state.repo.preview_reconcile(request))
}

async fn reconcile_apply(
    State(state): State<DesktopServerState>,
    Json(request): Json<DesktopReconcileApplyRequest>,
) -> Json<crate::model::DesktopReconcileApplyResult> {
    Json(state.repo.apply_reconcile(request))
}
```

Modify `crates/cairn-desktop/src/lib.rs`:

```rust
//! Desktop GUI backend for Cairn.

pub mod error;
pub mod fixture;
pub mod model;
pub mod repository;
pub mod server;
```

- [ ] **Step 5: Add server binary**

Create `crates/cairn-desktop/src/bin/cairn-desktop-server.rs`:

```rust
//! Development server for the Cairn desktop GUI alpha.

use std::net::SocketAddr;

use cairn_desktop::{fixture::DesktopFixture, repository::DesktopRepository, server::router};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn,cairn_desktop=info").init();

    let fixture = DesktopFixture::load_default()?;
    let app = router(DesktopRepository::from_fixture(fixture));
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    println!("cairn-desktop listening on http://{actual}");
    axum::serve(listener, app).await?;
    Ok(())
}
```

Add `anyhow` and `tracing-subscriber` to `crates/cairn-desktop/Cargo.toml` dependencies because the binary uses them:

```toml
anyhow = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 6: Run HTTP tests to verify GREEN**

Run:

```bash
cargo test -p cairn-desktop --test http_api
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/cairn-desktop
git commit -m "feat: expose desktop alpha http api"
```

## Task 5: Frontend Project Skeleton And API Client

**Files:**
- Create: `frontend/desktop-electron/package.json`
- Create: `frontend/desktop-electron/tsconfig.json`
- Create: `frontend/desktop-electron/vite.config.ts`
- Create: `frontend/desktop-electron/vitest.config.ts`
- Create: `frontend/desktop-electron/index.html`
- Create: `frontend/desktop-electron/src/api/types.ts`
- Create: `frontend/desktop-electron/src/api/client.ts`
- Create: `frontend/desktop-electron/src/api/client.test.ts`

- [ ] **Step 1: Write failing API client tests**

Create `frontend/desktop-electron/src/api/client.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DesktopApiClient } from "./client";

describe("DesktopApiClient", () => {
  const fetchMock = vi.fn();

  beforeEach(() => {
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    fetchMock.mockReset();
  });

  it("loads records from the backend", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify([{ id: "rec-alpha-001", title: "Project memory scaffold" }]), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    const records = await client.records();

    expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/records");
    expect(records[0].id).toBe("rec-alpha-001");
  });

  it("throws structured errors for failed requests", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ code: "record_not_found", message: "Record was not found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      }),
    );

    const client = new DesktopApiClient("http://127.0.0.1:4000");
    await expect(client.record("missing")).rejects.toMatchObject({
      code: "record_not_found",
    });
  });
});
```

- [ ] **Step 2: Add frontend tooling files**

Create `frontend/desktop-electron/package.json`:

```json
{
  "name": "@cairn/desktop-electron",
  "private": true,
  "version": "0.0.1",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "test": "vitest run"
  },
  "dependencies": {
    "@vitejs/plugin-react": "^5.0.0",
    "electron": "^38.0.0",
    "vite": "^7.0.0",
    "typescript": "^5.9.0",
    "react": "^19.0.0",
    "react-dom": "^19.0.0",
    "lucide-react": "^0.468.0"
  },
  "devDependencies": {
    "@testing-library/jest-dom": "^6.6.0",
    "@testing-library/react": "^16.0.0",
    "@types/react": "^19.0.0",
    "@types/react-dom": "^19.0.0",
    "jsdom": "^25.0.0",
    "vitest": "^2.1.0"
  }
}
```

Create `frontend/desktop-electron/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["DOM", "DOM.Iterable", "ES2022"],
    "allowJs": false,
    "skipLibCheck": true,
    "esModuleInterop": true,
    "allowSyntheticDefaultImports": true,
    "strict": true,
    "forceConsistentCasingInFileNames": true,
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx"
  },
  "include": ["src", "electron", "vite.config.ts", "vitest.config.ts"]
}
```

Create `frontend/desktop-electron/vite.config.ts`:

```ts
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
});
```

Create `frontend/desktop-electron/vitest.config.ts`:

```ts
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
  },
});
```

Create `frontend/desktop-electron/index.html`:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Cairn Desktop Alpha</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 3: Add API types and client**

Create `frontend/desktop-electron/src/api/types.ts` with TypeScript mirrors of the Rust DTOs:

```ts
export type DesktopVaultSummary = {
  id: string;
  name: string;
  root: string;
  recordCount: number;
  folderCount: number;
};

export type DesktopFolder = {
  id: string;
  name: string;
  parentId: string | null;
};

export type DesktopRecordSummary = {
  id: string;
  title: string;
  folderId: string;
  kind: string;
  tags: string[];
  version: number;
  confidence: number;
};

export type DesktopRecordDetail = DesktopRecordSummary & {
  body: string;
  backendHash: string;
  sourceHash: string;
  links: string[];
};

export type DesktopGraph = {
  nodes: Array<{ id: string; label: string; kind: string; group: string }>;
  edges: Array<{ id: string; source: string; target: string; label: string }>;
};

export type DesktopSearchResult = {
  recordId: string;
  title: string;
  snippet: string;
  score: number;
};

export type DesktopLintFinding = {
  id: string;
  severity: string;
  recordId: string | null;
  message: string;
};

export type DesktopRejectedField = {
  field: string;
  code: string;
  message: string;
};

export type DesktopReconcilePreviewRequest = {
  targetId: string;
  expectedVersion: number;
  backendHash: string;
  fieldDiff: Record<string, unknown>;
};

export type DesktopReconcilePreview = {
  accepted: boolean;
  targetId: string;
  expectedVersion: number;
  mutableDiff: Record<string, unknown>;
  rejectedFields: DesktopRejectedField[];
};

export type DesktopApiError = Error & {
  code: string;
  status: number;
};
```

Create `frontend/desktop-electron/src/api/client.ts`:

```ts
import type {
  DesktopApiError,
  DesktopFolder,
  DesktopGraph,
  DesktopLintFinding,
  DesktopRecordDetail,
  DesktopRecordSummary,
  DesktopReconcilePreview,
  DesktopReconcilePreviewRequest,
  DesktopSearchResult,
  DesktopVaultSummary,
} from "./types";

export class DesktopApiClient {
  constructor(private readonly baseUrl: string) {}

  vault(): Promise<DesktopVaultSummary> {
    return this.get("/api/v1/vault");
  }

  folders(): Promise<DesktopFolder[]> {
    return this.get("/api/v1/folders");
  }

  records(): Promise<DesktopRecordSummary[]> {
    return this.get("/api/v1/records");
  }

  record(id: string): Promise<DesktopRecordDetail> {
    return this.get(`/api/v1/records/${encodeURIComponent(id)}`);
  }

  graph(): Promise<DesktopGraph> {
    return this.get("/api/v1/graph");
  }

  search(query: string): Promise<DesktopSearchResult[]> {
    return this.get(`/api/v1/search?q=${encodeURIComponent(query)}`);
  }

  lint(): Promise<DesktopLintFinding[]> {
    return this.get("/api/v1/lint");
  }

  previewReconcile(request: DesktopReconcilePreviewRequest): Promise<DesktopReconcilePreview> {
    return this.post("/api/v1/reconcile/preview", request);
  }

  private async get<T>(path: string): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`);
    return readJson<T>(response);
  }

  private async post<T>(path: string, body: unknown): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    return readJson<T>(response);
  }
}

async function readJson<T>(response: Response): Promise<T> {
  const body = await response.json();
  if (response.ok) {
    return body as T;
  }
  const error = new Error(body.message ?? "Desktop API request failed") as DesktopApiError;
  error.code = body.code ?? "desktop_api_error";
  error.status = response.status;
  throw error;
}
```

- [ ] **Step 4: Install dependencies**

Run:

```bash
cd frontend/desktop-electron && npm install
```

Expected: dependencies install and `package-lock.json` is created. If the environment prefers Bun, run `bun install` only after confirming the repo accepts a `bun.lock` for this frontend package.

- [ ] **Step 5: Run API client tests to verify GREEN**

Run:

```bash
cd frontend/desktop-electron && npm test -- src/api/client.test.ts
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add frontend/desktop-electron
git commit -m "feat: add desktop electron api client"
```

## Task 6: React App Shell And Panels

**Files:**
- Create: `frontend/desktop-electron/src/main.tsx`
- Create: `frontend/desktop-electron/src/App.tsx`
- Create: `frontend/desktop-electron/src/App.test.tsx`
- Create: `frontend/desktop-electron/src/styles.css`
- Create: `frontend/desktop-electron/src/components/VaultSidebar.tsx`
- Create: `frontend/desktop-electron/src/components/RecordDetail.tsx`
- Create: `frontend/desktop-electron/src/components/GraphPanel.tsx`
- Create: `frontend/desktop-electron/src/components/SearchPanel.tsx`
- Create: `frontend/desktop-electron/src/components/LintPanel.tsx`
- Create: `frontend/desktop-electron/src/components/ReconcilePanel.tsx`

- [ ] **Step 1: Write failing UI tests**

Create `frontend/desktop-electron/src/App.test.tsx`:

```tsx
import "@testing-library/jest-dom/vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";

const api = {
  vault: vi.fn().mockResolvedValue({
    id: "desktop-alpha",
    name: "Desktop Alpha Fixture",
    root: "fixtures/desktop-gui-alpha",
    recordCount: 3,
    folderCount: 2,
  }),
  folders: vi.fn().mockResolvedValue([{ id: "folder-core", name: "Core Memories", parentId: null }]),
  records: vi.fn().mockResolvedValue([
    {
      id: "rec-alpha-001",
      title: "Project memory scaffold",
      folderId: "folder-core",
      kind: "skill",
      tags: ["alpha"],
      version: 2,
      confidence: 0.86,
    },
  ]),
  record: vi.fn().mockResolvedValue({
    id: "rec-alpha-001",
    title: "Project memory scaffold",
    folderId: "folder-core",
    body: "Markdown body",
    kind: "skill",
    tags: ["alpha"],
    version: 2,
    backendHash: "sha256:fixture-alpha-001",
    confidence: 0.86,
    sourceHash: "sha256:source-alpha-001",
    links: ["rec-alpha-002"],
  }),
  graph: vi.fn().mockResolvedValue({
    nodes: [{ id: "rec-alpha-001", label: "Project memory scaffold", kind: "skill", group: "folder-core" }],
    edges: [{ id: "rec-alpha-001--rec-alpha-002", source: "rec-alpha-001", target: "rec-alpha-002", label: "wikilink" }],
  }),
  lint: vi.fn().mockResolvedValue([{ id: "lint-alpha-001", severity: "warning", recordId: "rec-alpha-001", message: "Source hash is stale" }]),
  search: vi.fn().mockResolvedValue([]),
  previewReconcile: vi.fn(),
};

describe("App", () => {
  it("renders the vault inspector and loaded record", async () => {
    render(<App api={api} />);

    await waitFor(() => expect(screen.getByText("Desktop Alpha Fixture")).toBeInTheDocument());
    expect(screen.getByText("Project memory scaffold")).toBeInTheDocument();
    expect(screen.getByText("Markdown body")).toBeInTheDocument();
    expect(screen.getByText("Graph")).toBeInTheDocument();
    expect(screen.getByText("Lint")).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run UI tests to verify RED**

Run:

```bash
cd frontend/desktop-electron && npm test -- src/App.test.tsx
```

Expected: FAIL because `App` and components do not exist.

- [ ] **Step 3: Implement the app shell**

Create `frontend/desktop-electron/src/App.tsx` with a simple stateful shell:

```tsx
import { useEffect, useMemo, useState } from "react";
import { DesktopApiClient } from "./api/client";
import type { DesktopFolder, DesktopGraph, DesktopLintFinding, DesktopRecordDetail, DesktopRecordSummary, DesktopVaultSummary } from "./api/types";
import { GraphPanel } from "./components/GraphPanel";
import { LintPanel } from "./components/LintPanel";
import { RecordDetail } from "./components/RecordDetail";
import { SearchPanel } from "./components/SearchPanel";
import { VaultSidebar } from "./components/VaultSidebar";
import "./styles.css";

export type DesktopApi = Pick<
  DesktopApiClient,
  "vault" | "folders" | "records" | "record" | "graph" | "lint" | "search" | "previewReconcile"
>;

type AppState = {
  vault: DesktopVaultSummary | null;
  folders: DesktopFolder[];
  records: DesktopRecordSummary[];
  selected: DesktopRecordDetail | null;
  graph: DesktopGraph | null;
  lint: DesktopLintFinding[];
  error: string | null;
};

export function App({ api = new DesktopApiClient(import.meta.env.VITE_CAIRN_DESKTOP_API ?? "http://127.0.0.1:4000") }: { api?: DesktopApi }) {
  const [state, setState] = useState<AppState>({
    vault: null,
    folders: [],
    records: [],
    selected: null,
    graph: null,
    lint: [],
    error: null,
  });

  useEffect(() => {
    let cancelled = false;
    async function load() {
      try {
        const [vault, folders, records, graph, lint] = await Promise.all([
          api.vault(),
          api.folders(),
          api.records(),
          api.graph(),
          api.lint(),
        ]);
        const selected = records[0] ? await api.record(records[0].id) : null;
        if (!cancelled) {
          setState({ vault, folders, records, selected, graph, lint, error: null });
        }
      } catch (error) {
        if (!cancelled) {
          setState((current) => ({ ...current, error: error instanceof Error ? error.message : "Failed to load desktop data" }));
        }
      }
    }
    void load();
    return () => {
      cancelled = true;
    };
  }, [api]);

  const selectedId = state.selected?.id ?? null;
  const recordsByFolder = useMemo(() => state.records, [state.records]);

  async function selectRecord(id: string) {
    const selected = await api.record(id);
    setState((current) => ({ ...current, selected }));
  }

  if (state.error) {
    return <main className="app appError">{state.error}</main>;
  }

  return (
    <main className="app">
      <VaultSidebar
        vault={state.vault}
        folders={state.folders}
        records={recordsByFolder}
        selectedId={selectedId}
        onSelectRecord={(id) => void selectRecord(id)}
      />
      <section className="workspace">
        <RecordDetail record={state.selected} api={api} />
        <div className="lowerPanels">
          <GraphPanel graph={state.graph} />
          <SearchPanel api={api} onSelectRecord={(id) => void selectRecord(id)} />
          <LintPanel findings={state.lint} />
        </div>
      </section>
    </main>
  );
}
```

Create components with focused rendering:

```tsx
// frontend/desktop-electron/src/components/VaultSidebar.tsx
import type { DesktopFolder, DesktopRecordSummary, DesktopVaultSummary } from "../api/types";

export function VaultSidebar({
  vault,
  folders,
  records,
  selectedId,
  onSelectRecord,
}: {
  vault: DesktopVaultSummary | null;
  folders: DesktopFolder[];
  records: DesktopRecordSummary[];
  selectedId: string | null;
  onSelectRecord: (id: string) => void;
}) {
  return (
    <aside className="sidebar">
      <h1>{vault?.name ?? "Loading vault"}</h1>
      <p>{vault ? `${vault.recordCount} records · ${vault.folderCount} folders` : "Connecting"}</p>
      {folders.map((folder) => (
        <section key={folder.id} className="folderGroup">
          <h2>{folder.name}</h2>
          {records.filter((record) => record.folderId === folder.id).map((record) => (
            <button
              className={record.id === selectedId ? "recordButton selected" : "recordButton"}
              key={record.id}
              type="button"
              onClick={() => onSelectRecord(record.id)}
            >
              <span>{record.title}</span>
              <small>{record.kind}</small>
            </button>
          ))}
        </section>
      ))}
    </aside>
  );
}
```

```tsx
// frontend/desktop-electron/src/components/RecordDetail.tsx
import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopRecordDetail } from "../api/types";
import { ReconcilePanel } from "./ReconcilePanel";

export function RecordDetail({ record, api }: { record: DesktopRecordDetail | null; api: DesktopApi }) {
  const [draft, setDraft] = useState("");

  if (!record) {
    return <section className="recordDetail">Loading record...</section>;
  }

  const body = draft || record.body;

  return (
    <section className="recordDetail">
      <header>
        <h2>{record.title}</h2>
        <div className="metaLine">
          <span>{record.kind}</span>
          <span>v{record.version}</span>
          <span>{Math.round(record.confidence * 100)}%</span>
        </div>
      </header>
      <textarea aria-label="Record body" value={body} onChange={(event) => setDraft(event.target.value)} />
      <ReconcilePanel api={api} record={record} draftBody={body} />
    </section>
  );
}
```

```tsx
// frontend/desktop-electron/src/components/ReconcilePanel.tsx
import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopRecordDetail, DesktopReconcilePreview } from "../api/types";

export function ReconcilePanel({ api, record, draftBody }: { api: DesktopApi; record: DesktopRecordDetail; draftBody: string }) {
  const [preview, setPreview] = useState<DesktopReconcilePreview | null>(null);

  async function review() {
    const next = await api.previewReconcile({
      targetId: record.id,
      expectedVersion: record.version,
      backendHash: record.backendHash,
      fieldDiff: { body: draftBody },
    });
    setPreview(next);
  }

  return (
    <section className="reconcilePanel">
      <button type="button" onClick={() => void review()}>
        Review reconcile
      </button>
      {preview && (
        <p>{preview.accepted ? "Ready to apply" : preview.rejectedFields.map((field) => field.message).join(", ")}</p>
      )}
    </section>
  );
}
```

```tsx
// frontend/desktop-electron/src/components/GraphPanel.tsx
import type { DesktopGraph } from "../api/types";

export function GraphPanel({ graph }: { graph: DesktopGraph | null }) {
  return (
    <section className="panel">
      <h2>Graph</h2>
      <p>{graph ? `${graph.nodes.length} nodes · ${graph.edges.length} edges` : "Loading graph"}</p>
      <div className="graphList">
        {graph?.edges.map((edge) => (
          <span key={edge.id}>{edge.source} → {edge.target}</span>
        ))}
      </div>
    </section>
  );
}
```

```tsx
// frontend/desktop-electron/src/components/SearchPanel.tsx
import { useState } from "react";
import type { DesktopApi } from "../App";
import type { DesktopSearchResult } from "../api/types";

export function SearchPanel({ api, onSelectRecord }: { api: DesktopApi; onSelectRecord: (id: string) => void }) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<DesktopSearchResult[]>([]);

  async function runSearch(nextQuery: string) {
    setQuery(nextQuery);
    setResults(nextQuery.trim() ? await api.search(nextQuery) : []);
  }

  return (
    <section className="panel">
      <h2>Search</h2>
      <input aria-label="Search records" value={query} onChange={(event) => void runSearch(event.target.value)} />
      {results.map((result) => (
        <button key={result.recordId} type="button" onClick={() => onSelectRecord(result.recordId)}>
          {result.title}
        </button>
      ))}
    </section>
  );
}
```

```tsx
// frontend/desktop-electron/src/components/LintPanel.tsx
import type { DesktopLintFinding } from "../api/types";

export function LintPanel({ findings }: { findings: DesktopLintFinding[] }) {
  return (
    <section className="panel">
      <h2>Lint</h2>
      {findings.map((finding) => (
        <article key={finding.id} className="lintFinding">
          <strong>{finding.severity}</strong>
          <p>{finding.message}</p>
        </article>
      ))}
    </section>
  );
}
```

Create `frontend/desktop-electron/src/main.tsx`:

```tsx
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
```

- [ ] **Step 4: Add styling**

Create `frontend/desktop-electron/src/styles.css`:

```css
:root {
  color: #172026;
  background: #f6f7f4;
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}

body {
  margin: 0;
}

button,
input,
textarea {
  font: inherit;
}

.app {
  display: grid;
  grid-template-columns: 280px minmax(0, 1fr);
  min-height: 100vh;
}

.sidebar {
  border-right: 1px solid #d8ddd4;
  background: #fffdfa;
  padding: 16px;
  overflow: auto;
}

.sidebar h1,
.recordDetail h2 {
  margin: 0;
  font-size: 18px;
}

.folderGroup h2,
.panel h2 {
  font-size: 13px;
  margin: 18px 0 8px;
  text-transform: uppercase;
}

.recordButton {
  align-items: flex-start;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 6px;
  color: inherit;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 8px;
  text-align: left;
  width: 100%;
}

.recordButton.selected {
  background: #e9f2ec;
  border-color: #9fbea8;
}

.workspace {
  display: grid;
  grid-template-rows: minmax(360px, 1fr) 280px;
  min-width: 0;
}

.recordDetail {
  display: grid;
  gap: 12px;
  padding: 20px;
}

.metaLine {
  color: #57636b;
  display: flex;
  gap: 12px;
}

textarea {
  border: 1px solid #cfd7d2;
  border-radius: 6px;
  min-height: 220px;
  padding: 12px;
  resize: vertical;
}

.lowerPanels {
  border-top: 1px solid #d8ddd4;
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  min-height: 0;
}

.panel,
.reconcilePanel {
  border-right: 1px solid #d8ddd4;
  padding: 14px;
  overflow: auto;
}

.graphList {
  display: grid;
  gap: 6px;
}

.lintFinding {
  border: 1px solid #d8ddd4;
  border-radius: 6px;
  padding: 8px;
}

.appError {
  display: grid;
  place-items: center;
}
```

- [ ] **Step 5: Run UI tests to verify GREEN**

Run:

```bash
cd frontend/desktop-electron && npm test -- src/App.test.tsx
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add frontend/desktop-electron
git commit -m "feat: add desktop alpha react shell"
```

## Task 7: Electron Main Process And Documentation

**Files:**
- Create: `frontend/desktop-electron/electron/main.ts`
- Create: `frontend/desktop-electron/electron/preload.ts`
- Modify: `frontend/desktop-electron/package.json`
- Create: `frontend/desktop-electron/README.md`

- [ ] **Step 1: Add Electron scripts**

Modify `frontend/desktop-electron/package.json` scripts:

```json
{
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "test": "vitest run",
    "electron": "electron ."
  },
  "main": "electron/main.ts"
}
```

- [ ] **Step 2: Add Electron main and preload**

Create `frontend/desktop-electron/electron/main.ts`:

```ts
import { app, BrowserWindow } from "electron";
import { join } from "node:path";

async function createWindow() {
  const win = new BrowserWindow({
    width: 1320,
    height: 860,
    minWidth: 1024,
    minHeight: 720,
    webPreferences: {
      preload: join(__dirname, "preload.ts"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  const devUrl = process.env.VITE_DEV_SERVER_URL ?? "http://localhost:5173";
  if (process.env.NODE_ENV === "development") {
    await win.loadURL(devUrl);
  } else {
    await win.loadFile(join(__dirname, "../dist/index.html"));
  }
}

app.whenReady().then(() => {
  void createWindow();
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
```

Create `frontend/desktop-electron/electron/preload.ts`:

```ts
import { contextBridge } from "electron";

contextBridge.exposeInMainWorld("cairnDesktop", {
  apiBaseUrl: process.env.CAIRN_DESKTOP_API ?? "http://127.0.0.1:4000",
});
```

- [ ] **Step 3: Add README with Tauri note**

Create `frontend/desktop-electron/README.md`:

```markdown
# Cairn Desktop Electron Alpha

This package is the first Electron GUI alpha for issue #115. It is intentionally
fixture-backed and talks to the Rust `cairn-desktop` backend over local
HTTP/JSON.

## Development

Start the Rust backend:

```bash
cargo run -p cairn-desktop --bin cairn-desktop-server
```

Start the renderer:

```bash
npm run dev
```

Run checks:

```bash
npm test
npm run build
```

## Tauri Alternative

The design brief keeps Tauri as a slim shell alternative for users who prefer a
smaller runtime. This alpha implements Electron first because §13.2 names it as
the default desktop shell and because Chromium rendering parity matters for the
graph and editor surfaces. A future Tauri package should reuse the same Rust
backend API and renderer model rather than adding a second data path.
```

- [ ] **Step 4: Run frontend build**

Run:

```bash
cd frontend/desktop-electron && npm run build
```

Expected: PASS. If Electron TypeScript build complains about `__dirname` in ESM, update the main-process build path to use `fileURLToPath(import.meta.url)` and rerun.

- [ ] **Step 5: Commit**

```bash
git add frontend/desktop-electron
git commit -m "feat: add electron shell documentation"
```

## Task 8: Fixture Smoke Verification

**Files:**
- Modify: `crates/cairn-desktop/tests/http_api.rs`
- Modify: `frontend/desktop-electron/src/App.test.tsx`
- Modify: `docs/superpowers/specs/2026-05-13-issue-115-electron-gui-alpha-design.md` only if verification commands changed during implementation.

- [ ] **Step 1: Add backend smoke test**

Append to `crates/cairn-desktop/tests/http_api.rs`:

```rust
#[tokio::test]
async fn smoke_loads_all_desktop_alpha_surfaces() {
    let app = router(DesktopRepository::from_fixture(
        DesktopFixture::load_default().expect("fixture"),
    ));

    for path in [
        "/api/v1/vault",
        "/api/v1/folders",
        "/api/v1/records",
        "/api/v1/graph",
        "/api/v1/search?q=alpha",
        "/api/v1/lint",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
}
```

- [ ] **Step 2: Add frontend reconcile smoke assertion**

Extend `frontend/desktop-electron/src/App.test.tsx` with a second test:

```tsx
import userEvent from "@testing-library/user-event";

it("reviews a reconcile edit through the backend client", async () => {
  const user = userEvent.setup();
  api.previewReconcile.mockResolvedValueOnce({
    accepted: true,
    targetId: "rec-alpha-001",
    expectedVersion: 2,
    mutableDiff: { body: "Markdown body" },
    rejectedFields: [],
  });

  render(<App api={api} />);

  await screen.findByText("Project memory scaffold");
  await user.click(screen.getByRole("button", { name: "Review reconcile" }));

  expect(await screen.findByText("Ready to apply")).toBeInTheDocument();
  expect(api.previewReconcile).toHaveBeenCalledWith({
    targetId: "rec-alpha-001",
    expectedVersion: 2,
    backendHash: "sha256:fixture-alpha-001",
    fieldDiff: { body: "Markdown body" },
  });
});
```

Add `@testing-library/user-event` to `frontend/desktop-electron/package.json` dev dependencies:

```json
"@testing-library/user-event": "^14.6.0"
```

- [ ] **Step 3: Run smoke tests to verify GREEN**

Run:

```bash
cargo test -p cairn-desktop --test http_api smoke_loads_all_desktop_alpha_surfaces
cd frontend/desktop-electron && npm test -- src/App.test.tsx
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-desktop frontend/desktop-electron docs/superpowers/specs/2026-05-13-issue-115-electron-gui-alpha-design.md
git commit -m "test: add desktop alpha smoke coverage"
```

## Task 9: Final Verification And Cleanup

**Files:**
- Review all touched files.

- [ ] **Step 1: Run Rust verification**

Run:

```bash
cargo test -p cairn-desktop
```

Expected: PASS.

- [ ] **Step 2: Run frontend verification**

Run:

```bash
cd frontend/desktop-electron && npm test
cd frontend/desktop-electron && npm run build
```

Expected: PASS.

- [ ] **Step 3: Run workspace boundary check if core/workspace manifests changed**

Run:

```bash
scripts/check-core-boundary.sh
```

Expected: PASS. If the script requires a different invocation, use the repo documented command in `CLAUDE.md` and record it in the PR.

- [ ] **Step 4: Inspect git status and diff**

Run:

```bash
git status --short
git diff --stat HEAD
```

Expected: only issue #115 files are changed since the implementation commits.

- [ ] **Step 5: Commit any final cleanup**

If verification causes lockfile, docs, or cleanup changes:

```bash
git add Cargo.toml Cargo.lock crates/cairn-desktop fixtures/desktop-gui-alpha frontend/desktop-electron docs/superpowers/specs/2026-05-13-issue-115-electron-gui-alpha-design.md docs/superpowers/plans/2026-05-13-issue-115-electron-gui-alpha.md
git commit -m "chore: finalize desktop gui alpha"
```

Expected: branch contains the design commit, plan commit if created, and focused implementation commits.

## Self-Review

- Spec coverage: backend crate, fixture vault, Electron renderer, graph/search/lint panels, reconcile validation, Tauri documentation, frontend build/test, adapter-backed edit tests, and fixture smoke tests all map to tasks above.
- Placeholder scan: no task uses TBD/TODO/fill-in language; each code-producing step includes concrete file content or an exact patch target.
- Type consistency: Rust DTO names match TypeScript DTO names in camelCase JSON form; reconcile request and response names are consistent across backend and frontend tasks.
