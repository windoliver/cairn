---
description: Direct Cairn `forget` verb — deletes a record, session, or scope.
argument-hint: "<--record <id>|--session <id>|--scope <scope>> [--dry-run]"
---

<!-- BEGIN CAIRN PACK -->
ALWAYS run with `--dry-run` first unless the user has explicitly confirmed
a destructive forget on this exact target.

`cairn forget $ARGUMENTS`

Show the FlushPlan and ask for confirmation before any non-dry-run call.
<!-- END CAIRN PACK -->
