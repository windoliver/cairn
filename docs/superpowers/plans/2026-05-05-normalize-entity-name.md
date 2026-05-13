# `normalize_entity_name` Shared Helper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land a single source-of-truth helper `cairn_core::domain::graph::normalize_entity_name` and migrate every existing `name_norm` construction site to call it, so issue #190's `graph.get_entity` ByName arm can rely on identical normalization between insert and lookup.

**Architecture:** New pure module `crates/cairn-core/src/domain/graph/normalize.rs` with one public function. The function lower-cases, NFC-normalizes, strips ASCII punctuation, and collapses internal whitespace — deterministic, no I/O, no allocations beyond the returned `String`. All current `name_norm: ...` construction sites (currently three: one in core's contract test, one in store-sqlite's integration tests, one set of fixtures) call the helper instead of hard-coded strings. The store-sqlite `do_upsert_entity` path itself does not change — it still receives a pre-computed `name_norm` from the caller — but a new round-trip integration test asserts that a name like `"Auth Service (v2)"` passed through the helper inserts and looks up to the same row.

**Tech Stack:** Rust 2024 / 1.95.0, `unicode-normalization = "0.1"` (workspace dep, already present), `proptest` (workspace dev-dep, already present), `tokio_rusqlite` for the existing integration-test harness.

**Brief sources:** design-brief §3 (vault, EntityNode), §4 (MemoryStore contract); spec `docs/superpowers/specs/2026-05-05-mcp-graph-tools-design.md` §3.1 (Normalization).

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/cairn-core/src/domain/graph/normalize.rs` | **Create** | `pub fn normalize_entity_name(input: &str) -> String` + unit + proptest |
| `crates/cairn-core/src/domain/graph/mod.rs` | **Modify** | `pub mod normalize;` + re-export `normalize_entity_name` |
| `crates/cairn-core/src/contract/memory_store.rs:963-967` | **Modify** | `EntityNode { name_norm: "alice".into() ... }` → `name_norm: normalize_entity_name("alice")` |
| `crates/cairn-store-sqlite/tests/entity_graph.rs:397-462` | **Modify** | three `EntityNode { name_norm: "alice".into() ... }` literals → helper call (lines 400, 418, 428, 454) |
| `crates/cairn-store-sqlite/tests/entity_graph.rs:505-518` | **Modify** | `format!("alice-{suffix}")` literals → helper call wrapping the input form |
| `crates/cairn-store-sqlite/tests/entity_graph.rs` (new test fn) | **Modify** | new `upsert_entity_round_trip_punctuation_and_unicode` regression test |

> Search invariant before starting: `rg 'name_norm:\s*"' crates/` and `rg 'name_norm:\s*format!' crates/` should return zero hits at the end of this plan, except inside `normalize.rs`'s own tests.

---

## Task 1: Add the `normalize_entity_name` module with failing unit tests

**Files:**
- Create: `crates/cairn-core/src/domain/graph/normalize.rs`
- Modify: `crates/cairn-core/src/domain/graph/mod.rs`

- [ ] **Step 1: Add the module declaration and re-export**

In `crates/cairn-core/src/domain/graph/mod.rs`, near the top (above `use std::fmt;`):

```rust
//! Bitemporal knowledge-graph domain types (brief §3, §4).

pub mod normalize;

pub use normalize::normalize_entity_name;

use std::fmt;
```

- [ ] **Step 2: Write the failing module with unit tests, no implementation body yet**

Create `crates/cairn-core/src/domain/graph/normalize.rs`:

```rust
//! Canonical normalization for entity-node `name_norm`.
//!
//! Single source of truth for the dedup key used by every
//! `MemoryStore::upsert_entity` insertion site and every read-side
//! lookup (e.g. the `graph.get_entity` ByName arm, spec §3.1).
//!
//! Behaviour:
//! - NFC unicode normalization
//! - lowercase (Unicode-aware via `char::to_lowercase`)
//! - strip ASCII punctuation (`char::is_ascii_punctuation`)
//! - collapse runs of whitespace to a single ASCII space
//! - trim leading + trailing whitespace
//!
//! Pure: no I/O, no global state, no allocations beyond the return value.

