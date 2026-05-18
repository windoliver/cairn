# Issue 114 Frontend Adapter Alphas Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Rust alpha `FrontendAdapter` crates for Obsidian, VS Code, and Logseq that pass the shared conformance suite.

**Architecture:** Each adapter is a small workspace crate implementing the existing `FrontendAdapter` and `FrontendAdapterPlugin` contracts from `cairn-core`. The crates share equivalent fail-closed reconcile behavior and differ in capability declarations and projection sidecar shape.

**Tech Stack:** Rust 2024, existing `cairn-core` plugin registry/conformance APIs, `serde_json` for field diffs in tests.

---

### Task 1: Obsidian Adapter Alpha

**Files:**
- Create: `crates/cairn-frontend-obsidian/Cargo.toml`
- Create: `crates/cairn-frontend-obsidian/src/lib.rs`
- Create: `crates/cairn-frontend-obsidian/tests/adapter.rs`

- [ ] **Step 1: Write failing adapter tests**

Add tests that construct `ObsidianFrontendAdapter`, assert capabilities, inspect projection frontmatter/sidecars, register it with `PluginRegistry`, and assert every `run_conformance_for_plugin` outcome is `CaseStatus::Ok`.
Include projection snapshot coverage for frontmatter, timeline/evidence/consent sidecars, backlink metadata, and live-update metadata.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p cairn-frontend-obsidian --locked`

Expected: package is missing or symbols are undefined.

- [ ] **Step 3: Implement minimal adapter**

Implement `ObsidianFrontendAdapter` with:

- `NAME = "cairn-frontend-obsidian"`
- `frontmatter = true`
- `sidecar_files = true`
- `live_plugin = true`
- `graph_view = true`
- `max_frontmatter_fields = 16`
- conformance-compatible `project` and `reconcile`
- `backlinks.md` and `live.md` sidecars

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p cairn-frontend-obsidian --locked`

Expected: all Obsidian adapter tests pass.

### Task 2: VS Code Adapter Alpha

**Files:**
- Create: `crates/cairn-frontend-vscode/Cargo.toml`
- Create: `crates/cairn-frontend-vscode/src/lib.rs`
- Create: `crates/cairn-frontend-vscode/tests/adapter.rs`

- [ ] **Step 1: Write failing adapter tests**

Mirror the Obsidian tests with `VscodeFrontendAdapter`, checking VS Code-specific capabilities and projection sidecars.
Include projection snapshot coverage for frontmatter, timeline/evidence/consent sidecars, backlink metadata, and live-update metadata.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p cairn-frontend-vscode --locked`

Expected: package is missing or symbols are undefined.

- [ ] **Step 3: Implement minimal adapter**

Implement `VscodeFrontendAdapter` with:

- `NAME = "cairn-frontend-vscode"`
- `frontmatter = true`
- `sidecar_files = true`
- `live_plugin = true`
- `graph_view = false`
- `max_frontmatter_fields = 12`
- conformance-compatible `project` and `reconcile`
- `backlinks.md` and `live.md` sidecars

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p cairn-frontend-vscode --locked`

Expected: all VS Code adapter tests pass.

### Task 3: Logseq Adapter Alpha

**Files:**
- Create: `crates/cairn-frontend-logseq/Cargo.toml`
- Create: `crates/cairn-frontend-logseq/src/lib.rs`
- Create: `crates/cairn-frontend-logseq/tests/adapter.rs`

- [ ] **Step 1: Write failing adapter tests**

Mirror the adapter tests with `LogseqFrontendAdapter`, checking graph/outline-oriented capabilities and the `outline.md` sidecar.
Include projection snapshot coverage for frontmatter, timeline/evidence/consent sidecars, backlink metadata, live-update metadata, and the outline sidecar.

- [ ] **Step 2: Verify RED**

Run: `cargo test -p cairn-frontend-logseq --locked`

Expected: package is missing or symbols are undefined.

- [ ] **Step 3: Implement minimal adapter**

Implement `LogseqFrontendAdapter` with:

- `NAME = "cairn-frontend-logseq"`
- `frontmatter = true`
- `sidecar_files = true`
- `live_plugin = true`
- `graph_view = true`
- `max_frontmatter_fields = 14`
- conformance-compatible `project` and `reconcile`
- an `outline.md` sidecar
- `backlinks.md` and `live.md` sidecars

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p cairn-frontend-logseq --locked`

Expected: all Logseq adapter tests pass.

### Task 4: Workspace and Regression Verification

**Files:**
- Modify: `Cargo.toml`
- Modify: `docs/site/src/usage/plugins.md`

- [ ] **Step 1: Add workspace dependency entries**

Add `cairn-frontend-obsidian`, `cairn-frontend-vscode`, and
`cairn-frontend-logseq` to `[workspace.dependencies]` so they follow existing
publish metadata conventions.

- [ ] **Step 2: Update plugin usage docs**

Update `docs/site/src/usage/plugins.md` so bundled plugin docs list the three
frontend alpha crates.

- [ ] **Step 3: Run focused regressions**

Run:

```bash
cargo test -p cairn-core --test frontend_adapter_contract --locked
cargo test -p cairn-frontend-obsidian -p cairn-frontend-vscode -p cairn-frontend-logseq --locked
```

Expected: all tests pass.

- [ ] **Step 4: Run formatting/checks**

Run:

```bash
cargo fmt --all --check
cargo check --workspace --locked
```

Expected: formatting and workspace check pass.
