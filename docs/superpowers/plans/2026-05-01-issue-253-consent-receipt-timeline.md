# Issue #253 — Consent Receipt Timeline + Per-Record `consent_model` Gate

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the §6.5 deferred-info stub with full sensor-consent enforcement: introduce a `consent_timeline` table + `ConsentLookup` trait + per-row `records.consent_model` gate, then wire the lint sub-check matrix (covering-grant resolution, sensor binding, scope binding, issuance/expiry/revoke window, state-at-issue) under that gate.

**Architecture:** Add two SQLite migrations (records.consent_model column; consent_timeline table). Define `ConsentLookup` as a new pure-trait contract in `cairn-core` with `timeline()` + `covering_grant()`. Implement it in `cairn-store-sqlite`. Extend `LintInputs` with an optional `&dyn ConsentLookup`. Replace the `consent_deferred` stub with a real `consent.rs` check module that runs the sub-check matrix per-record, gated on `consent_model == ReceiptTimeline` (rows still tagged `LegacyEvent` are skipped — Phase-B flip is #255). Update lint mod tests so deferred-info count drops 5→4.

**Tech Stack:** Rust 2024, `tokio`, `rusqlite`/`sqlx`, `thiserror`, `rstest`, `proptest`, `insta`. SQLite via `cairn-store-sqlite`. Pure functions in `cairn-core::verbs::lint::checks`.

**Brief sources:** §14 (privacy/consent), §6.5 (provenance/`consent_ref`), §4 (contracts), §5.6 (WAL — for migration ordering).

**Spec:** `docs/superpowers/specs/2026-04-30-lint-checks-design.md` §6.5 + §11.

**Tracking issue replaces:** stub in `crates/cairn-core/src/verbs/lint/checks/consent_deferred.rs` (deletes after this lands).

---

## File Structure

**New:**
- `crates/cairn-core/src/contract/consent_lookup.rs` — `ConsentLookup` trait, `CoveringGrant`, errors.
- `crates/cairn-core/src/domain/consent_timeline.rs` — `ConsentTimelineEvent`, `ConsentTimelineEventKind` enum, parser/validator.
- `crates/cairn-core/src/verbs/lint/checks/consent.rs` — sub-check matrix (replaces `consent_deferred.rs`).
- `crates/cairn-store-sqlite/src/migrations/sql/0022_records_consent_model.sql` — adds `records.consent_model TEXT NOT NULL DEFAULT 'legacy_event'`.
- `crates/cairn-store-sqlite/src/migrations/sql/0023_consent_timeline.sql` — adds `consent_timeline` table keyed by `(consent_ref, seq)`.
- `crates/cairn-store-sqlite/src/consent_timeline.rs` — adapter impl of `ConsentLookup`.
- `crates/cairn-test-fixtures/src/fake_consent_lookup.rs` — in-memory `BTreeMap`-backed `FakeConsentLookup` for tests.

**Modified:**
- `crates/cairn-core/src/contract/mod.rs` — re-export `consent_lookup`.
- `crates/cairn-core/src/domain/mod.rs` — re-export `consent_timeline`.
- `crates/cairn-core/src/verbs/lint/mod.rs` — add `consent_lookup: Option<&'a dyn ConsentLookup>` to `LintInputs`; update `run_checks` to call new `consent` module instead of `consent_deferred`; update tests (5→4 deferred, info count drops by 1, three new test scenarios under the matrix).
- `crates/cairn-core/src/verbs/lint/checks/mod.rs` — `pub mod consent;` (drop `consent_deferred`).
- `crates/cairn-core/src/contract/memory_store.rs` — add **read-only** method to fetch `(record_id, consent_model)` pairs (`async fn list_consent_models(&self) -> Result<HashMap<RecordId, ConsentModelTag>, StoreError>`); default impl returns empty map (every row treated as `LegacyEvent`).
- `crates/cairn-store-sqlite/src/store.rs` — implement `list_consent_models` against `records.consent_model`.
- `crates/cairn-cli/src/verbs/lint.rs` — replace hard-coded `LegacyEvent` with per-row lookup; pass a `ConsentLookup` adapter into `LintInputs`.
- `crates/cairn-cli/src/verbs/lint.rs` (snapshot tests) — defect-matrix vault gains a §6.5 receipt-timeline-failing record; snapshots refresh.
- `docs/design/traceability.md` — map §6.5 to this issue (closing the deferred row).

**Deleted:**
- `crates/cairn-core/src/verbs/lint/checks/consent_deferred.rs` — replaced by `consent.rs`.

**Untouched (explicit non-goals — see #255):**
- `cairn-idl` IDLs — finding shape unchanged. New sub-check kinds reuse existing `Kind` variants (`MissingProvenance` for missing-grant, plus `MalformedRecord` where appropriate). No new `Kind` needed.
- Phase-B default flip + ingest writes to `consent_timeline` (those land in #255).
- `consent.log` materializer — timeline lives in its own table; no mirror change in this PR.

---

## Task 1: Add `records.consent_model` migration (Phase-A column, defaults to legacy_event)

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0022_records_consent_model.sql`
- Test: `crates/cairn-store-sqlite/tests/migrations_consent_model.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-store-sqlite/tests/migrations_consent_model.rs`:

```rust
//! 0022 records.consent_model — Phase-A column with backward-compat default.
//!
//! Brief §14, Issue #253.

use cairn_store_sqlite::testing::open_migrated_memory_store;

#[tokio::test]
async fn records_table_has_consent_model_column_with_legacy_default() {
    let store = open_migrated_memory_store().await.expect("open store");
    let conn = store.raw_pool_for_test();

    // Column exists, NOT NULL, default 'legacy_event'.
    let row: (String, i64, Option<String>) = sqlx::query_as(
        r#"
        SELECT name, "notnull", dflt_value
          FROM pragma_table_info('records')
         WHERE name = 'consent_model'
        "#,
    )
    .fetch_one(conn)
    .await
    .expect("column exists");

    assert_eq!(row.0, "consent_model");
    assert_eq!(row.1, 1, "consent_model must be NOT NULL");
    assert_eq!(
        row.2.as_deref(),
        Some("'legacy_event'"),
        "default must be 'legacy_event' for Phase-A backward compat"
    );

    // CHECK constraint enforces the closed enum.
    let bad: Result<sqlx::sqlite::SqliteQueryResult, _> = sqlx::query(
        "INSERT INTO records (record_id, target_id, version, path, kind, class, \
         visibility, scope, actor_chain, body, body_hash, created_at, updated_at, \
         active, tombstoned, is_static, consent_model) \
         VALUES ('r','t',1,'p','user','user','private','private','[]','b','h:0',0,0,0,0,0,'banana')",
    )
    .execute(conn)
    .await;
    assert!(bad.is_err(), "CHECK constraint must reject unknown values");
}
```

- [ ] **Step 2: Run the test — verify it fails**

Run: `cargo nextest run -p cairn-store-sqlite --test migrations_consent_model`
Expected: FAIL ("no such column: consent_model" or migration count off-by-one).

- [ ] **Step 3: Write the migration**

Create `crates/cairn-store-sqlite/src/migrations/sql/0022_records_consent_model.sql`:

```sql
-- Migration 0022: records.consent_model — Phase-A per-row gate.
-- Brief sources: §14 (privacy / consent). Issue #253.
-- Purpose: tag each record with the consent storage model that authorized
-- it. Phase-A (this migration) adds the column with default 'legacy_event'
-- so existing rows + every new ingest stay on the legacy consent journal
-- path. Phase-B (#255) flips the default to 'receipt_timeline' once the
-- timeline table is broadly populated.

ALTER TABLE records
  ADD COLUMN consent_model TEXT NOT NULL DEFAULT 'legacy_event'
  CHECK (consent_model IN ('legacy_event', 'receipt_timeline'));

CREATE INDEX records_consent_model_idx
  ON records(consent_model)
  WHERE active = 1 AND tombstoned = 0;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (22, '0022_records_consent_model', '', strftime('%s','now') * 1000);
```

- [ ] **Step 4: Register the migration**

Modify `crates/cairn-store-sqlite/src/migrations/mod.rs` (or wherever the `MIGRATIONS` array lives — search `0021_consent_kind_not_null` to find the array). Append:

```rust
include_str!("sql/0022_records_consent_model.sql"),
```

After the existing `0021` entry. Update the migration count assertion (e.g., `assert_eq!(MIGRATIONS.len(), 22);` if present).

- [ ] **Step 5: Run the test — verify it passes**

Run: `cargo nextest run -p cairn-store-sqlite --test migrations_consent_model`
Expected: PASS.

- [ ] **Step 6: Run the full store test suite — verify no regressions**

Run: `cargo nextest run -p cairn-store-sqlite`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0022_records_consent_model.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs \
        crates/cairn-store-sqlite/tests/migrations_consent_model.rs
git commit -m "feat(store): records.consent_model Phase-A column (brief §14, #253)"
```

---

## Task 2: Add `consent_timeline` migration

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0023_consent_timeline.sql`
- Test: `crates/cairn-store-sqlite/tests/migrations_consent_timeline.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-store-sqlite/tests/migrations_consent_timeline.rs`:

```rust
//! 0023 consent_timeline — receipt timeline (issued/expired/revoked events).
//! Brief §14, Issue #253.

use cairn_store_sqlite::testing::open_migrated_memory_store;

#[tokio::test]
async fn consent_timeline_table_exists_with_seq_pk_and_immutability_triggers() {
    let store = open_migrated_memory_store().await.expect("open store");
    let conn = store.raw_pool_for_test();

    // Table + PK shape.
    let cols: Vec<(String, i64)> = sqlx::query_as(
        r#"SELECT name, "notnull" FROM pragma_table_info('consent_timeline') ORDER BY cid"#,
    )
    .fetch_all(conn)
    .await
    .expect("table exists");
    let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "consent_ref",
            "seq",
            "kind",
            "sensor_id",
            "scope",
            "decided_at",
            "expires_at",
            "payload_json",
        ]
    );

    // (consent_ref, seq) primary key.
    let pk: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('consent_timeline') \
         WHERE pk > 0 ORDER BY pk",
    )
    .fetch_all(conn)
    .await
    .expect("pk");
    assert_eq!(pk, vec!["consent_ref".to_owned(), "seq".to_owned()]);

    // Append-only: UPDATE rejected.
    sqlx::query(
        "INSERT INTO consent_timeline \
            (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
         VALUES ('consent:01', 1, 'issued', 'snr:test', 'private', 1000, 2000, '{}')",
    )
    .execute(conn)
    .await
    .expect("insert");

    let upd: Result<sqlx::sqlite::SqliteQueryResult, _> = sqlx::query(
        "UPDATE consent_timeline SET sensor_id = 'snr:other' WHERE consent_ref = 'consent:01'",
    )
    .execute(conn)
    .await;
    assert!(upd.is_err(), "consent_timeline rows must be immutable");

    let del: Result<sqlx::sqlite::SqliteQueryResult, _> =
        sqlx::query("DELETE FROM consent_timeline WHERE consent_ref = 'consent:01'")
            .execute(conn)
            .await;
    assert!(del.is_err(), "consent_timeline must be append-only");

    // CHECK on `kind`.
    let bad: Result<sqlx::sqlite::SqliteQueryResult, _> = sqlx::query(
        "INSERT INTO consent_timeline \
            (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
         VALUES ('consent:02', 1, 'banana', 'snr:test', 'private', 1000, NULL, '{}')",
    )
    .execute(conn)
    .await;
    assert!(bad.is_err(), "kind CHECK must reject unknown values");
}
```

- [ ] **Step 2: Run the test — verify it fails**

Run: `cargo nextest run -p cairn-store-sqlite --test migrations_consent_timeline`
Expected: FAIL ("no such table: consent_timeline").

- [ ] **Step 3: Write the migration**

Create `crates/cairn-store-sqlite/src/migrations/sql/0023_consent_timeline.sql`:

```sql
-- Migration 0023: consent_timeline — ordered receipt events keyed by
-- (consent_ref, seq). Brief §14, Issue #253.
--
-- Each row is one transition for a covering grant: issued (the grant came
-- into force), expired (TTL elapsed), revoked (user / operator pulled it).
-- The lint `consent` check resolves the *covering* grant for a record's
-- provenance.consent_ref + sensor + scope + created_at by walking this
-- table; ingest writers (Phase-B, #255) append rows here and stamp the
-- record's consent_model='receipt_timeline'.

CREATE TABLE consent_timeline (
  consent_ref   TEXT    NOT NULL,
  seq           INTEGER NOT NULL,
  kind          TEXT    NOT NULL CHECK (kind IN ('issued','expired','revoked')),
  sensor_id     TEXT    NOT NULL,
  scope         TEXT    NOT NULL,
  decided_at    INTEGER NOT NULL,
  expires_at    INTEGER,
  payload_json  TEXT    NOT NULL DEFAULT '{}',
  PRIMARY KEY (consent_ref, seq)
);

CREATE INDEX consent_timeline_sensor_idx
  ON consent_timeline(sensor_id, decided_at);
CREATE INDEX consent_timeline_decided_idx
  ON consent_timeline(decided_at);

CREATE TRIGGER consent_timeline_immutable
  BEFORE UPDATE ON consent_timeline
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_timeline rows are immutable');
END;

CREATE TRIGGER consent_timeline_no_delete
  BEFORE DELETE ON consent_timeline
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_timeline is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (23, '0023_consent_timeline', '', strftime('%s','now') * 1000);
```

- [ ] **Step 4: Register the migration**

Append `include_str!("sql/0023_consent_timeline.sql"),` after the 0022 entry in the migrations array. Update the count assertion if present.

- [ ] **Step 5: Run the test — verify it passes**

Run: `cargo nextest run -p cairn-store-sqlite --test migrations_consent_timeline`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/sql/0023_consent_timeline.sql \
        crates/cairn-store-sqlite/src/migrations/mod.rs \
        crates/cairn-store-sqlite/tests/migrations_consent_timeline.rs
git commit -m "feat(store): add consent_timeline append-only table (brief §14, #253)"
```

---

## Task 3: Domain types — `ConsentTimelineEvent`, `ConsentTimelineEventKind`, `CoveringGrant`

**Files:**
- Create: `crates/cairn-core/src/domain/consent_timeline.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs:1-30` (add `pub mod consent_timeline;` + re-exports)

- [ ] **Step 1: Write the failing test**

In a new module `crates/cairn-core/src/domain/consent_timeline.rs`, add the failing tests at the bottom (the file initially won't compile — that's fine):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Identity, Rfc3339Timestamp, SensorLabel};

    fn ts(s: &str) -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse(s).expect("ts")
    }

    fn ev(kind: ConsentTimelineEventKind, decided: &str, expires: Option<&str>) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: "consent:01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            seq: 1,
            kind,
            sensor_id: SensorLabel::parse("local:screen:host:v1").expect("sensor"),
            scope: "private".to_owned(),
            decided_at: ts(decided),
            expires_at: expires.map(ts),
        }
    }

    #[test]
    fn covering_grant_resolves_when_only_issued_event_in_window() {
        let issued = ev(ConsentTimelineEventKind::Issued,
                        "2026-01-01T00:00:00Z",
                        Some("2026-12-31T00:00:00Z"));
        let grant = CoveringGrant::resolve(
            std::slice::from_ref(&issued),
            &SensorLabel::parse("local:screen:host:v1").unwrap(),
            "private",
            &ts("2026-06-01T00:00:00Z"),
        );
        assert!(grant.is_some());
    }

    #[test]
    fn covering_grant_none_after_expiry() {
        let issued = ev(ConsentTimelineEventKind::Issued,
                        "2026-01-01T00:00:00Z",
                        Some("2026-02-01T00:00:00Z"));
        let g = CoveringGrant::resolve(
            std::slice::from_ref(&issued),
            &SensorLabel::parse("local:screen:host:v1").unwrap(),
            "private",
            &ts("2026-06-01T00:00:00Z"),
        );
        assert!(g.is_none(), "expired grant must not cover");
    }

    #[test]
    fn covering_grant_none_after_revoke() {
        let issued = ev(ConsentTimelineEventKind::Issued,
                        "2026-01-01T00:00:00Z", None);
        let mut revoked = issued.clone();
        revoked.seq = 2;
        revoked.kind = ConsentTimelineEventKind::Revoked;
        revoked.decided_at = ts("2026-03-01T00:00:00Z");

        let events = vec![issued, revoked];
        let g = CoveringGrant::resolve(
            &events,
            &SensorLabel::parse("local:screen:host:v1").unwrap(),
            "private",
            &ts("2026-06-01T00:00:00Z"),
        );
        assert!(g.is_none(), "revoked grant must not cover later writes");
    }

    #[test]
    fn covering_grant_distinguishes_sensor_mismatch() {
        let issued = ev(ConsentTimelineEventKind::Issued,
                        "2026-01-01T00:00:00Z", None);
        let g = CoveringGrant::resolve(
            std::slice::from_ref(&issued),
            &SensorLabel::parse("local:terminal:host:v1").unwrap(),
            "private",
            &ts("2026-06-01T00:00:00Z"),
        );
        assert!(g.is_none(), "sensor mismatch must not cover");
    }

    #[test]
    fn covering_grant_distinguishes_scope_mismatch() {
        let issued = ev(ConsentTimelineEventKind::Issued,
                        "2026-01-01T00:00:00Z", None);
        let g = CoveringGrant::resolve(
            std::slice::from_ref(&issued),
            &SensorLabel::parse("local:screen:host:v1").unwrap(),
            "team:platform",
            &ts("2026-06-01T00:00:00Z"),
        );
        assert!(g.is_none(), "scope mismatch must not cover");
    }

    #[test]
    fn covering_grant_picks_latest_issued_before_t() {
        let mut a = ev(ConsentTimelineEventKind::Issued, "2026-01-01T00:00:00Z", None);
        a.seq = 1;
        let mut b = ev(ConsentTimelineEventKind::Issued, "2026-04-01T00:00:00Z", None);
        b.seq = 2;
        let events = vec![a, b];
        let g = CoveringGrant::resolve(
            &events,
            &SensorLabel::parse("local:screen:host:v1").unwrap(),
            "private",
            &ts("2026-06-01T00:00:00Z"),
        ).expect("grant");
        assert_eq!(g.issued_at, ts("2026-04-01T00:00:00Z"));
    }
}
```

- [ ] **Step 2: Write the type + resolver**

Replace the file body (above the test module) with:

```rust
//! Consent receipt timeline — Issue #253, brief §14.
//!
//! Append-only events keyed by `(consent_ref, seq)`. Each event is a
//! transition for a covering grant: `issued` (came into force), `expired`
//! (TTL elapsed via a writer-emitted event), `revoked` (user / operator
//! pulled the grant). The pure resolver `CoveringGrant::resolve` walks an
//! event slice and decides whether a candidate `(sensor, scope, t)` tuple
//! is covered. Ingest writers (Phase-B, #255) append rows; the SQLite
//! adapter persists them; the lint `consent` check reads them through the
//! `ConsentLookup` contract (see `crate::contract::consent_lookup`).
//!
//! Pure data + pure resolver. No I/O.

