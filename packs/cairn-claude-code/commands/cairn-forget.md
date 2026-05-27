---
description: Direct Cairn `forget` verb — deletes a record, session, or scope.
argument-hint: "<--record <id>|--session <id>|--scope <json>>"
---

<!-- BEGIN CAIRN PACK -->
Forget is destructive — the `cairn forget` CLI commits immediately and
has no `--dry-run` flag.

REQUIRED preflight (NOT optional, NOT skippable):

1. Spawn the `forget-planner` subagent with the same target the user
   passed. It will inspect the footprint via read-only MCP tools
   (`retrieve`, `search`, `lint`) and return a human-readable plan
   describing which records would be deleted, which edges would break,
   and which records reference the target.
2. Show the plan to the user verbatim and ASK FOR EXPLICIT CONFIRMATION.
   A simple "yes" is not enough — the user must repeat back the target
   id or scope they are forgetting.
3. Only after confirmation, run `cairn forget $ARGUMENTS`. Show the
   resulting receipt.

If the user refuses or hesitates, ABORT and do not shell out.
<!-- END CAIRN PACK -->
