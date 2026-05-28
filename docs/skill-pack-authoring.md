# Skill-Pack Authoring Guide

This guide describes public harness packs: source directories that declare
`schema: "cairn-pack/v1"` in `pack.json` and are checked with
`cairn plugins verify --pack-path`. Verification uses a temporary install round-trip and does not install into the current project. These packs are
distinct from Skillify `.cairnpack` archives managed by `cairn skillpack`;
archive generation and Skillify packaging have their own lifecycle and are not
the authoring format for harness integrations.

## Pack Layout

Start with the generator when possible:

```bash
cairn skill new my-pack --harness codex
```

The Codex starter pack contains:

- `pack.json`, the manifest and compatibility contract.
- `AGENTS.md`, the operating manual fragment installed for Codex.
- `agents/context-loader.md`, a sample subagent prompt.
- `commands/cairn-context.md`, a sample slash command.
- `hooks/hooks.json`, hook bindings for the harness lifecycle.
- `tests/smoke.sh`, a local smoke test for the pack.
- `.github/workflows/verify.yml`, CI for strict verification and smoke tests.

Harness source and install targets differ in a few places:

| Harness | Source manual fragment | Installed manual target | Source hook file | Installed hook target |
|---|---|---|---|---|
| Claude Code | `manual.md` | `CLAUDE.md` | `hooks/settings.json` | `.claude/settings.json` |
| Codex | `AGENTS.md` | `AGENTS.md` | `hooks/hooks.json` | `.codex/hooks.json` |
| Gemini | `GEMINI.md` | `GEMINI.md` | `hooks/hooks.json` | `.gemini/hooks.json` |

For a larger, maintained Claude Code reference pack, inspect
`packs/cairn-claude-code/`.

Keep every manifest-referenced file inside the pack root. Use simple,
reviewable directory names such as `agents/`, `commands/`, `hooks/`, and
`tests/`; do not rely on symlinks, absolute paths, generated files outside the
pack, or paths that only exist after installation.

## Manifest Schema

Every harness pack must include a top-level `pack.json` with
`"schema": "cairn-pack/v1"`. The required fields are:

- `pack_id`: stable pack identifier and path token.
- `name`: display token for the pack.
- `version`: semver release version.
- `harness`: target harness, such as `codex`, `claude-code`, or `gemini`.
- `cairn_mcp_compat`: minimum Cairn MCP contract range, for example
  `">=1.0.0"`.
- `description`: short human-readable purpose.
- `requires_capabilities`: stable capability identifiers required by the pack.
- `subagents`: declared subagent prompts and their MCP tool allowlists.
- `commands`: declared slash commands and their CLI backing verbs.
- `hooks`: lifecycle hook event bindings.
- `manual_fragment`: pack-relative operating manual fragment path.

Use safe identifiers and paths. `pack_id`, `name`, command ids, and subagent ids
must be nonempty ASCII tokens containing only letters, digits, `-`, or `_`.
Pack-relative paths must not be absolute, empty, `.` or `..` based, non-ASCII,
backslash based, control-character based, or able to escape the pack root.
Choose ids that can remain stable across releases; changing an id is a
compatibility event, not a cosmetic edit.

A minimal Codex pack manifest looks like:

```json
{
  "schema": "cairn-pack/v1",
  "pack_id": "my-pack",
  "name": "my-pack",
  "version": "0.1.0",
  "harness": "codex",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Load Cairn context for Codex.",
  "requires_capabilities": [],
  "subagents": [
    {
      "id": "context-loader",
      "path": "agents/context-loader.md",
      "uses_mcp_tools": ["status"]
    }
  ],
  "commands": [
    {
      "id": "cairn-context",
      "path": "commands/cairn-context.md",
      "kind": "verb-direct",
      "verb": "status"
    }
  ],
  "hooks": {
    "SessionStart": {
      "command": "cairn hook SessionStart --payload-file - --json"
    }
  },
  "manual_fragment": "AGENTS.md"
}
```

## Capability Declarations

`requires_capabilities` declares what the host must advertise before the pack
can be installed safely. Use the canonical stable `cairn.mcp.v1` capability
identifiers from the Cairn contract, not ad hoc local names.

Declare only capabilities the pack actually needs. A slash command that shells
out to `cairn status` should not request write or vault capabilities. A
subagent that reads context should request read-oriented capabilities and list
only the typed MCP tools it calls in `uses_mcp_tools`.

## Hook Binding Contract

Harness packs bind only the canonical lifecycle events:

- `SessionStart`
- `UserPromptSubmit`
- `PreToolUse`
- `PostToolUse`
- `Stop`

Hook commands should forward the harness payload on stdin and ask Cairn for JSON
output:

