---
description: Vault audit — librarian report + orphan impact inspection.
---

<!-- BEGIN CAIRN PACK -->
Run a vault audit. Read-only — no destructive verbs reachable.

1. Spawn the `vault-librarian` subagent: full lint report.
2. For each orphan finding, spawn the `forget-planner` subagent with
   the orphan record id. `forget-planner` does NOT call
   `mcp__cairn__forget`; it returns a read-only impact inspection
   (citations, broken edges, scope crossings) derived from `retrieve`,
   `search`, and `lint`. This is informational, not a CLI FlushPlan.
3. Render a consolidated report:
   - Lint summary (criticals / warnings / info).
   - Orphan candidates with the forget-planner impact summary
     (records + edges that would be affected by a forget).
4. Stop. To commit a forget, the user must run `/cairn-forget`
   themselves; this audit never shells out to a destructive verb.
<!-- END CAIRN PACK -->
