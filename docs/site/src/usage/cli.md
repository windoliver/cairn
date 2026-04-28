# CLI

The `cairn` binary is the primary local user interface. Its command tree is
generated partly from the IDL and partly from runtime management commands.

Implemented today:

- `status` reports contract and runtime status.
- `handshake` returns the contract prelude handshake.
- `bootstrap` writes `.cairn/config.yaml`.
- `plugins list` shows bundled plugin registrations.
- `plugins verify` runs conformance checks.

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
