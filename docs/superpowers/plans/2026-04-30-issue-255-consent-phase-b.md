# Issue #255 — Phase-B consent enforcement (default flip + check constraint)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote the `consent_journal.kind` column from nullable + trigger-gated to `NOT NULL` + table-level `CHECK` constraint, completing the Phase-B hardening of the §14 event log.

**Architecture:** SQLite cannot `ALTER TABLE ADD CHECK`, so the migration rebuilds `consent_journal` via the canonical 12-step rebuild idiom (CREATE new table → INSERT rowid-preserving SELECT → DROP old → RENAME → re-attach indexes + triggers). Legacy `kind IS NULL` rows from migration 0005 (pure GRANT/REVOKE) are backfilled by mapping the `decision` column onto `'grant'`/`'revoke'` during the rebuild SELECT, so the post-rebuild table has zero NULL-kind rows. All append-only / immutability triggers (0005, 0009, 0011) are dropped on the old table and re-attached to the renamed table by name. The async mirror (`cairn-workflows::ConsentLogMaterializer`) tails by `rowid`; rowid is preserved 1:1 by the `INSERT INTO ... (rowid, ...) SELECT rowid, ...` form.

**Tech Stack:** Rust 2024, `rusqlite`, in-memory SQLite for tests, `cargo nextest`. SQL migrations under `crates/cairn-store-sqlite/src/migrations/sql/`.

**Brief sources:** §14 (privacy / consent), §3 line 448 (`consent_journal` columns), §5.6 (WAL coupling). Issue #255. Issue #94 (Phase-A baseline).

---

## Out of scope

- Removing the `kind IS NOT NULL` clauses from existing 0009/0011 triggers (they become tautologically true once `kind` is `NOT NULL`, but stripping them is a no-op cleanup that risks rewriting committed migrations — leave them as defence-in-depth).
- Tightening other nullable columns (`actor`, `payload_json`, `decided_at_iso`). Those have full trigger coverage in 0011 and a `NOT NULL` flip would also need a table rebuild — file follow-up if desired, do not bundle.
- Reworking the `consent.rs` query helpers' `kind IS NOT NULL AND ...` filters. They become redundant after the flip, but keeping them costs nothing and a separate cleanup PR is cleaner.

---

## File map

| File | Role | Touch |
|---|---|---|
| `crates/cairn-store-sqlite/src/migrations/sql/0021_consent_kind_not_null.sql` | new migration: table rebuild | **create** |
| `crates/cairn-store-sqlite/src/migrations/mod.rs` | migration registry (or wherever `MIGRATIONS` is declared) | **modify** — register 0021 |
| `crates/cairn-store-sqlite/tests/migrations.rs` | round-trip + invariant tests | **modify** — invert `consent_journal_kind_null_back_compat` + add new tests |
| `crates/cairn-store-sqlite/src/verify.rs` (or equivalent fingerprint code) | head-migration constant | **modify** — bump head to 21 |

**Risk hot-spots, read before editing:**
- `crates/cairn-store-sqlite/src/migrations/sql/0011_consent_event_hardening.sql` — every trigger we re-attach
- `crates/cairn-store-sqlite/src/migrations/sql/0005_consent.sql` — base table + immutable / no-delete triggers
- `crates/cairn-workflows/src/consent_log/` — mirror tails by `rowid`; the rebuild MUST preserve it

---

## Discovery — run before Task 1

- [ ] **Locate the migration registry constant.**

```bash
grep -rn "0020_workflow_jobs\|MIGRATIONS\b" crates/cairn-store-sqlite/src/migrations/ | head
```

Note the file + array where migrations are listed in order. The new file must be registered there.

- [ ] **Locate the head-migration assertion in tests.**

```bash
grep -n "head=\|head_migration\|fresh_in_memory_opens_to_head" crates/cairn-store-sqlite/tests/migrations.rs
```

The head value will need bumping from 20 to 21.

- [ ] **Confirm rowid preservation pattern.**

SQLite preserves `rowid` only when explicitly listed in the column list of `INSERT`. Double-check the rebuild uses `INSERT INTO new (rowid, consent_id, ...) SELECT rowid, consent_id, ... FROM old`.

---

