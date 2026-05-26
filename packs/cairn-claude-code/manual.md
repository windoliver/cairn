<!-- BEGIN CAIRN PACK MANUAL -->
## Cairn (Claude Code reference pack)

This project uses the Cairn memory layer. Six subagents and 13 slash
commands are available.

### Subagents

| Agent | Purpose | MCP tools |
|---|---|---|
| context-loader | Pull minimal context for a topic | assemble_hot, retrieve, search |
| vault-librarian | Vault health report | lint |
| forget-planner | Dry-run forget plan | forget (dry-run only) |
| consolidator | Consolidate + summarize | lint, summarize |
| replay-checker | Replay vs golden cassette | capture_trace, retrieve |
| trace-summarizer | Session / turn rollups | summarize, retrieve |

### Slash commands

**Verb-direct:** `/cairn-ingest`, `/cairn-search`, `/cairn-retrieve`,
`/cairn-summarize`, `/cairn-assemble`, `/cairn-capture-trace`,
`/cairn-lint`, `/cairn-forget`, `/cairn-status`.

**Workflow:** `/cairn-standup`, `/cairn-wrap-up`, `/cairn-audit`,
`/cairn-recall`.

### Safety boundaries

- `forget-planner` is dry-run only. Human approval is required before
  any commit.
- Subagents never shell out to `cairn` — they use MCP tools only.
- Verb-direct slash commands shell out to the local `cairn` binary.
- `capture_trace` commands MUST run inside the user's consent envelope
  (see brief §14).
<!-- END CAIRN PACK MANUAL -->
