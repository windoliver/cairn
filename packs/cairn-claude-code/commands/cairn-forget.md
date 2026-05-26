---
description: Direct Cairn `forget` verb — deletes a record, session, or scope.
argument-hint: "<--record <id>|--session <id>|--scope <json>>"
---

<!-- BEGIN CAIRN PACK -->
Forget is destructive — the `cairn forget` CLI commits immediately.

ALWAYS spawn the `forget-planner` subagent FIRST to produce a dry-run
FlushPlan via MCP (`mcp__cairn__forget` with `dry_run=true`). Show the
plan to the user and require explicit confirmation before shelling out.

Once confirmed, run `cairn forget $ARGUMENTS`. Show the resulting receipt.
<!-- END CAIRN PACK -->
