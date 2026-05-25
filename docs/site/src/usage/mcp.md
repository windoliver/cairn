# MCP

For Claude Code, prefer the first-party setup command:

```bash
cairn setup claude-code --vault <name-or-path>
cairn doctor claude-code
```

See [Claude Code](claude-code.md) for the full reference-consumer workflow.

For Codex, use:

```bash
cairn setup codex --vault <name-or-path>
```

See [Codex](codex.md) for the second-consumer workflow, including the hook
fallback caveat.

`cairn-mcp` contains the lower-level MCP adapter crate, generated tool
declarations, plugin manifest, and stdio serving entry point.

Current truth:

- The generated MCP tool list covers the eight core verbs.
- `CairnMcpHandler` dispatches tool calls through `cairn_mcp::generated::TOOLS`
  to the fully wired verb layer backed by SQLite storage.
- The runtime `cairn mcp` command starts the stdio server and blocks until the
  client closes stdin or sends shutdown.

Use the generated [MCP tool reference](../reference/generated/mcp-tools.md) for
tool names, auth metadata, root capabilities, and mode-level overrides.

See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.