## Task 1: Failing test — head migration is 21

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/migrations.rs`

- [ ] **Step 1: Bump the head-migration expectation.**

Find the test that asserts the head migration ID (likely `fresh_in_memory_opens_to_head` from PR #227) and change `20` to `21`. This will fail until 0021 is registered.

```rust
// in fresh_in_memory_opens_to_head (or equivalent)
assert_eq!(head, 21, "expected head migration 21 (consent kind NOT NULL)");
```

- [ ] **Step 2: Run to verify FAIL.**

```bash
cargo nextest run -p cairn-store-sqlite fresh_in_memory_opens_to_head -- --no-capture
```

Expected: FAIL — head still 20 (or whatever the current value is; if current is not 20, adjust the increment).

- [ ] **Step 3: Commit the failing test.**

```bash
git add crates/cairn-store-sqlite/tests/migrations.rs
git commit -m "test(store): expect head migration 21 for consent kind NOT NULL"
```

---

## Task 2: New migration — table rebuild with NOT NULL + CHECK

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0021_consent_kind_not_null.sql`
- Modify: registry from Discovery step

- [ ] **Step 1: Write the migration file.**

```sql
-- Migration 0021: flip consent_journal.kind to NOT NULL + table-level
-- CHECK on the §14 domain. Completes Phase-B hardening of the consent
-- event log started in 0009 (additive nullable column + trigger gate)
-- and 0011 (direct-SQL hardening). Brief: §14 / §3 line 448. Issue #255.
--
-- WHY a table rebuild: SQLite cannot ALTER TABLE ADD CHECK, and the
-- existing kind-domain gate is a BEFORE INSERT trigger that fires only
-- when `kind IS NOT NULL`. Promoting the column to NOT NULL eliminates
-- the legacy null-kind path entirely; lifting the domain check from a
-- trigger to a column CHECK puts the constraint in the schema (visible
-- to migration verifiers, query planners, and any future tooling that
-- reads `sqlite_master`).
--
-- Legacy back-compat: rows written by migration 0005's pure GRANT/REVOKE
-- path have `kind IS NULL`. The rebuild backfills them from the existing
-- `decision` column: 'GRANT' -> 'grant', 'REVOKE' -> 'revoke'. The
-- broader 0011 hardening triggers gate INSERTs into the new table, but
-- they only fire when columns they require are populated. Backfilled
-- legacy rows lack `actor`/`payload_json`/`decided_at_iso` — those
-- triggers would fire and abort the rebuild. We therefore drop the
-- 0011 hardening triggers BEFORE the data move and re-attach them
-- AFTER, so backfilled legacy rows survive while every future INSERT
-- is gated normally.
--
-- ROWID PRESERVATION: the async mirror in cairn-workflows tails
-- `consent_journal` by `rowid`. The rebuild preserves rowid 1:1 via
-- `INSERT INTO new(rowid, …) SELECT rowid, …`. Any cursor sidecar at
-- `.cairn/consent.cursor` written by an older incarnation continues to
-- point at the right row after upgrade.
--
-- Append-only triggers (0005's consent_journal_immutable +
-- consent_journal_no_delete) are dropped + re-attached unchanged.

-- 1. Drop ALL triggers attached to the old consent_journal. They are
--    re-created at the end of this migration against the renamed table.
DROP TRIGGER IF EXISTS consent_journal_immutable;
DROP TRIGGER IF EXISTS consent_journal_no_delete;
DROP TRIGGER IF EXISTS consent_journal_kind_domain;
DROP TRIGGER IF EXISTS consent_journal_event_requires_iso;
DROP TRIGGER IF EXISTS consent_journal_forget_receipt_body_free;
DROP TRIGGER IF EXISTS consent_journal_event_requires_actor;
DROP TRIGGER IF EXISTS consent_journal_event_requires_payload;
DROP TRIGGER IF EXISTS consent_journal_payload_shape_matches_kind;
DROP TRIGGER IF EXISTS consent_journal_payload_body_free;
DROP TRIGGER IF EXISTS consent_journal_sensor_kind_requires_sensor_id;
DROP TRIGGER IF EXISTS consent_journal_sensor_id_matches_payload;
DROP TRIGGER IF EXISTS consent_journal_sensor_subject_matches_sensor_id;
DROP TRIGGER IF EXISTS consent_journal_non_sensor_kind_forbids_sensor_id;
DROP TRIGGER IF EXISTS consent_journal_hash_kind_subject_shape;
DROP TRIGGER IF EXISTS consent_journal_hash_kind_target_id_hash_shape;
DROP TRIGGER IF EXISTS consent_journal_payload_required_fields;
DROP TRIGGER IF EXISTS consent_journal_payload_unknown_top_level_keys;
DROP TRIGGER IF EXISTS consent_journal_payload_no_duplicate_keys;
DROP TRIGGER IF EXISTS consent_journal_subject_domain_for_non_hash_kinds;
DROP TRIGGER IF EXISTS consent_journal_event_requires_positive_rowid;
DROP TRIGGER IF EXISTS consent_journal_payload_keys_match_shape;
DROP TRIGGER IF EXISTS consent_journal_payload_scalar_domains;
DROP TRIGGER IF EXISTS consent_journal_event_metadata_domains;
DROP TRIGGER IF EXISTS consent_journal_sensor_id_domain;

-- 2. Drop indexes that reference the old name. SQLite recreates them
--    in step 7 against the renamed table. (Naming the indexes here
--    explicitly — DROP INDEX errors on missing names without IF EXISTS.)
DROP INDEX IF EXISTS consent_journal_subject_scope_idx;
DROP INDEX IF EXISTS consent_journal_op_idx;
DROP INDEX IF EXISTS consent_journal_actor_idx;
DROP INDEX IF EXISTS consent_journal_sensor_idx;
DROP INDEX IF EXISTS consent_journal_kind_idx;

-- 3. Create the new table with NOT NULL + CHECK on `kind`. Mirrors the
--    0005 + 0009 columns exactly otherwise.
CREATE TABLE consent_journal_v2 (
  consent_id      TEXT NOT NULL PRIMARY KEY,
  subject         TEXT NOT NULL,
  scope           TEXT NOT NULL,
  decision        TEXT NOT NULL CHECK (decision IN ('GRANT','REVOKE')),
  reason          TEXT,
  granted_by      TEXT NOT NULL,
  decided_at      INTEGER NOT NULL,
  expires_at      INTEGER,
  op_id           TEXT,
  kind            TEXT NOT NULL CHECK (kind IN (
    'sensor_enable',
    'sensor_disable',
    'policy_change',
    'remember_intent',
    'forget_intent',
    'grant',
    'revoke',
    'promote_receipt'
  )),
  sensor_id       TEXT,
  actor           TEXT,
  payload_json    TEXT,
  decided_at_iso  TEXT,
  expires_at_iso  TEXT
);

-- 4. Move data, preserving rowid. Backfill kind from decision for any
--    legacy null-kind rows (decision is already CHECK-constrained to
--    'GRANT'/'REVOKE'). The CASE here is total over the legal decision
--    domain.
INSERT INTO consent_journal_v2 (
  rowid,
  consent_id, subject, scope, decision, reason, granted_by,
  decided_at, expires_at, op_id, kind, sensor_id, actor,
  payload_json, decided_at_iso, expires_at_iso
)
SELECT
  rowid,
  consent_id, subject, scope, decision, reason, granted_by,
  decided_at, expires_at, op_id,
  COALESCE(
    kind,
    CASE decision WHEN 'GRANT' THEN 'grant' WHEN 'REVOKE' THEN 'revoke' END
  ) AS kind,
  sensor_id, actor,
  payload_json, decided_at_iso, expires_at_iso
FROM consent_journal;

-- 5. Drop old, rename new.
DROP TABLE consent_journal;
ALTER TABLE consent_journal_v2 RENAME TO consent_journal;

-- 6. Recreate indexes (identical to 0005 + 0009).
CREATE INDEX consent_journal_subject_scope_idx
  ON consent_journal(subject, scope, decided_at);

CREATE INDEX consent_journal_op_idx
  ON consent_journal(op_id)
  WHERE op_id IS NOT NULL;

CREATE INDEX consent_journal_actor_idx
  ON consent_journal(actor, decided_at)
  WHERE actor IS NOT NULL;

CREATE INDEX consent_journal_sensor_idx
  ON consent_journal(sensor_id, decided_at)
  WHERE sensor_id IS NOT NULL;

CREATE INDEX consent_journal_kind_idx
  ON consent_journal(kind, decided_at);

-- 7. Re-attach 0005 append-only triggers verbatim.
CREATE TRIGGER consent_journal_immutable
  BEFORE UPDATE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal rows are immutable');
END;

CREATE TRIGGER consent_journal_no_delete
  BEFORE DELETE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal is append-only');
END;

-- 8. Re-attach 0009 event-shape triggers (kind-domain trigger is now
--    redundant with the column CHECK; we drop it permanently and rely
--    on CHECK).
CREATE TRIGGER consent_journal_event_requires_iso
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.decided_at_iso IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require decided_at_iso (RFC3339)');
END;

CREATE TRIGGER consent_journal_forget_receipt_body_free
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind = 'forget_intent'
   AND NEW.payload_json IS NOT NULL
   AND (
        NEW.payload_json LIKE '%"body"%'
     OR NEW.payload_json LIKE '%"text"%'
     OR NEW.payload_json LIKE '%"content"%'
     OR NEW.payload_json LIKE '%"raw"%'
     OR NEW.payload_json LIKE '%"snippet"%'
     OR NEW.payload_json LIKE '%"command"%'
     OR NEW.payload_json LIKE '%"url"%'
     OR NEW.payload_json LIKE '%"title"%'
     OR NEW.payload_json LIKE '%"file_path"%'
     OR NEW.payload_json LIKE '%"input"%'
   )
BEGIN
  SELECT RAISE(ABORT, 'forget_intent payload must be body-free (§14)');
END;

-- 9. Re-attach 0011 hardening triggers — verbatim copy from
--    0011_consent_event_hardening.sql, with one mechanical edit:
--    every `WHEN NEW.kind IS NOT NULL AND ...` clause may keep the
--    `IS NOT NULL` guard (defence-in-depth; tautological now but
--    harmless and matches 0011 source exactly).
--
-- IMPLEMENTATION NOTE: copy the 14 trigger bodies verbatim from
-- 0011_consent_event_hardening.sql lines 25..578. Every `DROP
-- TRIGGER IF EXISTS ... CREATE TRIGGER ...` block goes here, in
-- the same order.

DROP TRIGGER IF EXISTS consent_journal_event_requires_actor;
CREATE TRIGGER consent_journal_event_requires_actor
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.actor IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require actor');
END;

-- … (paste the remaining 13 triggers from 0011 verbatim — see
-- IMPLEMENTATION NOTE above). When pasting, drop the leading
-- `WHEN NEW.kind IS NOT NULL AND` clause from each trigger that has
-- it (kind is now NOT NULL by column constraint), keeping the rest
-- of the WHEN clause intact. Every trigger keeps its DROP TRIGGER
-- IF EXISTS prelude.

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (21, '0021_consent_kind_not_null', '', strftime('%s','now') * 1000);
```

