# MCP

`cairn-mcp` contains the MCP adapter crate, generated tool declarations, plugin
manifest, and stdio serving entry point.

Current truth:

- The generated MCP tool list exists for the eight core verbs.
- `CairnMcpHandler` can list tools from `cairn_mcp::generated::TOOLS`.
- Tool calls return a P0 dispatch stub until verb dispatch is wired.
- The runtime `cairn` CLI does not expose a `cairn mcp` subcommand today.

Use the generated [MCP tool reference](../reference/generated/mcp-tools.md) for
tool names, auth metadata, root capabilities, and mode-level overrides.
