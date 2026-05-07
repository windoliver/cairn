-- Migration 0051: per-acquisition unique identifier for lock_holders.
--
-- ROWID is reused by SQLite after DELETE when the deleted row was the highest
-- (or the only) row in the table — so a stale handle whose row was GC'd and
-- replaced by a fresh acquire could end up with the same ROWID, defeating
-- the per-acquisition fence identity. This migration adds an explicit
-- `acquisition_ulid` column populated by `acquire()` with a fresh ULID
-- each time it inserts a holder. The Rust ULID generator is monotonic
-- per process and crypto-random across processes, so two acquisitions
-- never share an acquisition_ulid even on the same millisecond.
--
-- Backfills any existing rows with the sentinel '__pre_v3__' ulid; the
-- next acquire() call against that resource will GC the row and reinsert
-- with a real ULID. Release/with_fencing predicates explicitly reject
-- the sentinel so a pre-v3 row can never satisfy a fence check.

ALTER TABLE lock_holders
  ADD COLUMN acquisition_ulid TEXT NOT NULL DEFAULT '__pre_v3__';

CREATE INDEX lock_holders_acquisition_ulid_idx
  ON lock_holders(acquisition_ulid);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (51, '0051_lock_acquisition_ulid', '', strftime('%s','now') * 1000);
