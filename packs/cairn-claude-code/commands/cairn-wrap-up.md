---
description: End-of-session wrap-up — captures trace + persists summary.
---

<!-- BEGIN CAIRN PACK -->
Wrap up the current session.

1. Run `/cairn-capture-trace --session <current>` to persist the trajectory.
2. Spawn the `consolidator` subagent to lint + summarize any newly-stale
   records produced during the session.
3. Report:
   - Trace record id.
   - Summary record id (if `consolidator` produced one).
   - Open follow-ups not consolidated.
<!-- END CAIRN PACK -->
