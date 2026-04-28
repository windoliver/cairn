# Status

Cairn is pre-v0.1.

Implemented:

- Rust workspace and package boundaries
- IDL and codegen drift gate
- Generated CLI, SDK, MCP, and skill surfaces
- Config loader and `bootstrap`
- `status` and `handshake` preludes
- Bundled plugin registry, list, and verify commands
- Docs generator and mdBook source site

Stubbed or pending:

- Durable memory storage
- Real dispatch for the eight core memory verbs
- A runtime `cairn mcp` CLI subcommand
- Non-stdio MCP transports
- LLM-backed enrichment