use serde::{Deserialize, Serialize};

use crate::domain::{Rfc3339Timestamp, SensorLabel};

/// One row of the `consent_timeline` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentTimelineEvent {
    /// Grant identifier this event belongs to. Matches
    /// `MemoryRecord.provenance.consent_ref` for records written under
    /// this grant.
    pub consent_ref: String,
    /// Monotonic per-`consent_ref` sequence number, starting at 1. The
    /// SQLite primary key is `(consent_ref, seq)`.
    pub seq: u64,
    /// Transition kind.
    pub kind: ConsentTimelineEventKind,
    /// Sensor the grant was issued for.
    pub sensor_id: SensorLabel,
    /// Scope tuple in canonical wire form (matches
    /// `consent_journal.scope`).
    pub scope: String,
    /// When the transition was decided.
    pub decided_at: Rfc3339Timestamp,
    /// Optional TTL on the *issued* event; ignored on expired/revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Rfc3339Timestamp>,
}

/// Kind of timeline event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsentTimelineEventKind {
    /// Grant came into force.
    Issued,
    /// Grant's TTL elapsed (writer materialized the boundary as an event).
    Expired,
    /// Grant was revoked by user / operator.
    Revoked,
}

/// Resolved covering grant — what authorized a write at a given instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoveringGrant {
    /// `consent_ref` of the resolved grant.
    pub consent_ref: String,
    /// Sensor the grant binds to.
    pub sensor_id: SensorLabel,
    /// Scope tuple the grant binds to.
    pub scope: String,
    /// When the issuing event was decided.
    pub issued_at: Rfc3339Timestamp,
    /// TTL boundary, if any.
    pub expires_at: Option<Rfc3339Timestamp>,
}

