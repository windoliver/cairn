---
description: Inspect Cairn forget impact for a target. Does NOT commit — Claude will refuse to shell out the destructive form.
argument-hint: "<--record <id>|--session <id>|--scope <json>>"
---

<!-- BEGIN CAIRN PACK -->
This command is INTENTIONALLY plan-only in this pack version. It will
NOT run a destructive `cairn forget`. Two reasons:

1. `cairn forget --dry-run` returns a placeholder FlushPlan today
   (`placeholder: true`); the real deletion planner is pending in
   issue #9. Approving a placeholder plan does not guarantee the
   actual commit deletes only what the plan showed.
2. Destructive verbs in this reference pack are never reachable from
   an LLM-driven slash command. The user runs the irreversible step
   themselves, with their own eyes on the terminal output.

Steps:

1. Run `cairn forget --dry-run $ARGUMENTS` and show the placeholder
   plan to the user.
2. Spawn the `forget-planner` subagent for an MCP-only, read-only
   impact inspection (record citations, broken edges, orphan risk)
   that complements the placeholder CLI plan.
3. Tell the user: "To commit, run `cairn forget $ARGUMENTS` yourself
   in a terminal once #9 ships the real planner." Do NOT shell out
   to the destructive form. Refuse if the user insists.

If the user wants to bypass this safety policy, they can edit
`.claude/commands/cairn-forget.md` in their own project. That puts
the override on them, not on this reference pack.
<!-- END CAIRN PACK -->
