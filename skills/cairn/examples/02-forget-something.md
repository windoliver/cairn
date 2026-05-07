# Example: forget a stored fact

**User says:** "Forget what I said about preferring tabs."

**Cairn calls (two steps):**
```bash
# 1. Find the record id
cairn search "tabs preference" --limit 5 --json

# 2. Delete it (confirm with user before running forget)
cairn forget --record <id-from-search>
```

**Non-negotiable:** Always confirm with the user before running `cairn forget`.