```bash
cairn hook <event> --payload-file - --json
```

Use the concrete event name in generated bindings, for example
`cairn hook SessionStart --payload-file - --json`. Include extra flags such as
`--vault-path` only when the target environment requires them. Hook bindings
should be deterministic wrappers around Cairn, not scripts that parse or mutate
the vault independently.

## Subagent Prompt Contract

Subagents are prompt files declared in `subagents`. Each declaration names the
pack-relative path and the exact `uses_mcp_tools` allowlist. For Claude Code,
subagent YAML frontmatter must match the manifest allowlist so the runtime and
verifier agree on which tools the subagent may call. Codex and Gemini starter
subagents do not use YAML frontmatter; their harness-specific static checks
validate guarded manual blocks and `hooks/hooks.json` instead.

Subagents use typed MCP tool calls only. They must not write directly to the
database, write directly to a vault, bypass the WAL, call private Cairn internals,
or invent filesystem side effects that are not mediated by the declared tool
surface. Keep prompts narrow: describe when to call Cairn tools, what output to
return to the harness, and when to stop.

## Slash Command Contract

Slash commands are command prompt files declared in `commands`. A
`verb-direct` command wraps CLI ground truth: it should call the corresponding
`cairn <verb>` rather than re-describing the verb in prompt-only logic. A
`workflow` command may orchestrate subagents and verbs, but the authoritative
state changes still come from Cairn CLI or typed MCP calls.

Use ids that map cleanly to command filenames. Keep user-facing command text
short, and include enough argument guidance that the harness can pass through
flags without changing semantics.

## Operating Manual Fragments

The manual fragment teaches the target harness how the pack should be used after
installation. Wrap the complete fragment in ownership markers:

```markdown
<!-- BEGIN CAIRN PACK my-pack -->
Pack instructions go here.
<!-- END CAIRN PACK my-pack -->
```

The marker id must match `pack_id`. Cairn uses these markers to update pack-owned
content without overwriting unrelated local instructions. Use the harness manual
source and install target shown in the layout matrix. For Claude Code, the pack
source fragment is `manual.md` and the installer injects that guarded block into
the project's `CLAUDE.md`. For Codex, the source and target are both
`AGENTS.md`; for Gemini, the source and target are both `GEMINI.md`. The starter
scaffold chooses the correct fragment path for its selected harness.

## Versioning And Compatibility

Pack versions use semver. Increment patch for documentation-only or
backward-compatible prompt fixes, minor for new commands, subagents, hooks, or
capability additions that do not break existing users, and major for renamed ids,
removed files, changed command semantics, or stricter capability requirements.

`cairn_mcp_compat` is the minimum Cairn MCP contract the pack expects. Keep it as
low as the pack can honestly support, raise it when a pack starts using newer
tools or capabilities, and document the reason in the release notes. Do not use a
pack version bump to hide an incompatible host requirement; both fields matter.

## Publishing And CI

Before publishing or handing a pack to users, verify it from the pack root:

```bash
cairn plugins verify --pack-path . --strict
bash tests/smoke.sh
```

The generated GitHub Actions workflow should run those same commands. Treat
verification failures as release blockers: missing paths, unsafe ids, unknown
capabilities, Claude Code frontmatter drift, unknown hook events, invalid
harness hook JSON, and failed smoke scripts all indicate the pack is not
portable.

Publish source directories or archives according to the distribution channel,
but keep `pack.json`, manual fragments, prompts, hooks, commands, smoke tests,
and CI in version control. Consumers should be able to inspect the same files
that CI verified.

## Not In Scope For Packs

Harness packs do not define new Cairn MCP methods, new database schemas, new WAL
formats, new vault migration behavior, or new host-side trust policy. They do not
replace `cairn skillpack` or Skillify `.cairnpack` archives. They should not
vendor secrets, credentials, model keys, private vault content, or machine-local
absolute paths.

Packs also should not bypass Cairn to gain behavior that the manifest cannot
express. If a pack needs a new capability, hook event, CLI verb, or MCP tool,
add that to Cairn first and release the pack against the new public contract.

## Verification

Use the generator and verifier together when authoring:

```bash
cairn skill new my-pack --harness codex
cd my-pack
cairn plugins verify --pack-path . --strict
bash tests/smoke.sh
```

To verify a checked-out pack without changing directories, pass the path
directly:

```bash
cairn plugins verify --pack-path path/to/my-pack --strict
```

For local development, run the path-based verifier after every manifest,
subagent, hook, command, or manual-fragment change. The verifier parses the pack,
runs static checks, and performs a temporary install round-trip without writing
into the current project. For release candidates, run the verifier from a clean
checkout so generated or untracked local files do not mask missing pack content.
