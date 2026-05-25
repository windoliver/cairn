# Status

> See the [capability matrix](reference/capability-matrix.md) for the
> authoritative per-phase capability list.

Cairn is pre-v0.1.

Implemented:

- Rust workspace and package boundaries
- IDL and codegen drift gate
- Generated CLI, SDK, MCP, and skill surfaces
- Config loader and `bootstrap`
- `status` and `handshake` preludes
- Vault registry commands and active-vault resolution
- Bundled plugin registry, list, and verify commands
- Stdio `cairn mcp` server entry point
- Cairn skill bundle install command
- Docs generator and mdBook source site
- Durable memory storage (`cairn-store-sqlite` wired into CLI dispatch)
- Real dispatch for the eight core memory verbs (`ingest`, `search`, `retrieve`, `summarize`, `assemble_hot`, `capture_trace`, `lint`, `forget`)
- Harness lifecycle hook runner (`cairn hook`)
- Legacy archive import planner (`cairn import`)
- Reference-consumer diagnostics (`cairn doctor`)
- Benchmark scorecards and release gates (`cairn bench`)
- Nexus sandbox setup and diagnostics (`cairn nexus`)
- Harness integration setup (`cairn setup`)
- Administrative operations (`cairn admin`)
- Vault backup management (`cairn backup`)
- LLM provider diagnostics (`cairn llm`)
- Screen sensor diagnostics (`cairn screen`)
- Session-tree inspection (`cairn session`)
- Federation share-link management (`cairn share`)
- Sensor consent and policy gates (`cairn sensor`)
- Operator repair commands (`cairn repair`)
- Vault identity management (`cairn identity`)
- Human-review FlushPlan management (`cairn flush`)
- Nexus sidecar reindex (`cairn reindex`)

Stubbed or pending:

- Non-stdio MCP transports (v0.2+)
- LLM-backed enrichment (requires configured provider; local embedding available)

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
