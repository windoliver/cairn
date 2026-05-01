# Example: lint memory vault

**User says:** "Lint my memory" or run on a daily cadence.

**Cairn call:**
```bash
cairn lint --json
```

**Sample response:**
```json
{"issues": [
{"kind": "contradiction", "ids": ["01HQZ...", "01HQY..."], "summary": "two rules conflict"},
{"kind": "stale",         "id":  "01HQX...",               "summary": "fact older than 90 days"}
]}
```

**Why `lint`:** Detects contradictions, orphans, stale claims, and missing concept pages. Run on a cadence (daily or on PR) — not per turn.
