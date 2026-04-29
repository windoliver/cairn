# Example: remember a preference

**User says:** "Remember that I prefer snake_case for all variable names."

**Cairn call:**
```bash
cairn ingest --kind user --body "prefers snake_case for variable names"
```

**Why `kind: user`:** A preference about working style that should persist across sessions.