NOTE: When implementing, the "paste verbatim from 0011" section MUST contain every trigger body. The agent doing the implementation should open `0011_consent_event_hardening.sql` and copy each `DROP TRIGGER IF EXISTS … CREATE TRIGGER …` block (12 remaining after the actor one above) into the migration file.

- [ ] **Step 2: Register the migration.**

In the file located in Discovery step 1, append `0021_consent_kind_not_null.sql` to the migration array (or include! macro list). Match the existing pattern exactly.

- [ ] **Step 3: Run head-migration test to verify it now PASSES.**

```bash
cargo nextest run -p cairn-store-sqlite fresh_in_memory_opens_to_head
```

Expected: PASS.

- [ ] **Step 4: Run full migrations suite.**

```bash
cargo nextest run -p cairn-store-sqlite migrations
```

Expected: ONE failure — `consent_journal_kind_null_back_compat` (Task 3 inverts it). All others pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0021_consent_kind_not_null.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs
git commit -m "feat(store): migration 0021 — consent_journal.kind NOT NULL + CHECK (§14, #255)"
```

---

## Task 3: Invert the legacy back-compat test

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/migrations.rs:280-292`

After Phase-B, a direct INSERT with no `kind` column (relying on default NULL) MUST fail. The legacy back-compat test should be repurposed to assert that the NOT NULL constraint fires.

