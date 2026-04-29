# Architecture

Cairn keeps one contract at the center: `cairn.mcp.v1`. The IDL under
`crates/cairn-idl/schema/` generates the Rust SDK types in `cairn-core`, clap
command builders in `cairn-cli`, MCP tool declarations in `cairn-mcp`, and the
installable Cairn skill under `skills/cairn`.

The user-facing adapters are intentionally thin:

- `cairn-cli` owns terminal parsing, config bootstrap, plugin inspection, and
  dispatch into verb handlers.
- `cairn-mcp` owns MCP tool declarations and the stdio handler surface.
- `cairn-core` owns domain types, config types, contract traits, plugin
  registry, and conformance checks.
- Bundled plugin crates register implementations against `cairn-core`
  contracts.

The current P0 implementation favors fail-closed behavior. If a command or
plugin cannot provide the claimed behavior, it reports that state explicitly
instead of degrading silently.
