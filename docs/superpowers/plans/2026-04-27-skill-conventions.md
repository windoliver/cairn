# Skill Conventions (Issue #69) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expand the generated `skills/cairn/SKILL.md` and `conventions.md` to include the operational §18.d guidance — user-intent trigger table, output format, non-negotiable privacy rules — and add two missing example files (retrieve-context, lint-memory).

**Architecture:** All prose lives in `emit_skill.rs` (the cairn-codegen skill emitter). No IDL verb JSON files need changing — the new sections are harness-level guidance, not schema. Examples are write-once stubs emitted by the same function and write-protected by `cairn skill install`. After changing the emitter, regenerate `skills/cairn/` with `cargo run -p cairn-idl --bin cairn-codegen` and update the insta snapshot.

**Tech Stack:** Rust, insta snapshot tests, `cargo nextest`, `cargo run -p cairn-idl --bin cairn-codegen`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/cairn-idl/src/codegen/emit_skill.rs` | Modify | Add trigger table, output-format, non-negotiable-rules sections to `emit_skill_md()`; add taxonomy note to `emit_conventions()`; add examples 05 + 06 to `emit_examples()` |
| `crates/cairn-idl/tests/codegen_emit_skill.rs` | Modify | New assertion tests for the three new sections and two new examples |
| `crates/cairn-idl/tests/snapshots/codegen_snapshot__snapshot_skill_md.snap` | Update | Accept new snapshot after `cargo insta review` |
| `skills/cairn/SKILL.md` | Regenerate | `cargo run -p cairn-idl --bin cairn-codegen` |
| `skills/cairn/conventions.md` | Regenerate | `cargo run -p cairn-idl --bin cairn-codegen` |
| `skills/cairn/examples/05-retrieve-context.md` | Create | `cargo run -p cairn-idl --bin cairn-codegen` |
| `skills/cairn/examples/06-lint-memory.md` | Create | `cargo run -p cairn-idl --bin cairn-codegen` |

---

## Task 1: Write failing tests for new SKILL.md sections

**Files:**
- Modify: `crates/cairn-idl/tests/codegen_emit_skill.rs`

- [ ] **Step 1: Add four new failing test functions**

Append to the bottom of `crates/cairn-idl/tests/codegen_emit_skill.rs`:

```rust
#[test]
fn skill_md_contains_trigger_table() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    // §18.d "When to call cairn" table
    assert!(body.contains("## When to call cairn"), "missing trigger table heading");
    assert!(body.contains("cairn ingest --kind user"), "missing remember-user row");
    assert!(body.contains("cairn ingest --kind rule"), "missing remember-rule row");
    assert!(body.contains("cairn ingest --kind feedback"), "missing correction row");
    assert!(body.contains("cairn forget --record"), "missing forget row");
    assert!(body.contains("cairn assemble_hot"), "missing assemble_hot row");
    assert!(body.contains("cairn capture_trace"), "missing capture_trace row");
}

#[test]
fn skill_md_contains_output_format_section() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    assert!(body.contains("## Output format"), "missing output-format heading");
    assert!(body.contains("--json"), "output section must mention --json flag");
    assert!(body.contains("\"hits\""), "output section must show JSON response shape");
}

#[test]
fn skill_md_contains_non_negotiable_rules() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    assert!(body.contains("Non-negotiable"), "missing non-negotiable rules heading");
    // The five rules from §18.d
    assert!(body.contains("Never invent record IDs"), "rule 1 missing");
    assert!(body.contains("cairn forget"), "rule 2 (confirm before forget) missing");
    assert!(body.contains("stderr"), "rule 3 (surface stderr) missing");
    assert!(body.contains("CAIRN_IDENTITY"), "rule 4 (identity env var) missing");
    assert!(body.contains("trigger list"), "rule 5 (don't over-ingest) missing");
}