impl CoveringGrant {
    /// Resolve the covering grant for `(sensor, scope)` at instant `at`.
    ///
    /// Returns `Some(grant)` iff there exists an `Issued` event in
    /// `events` such that:
    ///   * `event.sensor_id == sensor`,
    ///   * `event.scope == scope`,
    ///   * `event.decided_at <= at`,
    ///   * no `Revoked` event with `decided_at <= at` follows it
    ///     (by `seq` order),
    ///   * `event.expires_at.is_none() || at < event.expires_at`.
    ///
    /// Among multiple candidates, returns the one with the largest
    /// `decided_at` (then largest `seq`) — the most recent grant in
    /// force.
    ///
    /// Pure function. `events` need not be sorted; the resolver sorts
    /// internally by `(decided_at, seq)`.
    #[must_use]
    pub fn resolve(
        events: &[ConsentTimelineEvent],
        sensor: &SensorLabel,
        scope: &str,
        at: &Rfc3339Timestamp,
    ) -> Option<Self> {
        // Filter to (sensor, scope) and decided_at <= at.
        let mut relevant: Vec<&ConsentTimelineEvent> = events
            .iter()
            .filter(|e| e.sensor_id == *sensor && e.scope == scope && e.decided_at <= *at)
            .collect();
        // Sort by (decided_at, seq) ascending.
        relevant.sort_by(|a, b| {
            a.decided_at
                .cmp(&b.decided_at)
                .then_with(|| a.seq.cmp(&b.seq))
        });

        // Walk forward; track the most recent issued event still in force.
        let mut current: Option<&ConsentTimelineEvent> = None;
        for ev in relevant {
            match ev.kind {
                ConsentTimelineEventKind::Issued => {
                    current = Some(ev);
                }
                ConsentTimelineEventKind::Revoked | ConsentTimelineEventKind::Expired => {
                    if current.is_some() {
                        current = None;
                    }
                }
            }
        }
        let issued = current?;
        if let Some(exp) = &issued.expires_at
            && at >= exp
        {
            return None;
        }
        Some(Self {
            consent_ref: issued.consent_ref.clone(),
            sensor_id: issued.sensor_id.clone(),
            scope: issued.scope.clone(),
            issued_at: issued.decided_at.clone(),
            expires_at: issued.expires_at.clone(),
        })
    }
}
```

> **Note:** `Rfc3339Timestamp` must implement `Ord` for the resolver. Verify via `cargo check -p cairn-core` after writing the file. If it does not, the smallest fix is to derive `PartialOrd, Ord` on `Rfc3339Timestamp` in `crates/cairn-core/src/domain/timestamp.rs` (the type already wraps a normalized `(secs, nanos)` tuple — search the file to confirm). Add this fix in the same task.

- [ ] **Step 3: Re-export from `domain/mod.rs`**

Modify `crates/cairn-core/src/domain/mod.rs`. Locate the existing `pub mod consent;` line and add immediately below:

```rust
pub mod consent_timeline;

pub use consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind, CoveringGrant};
```

- [ ] **Step 4: Run the tests — verify they pass**

Run: `cargo nextest run -p cairn-core consent_timeline`
Expected: 6 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/consent_timeline.rs \
        crates/cairn-core/src/domain/mod.rs \
        crates/cairn-core/src/domain/timestamp.rs  # only if Ord derive was added
git commit -m "feat(domain): consent timeline events + CoveringGrant resolver (#253)"
```

---

## Task 4: `ConsentLookup` contract trait

**Files:**
- Create: `crates/cairn-core/src/contract/consent_lookup.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs:1-40` (add `pub mod consent_lookup;` + re-export)

- [ ] **Step 1: Write the failing test**

Add an in-module test inside the new file (we'll write the trait surface first, then add a small fake to verify the trait is object-safe and the default `covering_grant` impl delegates to `timeline()`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    use std::collections::HashMap;

    struct StaticLookup {
        by_ref: HashMap<String, Vec<ConsentTimelineEvent>>,
    }

    #[async_trait::async_trait]
    impl ConsentLookup for StaticLookup {
        async fn timeline(
            &self,
            consent_ref: &str,
        ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
            Ok(self.by_ref.get(consent_ref).cloned().unwrap_or_default())
        }
    }

    fn ev(consent_ref: &str, seq: u64, sensor: &str, scope: &str) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: consent_ref.to_owned(),
            seq,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: SensorLabel::parse(sensor).unwrap(),
            scope: scope.to_owned(),
            decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn default_covering_grant_delegates_to_timeline() {
        let mut by_ref = HashMap::new();
        by_ref.insert("c:1".to_owned(), vec![ev("c:1", 1, "local:screen:h:v1", "private")]);
        let lk = StaticLookup { by_ref };

        let g = lk
            .covering_grant(
                "c:1",
                &SensorLabel::parse("local:screen:h:v1").unwrap(),
                "private",
                &Rfc3339Timestamp::parse("2026-06-01T00:00:00Z").unwrap(),
            )
            .await
            .expect("ok");
        assert!(g.is_some());
    }

    #[tokio::test]
    async fn returns_none_when_consent_ref_unknown() {
        let lk = StaticLookup { by_ref: HashMap::new() };
        let g = lk
            .covering_grant(
                "c:missing",
                &SensorLabel::parse("local:screen:h:v1").unwrap(),
                "private",
                &Rfc3339Timestamp::parse("2026-06-01T00:00:00Z").unwrap(),
            )
            .await
            .expect("ok");
        assert!(g.is_none());
    }

    #[test]
    fn trait_is_object_safe() {
        // Compiles iff `ConsentLookup` is dyn-compatible.
        fn _accept(_: &dyn ConsentLookup) {}
    }
}
```

- [ ] **Step 2: Write the trait + default `covering_grant` impl**

Above the test module:

```rust
//! `ConsentLookup` contract — read-only access to the consent timeline.
//!
//! Brief §14, Issue #253. Adapter implementations live in
//! `cairn-store-sqlite::consent_timeline`. Used by the lint `consent`
//! sub-check matrix to resolve the covering grant for a record's
//! `provenance.consent_ref`.
//!
//! Object-safe (dyn-compatible) so verb-layer code can pass a
//! `&dyn ConsentLookup` through `LintInputs` without leaking adapter types
//! into core.

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::consent_timeline::{ConsentTimelineEvent, CoveringGrant};
use crate::domain::{Rfc3339Timestamp, SensorLabel};

/// Errors raised by [`ConsentLookup`] implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConsentLookupError {
    /// Underlying store I/O failure (DB unavailable, connection lost, …).
    #[error("consent lookup backend error: {message}")]
    Backend {
        /// Adapter-supplied diagnostic. Never the underlying user content.
        message: String,
    },
}

