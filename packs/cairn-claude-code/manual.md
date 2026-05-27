<!-- BEGIN CAIRN PACK MANUAL -->
## Cairn (Claude Code reference pack)

This project uses the Cairn memory layer. Six subagents and 13 slash
commands are available.

### Subagents

| Agent | Purpose | MCP tools |
|---|---|---|
| context-loader | Pull minimal context for a topic | assemble_hot, retrieve, search |
| vault-librarian | Vault health report | lint |
| forget-planner | Forget-impact inspection (read-only) | retrieve, search, lint |
| consolidator | Consolidate + summarize | lint, summarize |
| replay-checker | Diff cassette records vs live vault | retrieve, search |
| trace-summarizer | Session / turn rollups | summarize, retrieve |

### Slash commands

**Verb-direct:** `/cairn-ingest`, `/cairn-search`, `/cairn-retrieve`,
`/cairn-summarize`, `/cairn-assemble`, `/cairn-capture-trace`,
`/cairn-lint`, `/cairn-forget`, `/cairn-status`.

**Workflow:** `/cairn-standup`, `/cairn-wrap-up`, `/cairn-audit`,
`/cairn-recall`.

### Safety boundaries

- `forget-planner` has no access to the `forget` MCP tool — it only
  reads (`retrieve`, `search`, `lint`). Destructive forgets go through
  `/cairn-forget`, which requires `--dry-run` preflight + explicit
  human confirmation before commit.
- Subagents never shell out to `cairn` — they use MCP tools only.
- Verb-direct slash commands shell out to the local `cairn` binary.
- `capture_trace` commands MUST run inside the user's consent envelope
  (see brief §14).
<!-- END CAIRN PACK MANUAL -->