use unicode_normalization::UnicodeNormalization;

/// Canonical form used as the `entity_nodes.name_norm` dedup key.
///
/// Idempotent: `normalize_entity_name(normalize_entity_name(x)) == normalize_entity_name(x)`
/// for every `x` (covered by proptest below).
#[must_use]
pub fn normalize_entity_name(input: &str) -> String {
    // 1. NFC-normalize so visually identical strings compare equal.
    let nfc: String = input.nfc().collect();

    // 2. Build the output: lowercase, drop punctuation, collapse whitespace.
    let mut out = String::with_capacity(nfc.len());
    let mut prev_was_space = true; // start true so leading WS is dropped
    for ch in nfc.chars() {
        if ch.is_ascii_punctuation() {
            // Treat punctuation as a soft separator: collapse to a space.
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
            continue;
        }
        if ch.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
            continue;
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
        prev_was_space = false;
    }

    // 3. Trim trailing whitespace produced by the loop above.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_ascii() {
        assert_eq!(normalize_entity_name("Alice"), "alice");
    }

    #[test]
    fn strips_punctuation_and_collapses_whitespace() {
        assert_eq!(
            normalize_entity_name("Auth Service (v2)"),
            "auth service v2"
        );
    }

    #[test]
    fn trims_and_collapses_runs_of_whitespace() {
        assert_eq!(normalize_entity_name("  foo \t\n bar  "), "foo bar");
    }

    #[test]
    fn nfc_normalises_decomposed_unicode() {
        // "café" composed (U+00E9) vs decomposed (e + U+0301)
        let composed = "Caf\u{00E9}";
        let decomposed = "Cafe\u{0301}";
        assert_eq!(
            normalize_entity_name(composed),
            normalize_entity_name(decomposed)
        );
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(normalize_entity_name(""), "");
        assert_eq!(normalize_entity_name("   "), "");
        assert_eq!(normalize_entity_name("!!!"), "");
    }

    #[test]
    fn ascii_only_inputs_are_byte_identical_after_one_pass() {
        // Regression guard: existing fixtures use "alice" — must round-trip unchanged.
        assert_eq!(normalize_entity_name("alice"), "alice");
        assert_eq!(normalize_entity_name("acme"), "acme");
    }
}
```

- [ ] **Step 3: Run the unit tests to verify they pass**

Run: `cargo nextest run -p cairn-core domain::graph::normalize`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/graph/normalize.rs \
        crates/cairn-core/src/domain/graph/mod.rs
git commit -m "feat(core): add normalize_entity_name shared helper (spec §3.1, #190)"
```

---

## Task 2: Add proptest invariants (idempotence, composition order)

**Files:**
- Modify: `crates/cairn-core/src/domain/graph/normalize.rs`

- [ ] **Step 1: Add the failing proptest module**

Append to the existing `#[cfg(test)] mod tests` in `crates/cairn-core/src/domain/graph/normalize.rs`:

```rust
    use proptest::prelude::*;

    proptest! {
        /// `norm(norm(x)) == norm(x)` for every input.
        #[test]
        fn idempotent(s in ".{0,128}") {
            let once = normalize_entity_name(&s);
            let twice = normalize_entity_name(&once);
            prop_assert_eq!(once, twice);
        }

        /// Trailing whitespace or punctuation never affects the result —
        /// i.e. the function is invariant under right-padding by junk.
        #[test]
        fn trailing_junk_is_absorbed(s in "[A-Za-z0-9 ]{0,32}", junk in "[ \t\n!?.,;:]{0,8}") {
            let plain = normalize_entity_name(&s);
            let padded = normalize_entity_name(&format!("{s}{junk}"));
            prop_assert_eq!(plain, padded);
        }

        /// Output never contains ASCII punctuation.
        #[test]
        fn output_has_no_punctuation(s in ".{0,128}") {
            let out = normalize_entity_name(&s);
            prop_assert!(!out.chars().any(|c| c.is_ascii_punctuation()));
        }

        /// Output never contains uppercase ASCII letters.
        #[test]
        fn output_is_lowercased(s in ".{0,128}") {
            let out = normalize_entity_name(&s);
            prop_assert!(!out.chars().any(|c| c.is_ascii_uppercase()));
        }
    }
```

