# Codex

Codex is Cairn's v0.2 second consumer. Register Cairn as a stdio MCP server
with one command:

```bash
cairn setup codex --vault work
```

By default this writes `~/.codex/config.toml` and points Codex at a project-local
`.codex/hooks.json` file. The MCP registration launches:

```bash
cairn --vault <vault> mcp
```

Project scope is available when you intentionally want the Codex config to live
with the checkout:

```bash
cairn setup codex --scope project --vault work
```

Codex hook support is best-effort. Cairn writes the five standard hook commands
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`) into
`.codex/hooks.json`, but Codex deployments that do not load that hook file
should use the skill and explicit replay path instead:

```bash
cairn skill install --harness codex
cairn capture_trace --from <transcript.jsonl> --json
```

This gives Codex the same Cairn surfaces through MCP, CLI, and skill fallback,
while making the unsupported hook path visible instead of pretending there is
full Claude Code parity.

The committed `codex_consumer` acceptance replay covers Codex-shaped
`capture_trace` events plus `assemble_hot`, `search`, `retrieve`, `summarize`,
`lint`, and `forget` checks against a deterministic temp vault.

Remove only the Cairn server entry:

```bash
cairn setup codex --vault work remove
```