#[test]
fn examples_include_retrieve_context_and_lint_memory() {
    let files = emit_skill::emit(&doc()).unwrap();
    let has_retrieve = files
        .iter()
        .any(|f| f.path.ends_with("examples/05-retrieve-context.md"));
    let has_lint = files
        .iter()
        .any(|f| f.path.ends_with("examples/06-lint-memory.md"));
    assert!(has_retrieve, "missing 05-retrieve-context.md example");
    assert!(has_lint, "missing 06-lint-memory.md example");
    // Verify content correctness
    let retrieve = files
        .iter()
        .find(|f| f.path.ends_with("examples/05-retrieve-context.md"))
        .unwrap();
    let lint = files
        .iter()
        .find(|f| f.path.ends_with("examples/06-lint-memory.md"))
        .unwrap();
    let retrieve_body = std::str::from_utf8(&retrieve.bytes).unwrap();
    let lint_body = std::str::from_utf8(&lint.bytes).unwrap();
    assert!(retrieve_body.contains("assemble_hot"), "retrieve example must call assemble_hot");
    assert!(lint_body.contains("cairn lint"), "lint example must call cairn lint");
}
```

- [ ] **Step 2: Run tests to confirm they all fail**

```bash
cd /path/to/cairn
cargo nextest run -p cairn-idl --test codegen_emit_skill 2>&1 | tail -30
```

Expected: All four new tests fail with assertion errors. Existing tests still pass.

- [ ] **Step 3: Commit the failing tests**

```bash
git add crates/cairn-idl/tests/codegen_emit_skill.rs
git commit -m "test(cairn-idl): add failing tests for §18.d skill guidance sections (#69)"
```

---

## Task 2: Add "When to call cairn" trigger table to SKILL.md

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs`

- [ ] **Step 1: Add `push_trigger_table()` helper function**

In `emit_skill.rs`, after the `push_verb_section()` function (around line 73), insert:

```rust
fn push_trigger_table(s: &mut String) {
    s.push_str("## When to call cairn\n\n");
    s.push_str("| User says / situation | Command |\n");
    s.push_str("|---|---|\n");
    let rows: &[(&str, &str)] = &[
        (
            r#""remember that I prefer X""#,
            r#"`cairn ingest --kind user --body "prefers X"`"#,
        ),
        (
            r#""remember: never do Y""#,
            r#"`cairn ingest --kind rule --body "never do Y"`"#,
        ),
        (
            r#""correction: it's actually Z""#,
            r#"`cairn ingest --kind feedback --body "Z"`"#,
        ),
        (
            r#""forget what I said about W""#,
            r#"`cairn forget --record $(cairn search "W" --limit 1 --json \| jq -r '.hits[0].id')`"#,
        ),
        (
            r#""what do you know about K?""#,
            r#"`cairn search "K" --limit 10 --json`"#,
        ),
        (
            r#""load my preferences for this session""#,
            r#"`cairn assemble_hot --session ${SESSION_ID} --json`"#,
        ),
        (
            "before answering any non-trivial question",
            r#"`cairn search "$USER_INTENT" --limit 5 --json`"#,
        ),
        (
            "after completing an ad-hoc procedure",
            r#"`cairn ingest --kind strategy_success --body "..."`"#,
        ),
        (
            "before ending the session",
            r#"`cairn capture_trace --from ${TRANSCRIPT_PATH} --json`"#,
        ),
    ];
    for (situation, command) in rows {
        let _ = writeln!(s, "| {situation} | {command} |");
    }
    s.push('\n');
}
```

- [ ] **Step 2: Call `push_trigger_table()` in `emit_skill_md()`**

Find the line in `emit_skill_md()` that reads:
```rust
    for verb in &doc.verbs {
        push_verb_section(&mut s, verb);
    }
```

Insert the trigger table call immediately BEFORE that loop:

```rust
    push_trigger_table(&mut s);
    s.push_str("---\n\n");
    for verb in &doc.verbs {
        push_verb_section(&mut s, verb);
    }
```

- [ ] **Step 3: Run the trigger-table test to confirm it passes**

```bash
cargo nextest run -p cairn-idl --test codegen_emit_skill skill_md_contains_trigger_table 2>&1
```

Expected: `skill_md_contains_trigger_table` PASSES. Other new tests still fail.

---

