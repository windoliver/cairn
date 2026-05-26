---
description: Vault audit — librarian report + orphan forget dry-run.
---

<!-- BEGIN CAIRN PACK -->
Run a vault audit.

1. Spawn the `vault-librarian` subagent: full lint report.
2. For each orphan finding, spawn the `forget-planner` subagent with the
   orphan record id to get a dry-run FlushPlan.
3. Render a consolidated report:
   - Lint summary (criticals / warnings / info).
   - Orphan candidates with forget cost (records + edges that would be
     dropped).
4. Stop. Never commit a forget from this command — the user runs
   `/cairn-forget` explicitly.
<!-- END CAIRN PACK -->
