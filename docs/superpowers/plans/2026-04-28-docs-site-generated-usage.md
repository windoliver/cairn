# Docs Site Generated Usage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a committed mdBook docs site, generate usage/reference docs from real Cairn surfaces, and enforce docs drift in CI with automatic GitHub Pages deployment from `main`.

**Architecture:** `cairn-docgen` is a maintainer-time binary in `cairn-cli` that writes generated Markdown under `docs/site/src/reference/generated/`. The runtime CLI and doc generator share `cairn_cli::command::build_command()`, so CLI flags and subcommands cannot drift from docs. GitHub Actions runs `cairn-docgen --check` and `mdbook build` on PRs; a separate Pages workflow deploys built static output on pushes to `main`.

**Tech Stack:** Rust 2024, clap 4, serde/serde_yaml, cargo metadata JSON, mdBook 0.4, GitHub Actions Pages.

---

### Task 1: Fix Current Compile Blocker

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/cairn-mcp/Cargo.toml`
- Modify: `crates/cairn-mcp/src/error.rs`
- Modify: `crates/cairn-mcp/src/lib.rs`

- [ ] **Step 1: Run the failing compile command**

Run: `cargo check -p cairn-mcp --locked`

Expected before the fix: fails because `rmcp` is not declared and `TransportError` does not name the enum in `error.rs`.

- [ ] **Step 2: Add the missing dependency and align the error type**

Add `rmcp = "1.5"` to workspace dependencies, consume it from `cairn-mcp`, re-export `McpTransportError`, and map rmcp service failures into a `Service(String)` variant.

- [ ] **Step 3: Verify the blocker is fixed**

Run: `cargo check -p cairn-mcp --locked`

Expected after the fix: command exits 0.

### Task 2: Share the Runtime Command Tree

**Files:**
- Create: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/lib.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Test: `crates/cairn-cli/tests/docgen.rs`

- [ ] **Step 1: Write a test that imports `cairn_cli::command::build_command()`**

The test must fail before implementation because `command` is not public yet. It should assert that top-level commands include `ingest`, `search`, `retrieve`, `summarize`, `assemble_hot`, `capture_trace`, `lint`, `forget`, `handshake`, `status`, `plugins`, and `bootstrap`.

- [ ] **Step 2: Move command construction out of `main.rs`**

Create `command.rs` with `pub fn build_command() -> clap::Command`, private `plugins_subcommand()`, and private `bootstrap_subcommand()`. Change `main.rs` to call `cairn_cli::command::build_command()`.

- [ ] **Step 3: Verify the command-tree test passes**

Run: `cargo test -p cairn-cli --test docgen docgen_command_tree_matches_runtime --locked`

Expected: test exits 0.

### Task 3: Add `cairn-docgen`

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml`
- Create: `crates/cairn-cli/src/docgen.rs`
- Create: `crates/cairn-cli/src/bin/cairn-docgen.rs`
- Test: `crates/cairn-cli/tests/docgen.rs`

- [ ] **Step 1: Write tests for write/check/drift/package coverage**

Tests must create a temp workspace, call docgen write mode, call check mode, mutate a generated file to verify drift is reported, and use a coverage manifest missing one package to verify the missing package name appears in the error.

- [ ] **Step 2: Implement docgen outputs**

Generate:
- `docs/site/src/reference/generated/cli.md`
- command pages under `docs/site/src/reference/generated/commands/`
- `config-defaults.md`
- `plugins.md`
- `mcp-tools.md`
- `packages.md`
- `contract-verbs.md`

- [ ] **Step 3: Verify generator behavior**

Run: `cargo test -p cairn-cli --test docgen --locked`

Expected: all docgen tests exit 0.

### Task 4: Add the mdBook Source Site

**Files:**
- Create: `docs/site/book.toml`
- Create: `docs/site/docs-coverage.toml`
- Create: `docs/site/src/SUMMARY.md`
- Create: `docs/site/src/index.md`
- Create: `docs/site/src/quickstart.md`
- Create: `docs/site/src/concepts/architecture.md`
- Create: `docs/site/src/concepts/vault-layout.md`
- Create: `docs/site/src/concepts/capability-model.md`
- Create: `docs/site/src/usage/cli.md`
- Create: `docs/site/src/usage/config.md`
- Create: `docs/site/src/usage/plugins.md`
- Create: `docs/site/src/usage/mcp.md`
- Create: `docs/site/src/usage/skill.md`
- Create: `docs/site/src/reference/idl.md`
- Create: `docs/site/src/reference/rust-api.md`
- Create: `docs/site/src/maintainers/codegen.md`
- Create: `docs/site/src/maintainers/docs.md`
- Create: `docs/site/src/maintainers/ci.md`
- Create: `docs/site/src/status.md`

- [ ] **Step 1: Add the human docs pages**

Pages must describe current truth: `status`, `handshake`, `bootstrap`, `plugins list`, and `plugins verify` are implemented; the eight memory verbs exist but fail closed as P0 stubs until storage/dispatch lands; `cairn-mcp` exposes crate/plugin/tool declarations but no `cairn mcp` CLI subcommand today.

- [ ] **Step 2: Generate committed reference docs**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --write`

Expected: generated Markdown files are created under `docs/site/src/reference/generated/`.

### Task 5: Wire CI and Auto Deploy

**Files:**
- Modify: `.github/workflows/docs.yml`
- Create: `.github/workflows/pages.yml`
- Modify: `docs/ci.md`
- Modify: `CLAUDE.md`
- Modify: `README.md`

- [ ] **Step 1: Add required docs jobs**

Add `docs / generated reference` running `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` and `docs / mdbook build` running `mdbook build docs/site`.

- [ ] **Step 2: Add GitHub Pages deployment**

Create a Pages workflow that runs only on pushes to `main` and `workflow_dispatch`, builds docs into `target/site`, uploads the artifact, and deploys with `actions/deploy-pages`.

- [ ] **Step 3: Update contributor docs and README**

Document the new required CI statuses, local verification commands, and the docs site source.

### Task 6: Final Verification

**Files:** all touched files

- [ ] **Step 1: Run generator and docs checks**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all commands exit 0.

- [ ] **Step 2: Run Rust checks**

Run:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo test -p cairn-cli --test docgen --locked
```

Expected: all commands exit 0.
