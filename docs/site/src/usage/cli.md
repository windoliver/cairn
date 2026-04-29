# CLI

The `cairn` binary is the primary local user interface. Its command tree is
generated partly from the IDL and partly from runtime management commands.

Implemented today:

- `status` reports contract and runtime status.
- `handshake` returns the contract prelude handshake.
- `bootstrap` writes the vault `.cairn/` layout.
- `vault add`, `vault list`, `vault switch`, and `vault remove` manage the
  local vault registry.
- `plugins list` shows bundled plugin registrations.
- `plugins verify` runs conformance checks.
- `mcp` starts the stdio MCP server.
- `skill install` writes the Cairn skill bundle for a supported agent harness.

Present but P0-stubbed:

- `ingest`
- `search`
- `retrieve`
- `summarize`
- `assemble_hot`
- `capture_trace`
- `lint`
- `forget`

Use the generated [CLI reference](../reference/generated/cli.md) for exact
usage, flags, and subcommands. CI regenerates that reference from the same
`clap::Command` tree used by the runtime binary.