- [ ] **Step 1: Replace the test body.**

```rust
#[test]
fn consent_journal_kind_not_null_enforced() {
    // Phase-B (issue #255): kind is NOT NULL at the column level. A
    // legacy-format INSERT that omits `kind` must fail with NOT NULL
    // constraint, not silently insert a null-kind row.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at) \
             VALUES ('legacy', 's', 'private', 'GRANT', 'hmn:t', 0)",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("NOT NULL") || msg.contains("kind"),
        "expected NOT NULL constraint failure on kind, got: {msg}"
    );
}
```

- [ ] **Step 2: Run the test.**

```bash
cargo nextest run -p cairn-store-sqlite consent_journal_kind_not_null_enforced
```

Expected: PASS.

- [ ] **Step 3: Commit.**

```bash
git add crates/cairn-store-sqlite/tests/migrations.rs
git commit -m "test(store): invert legacy null-kind back-compat to NOT NULL assertion"
```

---

## Task 4: New test — column CHECK rejects unknown kind

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/migrations.rs`

Phase-A used a trigger to gate the `kind` domain (error message: `'consent_journal.kind not in §14 domain'`). Phase-B uses a column `CHECK`, which surfaces a different rusqlite error string (`CHECK constraint failed: kind`). Add a test that pins the new behaviour.

- [ ] **Step 1: Add the test.**

```rust
#[test]
fn consent_journal_kind_check_constraint_rejects_unknown() {
    // Phase-B (issue #255): the §14 domain is a column-level CHECK on
    // `kind`. An unknown value MUST fail with a CHECK violation, not a
    // trigger ABORT.
    let conn = open_in_memory().expect("open");
    let err = conn
        .execute(
            "INSERT INTO consent_journal \
              (consent_id, subject, scope, decision, granted_by, decided_at, \
               kind, actor, decided_at_iso, payload_json) \
             VALUES ('c-bad-kind', 's', 'private', 'GRANT', 'hmn:t', 0, \
                     'totally_made_up_kind', 'hmn:t', '2026-04-30T00:00:00Z', \
                     '{}')",
            [],
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("CHECK") && msg.contains("kind"),
        "expected CHECK constraint failure on kind, got: {msg}"
    );
}
```

- [ ] **Step 2: Run the test.**

```bash
cargo nextest run -p cairn-store-sqlite consent_journal_kind_check_constraint_rejects_unknown
```

Expected: PASS.

- [ ] **Step 3: Update `consent_journal_kind_domain_enforced` (line ~192) if its assertion still mentions the trigger error string.**

```bash
grep -n "consent_journal_kind_domain_enforced\|§14 domain\|not in §14" crates/cairn-store-sqlite/tests/migrations.rs
```

If the test asserts the old trigger message, broaden the assertion to accept either `"CHECK"` or `"§14 domain"`, since both rejection paths protect the same invariant. Document the broadening with a one-line comment.

- [ ] **Step 4: Run full suite.**

```bash
cargo nextest run -p cairn-store-sqlite
```

Expected: all pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/cairn-store-sqlite/tests/migrations.rs
git commit -m "test(store): pin CHECK-constraint rejection for unknown consent kind"
```

