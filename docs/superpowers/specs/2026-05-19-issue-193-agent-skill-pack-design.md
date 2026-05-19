# Design: issue #193 agent skill-pack install

**Issue:** [#193](https://github.com/windoliver/cairn/issues/193)
**Design source:** `CLAUDE.md` section 8.0 and design brief sections 4, 8.0, 9.3, and 11
**Date:** 2026-05-19
**Status:** approved for first implementation slice

---

## Summary

Extend `cairn skill install` from a skill-bundle writer into an agent integration
writer. The first implementation slice writes deterministic, idempotent harness
files for Claude Code, Codex/OpenCode, Kiro, and Cursor while preserving the
existing `--harness` install behavior from issue #68.

The installed integration advertises Cairn as a memory and knowledge layer,
registers or describes the MCP server where the harness supports it, and gives
each harness a session-start instruction to run:

```bash
cairn ingest --folder . --mode keyword
```

The hook is represented as a generated command or instruction only. Runtime
nonblocking execution is owned by the harness and the existing Cairn CLI surfaces.

---

## Scope

### In scope

- Add `cairn skill install --agent <agent>` for:
  - `claude-code`
  - `codex`
  - `kiro`
  - `cursor`
- Add `cairn skill install --all`.
- Keep `cairn skill install --harness <harness>` working as a compatibility alias.
- Write generated harness files without overwriting unrelated user content.
- Use guarded Cairn blocks in markdown-like files.
- Emit stable JSON and human receipts that list skill bundle writes and agent
  integration writes.
- Add snapshot/unit tests for generated fragments and idempotency.

### Out of scope

- Running `claude mcp list` from tests.
- Proving background execution semantics inside Claude Code, Codex, Kiro, or Cursor.
- Adding new hook runtime commands beyond the existing `cairn hook` and verb CLI.
- Implementing `surprising_connections` if the MCP graph traversal tool is not
  already available.
- Changing `cairn setup claude-code`; this install path may reuse its JSON helper
  patterns but must not replace it.

---

## CLI Shape

`cairn skill install` accepts these options:

```bash
cairn skill install --agent claude-code
cairn skill install --agent codex
cairn skill install --agent kiro
cairn skill install --agent cursor
cairn skill install --all
cairn skill install --harness claude-code
```

Rules:

- `--agent` and `--all` are mutually exclusive.
- `--harness` remains accepted and maps to `--agent` for known overlapping
  values.
- `custom`, `gemini`, and `opencode` remain valid compatibility harnesses for
  the skill bundle and registration hint, but are not part of the first #193
  integration writer unless passed through `--harness`.
- `--target-dir`, `--force`, and `--json` keep their current meanings.
- The default project directory for generated harness files is the current
  working directory.

---

## Generated Content

### Shared Cairn guidance

Every generated agent-facing snippet includes:

- One sentence explaining Cairn's role.
- Positive trigger: use Cairn when persistent memory, recall, or graph-aware
  connections are needed.
- Negative trigger: do not use Cairn tools for ordinary file reads or code
  execution.
- Exclusivity hint: prefer exact search/retrieve paths when the user names a
  known record or concept; use graph exploration only for non-obvious
  connections.
- Session-start instruction: run `cairn ingest --folder . --mode keyword`.
- Hot context instruction: use `cairn assemble_hot` to build a memory prefix
  when the harness supports context preambles.

Generated markdown blocks use this guard:

```markdown
<!-- BEGIN CAIRN AGENT SKILL -->
...
<!-- END CAIRN AGENT SKILL -->
```

Reinstall replaces only the guarded block. If the file has user content outside
the block, it is preserved byte-for-byte.

### Claude Code

Files:

- `.claude/settings.json`
- `CLAUDE.md`

Behavior:

- Merge a `mcpServers.cairn` entry with `command: "cairn"` and
  `args: ["mcp"]`.
- Merge a session-start hook command that invokes
  `cairn ingest --folder . --mode keyword`.
- Append or replace the guarded Cairn block in `CLAUDE.md`.
- Preserve unrelated JSON keys and unrelated markdown content.

### Codex / OpenCode

Files:

- `AGENTS.md`

Behavior:

- Append or replace the guarded Cairn block.
- Include the session-start command as an instruction because Codex hook file
  formats are not stable in this repository yet.

### Kiro

Files:

- `.kiro/steering/cairn.md`

Behavior:

- Write a deterministic steering file with frontmatter:

```markdown
---
inclusion: always
---
```

- Include the shared Cairn guidance after the frontmatter.
- Treat the file as generated; `--force` may overwrite it, otherwise reinstall
  only rewrites it when it already contains the Cairn guard.

### Cursor

Files:

- `.cursor/rules/cairn.mdc`

Behavior:

- Write a deterministic rule file with frontmatter:

```markdown
---
alwaysApply: true
---
```

- Include the shared Cairn guidance after the frontmatter.
- Do not mutate legacy `.cursorrules` in the first slice; the human output may
  mention the modern `.cursor/rules/cairn.mdc` path.

---

## Data Flow

```text
cairn skill install --agent claude-code
  -> install or refresh ~/.cairn/skills/cairn
  -> resolve project directory from cwd
  -> render Claude Code integration fragments
  -> merge JSON and guarded markdown blocks
  -> return receipt with files_created, files_skipped, and integrations
```

`--all` repeats the integration render/write phase for each first-slice agent
after installing the shared skill bundle once.

---

## Error Handling

- Invalid combinations such as `--agent codex --all` fail with clap usage errors.
- Malformed JSON in `.claude/settings.json` fails closed and does not rewrite the
  file.
- Existing Kiro/Cursor generated files without the Cairn guard are treated as
  user-owned unless `--force` is passed.
- File system write failures return the existing `EX_IOERR` path.
- No network or LLM configuration is required.

---

## Testing

Unit tests in `crates/cairn-cli/src/skill.rs` cover:

- Agent enum parsing and compatibility mapping from `Harness`.
- Guarded markdown insertion into an absent file, a user-authored file, and a
  file with an existing Cairn block.
- Claude settings JSON merge that preserves unrelated keys.
- Kiro and Cursor generated-file idempotency.
- `--all` receipt includes all first-slice agents.

Snapshot tests cover:

- Shared Cairn markdown block.
- Claude settings fragment.
- Kiro steering file.
- Cursor rule file.
- Human and JSON receipts after a first install.

CLI integration tests cover:

- `cairn skill install --agent codex --target-dir <tmp>` creates `AGENTS.md`
  in the current project directory and the skill bundle in `<tmp>`.
- `cairn skill install --all --json` reports every generated integration.

---

## Acceptance Mapping

- Correct `settings.json` and `CLAUDE.md` without overwriting existing content:
  covered by the Claude merge and guarded-block tests.
- MCP registration intent: `mcpServers.cairn` is written; external
  `claude mcp list` verification remains manual.
- Session-start hook: generated command uses
  `cairn ingest --folder . --mode keyword`.
- Slash commands: first slice documents `/remember` and `/forget` mappings in
  generated guidance; native slash-command files can land separately if the
  harness path is confirmed.
- Offline behavior: generated hook uses keyword mode only.
- Stable snapshots: fragment and receipt snapshots pin output.

---

## Invariants

- CLI remains ground truth; generated skill files are wrappers over `cairn`
  commands.
- Core remains untouched; all work lives in `cairn-cli` install helpers and tests.
- Harness-specific logic stays in CLI adapter code, not `cairn-core`.
- Generated writes are idempotent and preserve user-authored content outside
  explicit Cairn-owned regions.
