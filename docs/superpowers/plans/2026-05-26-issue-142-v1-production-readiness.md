# Issue #142 v1.0 Production Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the issue #142 maintainer artifact that tells release owners exactly how to validate v1.0 production readiness across Claude Code, Codex, and the third supported harness path.

**Architecture:** Docs-only. The new maintainer page is the published acceptance guide and evidence template. It links to the existing beta readiness runner, capability matrix, MCP semver policy, Claude Code and Codex setup docs, and skill fallback docs instead of duplicating generated command reference.

**Tech Stack:** Markdown and mdBook navigation. Verification is `mdbook build docs/site` plus `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`; no Rust, IDL, or generated docs are changed.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `docs/site/src/maintainers/v1-production-readiness.md` | Create | Canonical issue #142 runbook, three-harness matrix, known-limitations policy, and sign-off template. |
| `docs/site/src/SUMMARY.md` | Modify | Add the new maintainer page to the published mdBook. |
| `docs/site/src/maintainers/beta-readiness.md` | Modify | Point beta release operators to the v1.0 production sign-off page when preparing GA. |

No code, no schema, no generated reference files, and no CI workflow YAML changes.

## Task 1: Add the v1.0 Readiness Runbook

**Files:**
- Create: `docs/site/src/maintainers/v1-production-readiness.md`

- [ ] **Step 1: Create the runbook with the issue source, prerequisites, and harness matrix**

Write the page with:

```markdown
# v1.0 Production Readiness

This is the release-owner runbook for issue #142.
```

Include concrete prerequisites:

```markdown
- #25, #139, #140, and #141 are closed.
- The release SHA is known and has CI runs.
- `scripts/beta-readiness.sh --full` has been attempted.
```

Include the required harness set:

```markdown
| Harness | Primary path | Required evidence |
|---------|--------------|-------------------|
| Claude Code | `cairn setup claude-code` + `cairn doctor claude-code` | Doctor JSON, hook loop smoke, P0 story checklist |
| Codex | `cairn setup codex` + project hooks + skill fallback | setup receipt, generated config and hooks, Codex-shaped trace replay |
| Gemini or equivalent third supported skill harness | `cairn skill install --harness gemini` | skill install receipt, CLI/skill verb smoke, documented hook/MCP limitation |
```

- [ ] **Step 2: Add the acceptance evidence commands**

Use only commands that already exist in the repo:

```bash
cargo build --release --locked -p cairn-cli --bin cairn
CAIRN_BIN="$PWD/target/release/cairn" scripts/install-smoke.sh
scripts/beta-readiness.sh --full
cargo test -p cairn-cli --test claude_code_setup --locked
cargo test -p cairn-cli --test codex_setup --locked
cargo test -p cairn-cli --test skill_agent_pack --locked
cargo run -p cairn-bench --release --locked -- coherence run --gate rc
```

- [ ] **Step 3: Add the known-limitations and sign-off sections**

Require the sign-off block to include:

```markdown
- release SHA
- release artifacts
- automated gate summary
- per-harness result
- known limitations copied from `docs/site/src/status.md`
- maintainer and date
```

## Task 2: Publish the Runbook in mdBook Navigation

**Files:**
- Modify: `docs/site/src/SUMMARY.md`
- Modify: `docs/site/src/maintainers/beta-readiness.md`

- [ ] **Step 1: Add the navigation entry**

Add this under `# Maintainers`:

```markdown
- [v1.0 Production Readiness](maintainers/v1-production-readiness.md)
```

- [ ] **Step 2: Cross-link from beta readiness**

Add a short paragraph after the beta readiness quick-start commands:

```markdown
For v1.0 GA, beta readiness is a prerequisite, not the final sign-off. Use the
[v1.0 production readiness](v1-production-readiness.md) runbook for the
three-harness acceptance matrix and release evidence block.
```

## Task 3: Verify Documentation

**Files:**
- All modified files above.

- [ ] **Step 1: Run mdBook**

Run:

```bash
mdbook build docs/site
```

Expected: exit 0. If `mdbook` is not installed, record that explicitly and run the docgen check.

- [ ] **Step 2: Run docgen check**

Run:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Expected: exit 0 with no generated-doc drift.