## Task 3: Add "Output format" section to SKILL.md

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs`

- [ ] **Step 1: Add `push_output_format()` helper function**

After `push_trigger_table()`, insert:

```rust
fn push_output_format(s: &mut String) {
    s.push_str("## Output format\n\n");
    s.push_str("Every `cairn` command returns JSON on stdout when called with `--json`. Parse it. Don't read prose.\n\n");
    s.push_str("```bash\n");
    s.push_str("$ cairn search \"pgvector\" --limit 2 --json\n");
    s.push_str("{\"hits\":[\n");
    s.push_str("  {\"id\":\"01HQZ...\",\"kind\":\"fact\",\"body\":\"pgvector needs extension\",\"score\":0.94},\n");
    s.push_str("  {\"id\":\"01HQY...\",\"kind\":\"feedback\",\"body\":\"user prefers sqlite-vec\",\"score\":0.81}\n");
    s.push_str("]}\n");
    s.push_str("```\n\n");
    s.push_str("All verbs support `--json`. Ingest returns `{\"record_id\": \"...\", \"session_id\": \"...\"}`. ");
    s.push_str("Forget returns `{\"deleted\": [\"...\"]}`.\n\n");
}
```

- [ ] **Step 2: Call `push_output_format()` in `emit_skill_md()` after the verb loop**

Find the lines at the end of `emit_skill_md()` just before the `---` separator and protocol preludes:

```rust
    s.push_str("---\n\n## Protocol preludes (not core verbs)\n\n");
```

Change to:

```rust
    push_output_format(&mut s);
    s.push_str("---\n\n## Protocol preludes (not core verbs)\n\n");
```

- [ ] **Step 3: Run the output-format test**

```bash
cargo nextest run -p cairn-idl --test codegen_emit_skill skill_md_contains_output_format_section 2>&1
```

Expected: PASSES.

---

## Task 4: Add "Non-negotiable rules" section to SKILL.md

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs`

- [ ] **Step 1: Add `push_non_negotiable_rules()` helper function**

After `push_output_format()`, insert:

```rust
fn push_non_negotiable_rules(s: &mut String) {
    s.push_str("## Non-negotiable rules\n\n");
    let rules: &[&str] = &[
        "Never invent record IDs. Always get them from `cairn search` or `cairn retrieve`.",
        "Never call `cairn forget` without confirming with the user — forget is irreversible.",
        "If a command fails, show the user `stderr` verbatim. Don't paper over errors.",
        "Every `ingest` signs with your agent identity — `cairn` reads it from `$CAIRN_IDENTITY` set at harness startup. Don't pass `--signed-intent` explicitly.",
        "Don't run `cairn ingest` for trivia the user didn't ask you to remember. Use the trigger list above — if it's not on the list, ask before storing.",
    ];
    for (i, rule) in rules.iter().enumerate() {
        let _ = writeln!(s, "{}. {rule}", i + 1);
    }
    s.push('\n');
}
```

- [ ] **Step 2: Call `push_non_negotiable_rules()` in `emit_skill_md()` between output format and protocol preludes**

The call chain after the verb loop should now read:

```rust
    push_output_format(&mut s);
    push_non_negotiable_rules(&mut s);
    s.push_str("---\n\n## Protocol preludes (not core verbs)\n\n");
```

- [ ] **Step 3: Run the non-negotiable-rules test**

```bash
cargo nextest run -p cairn-idl --test codegen_emit_skill skill_md_contains_non_negotiable_rules 2>&1
```

Expected: PASSES.

---

## Task 5: Add taxonomy source note to `conventions.md`

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs`

- [ ] **Step 1: Update `emit_conventions()` to add IDL source note**

Find the line in `emit_conventions()`:

```rust
    s.push_str("\n## Kind cheat-sheet (pick one — never invent new kinds)\n\n");
```

Change to:

```rust
    s.push_str("\n## Kind cheat-sheet (pick one — never invent new kinds)\n\n");
    s.push_str("These kinds are sourced from the IDL taxonomy (§6 of the design brief). ");
    s.push_str("The authoritative list is regenerated by `cargo run -p cairn-idl --bin cairn-codegen`.\n\n");