---

## Task 5: Rowid + mirror invariant test

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/migrations.rs`

The async mirror tails by `rowid`. A regression where the rebuild renumbers rowids would silently corrupt every existing vault's mirror cursor on upgrade. Lock it down with a test.

- [ ] **Step 1: Write the test.**

```rust
#[test]
fn consent_journal_rebuild_preserves_rowid() {
    // Phase-B (issue #255): the rebuild migration must preserve rowid
    // 1:1 because the async consent.log materializer in cairn-workflows
    // anchors its cursor on consent_journal.rowid. A renumbering would
    // make every persisted cursor point at the wrong (or missing) row.
    //
    // We exercise this by opening an in-memory DB, applying every
    // migration up through 0020, inserting a row at a known rowid via
    // the legacy GRANT path (kind=NULL allowed pre-0021), then applying
    // 0021 and asserting the row is still at the same rowid with kind
    // backfilled from decision.
    use crate::test_support::apply_migrations_through;  // adapt to local helper
    let conn = open_in_memory_at_migration(20).expect("open at v20");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy-rowid', 'sub', 'private', 'REVOKE', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert pre-0021");
    let rowid_before: i64 = conn
        .query_row(
            "SELECT rowid FROM consent_journal WHERE consent_id = 'legacy-rowid'",
            [],
            |r| r.get(0),
        )
        .expect("rowid before");

    apply_migrations_through(&conn, 21).expect("apply 0021");

    let (rowid_after, kind_after): (i64, String) = conn
        .query_row(
            "SELECT rowid, kind FROM consent_journal WHERE consent_id = 'legacy-rowid'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row after");

    assert_eq!(rowid_before, rowid_after, "rebuild must preserve rowid");
    assert_eq!(kind_after, "revoke", "REVOKE decision should backfill to 'revoke'");
}
```

NOTE on helpers: this test needs a way to apply migrations up to a chosen version. If `open_in_memory_at_migration` / `apply_migrations_through` don't exist, either (a) add them as `#[cfg(test)] pub` helpers in the migrations module, or (b) inline the SQL setup using the literal contents of `0001..=0020` files (less DRY; prefer (a)). Search first:

