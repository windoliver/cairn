# Capability Model

Cairn exposes capability strings through the IDL and plugin contracts so callers
can decide what is safe to invoke before doing work.

Examples:

- Search modes expose separate capabilities such as
  `cairn.mcp.v1.search.keyword`, `cairn.mcp.v1.search.semantic`, and
  `cairn.mcp.v1.search.hybrid`.
- Retrieve and forget modes expose separate mode-level capabilities.
- Bundled plugins expose contract-specific feature booleans and are checked by
  `cairn plugins verify`.

The generated [contract verb reference](../reference/generated/contract-verbs.md),
[MCP tool reference](../reference/generated/mcp-tools.md), and
[plugin reference](../reference/generated/plugins.md) are the committed views
that CI keeps in sync.
