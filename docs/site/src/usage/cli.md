# CLI

The `cairn` binary is the primary local user interface. Its command tree is
generated partly from the IDL and partly from runtime management commands.

## Core verbs (v0.1)

All eight verbs are fully wired with SQLite storage in v0.1:

- `ingest` — persist records into the vault.
- `search` — query the vault by keyword, semantic, or hybrid mode.
- `retrieve` — fetch a record, session, or folder by id.
- `summarize` — roll up a list of record ids into a synthesis.
- `assemble_hot` — load the hot-memory prefix for the current turn.
- `capture_trace` — persist a reasoning trajectory for ACE distillation.
- `lint` — check vault health (contradictions, orphans, stale claims).
- `forget` — delete a record, session, or scope.

## Management commands (v0.1)

- `status` reports contract and runtime status.
- `handshake` returns the contract prelude handshake.
- `bootstrap` writes the vault `.cairn/` layout.
- `doctor claude-code` verifies the v0.1 reference-consumer setup end to end.
- `vault add`, `vault list`, `vault switch`, and `vault remove` manage the
  local vault registry.
- `plugins list` shows bundled plugin registrations.
- `plugins verify` runs conformance checks.
- `mcp` starts the stdio MCP server.
- `skill install` writes the Cairn skill bundle for a supported agent harness.

Use the generated [CLI reference](../reference/generated/cli.md) for exact
usage, flags, and subcommands. CI regenerates that reference from the same
`clap::Command` tree used by the runtime binary.

See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.
