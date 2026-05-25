# Rust API

Rust API reference is built with rustdoc in CI:

```bash
cargo doc --workspace --no-deps --document-private-items --locked
```

Package map:

- `cairn-core`: domain types, config types, contract traits, generated SDK
  types, plugin registry, and conformance checks.
- `cairn-cli`: CLI config helpers, plugin renderers, verb handlers, command
  tree, and docs generator internals.
- `cairn-sdk`: typed in-process SDK surface over the eight verbs (`status`,
  `handshake`). Depends on `cairn-core` only.
- `cairn-mcp`: MCP handler, transport entry point, plugin registration, and
  generated tool declarations.
- `cairn-idl`: IDL loader, IR, and code generators.
- `cairn-store-sqlite`, `cairn-sensors-local`, `cairn-workflows`: bundled P0
  plugin implementations.
- `cairn-bench`: retrieval-quality scorecard and release gate harness
  (latency, memory, privacy, SRE, coherence).
- `cairn-agent-core`, `cairn-keychain`, `cairn-llm-openai-compat`,
  `cairn-embeddings-local`, `cairn-embeddings-openai`: optional adapters
  (P1+). Not part of the P0 binary.
- `cairn-desktop`, `cairn-frontend-logseq`, `cairn-frontend-obsidian`,
  `cairn-frontend-vscode`: frontend integration crates (P1+).
- `cairn-test-fixtures`: contributor-only test fixtures.

`docs / cargo doc` runs with rustdoc warnings denied, so broken intra-doc links
fail CI.