```

- [ ] **Step 2: Run emit_skill tests to confirm nothing broke**

```bash
cargo nextest run -p cairn-idl --test codegen_emit_skill 2>&1 | tail -20
```

Expected: All 8 tests pass (4 original + 4 new, excluding examples test which still needs examples).

---

## Task 6: Add examples 05 and 06 to `emit_examples()`

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs`

- [ ] **Step 1: Add two entries to the `examples` slice in `emit_examples()`**

Find the closing of the `examples` slice in `emit_examples()`:

```rust
        (
            "skills/cairn/examples/04-skillify-this.md",
            "# Example: skillify a procedure\n\
            ...
        ),
    ];
```

Add after the `04-skillify-this.md` entry (before `];`):

```rust
        (
            "skills/cairn/examples/05-retrieve-context.md",
            "# Example: retrieve context for a session\n\
\n\
**User says:** \"Load my preferences for this session.\"\n\
\n\
**Cairn call:**\n\
```bash\n\
cairn assemble_hot --session ${SESSION_ID} --json\n\
```\n\
\n\
**Why `assemble_hot`:** Loads the hot-memory prefix — your preferences, active rules, and recent context — so they are in scope for the rest of the session. Call once per session, not per turn.\n\
\n\
**When `SESSION_ID` is unknown:** Omit `--session`; Cairn resolves the current session from `$CAIRN_SESSION_ID` or creates one.\n",
        ),
        (
            "skills/cairn/examples/06-lint-memory.md",
            "# Example: lint memory vault\n\
\n\
**User says:** \"Lint my memory\" or run on a daily cadence.\n\
\n\
**Cairn call:**\n\
```bash\n\
cairn lint --json\n\
```\n\
\n\
**Sample response:**\n\
```json\n\
{\"issues\": [\n\
  {\"kind\": \"contradiction\", \"ids\": [\"01HQZ...\", \"01HQY...\"], \"summary\": \"two rules conflict\"},\n\
  {\"kind\": \"stale\",         \"id\":  \"01HQX...\",               \"summary\": \"fact older than 90 days\"}\n\
]}\n\
```\n\
\n\
**Why `lint`:** Detects contradictions, orphans, stale claims, and missing concept pages. Run on a cadence (daily or on PR) — not per turn.\n",
        ),
```

- [ ] **Step 2: Run the examples test**

```bash
cargo nextest run -p cairn-idl --test codegen_emit_skill examples_include_retrieve_context_and_lint_memory 2>&1
```

Expected: PASSES.

- [ ] **Step 3: Run all emit_skill tests to confirm all 8 pass**

```bash
cargo nextest run -p cairn-idl --test codegen_emit_skill 2>&1 | tail -20
```

Expected: 8/8 PASS.

- [ ] **Step 4: Commit the emitter changes**

```bash
git add crates/cairn-idl/src/codegen/emit_skill.rs
git commit -m "feat(cairn-idl): add §18.d skill guidance — trigger table, output format, rules, examples 05-06 (#69)"
```

---

## Task 7: Update the insta snapshot

**Files:**
- Modify: `crates/cairn-idl/tests/snapshots/codegen_snapshot__snapshot_skill_md.snap`

- [ ] **Step 1: Run the snapshot test to see the diff**

```bash
cargo nextest run -p cairn-idl --test codegen_snapshot snapshot_skill_md 2>&1
```

Expected: FAILS with insta "snapshot does not match" showing the new sections.

- [ ] **Step 2: Accept the new snapshot**

```bash
cargo insta review
```

In the review UI: press `a` (accept) for the `snapshot_skill_md` diff. Verify the diff shows:
- A "When to call cairn" table section
- An "Output format" section
- A "Non-negotiable rules" section
(All before the existing "Protocol preludes" footer.)

- [ ] **Step 3: Confirm snapshot test passes**

```bash
cargo nextest run -p cairn-idl --test codegen_snapshot snapshot_skill_md 2>&1
```

Expected: PASSES.

- [ ] **Step 4: Commit the updated snapshot**

```bash
git add crates/cairn-idl/tests/snapshots/codegen_snapshot__snapshot_skill_md.snap
git commit -m "chore(snapshots): update skill_md snapshot for §18.d guidance additions (#69)"
```

---

## Task 8: Regenerate the skill bundle

**Files:**
- Regenerate: `skills/cairn/SKILL.md`, `skills/cairn/conventions.md`, `skills/cairn/examples/05-retrieve-context.md`, `skills/cairn/examples/06-lint-memory.md`

- [ ] **Step 1: Run cairn-codegen**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked 2>&1
```

