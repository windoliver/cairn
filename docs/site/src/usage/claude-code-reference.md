# Claude Code Reference Consumer

This guide verifies the v0.1 reference-consumer path from design brief §18.c
and §19: Claude Code should discover Cairn over MCP, run the five lifecycle
hooks, and exercise the P0 memory stories against a dogfood or temporary vault.

Use a temporary vault for rehearsal:

```bash
cairn bootstrap --vault-path /tmp/cairn-cc-vault
cairn vault add /tmp/cairn-cc-vault --name cc-dogfood
cairn vault switch cc-dogfood
```

Register the local project with Claude Code:

```bash
cairn setup claude-code \
  --project-dir . \
  --vault /tmp/cairn-cc-vault
```

Then run the non-mutating diagnostic:

```bash
cairn doctor claude-code --project-dir . --json
```

Expected outcome: every stage is `ok`: MCP config, binary resolution,
registration shape, MCP startup, `status` over MCP, and all five hook entries.
If a stage fails, fix the stage-specific remediation before continuing.

## Hook Loop Smoke

The installer writes `.claude/settings.local.json` using Claude Code's nested
hook shape. Each command reads Claude Code's hook JSON from stdin and writes
artifacts under `.cairn/hooks/`.

Run a local hook-loop rehearsal:

```bash
printf '{"session_id":"cc-smoke","hook_event_name":"SessionStart","cwd":"%s"}' "$PWD" \
  | cairn hook SessionStart --vault-path /tmp/cairn-cc-vault --payload-file - --json

printf '{"session_id":"cc-smoke","hook_event_name":"UserPromptSubmit","prompt":"remember the release checklist","cwd":"%s"}' "$PWD" \
  | cairn hook UserPromptSubmit --vault-path /tmp/cairn-cc-vault --payload-file - --json

printf '{"session_id":"cc-smoke","hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"toolu_smoke","tool_input":{"command":"cairn status --json"},"cwd":"%s"}' "$PWD" \
  | cairn hook PreToolUse --vault-path /tmp/cairn-cc-vault --payload-file - --json

printf '{"session_id":"cc-smoke","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"toolu_smoke","tool_input":{"command":"cairn status --json"},"tool_response":{"stdout":"ok","stderr":"","interrupted":false},"cwd":"%s"}' "$PWD" \
  | cairn hook PostToolUse --vault-path /tmp/cairn-cc-vault --payload-file - --json

printf '{"session_id":"cc-smoke","hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"done","cwd":"%s"}' "$PWD" \
  | cairn hook Stop --vault-path /tmp/cairn-cc-vault --payload-file - --json
```

Expected outcome:

- `SessionStart` returns a hot-memory artifact path.
- `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, and `Stop` each write a
  trace artifact.
- `PreToolUse` and `PostToolUse` preserve Claude Code's `tool_use_id` as
  Cairn's `tool_call_id`.
- `Stop` writes one pending post-turn queue artifact.

Inspect artifacts:

```bash
find /tmp/cairn-cc-vault/.cairn/hooks -type f | sort
```

## P0 Acceptance Checklist

Run these checks inside an actual Claude Code session after `doctor` passes.
Use `cairn status --json` first and only run capability-specific checks that
the runtime advertises. Capability failures should be explicit
`CapabilityUnavailable` errors, never silent fallbacks.

| Story | Check | Expected outcome | Triage area |
|---|---|---|---|
| US1 turn sequence | Submit a prompt and let `Stop` fire. Inspect `.cairn/hooks/traces`. | Prompt, tool, and stop events share one `session_id` and preserve order by artifact time. | #102 hook mapping |
| US2 active reload | Restart Claude Code in the same project and run `SessionStart`. | Hot-memory hook runs without replacing the session id or losing the vault path. | #14 hot memory / #102 hooks |
| US3 user memory | Ask Claude Code to remember a durable preference, then run `cairn search --json --query <term>`. | A committed memory is searchable, subject to capability advertisement. | #11 / #18 memory verbs |
| US4 rolling summary | Drive enough turns to trigger the configured consolidation cadence. | Summary artifacts or workflow status show a rolling-summary pass, not reflection tiers. | #14 consolidation |
| US5 tool calls | Run one tool from Claude Code. | `PreToolUse` and `PostToolUse` traces share the same `tool_call_id`. | #102 hook mapping |
| US7 keyword | `cairn search --mode keyword --query <term> --json`. | Keyword search returns matching memories or an empty committed result. | #18 search |
| US7 semantic | `cairn search --mode semantic --query <term> --json`. | Runs only when semantic is advertised; otherwise rejects fail-closed. | #18 search / local embeddings |
| US7 hybrid | `cairn search --mode hybrid --query <term> --json`. | Runs only when hybrid is advertised; otherwise rejects fail-closed. | #18 search / local embeddings |
| US8 record forget | Search or retrieve a real record id, confirm with the user, then run `cairn forget --record <id> --json`. | The record no longer appears in search or retrieve results. | #14 forget / privacy |

## Dogfood Checklist

Use this shorter loop for daily maintainer runs:

- `cairn doctor claude-code --json` is green before starting.
- `SessionStart` produces hot memory for the active vault.
- `UserPromptSubmit` and `Stop` write hook traces for the current session.
- At least one tool loop records matching `PreToolUse` and `PostToolUse`
  `tool_call_id` values.
- `cairn search --mode keyword --json` works against dogfood memories.
- Semantic and hybrid search either work or fail closed according to
  `cairn status --json`.
- Rolling-summary status is checked after a long session.
- Record-level `forget` is verified only after explicit user confirmation.

## Safe Removal

To remove the integration, delete the `cairn` MCP server entry from `.mcp.json`
and remove the five Cairn hook matcher groups from `.claude/settings.local.json`.
Leave unrelated Claude Code hooks and MCP servers in place. Re-run
`cairn doctor claude-code --project-dir . --json`; it should fail at the removed
stage with remediation instead of mutating any config.
