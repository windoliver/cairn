# cairn-claude-code dogfood acceptance checklist

Run against `packs/cairn-claude-code/fixtures/dogfood-vault/` (5-record
fixture). Mark each step pass / fail.

1. [ ] Install: `cairn skill install --harness claude-code --target <tmp>`.
2. [ ] `/cairn-status` returns the capability table and the advertised
       verbs.
3. [ ] `/cairn-ingest --kind user --body "test"` returns a new record id.
4. [ ] `/cairn-search test` finds the record from step 3.
5. [ ] `/cairn-retrieve <id>` returns the record body.
6. [ ] Spawning `context-loader` for topic "cairn" returns at least one
       record, all calls via `mcp__cairn__*`.
7. [ ] Spawning `vault-librarian` returns a lint report with zero
       criticals.
8. [ ] Spawning `forget-planner` for the record from step 3 returns a
       dry-run FlushPlan and does NOT delete.
9. [ ] Spawning `consolidator` records a `summarize --persist` call and
       returns a new summary record id.
10. [ ] Spawning `replay-checker` against a recorded cassette returns
        zero diffs.
11. [ ] Spawning `trace-summarizer` for the last session returns a
        cited synthesis.
12. [ ] `/cairn-standup --days 1` returns a combined `trace-summarizer`
        + `context-loader` output.
13. [ ] `/cairn-wrap-up` runs `capture_trace` then `summarize --persist`.
14. [ ] `/cairn-audit` returns lint + orphan dry-run output.
15. [ ] `/cairn-recall cairn` returns context-loader output.