- [ ] **Step 2: Run the proptest cases**

Run: `cargo nextest run -p cairn-core domain::graph::normalize`
Expected: 6 unit tests + 4 proptest cases pass (each proptest case runs 256 generated inputs by default).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/domain/graph/normalize.rs
git commit -m "test(core): proptest idempotence + invariants for normalize_entity_name"
```

---

## Task 3: Migrate the three existing `name_norm` construction sites

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs:957-986`
- Modify: `crates/cairn-store-sqlite/tests/entity_graph.rs:392-525`

The migration is mechanical: replace each hand-written `name_norm: "<lit>".into()` (or `format!("...")`) with `name_norm: normalize_entity_name("<canonical input>")`. The canonical input is the value of the sibling `name:` field, except where the fixture deliberately uses a different `name_norm` to test dedup behaviour — in those cases keep the original literal-with-suffix shape but pass it through the helper.

Three call-site clusters to migrate (enumerated by file + line, taken from `rg 'EntityNode \{' crates/` and `rg 'name_norm:' crates/`):

| Site | File | Lines | Original | Migrated |
|---|---|---|---|---|
| A | `crates/cairn-core/src/contract/memory_store.rs` | 963–969 | `name_norm: "alice".into()` | `name_norm: normalize_entity_name("alice")` |
| B1 | `crates/cairn-store-sqlite/tests/entity_graph.rs` | 397–404 (`upsert_entity_inserts_new_returns_supplied_id`) | `name_norm: "alice".into()` | `name_norm: normalize_entity_name("Alice")` (note: input is the display name) |
| B2 | `crates/cairn-store-sqlite/tests/entity_graph.rs` | 415–421 (`upsert_entity_dedup_returns_existing_id`, first insert) | `name_norm: "alice".into()` | `name_norm: normalize_entity_name("Alice")` |
| B3 | `crates/cairn-store-sqlite/tests/entity_graph.rs` | 425–431 (`upsert_entity_dedup_returns_existing_id`, dup insert) | `name_norm: "alice".into()` | `name_norm: normalize_entity_name("ALICE")` (proves case-fold dedup hits) |
| B4 | `crates/cairn-store-sqlite/tests/entity_graph.rs` | 451–457 | `name_norm: "alice-link".into()` | leave the literal — this fixture deliberately exercises a non-helper-derived dedup key (the `-link` suffix is a marker, not a real name). Wrap as `name_norm: normalize_entity_name("alice link")` and update the assertion if it pins on the exact string. |
| B5 | `crates/cairn-store-sqlite/tests/entity_graph.rs` | 505–518 (suffix-parametrized fixture builder) | `name_norm: format!("alice-{suffix}")` | `name_norm: normalize_entity_name(&format!("alice {suffix}"))` and same for `acme` |

> The `INSERT INTO entity_nodes ... name_norm` SQL literals later in the same file (lines 84, 92, 108, 116, 175, 207, 251, 299, 1817, 1823, 1831 — all inside raw `tx.execute` test setup that bypasses `upsert_entity`) **stay as raw strings**: they are testing schema-level invariants (UNIQUE constraint, expired rows, etc.) where the helper is not in scope. They are not consumers of `normalize_entity_name`. Verify by `rg 'name_norm' crates/cairn-store-sqlite/tests/entity_graph.rs | rg -v 'INSERT|SELECT|--'` after the migration — only `EntityNode { name_norm: ...` lines should remain, and all of them should call the helper.

- [ ] **Step 1: Add the import to `crates/cairn-core/src/contract/memory_store.rs`**

Inside the `#[cfg(test)] mod tests { ... }` block at line ~957, find the existing line:

```rust
            EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode, GraphEdgesArgs,
```

…and extend it to also import the helper:

```rust
            EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, EntityNode, GraphEdgesArgs,
            normalize_entity_name,
```

- [ ] **Step 2: Migrate Site A**