/// Read-only access to the `consent_timeline`. Implementations must be
/// safe to call from the verb layer (no hidden global state, no panics).
#[async_trait]
pub trait ConsentLookup: Send + Sync {
    /// Return the full event list for `consent_ref`, in any order.
    /// Empty vec when the ref is unknown.
    async fn timeline(
        &self,
        consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError>;

    /// Resolve the covering grant for `(sensor, scope)` at instant `at`.
    /// Default impl walks `timeline(consent_ref)` and delegates to
    /// `CoveringGrant::resolve`. Adapters with a one-shot SQL query
    /// should override.
    async fn covering_grant(
        &self,
        consent_ref: &str,
        sensor: &SensorLabel,
        scope: &str,
        at: &Rfc3339Timestamp,
    ) -> Result<Option<CoveringGrant>, ConsentLookupError> {
        let events = self.timeline(consent_ref).await?;
        Ok(CoveringGrant::resolve(&events, sensor, scope, at))
    }
}
```

- [ ] **Step 3: Re-export from `contract/mod.rs`**

Locate `pub mod memory_store;` in `crates/cairn-core/src/contract/mod.rs` and add immediately below:

```rust
pub mod consent_lookup;

pub use consent_lookup::{ConsentLookup, ConsentLookupError};
```

- [ ] **Step 4: Run the tests — verify pass**

Run: `cargo nextest run -p cairn-core consent_lookup`
Expected: 3 tests pass (including object-safety check).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/contract/consent_lookup.rs \
        crates/cairn-core/src/contract/mod.rs
git commit -m "feat(contract): ConsentLookup trait + default covering_grant (#253)"
```

---

## Task 5: SQLite `ConsentLookup` adapter

**Files:**
- Create: `crates/cairn-store-sqlite/src/consent_timeline.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs:1-40` (add `pub mod consent_timeline;`)
- Modify: `crates/cairn-store-sqlite/src/store.rs` (impl `ConsentLookup` on the existing `SqliteStore` struct)
- Test: `crates/cairn-store-sqlite/tests/consent_lookup_smoke.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-store-sqlite/tests/consent_lookup_smoke.rs`:

```rust
//! Smoke test for the SQLite ConsentLookup adapter — Issue #253.

use cairn_core::contract::consent_lookup::ConsentLookup;
use cairn_core::domain::consent_timeline::ConsentTimelineEventKind;
use cairn_core::domain::{Rfc3339Timestamp, SensorLabel};
use cairn_store_sqlite::testing::open_migrated_memory_store;

#[tokio::test]
async fn timeline_round_trips_through_sqlite() {
    let store = open_migrated_memory_store().await.expect("open");
    let conn = store.raw_pool_for_test();

    sqlx::query(
        "INSERT INTO consent_timeline \
            (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
         VALUES \
            ('c:1', 1, 'issued',  'local:screen:h:v1', 'private', 1735689600, 1767225600, '{}'), \
            ('c:1', 2, 'revoked', 'local:screen:h:v1', 'private', 1751328000, NULL, '{}')",
    )
    .execute(conn)
    .await
    .expect("seed");

    let events = store.timeline("c:1").await.expect("ok");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, ConsentTimelineEventKind::Issued);
    assert_eq!(events[1].kind, ConsentTimelineEventKind::Revoked);

    // Covering grant resolution: at t=1740000000 (post-issue, pre-revoke) covered;
    // at t=1760000000 (post-revoke) not covered.
    let sensor = SensorLabel::parse("local:screen:h:v1").unwrap();
    let covered = store
        .covering_grant(
            "c:1",
            &sensor,
            "private",
            &Rfc3339Timestamp::from_unix_secs(1_740_000_000).expect("ts"),
        )
        .await
        .expect("ok");
    assert!(covered.is_some());

    let uncovered = store
        .covering_grant(
            "c:1",
            &sensor,
            "private",
            &Rfc3339Timestamp::from_unix_secs(1_760_000_000).expect("ts"),
        )
        .await
        .expect("ok");
    assert!(uncovered.is_none());
}
```

> **Note:** if `Rfc3339Timestamp::from_unix_secs` does not exist, replace with `Rfc3339Timestamp::parse("...")` literals. Search `crates/cairn-core/src/domain/timestamp.rs` first.

- [ ] **Step 2: Run the test — verify it fails**

Run: `cargo nextest run -p cairn-store-sqlite --test consent_lookup_smoke`
Expected: FAIL ("no method named `timeline`" on `SqliteStore`).

- [ ] **Step 3: Write the adapter module**

Create `crates/cairn-store-sqlite/src/consent_timeline.rs`:

```rust
//! `ConsentLookup` impl for `SqliteStore` — Issue #253.

use async_trait::async_trait;
use sqlx::Row;

use cairn_core::contract::consent_lookup::{ConsentLookup, ConsentLookupError};
use cairn_core::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
use cairn_core::domain::{Rfc3339Timestamp, SensorLabel};

use crate::store::SqliteStore;

#[async_trait]
impl ConsentLookup for SqliteStore {
    async fn timeline(
        &self,
        consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
        let pool = self.pool();
        let rows = sqlx::query(
            "SELECT consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at \
               FROM consent_timeline \
              WHERE consent_ref = ?1 \
              ORDER BY seq ASC",
        )
        .bind(consent_ref)
        .fetch_all(pool)
        .await
        .map_err(|e| ConsentLookupError::Backend {
            message: format!("consent_timeline read: {e}"),
        })?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let kind: String = row.try_get("kind").map_err(backend)?;
            let kind = match kind.as_str() {
                "issued" => ConsentTimelineEventKind::Issued,
                "expired" => ConsentTimelineEventKind::Expired,
                "revoked" => ConsentTimelineEventKind::Revoked,
                other => {
                    return Err(ConsentLookupError::Backend {
                        message: format!("consent_timeline.kind unknown variant: {other}"),
                    });
                }
            };
            let sensor: String = row.try_get("sensor_id").map_err(backend)?;
            let sensor = SensorLabel::parse(sensor).map_err(|e| ConsentLookupError::Backend {
                message: format!("consent_timeline.sensor_id parse: {e}"),
            })?;
            let decided_at_secs: i64 = row.try_get("decided_at").map_err(backend)?;
            let decided_at = Rfc3339Timestamp::from_unix_secs(decided_at_secs)
                .map_err(|e| ConsentLookupError::Backend {
                    message: format!("decided_at: {e}"),
                })?;
            let expires_at: Option<i64> = row.try_get("expires_at").map_err(backend)?;
            let expires_at = expires_at
                .map(Rfc3339Timestamp::from_unix_secs)
                .transpose()
                .map_err(|e| ConsentLookupError::Backend {
                    message: format!("expires_at: {e}"),
                })?;

            out.push(ConsentTimelineEvent {
                consent_ref: row.try_get("consent_ref").map_err(backend)?,
                seq: row.try_get::<i64, _>("seq").map_err(backend)? as u64,
                kind,
                sensor_id: sensor,
                scope: row.try_get("scope").map_err(backend)?,
                decided_at,
                expires_at,
            });
        }
        Ok(out)
    }
}

fn backend(e: sqlx::Error) -> ConsentLookupError {
    ConsentLookupError::Backend {
        message: e.to_string(),
    }
}
```

> **Note:** if `SqliteStore::pool()` is private, expose a `pub(crate) fn pool(&self) -> &SqlitePool` accessor on `store.rs`. If `Rfc3339Timestamp::from_unix_secs` does not exist, add it to `crates/cairn-core/src/domain/timestamp.rs` (it is required by the resolver tests in Task 5 and the SQLite adapter both — implement once here):
>
> ```rust
> impl Rfc3339Timestamp {
>     /// Construct from a Unix timestamp in seconds.
>     pub fn from_unix_secs(secs: i64) -> Result<Self, DomainError> { ... }
> }
> ```
>
> Land that addition in this commit if needed (check first with `grep -n from_unix_secs crates/cairn-core/src/domain/timestamp.rs`).

- [ ] **Step 4: Wire into `lib.rs`**

In `crates/cairn-store-sqlite/src/lib.rs`, after the existing `pub mod consent;` line, add:

```rust
pub mod consent_timeline;
```

- [ ] **Step 5: Run the test — verify pass**

Run: `cargo nextest run -p cairn-store-sqlite --test consent_lookup_smoke`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/consent_timeline.rs \
        crates/cairn-store-sqlite/src/lib.rs \
        crates/cairn-store-sqlite/src/store.rs \
        crates/cairn-store-sqlite/tests/consent_lookup_smoke.rs \
        crates/cairn-core/src/domain/timestamp.rs  # only if from_unix_secs was added
git commit -m "feat(store): SqliteStore impls ConsentLookup (#253)"
```

---

## Task 6: `MemoryStore::list_consent_models` + adapter impl

**Files:**
- Modify: `crates/cairn-core/src/contract/memory_store.rs:60-180` (add new method with default-empty impl)
- Modify: `crates/cairn-core/src/verbs/lint/mod.rs:30-40` (add `ConsentModelTag` newtype mirroring DB column values; or reuse existing `ConsentModel`)
- Modify: `crates/cairn-store-sqlite/src/store.rs` (override `list_consent_models`)
- Test: `crates/cairn-store-sqlite/tests/list_consent_models.rs`

> **Decision:** reuse the existing `cairn_core::verbs::lint::ConsentModel` enum (`LegacyEvent` | `ReceiptTimeline`). Move it from `verbs::lint::mod` to `domain::consent_timeline::ConsentModel` so it can be a `MemoryStore` return type without `cairn-core` cross-module weirdness; re-export from `verbs::lint::mod` for back-compat.

- [ ] **Step 1: Move `ConsentModel` to `domain::consent_timeline`**

Cut the enum from `crates/cairn-core/src/verbs/lint/mod.rs` lines ~28-37 and paste at the bottom of `crates/cairn-core/src/domain/consent_timeline.rs`. Re-export from `verbs::lint::mod`:

```rust
// in verbs/lint/mod.rs, near the top
pub use crate::domain::consent_timeline::ConsentModel;
```

Run `cargo check --workspace` to confirm nothing else broke.

- [ ] **Step 2: Write the failing test**

Create `crates/cairn-store-sqlite/tests/list_consent_models.rs`:

```rust
//! `list_consent_models` smoke test — Issue #253.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_store_sqlite::testing::open_migrated_memory_store;

#[tokio::test]
async fn returns_one_entry_per_active_record_with_correct_tag() {
    let store = open_migrated_memory_store().await.expect("open");
    let conn = store.raw_pool_for_test();

    // Seed: one record on legacy_event (column default), one on receipt_timeline.
    sqlx::query(
        "INSERT INTO records (record_id, target_id, version, path, kind, class, \
         visibility, scope, actor_chain, body, body_hash, created_at, updated_at, \
         active, tombstoned, is_static) \
         VALUES ('r1','t1',1,'p','user','user','private','private','[]','b1','h:1',0,0,1,0,0)",
    ).execute(conn).await.expect("r1");
    sqlx::query(
        "INSERT INTO records (record_id, target_id, version, path, kind, class, \
         visibility, scope, actor_chain, body, body_hash, created_at, updated_at, \
         active, tombstoned, is_static, consent_model) \
         VALUES ('r2','t2',1,'p','user','user','private','private','[]','b2','h:2',0,0,1,0,0,'receipt_timeline')",
    ).execute(conn).await.expect("r2");

    let map = store.list_consent_models().await.expect("ok");
    assert_eq!(map.len(), 2);
    let r1: cairn_core::domain::record::RecordId = "r1".parse().unwrap();
    let r2: cairn_core::domain::record::RecordId = "r2".parse().unwrap();
    assert_eq!(map.get(&r1), Some(&ConsentModel::LegacyEvent));
    assert_eq!(map.get(&r2), Some(&ConsentModel::ReceiptTimeline));
}
```

- [ ] **Step 3: Run — expect fail**

Run: `cargo nextest run -p cairn-store-sqlite --test list_consent_models`
Expected: FAIL ("no method named `list_consent_models`").

- [ ] **Step 4: Add the trait method (default empty)**

In `crates/cairn-core/src/contract/memory_store.rs`, inside `trait MemoryStore`:

```rust
/// Per-record consent storage model (Issue #253). Default impl returns
/// an empty map so adapters that haven't shipped the column yet
/// (Phase-A pre-#253 fixtures) cleanly degrade — every record is
/// treated as `LegacyEvent` by callers that look up via `.get(id)`.
async fn list_consent_models(
    &self,
) -> Result<std::collections::HashMap<crate::domain::record::RecordId, crate::domain::consent_timeline::ConsentModel>, StoreError> {
    Ok(std::collections::HashMap::new())
}
```

- [ ] **Step 5: Override in `SqliteStore`**

In `crates/cairn-store-sqlite/src/store.rs`, inside the `impl MemoryStore for SqliteStore` block, add the override:

```rust
async fn list_consent_models(
    &self,
) -> Result<std::collections::HashMap<RecordId, ConsentModel>, StoreError> {
    let rows = sqlx::query(
        "SELECT record_id, consent_model FROM records \
          WHERE active = 1 AND tombstoned = 0",
    )
    .fetch_all(self.pool())
    .await
    .map_err(|e| Box::new(e) as StoreError)?;

    let mut out = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("record_id").map_err(|e| Box::new(e) as StoreError)?;
        let id = RecordId::parse(id).map_err(|e| Box::new(e) as StoreError)?;
        let m: String = row.try_get("consent_model").map_err(|e| Box::new(e) as StoreError)?;
        let m = match m.as_str() {
            "legacy_event" => ConsentModel::LegacyEvent,
            "receipt_timeline" => ConsentModel::ReceiptTimeline,
            other => {
                return Err(format!("unknown records.consent_model: {other}").into());
            }
        };
        out.insert(id, m);
    }
    Ok(out)
}
```

Add `use cairn_core::domain::consent_timeline::ConsentModel;` and `use cairn_core::domain::record::RecordId;` at the top of `store.rs` if not already imported.

- [ ] **Step 6: Run the test — pass**

Run: `cargo nextest run -p cairn-store-sqlite --test list_consent_models`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/contract/memory_store.rs \
        crates/cairn-core/src/verbs/lint/mod.rs \
        crates/cairn-core/src/domain/consent_timeline.rs \
        crates/cairn-store-sqlite/src/store.rs \
        crates/cairn-store-sqlite/tests/list_consent_models.rs
git commit -m "feat(store): list_consent_models for per-row lint gate (#253)"
```