Expected: exits 0, no diff errors. The codegen writes to `skills/cairn/`.

- [ ] **Step 2: Confirm the skill bundle files changed**

```bash
git diff --stat skills/cairn/
```

Expected output should show:
```
skills/cairn/SKILL.md                     | N +++++
skills/cairn/conventions.md               | N +++++
skills/cairn/examples/05-retrieve-context.md | N +++++ (new file)
skills/cairn/examples/06-lint-memory.md   | N +++++ (new file)
```

- [ ] **Step 3: Spot-check `skills/cairn/SKILL.md`**

```bash
grep -n "When to call cairn\|Output format\|Non-negotiable" skills/cairn/SKILL.md
```

Expected: all three headings appear at appropriate line numbers.

- [ ] **Step 4: Spot-check example content**

```bash
grep "assemble_hot" skills/cairn/examples/05-retrieve-context.md
grep "cairn lint" skills/cairn/examples/06-lint-memory.md
```

Expected: both match.

- [ ] **Step 5: Commit the regenerated bundle**

```bash
git add skills/cairn/
git commit -m "chore(skills): regenerate cairn skill bundle with §18.d guidance (#69)"
```

---

## Task 9: Run full CI verification

- [ ] **Step 1: Format check**

```bash
cargo fmt --all --check
```

Expected: no output (clean).

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | tail -20
```

Expected: clean.

- [ ] **Step 3: Full test suite**

```bash
cargo nextest run --workspace --locked --no-fail-fast 2>&1 | tail -30
```

Expected: all tests pass including the 4 new emit_skill tests.

- [ ] **Step 4: Codegen check (--check mode)**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check 2>&1
```

Expected: exits 0 (no diff between what codegen would write and what's on disk).

- [ ] **Step 5: Core boundary check**

```bash
./scripts/check-core-boundary.sh
```

Expected: passes.

- [ ] **Step 6: Doctests**

```bash
cargo test --doc --workspace --locked 2>&1 | tail -10
```

Expected: passes.

---

## Self-Review

**Spec coverage check (§18.d):**

| §18.d requirement | Covered by |
|---|---|
| "When to call cairn" table with 9 rows | Task 2 trigger table in SKILL.md |
| Exact CLI/MCP call shapes with `--kind` values | Task 2 rows + verb sections |
| Kind cheat-sheet (never invent kinds) | `conventions.md` (existing) + taxonomy note (Task 5) |
| Output format / JSON parsing | Task 3 output format section |
| Non-negotiable rules (5 rules) | Task 4 rules section |
| `remember` flow | Trigger table row 1-2 + example 01 |
| `forget` flow | Trigger table row 4 + example 02 |
| `skillify` flow | Trigger table row 8 + example 04 |
| `retrieve context` flow | Trigger table row 6 + example 05 (Task 6) |
| `lint memory` flow | Trigger table row (lint not in §18.d table, but in verb section) + example 06 (Task 6) |
| Privacy rules visible before capture | Non-negotiable rules section (rule 5) |
| Bash-capable harness works without MCP | All CLI commands use `cairn <verb>` directly |
| Same verbs as CLI/MCP | Trigger table calls identical `cairn` commands |

**Acceptance criteria check:**
- [x] A bash-capable harness can use the skill without native MCP support — all commands are plain `cairn <verb>` CLI calls.
- [x] Explicit remember/forget flows call the same verbs as CLI/MCP — trigger table rows use `cairn ingest` and `cairn forget` directly.
- [x] Privacy and redaction rules are visible to the agent before capture — non-negotiable rules section rule 5 + rule 2 (confirm before forget).

**Placeholder scan:** None. All code blocks contain real commands with real argument shapes.

**Type consistency:** No types involved — this is prose generation. String literals in `emit_skill.rs` are consistent across tasks.