In `crates/cairn-core/src/contract/memory_store.rs`, replace:

```rust
        let node = EntityNode {
            id: EntityId::from("01HZE7JV5N0000000000000001"),
            name: "alice".into(),
            name_norm: "alice".into(),
            summary: None,
            created_at: 1,
```

with:

```rust
        let node = EntityNode {
            id: EntityId::from("01HZE7JV5N0000000000000001"),
            name: "alice".into(),
            name_norm: normalize_entity_name("alice"),
            summary: None,
            created_at: 1,
```

- [ ] **Step 3: Add the import to the integration-test file**

In `crates/cairn-store-sqlite/tests/entity_graph.rs`, find each `use cairn_core::domain::graph::{EntityId, EntityNode};` line (lines 394, 412, 445, 504 per current `rg`) and extend each to:

```rust
    use cairn_core::domain::graph::{EntityId, EntityNode, normalize_entity_name};
```

- [ ] **Step 4: Migrate Sites B1–B5**

For each row in the table above, change the `name_norm:` line as specified. Concrete patches:

```rust
// B1 (line ~400)
-        name_norm: "alice".into(),
+        name_norm: normalize_entity_name("Alice"),

// B2 (line ~418)
-        name_norm: "alice".into(),
+        name_norm: normalize_entity_name("Alice"),

// B3 (line ~428)
-        name_norm: "alice".into(),
+        name_norm: normalize_entity_name("ALICE"),

// B4 (line ~454) — preserve uniqueness across tests by keeping the marker
-        name_norm: "alice-link".into(),
+        name_norm: normalize_entity_name("alice link"),

// B5 (lines ~508 and ~516) — suffix-parameterized
-        name_norm: format!("alice-{suffix}"),
+        name_norm: normalize_entity_name(&format!("alice {suffix}")),
-        name_norm: format!("acme-{suffix}"),
+        name_norm: normalize_entity_name(&format!("acme {suffix}")),
```

- [ ] **Step 5: Run the full graph test suites to confirm no behaviour change**

Run: `cargo nextest run -p cairn-core --lib contract::memory_store`
Expected: pre-existing tests pass.

Run: `cargo nextest run -p cairn-store-sqlite --test entity_graph`
Expected: every existing test passes (the assertion in B3 — `assert_eq!(id_a, id_b, "duplicate name_norm collapses to existing id")` — still holds because `normalize_entity_name("Alice") == normalize_entity_name("ALICE") == "alice"`).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs \
        crates/cairn-store-sqlite/tests/entity_graph.rs
git commit -m "refactor(core,store-sqlite): route name_norm fixtures through normalize_entity_name"
```

---

## Task 4: Round-trip regression test for `"Auth Service (v2)"`

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/entity_graph.rs` (append a new `#[tokio::test]`)

This is the spec §3.1 test 1 in storage form: insert with the original display name, recompute `name_norm` via the helper, look up by `name_norm` directly through the existing `entity_nodes` row, and assert we get back the same `id`.

- [ ] **Step 1: Write the failing regression test**

Append to `crates/cairn-store-sqlite/tests/entity_graph.rs` (after the last `upsert_entity_*` test, before the `upsert_entity_edge_*` block):

