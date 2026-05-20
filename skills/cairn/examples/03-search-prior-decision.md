# Example: search for a prior decision

**User says:** "What did we decide about the database schema?"

**Cairn call:**
```bash
cairn search "database schema decision" --limit 10 --json
```

**Parse the JSON response:**
```json
{"hits":[
{"id":"01HQZ...","kind":"fact","body":"decided to use sqlite-vec for ANN","score":0.91}
]}
```
