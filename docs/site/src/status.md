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

## Open Work

GitHub is the live source for open work. Useful filtered views:

- [All open issues](https://github.com/windoliver/cairn/issues?q=is%3Aissue%20is%3Aopen)
- [P0 v0.1 issues](https://github.com/windoliver/cairn/issues?q=is%3Aissue%20is%3Aopen%20label%3Apriority%3AP0%20label%3Aphase%3Av0.1)
- [API surface issues (CLI, SDK, MCP, skill)](https://github.com/windoliver/cairn/issues?q=is%3Aissue%20is%3Aopen%20label%3Aarea%3Aapi)
- [Documentation issues](https://github.com/windoliver/cairn/issues?q=is%3Aissue%20is%3Aopen%20label%3Aarea%3Adocumentation)
- [Storage and WAL issues](https://github.com/windoliver/cairn/issues?q=is%3Aissue%20is%3Aopen%20label%3Aarea%3Astorage%20OR%20label%3Aarea%3Awal)

Known open P0 v0.1 themes include MCP request mapping, capability rejection,
skill compatibility, storage/dispatch, privacy gates, sensors, workflows,
packaging, and release gates. This docs site links to live issue queries instead
of committing a generated issue list, so CI does not need GitHub credentials to
build documentation.