```rust
#[tokio::test]
async fn upsert_entity_round_trip_punctuation_and_unicode() {
    use cairn_core::domain::graph::{EntityId, EntityNode, normalize_entity_name};

    let store = fresh_store().await;

    let display = "Auth Service (v2)";
    let node = EntityNode {
        id: EntityId::from("01HZE7JV5N0000000000000099"),
        name: display.into(),
        name_norm: normalize_entity_name(display),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let inserted_id = store.upsert_entity(&node).await.expect("insert");

    // The §3.1 ByName arm computes `name_norm` from the user-provided
    // `name` and probes `entity_nodes.name_norm` directly. Simulate that
    // here by recomputing from the *display* form (whitespace/punctuation
    // intact) and asserting the row is found.
    let probe_norm = normalize_entity_name("Auth Service (v2)");
    assert_eq!(
        probe_norm,
        node.name_norm,
        "helper must be deterministic across call sites"
    );

    let conn = store.raw_conn_for_test();
    let found_id: String = conn
        .call(move |c| {
            let id: String = c.query_row(
                "SELECT id FROM entity_nodes WHERE name_norm = ?1",
                rusqlite::params![&probe_norm],
                |r| r.get(0),
            )?;
            Ok(id)
        })
        .await
        .expect("lookup");

    assert_eq!(found_id, inserted_id.as_str());

    // A naive `lower()` lookup would NOT find this row — assert that.
    let conn = store.raw_conn_for_test();
    let display_owned = display.to_owned();
    let found_lower: Option<String> = conn
        .call(move |c| {
            let res = c
                .query_row(
                    "SELECT id FROM entity_nodes WHERE name_norm = lower(?1)",
                    rusqlite::params![&display_owned],
                    |r| r.get::<_, String>(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            Ok(res)
        })
        .await
        .expect("naive-lower probe");
    assert!(
        found_lower.is_none(),
        "naive lower() must NOT find the row — proves the helper is load-bearing",
    );
}
```

> If `fresh_store()` and `raw_conn_for_test()` are not the names already used in this file, look at the helper near the top of `crates/cairn-store-sqlite/tests/entity_graph.rs` (the file's existing tests build a store via a shared helper around `SqliteMemoryStore::open`) and reuse the exact symbols. The two pre-existing tests `upsert_entity_inserts_new_returns_supplied_id` and `upsert_entity_dedup_returns_existing_id` already do this — copy their setup verbatim.

- [ ] **Step 2: Run only the new test, expect PASS**

Run: `cargo nextest run -p cairn-store-sqlite --test entity_graph upsert_entity_round_trip_punctuation_and_unicode`
Expected: PASS.

If it fails on the `naive-lower` assertion, that's expected on the first run iff the helper output equals plain `lower(display)` — re-check the helper output: `normalize_entity_name("Auth Service (v2)")` must produce `"auth service v2"`, while `lower("Auth Service (v2)")` produces `"auth service (v2)"`. The two differ, so the negative branch holds.

- [ ] **Step 3: Run the full integration suite to confirm zero regressions**

Run: `cargo nextest run -p cairn-store-sqlite --test entity_graph`
Expected: all prior tests + the new one pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/tests/entity_graph.rs
git commit -m "test(store-sqlite): round-trip 'Auth Service (v2)' via normalize_entity_name (spec §3.1)"
```

---

## Task 5: Final verification + boundary check

**Files:** none modified.

- [ ] **Step 1: Confirm no stray hand-rolled `name_norm` literals slipped through**

Run: `rg 'name_norm:\s*"' crates/`
Expected: zero matches.

Run: `rg 'name_norm:\s*format!' crates/`
Expected: zero matches.

(Raw `INSERT INTO entity_nodes ... name_norm` SQL strings inside test setup are fine — those columns are written without going through `EntityNode`.)

- [ ] **Step 2: Confirm `cairn-core` boundary still clean**

Run: `./scripts/check-core-boundary.sh`
Expected: exits 0.

- [ ] **Step 3: Run the full workspace verification checklist (CLAUDE.md §8)**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all green. No IDL re-codegen needed (the helper is internal core API; no IDL surface change).

- [ ] **Step 4: Final commit (if `cargo fmt` produced any whitespace fixes)**

```bash
# Only if there are formatting deltas left over.
git add -u
git commit -m "chore: cargo fmt"
```

---

## Done

When all five tasks are checked, this PR delivers:

1. `cairn_core::domain::graph::normalize_entity_name` — pure, idempotent, NFC-aware, lower + punctuation-stripping + whitespace-collapsing.
2. Every existing `EntityNode` constructor in the repo routes through that helper.
3. A round-trip integration test against the real SQLite store using `"Auth Service (v2)"` proves the helper is the dedup key.
4. Pre-existing `"alice"` / `"acme"` ASCII fixtures are byte-identical after the helper, so no behaviour change for already-passing tests.
5. The codebase is ready for issue #190's `graph.get_entity` ByName arm to call the same helper at lookup time.
