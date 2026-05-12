# Example: retrieve context for a session

**User says:** "Load my preferences for this session."

**Cairn call:**
```bash
cairn assemble_hot --session ${SESSION_ID} --json
```

**Why `assemble_hot`:** Loads the hot-memory prefix — your preferences, active rules, and recent context — so they are in scope for the rest of the session. Call once per session, not per turn.

**When `SESSION_ID` is unknown:** Omit `--session`; Cairn resolves the current session from `$CAIRN_SESSION_ID` or creates one.
