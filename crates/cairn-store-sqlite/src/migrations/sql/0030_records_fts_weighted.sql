-- Migration 0030: replace single-column body-only `records_fts` with a
-- 4-column weighted FTS5 index over `(kind, class, scope, body)`.
--
-- Brief sources: §5.1 (keyword search), §3 (records-in-SQLite).
-- Plan ref: docs/superpowers/plans/2026-04-30-hybrid-rerank-brainbench.md §Task 4.
--
-- Columns (in order, matching bm25() positional weight args):
--   1. kind   — high weight; entity-type queries land here
--   2. class  — high weight; intent/category queries
--   3. scope  — medium weight; user/agent/project_root context tuple
--   4. body   — base weight; rich-prose match
--
-- The default query-time weights are [10.0, 10.0, 5.0, 1.0]. They are
-- configurable via SearchConfig::fts_column_weights and supplied as bm25()
-- positional arguments by the store at search time (Task 5).
--
-- Migration 0001 created `records_fts` as an external-content FTS5 table
-- (`content='records'`). We drop and recreate as an internally-stored FTS5
-- table because the new column set is not a subset of the records columns
-- in a way external content can index without per-row triggers — and the
-- triggers below already keep the shadow in lockstep regardless. Triggers
-- and shadow rows are dropped first, recreated next, then the source
-- records are backfilled.

DROP TRIGGER IF EXISTS records_fts_au;
DROP TRIGGER IF EXISTS records_fts_ad;
DROP TRIGGER IF EXISTS records_fts_ai;
DROP TABLE IF EXISTS records_fts;

CREATE VIRTUAL TABLE records_fts USING fts5(
    kind,
    class,
    scope,
    body,
    tokenize='porter unicode61'
);

-- Backfill: rebuild FTS rows from existing records (excluding tombstoned).
INSERT INTO records_fts(rowid, kind, class, scope, body)
SELECT
    rowid,
    kind,
    class,
    scope,
    body
FROM records
WHERE tombstoned = 0;

-- Trigger note: migration 0001 used the FTS5 'delete' command-row form
-- (`INSERT INTO records_fts(records_fts, rowid, ...) VALUES ('delete', ...)`)
-- because the original `records_fts` was external-content (`content='records'`).
-- This migration recreates `records_fts` as an internally-stored FTS5 table,
-- so the canonical resync form is `DELETE FROM records_fts WHERE rowid = ?`
-- followed by an INSERT. The 'delete' command-row form raises a generic SQL
-- logic error against an internally-stored table.

-- Insert trigger.
CREATE TRIGGER records_fts_ai AFTER INSERT ON records BEGIN
  INSERT INTO records_fts(rowid, kind, class, scope, body)
  VALUES (new.rowid, new.kind, new.class, new.scope, new.body);
END;

-- Delete trigger.
CREATE TRIGGER records_fts_ad AFTER DELETE ON records BEGIN
  DELETE FROM records_fts WHERE rowid = old.rowid;
END;

-- Update trigger: re-syncs all four indexed columns so structural field
-- changes (e.g. kind/class/scope reclassification) propagate to the
-- shadow, not just body edits.
CREATE TRIGGER records_fts_au AFTER UPDATE ON records BEGIN
  DELETE FROM records_fts WHERE rowid = old.rowid;
  INSERT INTO records_fts(rowid, kind, class, scope, body)
  VALUES (new.rowid, new.kind, new.class, new.scope, new.body);
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (30, '0030_records_fts_weighted', '', strftime('%s','now') * 1000);