---

## Task 7: `FakeConsentLookup` test fixture

**Files:**
- Create: `crates/cairn-test-fixtures/src/fake_consent_lookup.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs:1-30` (re-export)

- [ ] **Step 1: Write the fixture**

Create `crates/cairn-test-fixtures/src/fake_consent_lookup.rs`:

```rust
//! In-memory `ConsentLookup` for tests — Issue #253.

use std::collections::HashMap;

use async_trait::async_trait;

use cairn_core::contract::consent_lookup::{ConsentLookup, ConsentLookupError};
use cairn_core::domain::consent_timeline::ConsentTimelineEvent;

/// Map-backed `ConsentLookup`. Tests seed it with a `Vec<ConsentTimelineEvent>`
/// per `consent_ref` and pass it as `&dyn ConsentLookup` into `LintInputs`.
#[derive(Debug, Default, Clone)]
pub struct FakeConsentLookup {
    by_ref: HashMap<String, Vec<ConsentTimelineEvent>>,
}

impl FakeConsentLookup {
    /// Empty lookup — every `consent_ref` resolves to no events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed `consent_ref` with `events`. Replaces any prior entry.
    pub fn with(mut self, consent_ref: impl Into<String>, events: Vec<ConsentTimelineEvent>) -> Self {
        self.by_ref.insert(consent_ref.into(), events);
        self
    }

    /// Append events to an existing or new `consent_ref` entry.
    pub fn extend(&mut self, consent_ref: impl Into<String>, events: Vec<ConsentTimelineEvent>) {
        self.by_ref.entry(consent_ref.into()).or_default().extend(events);
    }
}

#[async_trait]
impl ConsentLookup for FakeConsentLookup {
    async fn timeline(
        &self,
        consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
        Ok(self.by_ref.get(consent_ref).cloned().unwrap_or_default())
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

In `crates/cairn-test-fixtures/src/lib.rs`, add:

```rust
pub mod fake_consent_lookup;

pub use fake_consent_lookup::FakeConsentLookup;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p cairn-test-fixtures`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-test-fixtures/src/fake_consent_lookup.rs \
        crates/cairn-test-fixtures/src/lib.rs
git commit -m "test(fixtures): FakeConsentLookup for #253 sub-check tests"
```

---

## Task 8: Replace `consent_deferred.rs` stub with real `consent.rs` matrix — sub-check 1 of 4 (no-covering-grant)

**Files:**
- Create: `crates/cairn-core/src/verbs/lint/checks/consent.rs`
- Modify: `crates/cairn-core/src/verbs/lint/checks/mod.rs:1-15` (`pub mod consent;` — leave `consent_deferred` in place; we'll delete it in Task 12)
- Modify: `crates/cairn-core/src/verbs/lint/mod.rs` (extend `LintInputs` with `consent_lookup: Option<&'a (dyn ConsentLookup + 'a)>`)

- [ ] **Step 1: Write the failing test**

In a new file `crates/cairn-core/src/verbs/lint/checks/consent.rs`, sketch the test up front:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::consent_timeline::ConsentModel;
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::{LintInputs, LintRecord, SchemaVersion};
    use cairn_test_fixtures::FakeConsentLookup;

    fn lint_record_with(consent_ref: &str, model: ConsentModel) -> LintRecord {
        let mut r = sample_record();
        r.provenance.consent_ref = consent_ref.to_owned();
        LintRecord {
            stored: StoredRecord { record: r, version: 1 },
            consent_model: model,
        }
    }

    #[tokio::test]
    async fn flags_receipt_timeline_record_without_covering_grant() {
        let r = lint_record_with("consent:missing", ConsentModel::ReceiptTimeline);
        let cfg = CairnConfig::default();
        let lookup = FakeConsentLookup::new();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            schema_version: SchemaVersion { major: 0, minor: 1 },
            consent_lookup: Some(&lookup),
        };
        let findings = run(&inputs).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("no covering grant"));
        assert!(findings[0].target.is_some());
    }

    #[tokio::test]
    async fn skips_legacy_event_records() {
        let r = lint_record_with("consent:legacy", ConsentModel::LegacyEvent);
        let cfg = CairnConfig::default();
        let lookup = FakeConsentLookup::new();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            schema_version: SchemaVersion { major: 0, minor: 1 },
            consent_lookup: Some(&lookup),
        };
        assert!(run(&inputs).await.is_empty());
    }

    #[tokio::test]
    async fn no_lookup_means_no_findings_even_for_receipt_timeline() {
        // Defensive: if the CLI didn't wire a lookup, the check is a
        // no-op (it cannot resolve grants without one). Operators see
        // this as zero §6.5 findings, never a panic.
        let r = lint_record_with("consent:any", ConsentModel::ReceiptTimeline);
        let cfg = CairnConfig::default();
        let inputs = LintInputs {
            records: std::slice::from_ref(&r),
            config: &cfg,
            index_stats: IndexStats::new(1, 1),
            schema_version: SchemaVersion { major: 0, minor: 1 },
            consent_lookup: None,
        };
        assert!(run(&inputs).await.is_empty());
    }
}
```

- [ ] **Step 2: Extend `LintInputs` with `consent_lookup`**

In `crates/cairn-core/src/verbs/lint/mod.rs`, modify the struct:

```rust
use crate::contract::consent_lookup::ConsentLookup;

#[derive(Debug)]
pub struct LintInputs<'a> {
    pub records: &'a [LintRecord],
    pub config: &'a CairnConfig,
    pub index_stats: IndexStats,
    pub schema_version: SchemaVersion,
    /// `ConsentLookup` adapter (Issue #253). `None` when the CLI
    /// hasn't wired one — the §6.5 check downgrades to a no-op.
    pub consent_lookup: Option<&'a (dyn ConsentLookup + 'a)>,
}
```

Drop the `#[derive(Debug)]` if `dyn ConsentLookup` doesn't impl Debug (it doesn't — `ConsentLookup: Send + Sync` only). Replace with a manual impl that prints `"<lookup>"` when present:

```rust
impl std::fmt::Debug for LintInputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LintInputs")
            .field("records", &self.records)
            .field("config", &"<CairnConfig>")
            .field("index_stats", &self.index_stats)
            .field("schema_version", &self.schema_version)
            .field("consent_lookup", &self.consent_lookup.is_some())
            .finish()
    }
}
```

Update every call site that constructs `LintInputs` — search:

```bash
grep -rn "LintInputs {" crates/
```

— and add `consent_lookup: None,` to each. (CLI handler is updated in Task 11.)

- [ ] **Step 3: Wire the new `consent` module**

`crates/cairn-core/src/verbs/lint/checks/mod.rs`:

```rust
pub mod consent;
pub mod consent_deferred;  // kept until Task 12 swap, but unused after Task 11
```

- [ ] **Step 4: Implement sub-check 1 (no covering grant)**

Top of `consent.rs`:

```rust
//! §6.5 — sensor-consent enforcement (Issue #253).
//!
//! Sub-check matrix per record:
//!   1. Covering-grant resolution: does any issued event match
//!      `(sensor, scope)` and cover `created_at`?
//!   2. Sensor binding: does the grant's `sensor_id` equal the record's
//!      `provenance.source_sensor`'s sensor label form?
//!   3. Scope binding: does the grant's `scope` equal the record's `scope`?
//!   4. Issuance/expiry/revoke window: is `created_at` within
//!      `[issued_at, expires_at)` and not after a revoke event?
//!   5. State-at-issue: was the consent flow already in `issued`
//!      state at `created_at`? (Catches out-of-order writes.)
//!
//! Sub-checks (2)–(5) are partly redundant with `CoveringGrant::resolve`;
//! when the resolver returns `Some(_)` we still cross-check the bindings
//! to surface specific reasons in the finding's message.
//!
//! Records tagged `ConsentModel::LegacyEvent` are skipped — Phase-B
//! (#255) flips the default and adds a check constraint that rejects
//! mismatched ingests.

use crate::contract::consent_lookup::ConsentLookup;
use crate::domain::consent_timeline::ConsentModel;
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, LintRecord, finding, target_record};

/// Run the §6.5 sub-check matrix.
#[must_use]
pub async fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(lookup) = inputs.consent_lookup else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for r in inputs.records {
        if r.consent_model == ConsentModel::LegacyEvent {
            continue;
        }
        out.extend(check_record(lookup, r).await);
    }
    out
}

async fn check_record(lookup: &dyn ConsentLookup, r: &LintRecord) -> Vec<Finding> {
    let consent_ref = &r.stored.record.provenance.consent_ref;
    let created_at = &r.stored.record.provenance.created_at;
    let scope = r.stored.record.scope.as_str();

    // Sub-check 1 — sensor-binding-aware covering-grant resolution.
    // We pass the record's source_sensor *as a SensorLabel* so the
    // resolver can do (sensor, scope, time) at once. If the record's
    // identity isn't sensor-shaped, that's a separate provenance-shape
    // failure and we fall through to a "no covering grant" finding.
    let sensor = match crate::domain::SensorLabel::from_identity(&r.stored.record.provenance.source_sensor) {
        Ok(s) => s,
        Err(_) => {
            return vec![finding_no_grant_with(
                r,
                "provenance.source_sensor is not a sensor identity",
            )];
        }
    };

    let timeline = match lookup.timeline(consent_ref).await {
        Ok(t) => t,
        Err(e) => {
            let mut f = finding(
                Kind::MissingProvenance,
                Severity::Error,
                format!(
                    "consent timeline lookup failed for {consent_ref}: {e}"
                ),
            );
            f.target = Some(target_record(&r.stored.record.id));
            return vec![f];
        }
    };

    let grant = crate::domain::consent_timeline::CoveringGrant::resolve(
        &timeline,
        &sensor,
        scope,
        created_at,
    );

    if grant.is_none() {
        return vec![finding_no_grant_with(
            r,
            &format!(
                "no covering grant in consent_timeline for ref {consent_ref} at {created} \
                 (sensor={sensor}, scope={scope})",
                created = created_at.as_str(),
                sensor = sensor.as_str(),
            ),
        )];
    }

    // Sub-checks 2–5 land in subsequent tasks (Task 9, Task 10).
    Vec::new()
}

fn finding_no_grant_with(r: &LintRecord, message: &str) -> Finding {
    let mut f = finding(Kind::MissingProvenance, Severity::Error, message.to_owned());
    f.target = Some(target_record(&r.stored.record.id));
    f.suggested_fix = Some(
        "ensure ingest writes a `consent_timeline` issued event matching \
         (sensor, scope, time) before the record is stored, or set \
         `records.consent_model='legacy_event'` for legacy ingests".to_owned(),
    );
    f
}
```

- [ ] **Step 5: Make `run_checks` await the new module**

In `crates/cairn-core/src/verbs/lint/mod.rs`, `run_checks` is currently sync. Change to `pub async fn run_checks(...) -> LintData`:

```rust
pub async fn run_checks(inputs: &LintInputs<'_>) -> LintData {
    let mut findings: Vec<Finding> = Vec::new();
    findings.extend(checks::malformed::run(inputs));
    findings.extend(checks::actor_chain::run(inputs));
    findings.extend(checks::provenance::run(inputs));
    findings.extend(checks::schema::run(inputs));
    findings.extend(checks::hot_memory::run(inputs));
    findings.extend(checks::index_drift::run(inputs));
    findings.extend(checks::consent::run(inputs).await);
    let summary = summarize(&findings);
    LintData { findings, summary, report_path: None }
}
```

Update every test in `verbs/lint/mod.rs` and the CLI handler to `.await` the call.

- [ ] **Step 6: Run the new tests + the workspace**

Run: `cargo nextest run -p cairn-core lint::checks::consent`
Expected: 3 tests pass.

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: most tests pass; `verbs::lint::mod::tests` will fail because deferred-info count is now 6 (5 old + 1 new from `consent` matrix). Fix in Task 12.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/verbs/lint/checks/consent.rs \
        crates/cairn-core/src/verbs/lint/checks/mod.rs \
        crates/cairn-core/src/verbs/lint/mod.rs
git commit -m "feat(lint): consent sub-check 1 (covering-grant resolution) (#253)"
```

---

## Task 9: Sub-check 2+3 (sensor binding + scope binding)

**Files:**
- Modify: `crates/cairn-core/src/verbs/lint/checks/consent.rs` (extend `check_record`)

These sub-checks are mostly redundant with sub-check 1 (the resolver already filters by sensor + scope), so the value here is **diagnostic specificity**: when no grant is found, we explicitly distinguish "right sensor, wrong scope" / "right scope, wrong sensor" / "neither" so operators get an actionable finding.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `consent.rs`:

```rust
#[tokio::test]
async fn flags_sensor_mismatch_with_specific_message() {
    use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    let r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
    let other = ConsentTimelineEvent {
        consent_ref: "consent:c1".to_owned(),
        seq: 1,
        kind: ConsentTimelineEventKind::Issued,
        sensor_id: SensorLabel::parse("local:terminal:host:v1").unwrap(),
        scope: r.stored.record.scope.as_str().to_owned(),
        decided_at: Rfc3339Timestamp::parse("2020-01-01T00:00:00Z").unwrap(),
        expires_at: None,
    };
    let lookup = FakeConsentLookup::new().with("consent:c1", vec![other]);
    let cfg = CairnConfig::default();
    let inputs = LintInputs {
        records: std::slice::from_ref(&r),
        config: &cfg,
        index_stats: IndexStats::new(1, 1),
        schema_version: SchemaVersion { major: 0, minor: 1 },
        consent_lookup: Some(&lookup),
    };
    let f = run(&inputs).await;
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("sensor"));
    assert!(f[0].message.contains("mismatch") || f[0].message.contains("does not match"));
}

