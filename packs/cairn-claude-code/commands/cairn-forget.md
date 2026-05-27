---
description: Direct Cairn `forget` verb — deletes a record, session, or scope.
argument-hint: "<--record <id>|--session <id>|--scope <json>> [--dry-run|--human-review]"
---

<!-- BEGIN CAIRN PACK -->
Forget is destructive — once committed, the records are gone.

REQUIRED preflight (NOT optional, NOT skippable):

1. Run `cairn forget --dry-run $ARGUMENTS` FIRST. The `--dry-run` flag
   prints the FlushPlan that the commit would produce, without writing
   anything. (`--human-review` is an equivalent alternative that
   persists the plan to `.cairn/flush/pending/` for review.)
2. Show the FlushPlan to the user and ASK FOR EXPLICIT CONFIRMATION.
   A simple "yes" is not enough — the user must repeat back the target
   id or scope they are forgetting.
3. Only after confirmation, run `cairn forget $ARGUMENTS` (without
   `--dry-run`). Show the resulting receipt.

If the user refuses or hesitates, ABORT and do not commit.

Optional: spawn the `forget-planner` subagent for an MCP-only,
read-only impact analysis (record citations, orphan implications)
that complements the CLI FlushPlan.
<!-- END CAIRN PACK -->
