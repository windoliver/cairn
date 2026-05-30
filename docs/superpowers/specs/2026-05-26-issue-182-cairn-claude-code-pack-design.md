# cairn-claude-code reference skill-pack — design

- **Issue:** [#182](https://github.com/windoliver/cairn/issues/182)
- **Parent:** [#19](https://github.com/windoliver/cairn/issues/19) Integrate Claude Code as v0.1 reference consumer
- **Brief refs:** §8 (CLI ground truth, MCP/SDK/skill mirror), CLAUDE.md §4 invariant 1 (harness-agnostic core)
- **Status:** approved, pre-implementation

## 1. Purpose

Ship the canonical Cairn skill-pack for Claude Code as the reference
implementation of the v0.1 plugin contract: six subagents, thirteen slash
commands, five-event hook bindings, and an installable manifest. The pack is
the *content* counterpart to issues #11/#68 (install machinery), #102 (hook
lifecycle), and #143 (plugin registration). It also establishes the layout
third-party packs (`cairn-codex`, `cairn-gemini`, `cairn-cursor`, …) follow.

This design respects the harness-agnostic core invariant: no Claude-Code-
specific content lives in `cairn-core` or `cairn-cli` after this lands. The
pack content lives in `packs/cairn-claude-code/` as plain markdown and JSON;
`cairn-cli` ships only a generic pack runtime that embeds, validates, and
installs any cairn-pack v1 manifest.

## 2. Scope and non-goals

In scope:

- New `cairn-pack/v1` manifest schema for harness packs (distinct from the
  skillify `SkillPackManifest`, which models evolved skill bundles).
- Pack source files in `packs/cairn-claude-code/`: 6 subagents, 13 slash
  commands, hook bindings, manual fragment, dogfood fixture vault.
- Generic pack runtime in `crates/cairn-cli/src/packs/` (loader, manifest
  validator, installer, verify-suite hook).
- Migration of existing inline Claude-Code content out of
  `crates/cairn-cli/src/skill.rs` into the pack.
- Snapshot tests of installed file bytes, conformance tests via
  `cairn plugins verify`, dogfood acceptance checklist.

Out of scope (per issue):

- Packs for Codex, Gemini, Cursor, OpenCode (each gets its own issue +
  follow-on PR using this design as the template).
- Domain-specific opinionated workflows (perf reviews, incident playbooks)
  belong in third-party packs.
- Dynamic frontmatter views or host-app-specific UI integration.

## 3. Source-of-truth layout

```
packs/cairn-claude-code/                  # source of truth — pure content
├── pack.json                              # cairn-pack/v1 manifest
├── manual.md                              # CLAUDE.md fragment (block-guarded)
├── agents/
│   ├── context-loader.md
│   ├── vault-librarian.md
│   ├── forget-planner.md
│   ├── consolidator.md
│   ├── replay-checker.md
│   └── trace-summarizer.md
├── commands/
│   ├── cairn-ingest.md
│   ├── cairn-search.md
│   ├── cairn-retrieve.md
│   ├── cairn-summarize.md
│   ├── cairn-assemble.md
│   ├── cairn-capture-trace.md
│   ├── cairn-lint.md
│   ├── cairn-forget.md
│   ├── cairn-status.md
│   ├── cairn-standup.md
│   ├── cairn-wrap-up.md
│   ├── cairn-audit.md
│   └── cairn-recall.md
├── hooks/
│   └── settings.json                       # merged into .claude/settings.json
├── fixtures/
│   └── dogfood-vault/                      # 5-record fixture for acceptance test
└── ACCEPTANCE.md                           # dogfood checklist

crates/cairn-cli/src/packs/                # generic pack runtime
├── mod.rs                                  # re-exports + pack registry
├── manifest.rs                             # serde + schema validator
├── install.rs                              # writes pack files into target project
├── embed.rs                                # `include_dir!` wrapper
└── verify.rs                               # `cairn plugins verify` integration

crates/cairn-cli/src/skill.rs               # shrinks ~1645 → ~200 LOC
crates/cairn-cli/tests/claude_code_pack_install.rs   # snapshot suite
crates/cairn-cli/tests/claude_code_pack_verify.rs    # conformance suite
```

Boundary rules:

1. `packs/cairn-claude-code/` contains zero Rust. Pure content.
2. `crates/cairn-cli/src/packs/` contains zero Claude-Code-specific strings.
   Generic pack runtime, harness-agnostic.
3. Adding `packs/cairn-codex/` later only requires new content files plus a
   one-line registration in `crates/cairn-cli/src/packs/mod.rs`.
4. `cairn-core` untouched. Harness-agnostic invariant preserved.

## 4. cairn-pack/v1 manifest schema

`pack.json` shape:

```json
{
  "schema": "cairn-pack/v1",
  "pack_id": "cairn-claude-code",
  "name": "cairn-claude-code",
  "version": "0.1.0",
  "harness": "claude-code",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Reference Claude Code skill-pack for Cairn.",
  "requires_capabilities": [
    "cairn.mcp.v1.search.keyword",
    "cairn.mcp.v1.retrieve.record",
    "cairn.mcp.v1.forget.record"
  ],
  "subagents": [
    {
      "id": "context-loader",
      "path": "agents/context-loader.md",
      "uses_mcp_tools": ["assemble_hot", "retrieve", "search"]
    }
    // … 5 more
  ],
  "commands": [
    {
      "id": "cairn-ingest",
      "path": "commands/cairn-ingest.md",
      "kind": "verb-direct",
      "verb": "ingest"
    }
    // … 12 more (9 verb-direct + 4 workflow)
  ],
  "hooks": {
    "SessionStart":     { "command": "cairn hook SessionStart" },
    "UserPromptSubmit": { "command": "cairn hook UserPromptSubmit" },
    "PreToolUse":       { "command": "cairn hook PreToolUse" },
    "PostToolUse":      { "command": "cairn hook PostToolUse" },
    "Stop":             { "command": "cairn hook Stop" }
  },
  "manual_fragment": "manual.md"
}
```

Schema invariants (enforced by `crates/cairn-cli/src/packs/manifest.rs`,
`serde(deny_unknown_fields)` on every struct):

1. `schema` is exactly `"cairn-pack/v1"`. Unknown values reject with
   `PackError::SchemaUnknown { got }`.
2. `pack_id`, `name`: snake/kebab token; no path separators or dot
   components. Same `is_safe_path_token` predicate already used by
   `SkillPackManifest`.
3. `version`: parseable as semver (use the `semver` crate already in
   workspace deps via `cairn-bench`/`cairn-workflows`; add to
   `cairn-cli`'s deps explicitly).
4. `cairn_mcp_compat`: range expression, MUST start with `>=` in v1.
   Future-compat with `>=, <` ranges deferred to v2.
5. `harness`: enum `claude-code` (extensible without breakage; unknown
   values reject at install with `HarnessMismatch`).
6. Every referenced file path resolves inside the pack root (no `..`,
   no absolute paths, no symlinks). Path tokens validated up-front.
7. Every `commands[].verb` matches an MCP tool name from
   `cairn_mcp::generated::TOOLS`. Compile-time aware via a const lookup
   table generated by `cairn-codegen`.
8. Every `subagents[].uses_mcp_tools` entry resolves to an MCP tool
   name (same table as 7). Manifest entries are **bare verb names**
   (e.g. `assemble_hot`). The on-disk subagent file at
   `packs/cairn-claude-code/agents/<id>.md` carries the Claude-Code
   frontmatter form (`tools: mcp__cairn__assemble_hot, ...`).
   Pass B **cross-validates** that every frontmatter `tools:` entry
   strips to `mcp__cairn__<bare>` for some `bare` listed in the
   manifest's `uses_mcp_tools`, and that the two sets agree exactly.
   No template rendering at install — files are copied verbatim.
9. Every `hooks.<event>` key is one of the five canonical lifecycle
   events (`HookName::ALL`).
10. Every `requires_capabilities[]` entry resolves in
    `cairn-core::status::advertise`. (Capability strings are stable
    per the `cairn.mcp.v1` semver contract — issue #140.)
11. `subagent.id` and `command.id` are unique within the pack.

### 4.1 Why not extend `SkillPackManifest`

`SkillPackManifest` (in `cairn-core::pipeline::skillify::pack`) models the
output of the skillify pipeline: a `.cairnpack` archive of evolved skill
bundles, each with a `candidate_id`, `lane`, `slug`, `bundle_version`, and
SHA-256 digest. Its validator gates on candidate gate reports
(`SkillPackBuildError::GateNotPassing`). Force-fitting subagent/command
entries into `SkillPackEntry` would couple two unrelated runtime concepts
and require either dropping the gate-report invariant or inventing fake
gate reports for static markdown. The cleaner answer is a parallel
manifest type for harness packs, which is what this design specifies.

Issue #182 quotes "SkillPack schema (#128)" in the implementation detail
section. We treat that as shorthand for "the pack-manifest concept
introduced in #128" rather than literal reuse of the `SkillPackManifest`
Rust type. The traceability map (`docs/design/traceability.md`) will be
updated to record this split.

## 5. Pack runtime (`crates/cairn-cli/src/packs/`)

### 5.1 Embedding (`embed.rs`)

```rust
use include_dir::{include_dir, Dir};

pub static CAIRN_CLAUDE_CODE_PACK: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../../packs/cairn-claude-code");
```

`include_dir` (workspace dep, added with this PR) recursively embeds every
file under `packs/cairn-claude-code/` into the `cairn` binary at build
time. Source files remain plain markdown/JSON editable in-tree; the
binary is self-contained per the P0 standalone invariant. Build-time
content is canonical — runtime never reads from disk.

### 5.2 Loading + validation (`manifest.rs`)

`PackManifest` mirrors the JSON shape with `serde` derives and
`deny_unknown_fields`. Validation runs in two passes:

- Pass A: structural — schema string, semver, path tokens, uniqueness.
- Pass B: cross-reference — MCP tools, capabilities, hook names. Pass B
  takes an `&McpToolIndex` and `&CapabilityTable` so tests can inject
  stubs. Production wiring reads from `cairn_mcp::generated::TOOLS` and
  `cairn_core::status::REMEDIATION`/`advertise`.

```rust
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("manifest invalid: {reason}")]
    ManifestInvalid { reason: String },
    #[error("unknown schema: {got}")]
    SchemaUnknown { got: String },
    #[error("harness mismatch: pack declares {want}, requested {got}")]
    HarnessMismatch { want: String, got: String },
    #[error("unknown capability `{cap}`")]
    CapabilityUnknown { cap: String },
    #[error("unknown MCP tool `{tool}`")]
    McpToolUnknown { tool: String },
    #[error("unknown hook event `{hook}`")]
    HookUnknown { hook: String },
    #[error("merge conflict in {file}: {reason}")]
    MergeConflict { file: String, reason: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

### 5.3 Install (`install.rs`)

```rust
pub struct PackInstallOpts {
    pub harness: Harness,
    pub project_dir: PathBuf,
    pub force: bool,
}

pub struct PackInstallReceipt {
    pub pack_id: String,
    pub version: String,
    pub files_created: Vec<PathBuf>,
    pub files_merged: Vec<PathBuf>,
    pub files_skipped: Vec<PathBuf>,
    pub warnings: Vec<String>,        // non-fatal capability mismatches, etc.
    pub degraded: bool,               // true if any required cap is unavailable
}
```

Install algorithm:

1. Resolve pack from registry by `harness`. Currently:
   `Harness::ClaudeCode → CAIRN_CLAUDE_CODE_PACK`.
2. Parse `pack.json` from embed; run Pass A + Pass B validation.
3. For each `subagents[i]`: write `<project_dir>/.claude/agents/<id>.md`
   from `agents/<file>`. Existing file with identical bytes → skip;
   different → guarded re-write (block markers in body); no markers and
   `force=false` → skip with warning.
4. For each `commands[i]`: write `<project_dir>/.claude/commands/<id>.md`
   under the same rules.
5. Merge `hooks/settings.json` into `<project_dir>/.claude/settings.json`.
   Existing user-added hooks for the same event are appended-to, not
   replaced. Block markers wrap the pack-owned entries so re-install can
   round-trip cleanly.
6. Merge `hooks/.mcp.json` snippet into `<project_dir>/.mcp.json` if
   present (registers the `cairn` MCP server entry).
7. Inject `manual.md` into `<project_dir>/CLAUDE.md` between
   `<!-- BEGIN CAIRN PACK MANUAL -->` and `<!-- END CAIRN PACK MANUAL -->`.
8. Return receipt.

Idempotency: running install twice with same pack version is a no-op
(every step is content-addressed by SHA-256 before write).

Capability degradation is **non-fatal at install time**. A pack that
requires `cairn.mcp.v1.search.semantic` installs successfully even if
the local Cairn deployment doesn't advertise that capability — the
receipt sets `degraded: true` and the warning surface tells the user
which subagents will hit `CapabilityUnavailable` at runtime. Fail-closed
remains the runtime contract; install stays permissive so a user can
prepare the pack ahead of enabling a capability.

### 5.4 Verify integration (`verify.rs`)

`crates/cairn-cli/src/plugins/verify.rs` already drives a conformance
suite per plugin. Pack-verify adds a new plugin-like entry:

- **Tier 1 (manifest):** Pass A + Pass B validation; every referenced
  file present in embed; every MCP tool / capability / hook name
  resolves.
- **Tier 2 (install round-trip):** install pack into tempdir, re-read,
  byte-compare to embed.
- **Tier 3 (snapshot):** every emitted file matches its committed
  `insta` snapshot.

`cairn plugins verify --pack cairn-claude-code` runs Tiers 1–3.
Default `cairn plugins verify` (no `--pack`) includes the bundled pack
alongside the existing 8 bundled crate plugins.

## 6. Pack content shape

### 6.1 Subagents (`agents/<id>.md`)

Claude Code subagent frontmatter + body. MCP-tools-only `tools:` field.
Every subagent body prescribes a bounded procedure with explicit MCP
tool calls and explicit safety boundaries.

Example (`agents/context-loader.md`):

```markdown
---
name: context-loader
description: Use when you need Cairn-resident context for a topic, person, or project before generating an answer.
tools: mcp__cairn__assemble_hot, mcp__cairn__retrieve, mcp__cairn__search
---

# Context Loader

You are a Cairn context-loader. Your job is to pull the smallest sufficient
context for the asked-about entity from the Cairn vault using MCP tools only.

## Procedure

1. Call `mcp__cairn__assemble_hot` with the topic/person/project as scope to
   get the hot-memory prefix.
2. If the prefix references record ids you don't yet hold, call
   `mcp__cairn__retrieve` with `target=record` for each.
3. If the prefix is thin (<3 records) or scope is broad, call
   `mcp__cairn__search` with `mode=hybrid` to discover adjacent records.
4. Return a single concise context block. Do NOT shell out to `cairn` —
   MCP only.

## Boundaries

- Never call `mcp__cairn__ingest` or `mcp__cairn__forget` — read-only.
- Never include record bodies above 500 chars per record in the return.
- If `assemble_hot` returns `CapabilityUnavailable`, fall back to `search`
  + `retrieve` and note the degradation in the response.
```

All six subagents follow this skeleton. The six are:

| id | purpose | MCP tools | write? |
|---|---|---|---|
| context-loader | dispatches `assemble_hot` + targeted `retrieve` for a topic, person, project | `assemble_hot`, `retrieve`, `search` | no |
| vault-librarian | runs `lint`, reports orphans, broken edges, schema drift | `lint` | no |
| forget-planner | dry-runs forget fan-out, returns FlushPlan for human review | `forget` (dry-run only) | no |
| consolidator | kicks the local consolidation workflow on demand | `lint`, `summarize` (persist) | yes |
| replay-checker | runs replay against a golden cassette and reports diffs | `capture_trace`, `retrieve` | no |
| trace-summarizer | calls `summarize` over a session / turn window | `summarize`, `retrieve` | no (unless `--persist`) |

### 6.2 Verb-direct slash commands

Shell out to the local `cairn` binary. The slash command is a UX
shortcut to the CLI verb — there is no value in indirecting through
MCP for what the CLI already does directly, and "CLI is ground truth"
(brief §8) is satisfied.

Example (`commands/cairn-ingest.md`):

```markdown
---
description: Direct Cairn `ingest` verb.
argument-hint: "<--kind k> <--body 'text'>"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn ingest $ARGUMENTS` and report the receipt.

If the user passed a free-text body without flags, default to
`cairn ingest --kind user --body "$ARGUMENTS"`.

Show the resulting record id and ingest mode.
<!-- END CAIRN PACK -->
```

Nine verb-direct commands, one per verb: `ingest`, `search`, `retrieve`,
`summarize`, `assemble` (→ `assemble_hot`), `capture-trace`, `lint`,
`forget`, `status`.

### 6.3 Workflow slash commands

Compose subagents and verbs into user-facing macros. Four total:

| id | composition |
|---|---|
| `/cairn-standup` | `trace-summarizer` over last N days + `context-loader` for open threads |
| `/cairn-wrap-up` | `capture-trace` of session + `consolidator` (lint + summarize persist) |
| `/cairn-audit` | `vault-librarian` lint report + `forget-planner` orphan dry-run |
| `/cairn-recall` | `context-loader` for a named topic / person |

### 6.4 Hooks (`hooks/settings.json`)

Maps the five Claude Code hook events to `cairn hook <Event>`
invocations.

```json
{
  "hooks": {
    "SessionStart":     [{ "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook SessionStart" }]}],
    "UserPromptSubmit": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook UserPromptSubmit" }]}],
    "PreToolUse":       [{ "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook PreToolUse" }]}],
    "PostToolUse":      [{ "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook PostToolUse" }]}],
    "Stop":             [{ "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook Stop" }]}]
  }
}
```

The installer merges this into the project's existing
`.claude/settings.json`, preserving user-added entries. Pack-owned
entries are wrapped in block markers inside the merged JSON via a custom
serializer that tags pack origin in a sibling `_pack` field
(`_pack: "cairn-claude-code@0.1.0"`). This lets re-install identify and
update its own entries without trampling user customisations.

### 6.5 Manual fragment (`manual.md`)

Block-guarded markdown injected into `CLAUDE.md`:

```markdown
<!-- BEGIN CAIRN PACK MANUAL -->
## Cairn (Claude Code reference pack)

This project uses the Cairn memory layer. Six subagents and 13 slash
commands are available.

### Subagents
| Agent | Purpose | MCP tools |
|---|---|---|
| context-loader | Pull minimal context for a topic | assemble_hot, retrieve, search |
| vault-librarian | Vault health report | lint |
| forget-planner | Dry-run forget plan | forget (dry-run only) |
| consolidator | Consolidate + summarize | lint, summarize |
| replay-checker | Replay vs golden cassette | capture_trace, retrieve |
| trace-summarizer | Session/turn rollups | summarize, retrieve |

### Slash commands

Verb-direct: /cairn-ingest, /cairn-search, /cairn-retrieve,
/cairn-summarize, /cairn-assemble, /cairn-capture-trace, /cairn-lint,
/cairn-forget, /cairn-status.

Workflow: /cairn-standup, /cairn-wrap-up, /cairn-audit, /cairn-recall.

### Safety boundaries

- `forget-planner` is dry-run only. Human approval required to commit.
- Subagents never shell out to `cairn`; they use MCP tools.
- Verb-direct commands shell out to the local `cairn` binary.
- Capture-trace commands MUST run inside the consent envelope (brief §14).
<!-- END CAIRN PACK MANUAL -->
```

## 7. Migration from `crates/cairn-cli/src/skill.rs`

The existing 1645 LOC `skill.rs` carries Claude-Code-specific content
inline. Migration:

- Delete `CLAUDE_REMEMBER_COMMAND`, `CLAUDE_FORGET_COMMAND`,
  `CLAUDE_RECALL_COMMAND`, `CLAUDE_GRAPH_COMMAND` consts.
- Delete `claude_slash_commands()`.
- Replace `install_claude_code_integration()` with a call into
  `packs::install::install_pack(Harness::ClaudeCode, project_dir, force)`.
- Keep `render_agent_markdown_block()` for harnesses that still use the
  inline path (Codex, Kiro, Cursor) until each gets its own pack.
- Keep `Agent` enum; it still drives the four inline integrations.
- Result: `skill.rs` drops to roughly 200 LOC handling only the
  non-pack harnesses.

The four commands the pack ships (`/cairn-ingest`, `/cairn-forget`,
`/cairn-recall` equivalents) supersede the four inline commands.
Behaviour is preserved (same `cairn` CLI calls), but argument hints
and bodies become richer.

## 8. Error handling

Library errors use `thiserror::Error` per CLAUDE.md §6.2. Top-level
binary surfaces map them to sysexits-style exit codes:

- Bad manifest / unknown schema → `EX_CONFIG` (78).
- Runtime install failure (filesystem, JSON merge) → `EX_UNAVAILABLE`
  (69), matches the existing `CapabilityUnavailable` mapping.
- Schema validation errors include the failing field path and the
  offending value in `PackError::ManifestInvalid.reason`.

Capability mismatch ≠ install failure (§5.3). Fail-closed remains the
runtime contract.

## 9. Testing strategy

1. **Unit (`crates/cairn-cli/src/packs/manifest.rs`):** every schema
   invariant 1–11 from §4 has a positive and a negative test.
   `proptest` round-trip on `serde_json::to_string` ↔ `from_str` for
   manifest stability.
2. **Unit (`crates/cairn-cli/src/packs/install.rs`):** tempdir install
   under `tempfile::tempdir()`; verifies file set + content;
   `force=false` skip-behaviour; idempotency (run twice → identical
   filesystem).
3. **Integration (`crates/cairn-cli/tests/claude_code_pack_install.rs`):**
   full install into tempdir; `insta` snapshot per emitted file under
   `crates/cairn-cli/snapshots/claude_code_pack_install__*.snap`. One
   snapshot per: 6 agents, 13 commands, 1 merged `settings.json`, 1
   merged `.mcp.json`, 1 CLAUDE.md fragment, 1 install receipt JSON.
4. **Conformance (`crates/cairn-cli/tests/claude_code_pack_verify.rs`):**
   runs `cairn plugins verify --pack cairn-claude-code`; asserts every
   Tier-1, Tier-2, Tier-3 case passes.
5. **Dogfood acceptance (`packs/cairn-claude-code/ACCEPTANCE.md`):**
   manual checklist exercised against
   `packs/cairn-claude-code/fixtures/dogfood-vault/` (5-record
   fixture); ensures every subagent and every command terminates
   with an expected MCP call sequence (recorded via the
   `cairn-mcp` test fixtures' call tracer).
6. **CI:** `cargo nextest run -p cairn-cli claude_code_pack` runs under
   the existing workspace job (`ci.yml`). No new CI job.

## 10. Dogfood fixture vault

`packs/cairn-claude-code/fixtures/dogfood-vault/`:

```
.cairn/config.yaml      # min config
purpose.md              # project purpose for the dogfood project
sources/2026-05-01-spec.md
raw/r_001.md            # captured user statement
raw/r_002.md            # ingested clip
raw/r_003.md            # captured trace fragment
wiki/concept-cairn.md   # concept page
```

Five records — enough to exercise `search`, `retrieve`, `summarize`,
`lint`, and `assemble_hot` against a known corpus; small enough to be
reviewable in a PR. Records are checked in and treated as part of the
pack contract — any change to fixture content requires updating
snapshots.

## 11. Acceptance checklist (dogfood)

`ACCEPTANCE.md`, run against the fixture vault in §10:

1. [ ] Install pack into tempdir via
       `cairn skill install --harness claude-code --target <tmp>`.
2. [ ] `/cairn-status` → expect capability table + advertised verbs.
3. [ ] `/cairn-ingest --kind user --body "test"` → expect new record id.
4. [ ] `/cairn-search test` → expect the record from step 3.
5. [ ] `/cairn-retrieve <id>` → expect record body.
6. [ ] Spawn `context-loader` for topic "cairn" → expect ≥1 record
       in result, all calls via `mcp__cairn__*`.
7. [ ] Spawn `vault-librarian` → expect lint report with zero
       criticals.
8. [ ] Spawn `forget-planner` for record from step 3 → expect dry-run
       FlushPlan; no actual delete.
9. [ ] Spawn `consolidator` → expect `summarize --persist` call
       trace + new summary record id.
10. [ ] Spawn `replay-checker` against a recorded cassette → expect
        zero diffs.
11. [ ] Spawn `trace-summarizer` for last session → expect summary.
12. [ ] `/cairn-standup --days 1` → expect composite output from
        `trace-summarizer` + `context-loader`.
13. [ ] `/cairn-wrap-up` → expect `capture_trace` + `summarize --persist`.
14. [ ] `/cairn-audit` → expect lint + orphan dry-run output.
15. [ ] `/cairn-recall cairn` → expect context-loader output.

## 12. Verification (per CLAUDE.md §8)

Run before PR:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen -- --write   # commit generated docs
```

`cairn-docgen` regenerates the pack reference page under
`docs/site/src/reference/generated/packs/cairn-claude-code.md` from
the manifest + content. CI gates on no-diff.

## 13. Risks and open questions

- **Manifest schema drift from issue wording.** Issue #182 cites the
  `SkillPack schema (#128)`. This design splits to a parallel
  `cairn-pack/v1` schema. Mitigation: §4.1 explains the rationale; the
  PR description quotes the issue and notes the deviation; traceability
  map (`docs/design/traceability.md`) is updated.
- **`include_dir!` build-time path.** Path resolves relative to
  `$CARGO_MANIFEST_DIR`, so the pack lives at
  `crates/cairn-cli/../../packs/cairn-claude-code/`. If the workspace
  layout changes, this needs to follow. Mitigation: a unit test under
  `embed.rs` asserts the pack contains a known file (`pack.json`) so
  build-time path breakage fails CI immediately.
- **Hook merge complexity.** Settings.json merges can collide with
  user-added hooks. Mitigation: pack-owned entries tagged with `_pack`
  marker (§6.4); merge is append-only by event; documented in
  `ACCEPTANCE.md` and the manual fragment.
- **Pack version vs cairn-core semver.** Pack ships at `0.1.0` with
  `cairn_mcp_compat: ">=1.0.0"`. Mismatch (running cairn predates the
  required MCP contract) → install rejects with `IncompatibleCairn`.
  Aligned with #140 (MCP semver freeze) — pack version evolves
  independently of core.
- **Subagent `tools:` allowlist accuracy.** Frontmatter must match
  Claude Code's actual MCP tool naming convention (`mcp__cairn__<verb>`).
  Mitigation: live integration test in `claude_code_pack_install.rs`
  parses every subagent frontmatter and asserts each `tools:` entry
  is well-formed.
- **Dogfood fixture cassette maintenance.** Recorded MCP cassettes
  drift when verb semantics change. Mitigation: cassette
  regeneration is a documented step in `ACCEPTANCE.md`; CI
  asserts cassette + snapshot agreement.

## 14. Out of scope (recap)

- Packs for Codex, Gemini, Cursor, OpenCode.
- Domain-specific workflows (perf reviews, incident playbooks).
- Dynamic frontmatter / host-app UI integrations.
- Third-party pack registry / distribution. Each third-party pack ships
  via its own repo and gets installed via a future
  `cairn skill install --pack-repo <url>` flow (separate issue).