#[tokio::test]
async fn flags_scope_mismatch_with_specific_message() {
    use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    let r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
    let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor).unwrap();
    let other_scope = ConsentTimelineEvent {
        consent_ref: "consent:c1".to_owned(),
        seq: 1,
        kind: ConsentTimelineEventKind::Issued,
        sensor_id: sensor,
        scope: "team:other".to_owned(),
        decided_at: Rfc3339Timestamp::parse("2020-01-01T00:00:00Z").unwrap(),
        expires_at: None,
    };
    let lookup = FakeConsentLookup::new().with("consent:c1", vec![other_scope]);
    let cfg = CairnConfig::default();
    let inputs = LintInputs {
        records: std::slice::from_ref(&r),
        config: &cfg,
        index_stats: IndexStats::new(1, 1),
        schema_version: SchemaVersion { major: 0, minor: 1 },
        consent_lookup: Some(&lookup),
    };
    let f = run(&inputs).await;
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("scope"));
}
```

- [ ] **Step 2: Extend `check_record` to diagnose `None` from the resolver**

When `CoveringGrant::resolve` returns `None`, walk `timeline` looking for the closest miss and emit a more specific message:

```rust
if grant.is_none() {
    let any_for_sensor = timeline.iter().any(|e| e.sensor_id == sensor);
    let any_for_scope  = timeline.iter().any(|e| e.scope == scope);
    let detail = match (any_for_sensor, any_for_scope) {
        (false, true)  => "sensor mismatch (timeline has no event for this sensor)",
        (true,  false) => "scope mismatch (timeline has no event for this scope)",
        (false, false) => "no events match either sensor or scope",
        (true,  true)  => "no event covers created_at within window (sensor + scope match present)",
    };
    return vec![finding_no_grant_with(
        r,
        &format!(
            "no covering grant for {consent_ref} at {created}: {detail} \
             (record sensor={s}, scope={scope})",
            created = created_at.as_str(),
            s = sensor.as_str(),
        ),
    )];
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo nextest run -p cairn-core lint::checks::consent`
Expected: all 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/lint/checks/consent.rs
git commit -m "feat(lint): consent sub-checks 2+3 (sensor + scope binding) (#253)"
```

---

## Task 10: Sub-check 4+5 (window + state-at-issue)

**Files:**
- Modify: `crates/cairn-core/src/verbs/lint/checks/consent.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn flags_record_written_after_revoke() {
    use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    let mut r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
    r.stored.record.provenance.created_at =
        Rfc3339Timestamp::parse("2026-04-01T00:00:00Z").unwrap();
    let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor).unwrap();
    let issued = ConsentTimelineEvent {
        consent_ref: "consent:c1".to_owned(),
        seq: 1,
        kind: ConsentTimelineEventKind::Issued,
        sensor_id: sensor.clone(),
        scope: r.stored.record.scope.as_str().to_owned(),
        decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
        expires_at: None,
    };
    let revoked = ConsentTimelineEvent {
        seq: 2,
        kind: ConsentTimelineEventKind::Revoked,
        decided_at: Rfc3339Timestamp::parse("2026-03-01T00:00:00Z").unwrap(),
        ..issued.clone()
    };
    let lookup = FakeConsentLookup::new().with("consent:c1", vec![issued, revoked]);
    let cfg = CairnConfig::default();
    let inputs = LintInputs {
        records: std::slice::from_ref(&r),
        config: &cfg,
        index_stats: IndexStats::new(1, 1),
        schema_version: SchemaVersion { major: 0, minor: 1 },
        consent_lookup: Some(&lookup),
    };
    let f = run(&inputs).await;
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("revoke") || f[0].message.contains("revoked"));
}

#[tokio::test]
async fn flags_record_written_before_issue() {
    use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    let mut r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
    r.stored.record.provenance.created_at =
        Rfc3339Timestamp::parse("2025-12-31T00:00:00Z").unwrap();
    let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor).unwrap();
    let issued = ConsentTimelineEvent {
        consent_ref: "consent:c1".to_owned(),
        seq: 1,
        kind: ConsentTimelineEventKind::Issued,
        sensor_id: sensor,
        scope: r.stored.record.scope.as_str().to_owned(),
        decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
        expires_at: None,
    };
    let lookup = FakeConsentLookup::new().with("consent:c1", vec![issued]);
    let cfg = CairnConfig::default();
    let inputs = LintInputs {
        records: std::slice::from_ref(&r),
        config: &cfg,
        index_stats: IndexStats::new(1, 1),
        schema_version: SchemaVersion { major: 0, minor: 1 },
        consent_lookup: Some(&lookup),
    };
    let f = run(&inputs).await;
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("before") || f[0].message.contains("not yet issued"));
}

#[tokio::test]
async fn passes_record_strictly_inside_window() {
    use crate::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    let mut r = lint_record_with("consent:c1", ConsentModel::ReceiptTimeline);
    r.stored.record.provenance.created_at =
        Rfc3339Timestamp::parse("2026-06-01T00:00:00Z").unwrap();
    let sensor = SensorLabel::from_identity(&r.stored.record.provenance.source_sensor).unwrap();
    let issued = ConsentTimelineEvent {
        consent_ref: "consent:c1".to_owned(),
        seq: 1,
        kind: ConsentTimelineEventKind::Issued,
        sensor_id: sensor,
        scope: r.stored.record.scope.as_str().to_owned(),
        decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
        expires_at: Some(Rfc3339Timestamp::parse("2026-12-31T00:00:00Z").unwrap()),
    };
    let lookup = FakeConsentLookup::new().with("consent:c1", vec![issued]);
    let cfg = CairnConfig::default();
    let inputs = LintInputs {
        records: std::slice::from_ref(&r),
        config: &cfg,
        index_stats: IndexStats::new(1, 1),
        schema_version: SchemaVersion { major: 0, minor: 1 },
        consent_lookup: Some(&lookup),
    };
    assert!(run(&inputs).await.is_empty());
}
```

- [ ] **Step 2: Extend `check_record` with the window/state path**

When `CoveringGrant::resolve` returns `None` despite sensor+scope match, walk the timeline a second time to surface a precise reason:

```rust
// (true, true) branch from Task 9 — drill in:
let any_revoke_after_issue = timeline.iter().any(|e| {
    e.kind == ConsentTimelineEventKind::Revoked
        && e.sensor_id == sensor
        && e.scope == scope
        && e.decided_at <= *created_at
});
let any_issue_before_t = timeline.iter().any(|e| {
    e.kind == ConsentTimelineEventKind::Issued
        && e.sensor_id == sensor
        && e.scope == scope
        && e.decided_at <= *created_at
});

let detail = if any_revoke_after_issue {
    "consent was revoked before record was written"
} else if !any_issue_before_t {
    "record written before any issue event for this consent_ref (state-at-issue mismatch)"
} else {
    "record written after issued grant expired (window mismatch)"
};
```

Replace the `(true, true)` arm of the `match` from Task 9 with this drill-in.

- [ ] **Step 3: Run the tests**

Run: `cargo nextest run -p cairn-core lint::checks::consent`
Expected: all 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/lint/checks/consent.rs
git commit -m "feat(lint): consent sub-checks 4+5 (window + state-at-issue) (#253)"
```

---

## Task 11: Wire `ConsentLookup` + per-row `consent_model` into the CLI lint handler

**Files:**
- Modify: `crates/cairn-cli/src/verbs/lint.rs:369-440` (the section that builds `lint_records`)

- [ ] **Step 1: Read the current handler**

Read `crates/cairn-cli/src/verbs/lint.rs` lines 360-460 to confirm the structure (the `LintInputs { ... }` literal and the `LegacyEvent` map).

- [ ] **Step 2: Update the handler**

Replace the hard-coded `LegacyEvent` block with:

```rust
// Issue #253: per-row consent_model gate.
let consent_models = store
    .list_consent_models()
    .await
    .map_err(|e| anyhow::anyhow!("store: list_consent_models: {e}"))
    .context("lint: list_consent_models")?;

let lint_records: Vec<LintRecord> = stored
    .into_iter()
    .map(|s| {
        let model = consent_models
            .get(&s.record.id)
            .copied()
            .unwrap_or(ConsentModel::LegacyEvent);
        LintRecord { stored: s, consent_model: model }
    })
    .collect();

let inputs = LintInputs {
    records: &lint_records,
    config,
    index_stats,
    schema_version,
    consent_lookup: Some(store.as_ref() as &dyn ConsentLookup),
};
let mut data = run_checks(&inputs).await;
```

> The `store` binding here is whatever the bootstrap path yields (`Arc<dyn MemoryStore>` or a concrete `Arc<SqliteStore>`). If it's a trait object that doesn't expose `ConsentLookup`, the cleanest fix is to construct it as `Arc<SqliteStore>` in the bootstrap layer and pass two refs into the handler — confirm by reading the bootstrap path in `cairn-cli/src/main.rs` or `cairn-cli/src/bootstrap.rs`. If the bootstrap returns `Arc<dyn MemoryStore>`, add a `pub fn consent_lookup(&self) -> &dyn ConsentLookup` accessor on the trait or downcast at the boundary.

Add `use cairn_core::contract::consent_lookup::ConsentLookup;` and `use cairn_core::domain::consent_timeline::ConsentModel;` at the top of `lint.rs`.

- [ ] **Step 3: Run the CLI integration tests**

Run: `cargo nextest run -p cairn-cli`
Expected: PR-1 snapshot tests likely break — defect-matrix vault now sees zero §6.5 deferred-info findings (since the deferred check is gone and the real check sees no `ReceiptTimeline` rows). Snapshots refresh in Task 13.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/verbs/lint.rs
git commit -m "feat(cli): wire ConsentLookup + per-row consent_model into lint (#253)"
```

---

## Task 12: Delete the `consent_deferred` stub + drop deferred count 5→4

**Files:**
- Delete: `crates/cairn-core/src/verbs/lint/checks/consent_deferred.rs`
- Modify: `crates/cairn-core/src/verbs/lint/checks/mod.rs:1-15`
- Modify: `crates/cairn-core/src/verbs/lint/mod.rs:200-300` (test count assertions)

- [ ] **Step 1: Remove the module declaration**

In `crates/cairn-core/src/verbs/lint/checks/mod.rs`, delete `pub mod consent_deferred;`. Run `cargo check -p cairn-core` and confirm nothing else references it (`grep -rn consent_deferred crates/`).

- [ ] **Step 2: Delete the file**

```bash
git rm crates/cairn-core/src/verbs/lint/checks/consent_deferred.rs
```

- [ ] **Step 3: Update `verbs/lint/mod.rs::tests`**

Find both occurrences of `count == 5` in the file and change to `4`. Update the matching `info` count (`by_severity.info` and `summary.total` assertions) to drop by 1. Update the comments that enumerate `§6.2/#256 + §6.3/#257 + §6.4/#258 + §6.5/#253 + §6.6/#259` to omit `§6.5/#253`:

```rust
// actor_chain (#256), provenance (#257), schema (#258), and hot_memory (#259)
// each emit one deferred-check info finding; consent (#253) is now live.
assert_eq!(
    data.findings
        .iter()
        .filter(|f| matches!(f.kind, Kind::DeferredCheck))
        .count(),
    4
);
assert_eq!(data.summary.by_severity.info, 4);
```

Same change in both `run_checks_on_empty_inputs_returns_no_findings_yet` and `run_checks_with_one_record_aggregates_summary_correctly`.

- [ ] **Step 4: Run lint tests**

Run: `cargo nextest run -p cairn-core verbs::lint`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verbs/lint/checks/mod.rs \
        crates/cairn-core/src/verbs/lint/mod.rs
git commit -m "refactor(lint): remove consent_deferred stub; drop deferred 5→4 (#253)"
```

---

## Task 13: Refresh CLI lint defect-matrix integration test + snapshots

**Files:**
- Modify: `crates/cairn-cli/tests/lint.rs` (or wherever the defect-matrix test lives — locate via `grep -rn "defect-matrix\|defect_matrix" crates/cairn-cli/tests/`)
- Modify: `crates/cairn-cli/tests/snapshots/*.snap` (regenerate)

- [ ] **Step 1: Locate the defect-matrix test**

Run: `grep -rn "DeferredCheck\|deferred_check\|consent" crates/cairn-cli/tests/ | head`. Read the file the matches point to.

- [ ] **Step 2: Add a §6.5 seeded defect**

Inside the defect-matrix vault setup, seed: a single record with `consent_model='receipt_timeline'`, a `provenance.consent_ref` pointing at a `consent_timeline` row that has the wrong sensor:

```rust
// Seed a §6.5 violation: receipt_timeline record whose covering grant
// has the wrong sensor_id.
sqlx::query(
    "INSERT INTO consent_timeline (consent_ref, seq, kind, sensor_id, scope, \
     decided_at, expires_at, payload_json) \
     VALUES ('consent:bad', 1, 'issued', 'local:terminal:host:v1', 'private', \
             1735689600, NULL, '{}')",
).execute(conn).await.expect("seed");
// Then: ingest a record whose provenance.source_sensor is `local:screen:host:v1`,
// provenance.consent_ref = 'consent:bad', records.consent_model = 'receipt_timeline'.
// (Use the same in-test ingest helper PR-1 used for the other defects;
// add a `consent_model` parameter or a follow-up UPDATE on records.consent_model.)
```

- [ ] **Step 3: Update assertion counts**

The defect matrix had 6 implemented defects + 1 §6.5 deferred-info = 7 findings. After #253: 7 implemented defects + 0 deferred-info for §6.5 = **7 findings, none of which are `Kind::DeferredCheck` for §6.5**. Update the count assertions accordingly:

```rust
assert_eq!(findings.len(), 7);
assert!(
    findings.iter().any(|f|
        f.kind == Kind::MissingProvenance
        && f.message.contains("no covering grant")
    ),
    "expected §6.5 sensor-mismatch finding"
);
```

- [ ] **Step 4: Refresh snapshots**

Run: `cargo nextest run -p cairn-cli` (snapshot tests will fail).
Run: `cargo insta review` and accept changes that reflect the new §6.5 finding + dropped deferred-info.

- [ ] **Step 5: Run the full CLI test suite**

Run: `cargo nextest run -p cairn-cli`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/tests/ \
        crates/cairn-cli/tests/snapshots/
git commit -m "test(lint): defect-matrix gains §6.5 receipt-timeline violation (#253)"
```

---

## Task 14: Property test — `CoveringGrant::resolve` is order-independent

**Files:**
- Modify: `crates/cairn-core/src/domain/consent_timeline.rs` (add proptest)
- Modify: `crates/cairn-core/Cargo.toml` (proptest already a dev-dep — confirm with `grep proptest crates/cairn-core/Cargo.toml`)

- [ ] **Step 1: Add the proptest**

```rust
#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    use proptest::prelude::*;

    fn arb_event() -> impl Strategy<Value = ConsentTimelineEvent> {
        (
            1u64..=100,
            prop::sample::select(vec![
                ConsentTimelineEventKind::Issued,
                ConsentTimelineEventKind::Revoked,
                ConsentTimelineEventKind::Expired,
            ]),
            1_700_000_000i64..1_800_000_000,
            prop::option::of(1_800_000_001i64..1_900_000_000),
        )
            .prop_map(|(seq, kind, decided, expires)| ConsentTimelineEvent {
                consent_ref: "c:1".to_owned(),
                seq,
                kind,
                sensor_id: SensorLabel::parse("local:s:h:v1").unwrap(),
                scope: "private".to_owned(),
                decided_at: Rfc3339Timestamp::from_unix_secs(decided).unwrap(),
                expires_at: expires.map(|s| Rfc3339Timestamp::from_unix_secs(s).unwrap()),
            })
    }

    proptest! {
        #[test]
        fn resolve_is_order_independent(events in prop::collection::vec(arb_event(), 0..20)) {
            let sensor = SensorLabel::parse("local:s:h:v1").unwrap();
            let at = Rfc3339Timestamp::from_unix_secs(1_750_000_000).unwrap();

            let forward = CoveringGrant::resolve(&events, &sensor, "private", &at);
            let mut reversed = events.clone();
            reversed.reverse();
            let backward = CoveringGrant::resolve(&reversed, &sensor, "private", &at);

            prop_assert_eq!(forward, backward);
        }
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-core consent_timeline::prop_tests`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/domain/consent_timeline.rs
git commit -m "test(consent): property test — CoveringGrant::resolve is order-independent (#253)"
```

---

## Task 15: Update traceability + verification

**Files:**
- Modify: `docs/design/traceability.md` (locate the §6.5 row)

- [ ] **Step 1: Update traceability**

Find the row mapping spec §6.5 (and brief §14 sub-section) currently pointing at issue #253 as deferred. Change status from "deferred" / "open" to "closed by PR-N" with the PR number once the PR is opened. For now, drop a one-line note:

```markdown
| §14 / spec §6.5 | sensor-consent enforcement | `cairn-core::verbs::lint::checks::consent`, `consent_timeline` table, `ConsentLookup` trait | #253 (this PR) |
```

- [ ] **Step 2: Run the full verification checklist**

Per CLAUDE.md §8:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Each must pass before the PR is opened. If `cairn-codegen --check` reports diff: this plan does NOT touch IDL, so any diff is unrelated drift — investigate before pushing. If `cairn-docgen --check` reports diff: regenerate (`cargo run -p cairn-cli --bin cairn-docgen -- --write`) and commit.

- [ ] **Step 3: Open PR**

```bash
git push -u origin feat/issue-253-consent-receipt-timeline
gh pr create --title "feat(consent): receipt timeline + per-record consent_model gate (#253)" \
  --body "$(cat <<'EOF'
## Summary
- Adds `consent_timeline` append-only table + `records.consent_model` Phase-A column.
- Introduces `ConsentLookup` contract trait (`cairn-core`) with SQLite impl.
- Replaces §6.5 lint deferred-info stub with full sub-check matrix: covering-grant resolution + sensor binding + scope binding + window + state-at-issue.
- Lint deferred-info count drops 5 → 4.

## Brief / spec sources
- Brief §14 (privacy / consent), §6.5 (provenance / `consent_ref`), §4 (contracts).
- Spec: `docs/superpowers/specs/2026-04-30-lint-checks-design.md` §6.5 + §11.

## Invariants touched
- §4 (contracts): adds `ConsentLookup` — additive, no signature changes to existing contracts.
- §5.6 (WAL): no WAL changes; Phase-A column has a backward-compat default.
- §14 (privacy): widens consent surface from event journal to receipt timeline; `consent_model` per-row gate keeps backward compat until #255 flips the Phase-B default.
- §10 (sources immutable): consent_timeline rows are append-only via triggers.

## Phase-B follow-ups
- #255 — flip `records.consent_model` default to `receipt_timeline`; add CHECK constraint that rejects mismatched ingests.
- Ingest paths that emit `consent_timeline` `issued` / `expired` / `revoked` events land separately (gated by sensor enablement; see §14 amendment in #255).

## Verification
$(paste verification output)

Closes #253.
EOF
)"
```

- [ ] **Step 4: Final commit (traceability + any verification fixups)**

```bash
git add docs/design/traceability.md
git commit -m "docs(traceability): map §6.5 sensor-consent → #253"
```

---

## Self-review notes

Cross-checked against the spec §6.5 + §11 expectations:

- ✅ `consent_timeline` table keyed by `(consent_ref, seq)` — Task 2.
- ✅ `ConsentLookup` trait with `timeline()` + `covering_grant()` — Task 4.
- ✅ Per-row `records.consent_model` column — Task 1.
- ✅ Sub-check matrix: sensor binding (Task 9), scope binding (Task 9), issuance/expiry/revoke window (Task 10), state-at-issue (Task 10), covering-grant resolution (Task 8).
- ✅ Lint mod.rs deferred-info count drops 5→4 — Task 12.
- ✅ §6.5 deferred-info stub removed — Task 12.
- ✅ Tests: rstest-style table + proptest order-independence + integration defect-matrix + snapshot refresh.
- ❌ Phase-B default flip + ingest writers — explicitly out of scope; #255.
- ❌ Brief §14 amendment — depends on #255 wording; defer.

Type / signature consistency: `LintInputs` extension is the only call-site break; every constructor is updated in Task 8 step 2 via grep+edit. `MemoryStore::list_consent_models` has a default empty impl so non-SQLite adapters (e.g., `FixtureStore`) compile cleanly.