```bash
grep -rn "apply_migrations\|open_in_memory_at" crates/cairn-store-sqlite/tests/ crates/cairn-store-sqlite/src/migrations/ | head
```

If no helper exists and adding one is non-trivial, simplify the test to: open a fresh DB at HEAD (0021), assert the head-migration state has zero null-kind rows, and add a separate unit test for the COALESCE backfill SQL using a hand-built table.

- [ ] **Step 2: Run the test.**

```bash
cargo nextest run -p cairn-store-sqlite consent_journal_rebuild_preserves_rowid
```

Expected: PASS.

- [ ] **Step 3: Test the GRANT backfill path too.**

```rust
#[test]
fn consent_journal_rebuild_backfills_grant_kind() {
    // Mirror of the REVOKE test above, but for the 'GRANT' -> 'grant'
    // backfill arm of the COALESCE in 0021.
    let conn = open_in_memory_at_migration(20).expect("open at v20");
    conn.execute(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('legacy-grant', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
        [],
    )
    .expect("legacy insert pre-0021");
    apply_migrations_through(&conn, 21).expect("apply 0021");
    let kind: String = conn
        .query_row(
            "SELECT kind FROM consent_journal WHERE consent_id = 'legacy-grant'",
            [],
            |r| r.get(0),
        )
        .expect("kind after");
    assert_eq!(kind, "grant");
}
```

- [ ] **Step 4: Run + commit.**

```bash
cargo nextest run -p cairn-store-sqlite consent_journal_rebuild
git add crates/cairn-store-sqlite/tests/migrations.rs
git commit -m "test(store): pin rowid preservation + decision->kind backfill for 0021"
```

---

## Task 6: Verify the broader invariant suite still passes

The Phase-A trigger guards in 0009 + 0011 still gate every event-row INSERT. Phase-B did not modify their bodies — it just re-attached them to the rebuilt table. The full migrations test suite + consent integration tests should all pass unchanged.

- [ ] **Step 1: Run the full workspace test suite.**

```bash
cargo nextest run --workspace --no-fail-fast
```

Expected: all pass (the same 2037 baseline you started with, plus the new tests from this PR).

- [ ] **Step 2: Run doctests.**

```bash
cargo test --doc --workspace
```

Expected: pass.

- [ ] **Step 3: Lints + format.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: clean.

- [ ] **Step 4: Core boundary check.**

```bash
./scripts/check-core-boundary.sh
```

Expected: clean (this PR only touches store + tests).

- [ ] **Step 5: Codegen check.**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: clean (no IDL changes).

- [ ] **Step 6: Final commit if anything pending (e.g., fmt fixes).**

```bash
git status
# if dirty:
git add -A && git commit -m "chore: fmt + lint cleanups for 0021"
```

---

## Task 7: Open the PR

- [ ] **Step 1: Push the branch.**

```bash
git push -u origin feat/issue-255-consent-phase-b
```

- [ ] **Step 2: Open PR.**

Title: `feat(store): consent_journal.kind NOT NULL + CHECK (#255)`

Body must include:
- Brief sources: §14, §3 line 448, §5.6
- Issue: closes #255
- Invariants touched: §14 append-only (preserved — triggers re-attached), §3 storage authority (preserved), §5.6 (preserved — rowid 1:1)
- Out-of-scope items copied from this plan
- Verification block (paste the output of every command from Task 6)

---

## Self-review notes

**Spec coverage:**
- Title says "default flip" → Task 2 step 1 (NOT NULL on kind column).
- Title says "check constraint" → Task 2 step 1 (`CHECK (kind IN ...)`).
- Phase-B implies Phase-A is preserved → Task 2 step 9 re-attaches every 0011 trigger; Task 6 verifies the integration tests for those triggers still pass.

**Placeholder scan:** Task 2 step 1 contains an "IMPLEMENTATION NOTE" telling the implementer to copy 13 triggers verbatim from 0011. This is intentional — pasting 500+ lines of trigger bodies into the plan would duplicate code the implementer can read directly. Every other step contains the literal SQL or Rust to write.

**Type consistency:** the migration uses `migration_id = 21` and the new file is `0021_…` matching the registry naming. The `INSERT INTO schema_migrations` block uses the same column set the 0011 migration uses (`migration_id, name, sql_hash, applied_at`). 0005 used `sql_blake3` instead of `sql_hash` — verify which the current schema uses by checking another recent migration (e.g., 0020) before committing.
