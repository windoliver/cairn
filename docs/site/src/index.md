# Cairn

Cairn is a Rust agent-memory framework built around the `cairn.mcp.v1`
contract. The project is pre-v0.1: the workspace, IDL, generated SDK/CLI/MCP
surfaces, config loader, and plugin registry are present, while durable memory
storage and the eight core verb implementations are still P0 stubs.

Current user-facing commands that work today:

- `cairn status`
- `cairn handshake`
- `cairn bootstrap`
- `cairn plugins list`
- `cairn plugins verify`

The eight memory verbs are already in the CLI and generated references:
`ingest`, `search`, `retrieve`, `summarize`, `assemble_hot`, `capture_trace`,
`lint`, and `forget`. They fail closed until storage and dispatch are wired.

Start with the [quickstart](quickstart.md), then use the generated
[CLI reference](reference/generated/cli.md) for exact flags.
