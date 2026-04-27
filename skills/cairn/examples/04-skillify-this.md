# Example: skillify a procedure

**User says:** "Skillify this — we just figured out how to run the benchmarks."

**Cairn call:**
```bash
cairn ingest \
  --kind strategy_success \
  --body "Run benchmarks: cargo criterion --bench <name>; results in target/criterion/" \
  --tag benchmark,procedure
```

**Why `kind: strategy_success`:** A procedure that worked — worth keeping for next time.
