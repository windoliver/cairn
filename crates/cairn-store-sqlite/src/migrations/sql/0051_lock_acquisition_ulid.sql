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

-- Replace the shared `__pre_v3__` sentinel with a per-row unique value
-- so each pre-v3 holder is distinctly addressable. randomblob(13) gives
-- 26 hex chars after lower(hex(...)) — comparable in length and
-- uniqueness to a real ULID. The `__pre_v3_` prefix preserves the
-- "this is a pre-upgrade row" semantic for log filters and ad-hoc
-- diagnostics, while making each value unique.
--
-- These rows are then GC'd on the next Store::open by init_incarnation
-- (their owner_incarnation is the prior `__pre_v2__` sentinel from
-- migration 0050, which never matches the freshly-minted ULID), so
-- this distinct-backfill is belt+suspenders against the unlikely case
-- where init_incarnation hasn't yet run.
UPDATE lock_holders
   SET acquisition_ulid = '__pre_v3_' || lower(hex(randomblob(13)))
 WHERE acquisition_ulid = '__pre_v3__';

CREATE INDEX lock_holders_acquisition_ulid_idx
  ON lock_holders(acquisition_ulid);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (51, '0051_lock_acquisition_ulid', '', strftime('%s','now') * 1000);
