-- Migration 0050: epoch fencing + daemon-incarnation columns (brief §5.6).
--
-- Additive only. Backfills:
--   - locks.epoch          DEFAULT 0     (lowest epoch; first reclaim bumps to 1)
--   - lock_holders.acquired_epoch    DEFAULT 0
--   - lock_holders.owner_incarnation DEFAULT '__pre_v2__'  (sentinel; init_incarnation deletes
--                                                            on first open after upgrade)
-- View `reader_fence_pending_count` lets `wait_for_drain` poll without scanning the table.

ALTER TABLE locks
  ADD COLUMN epoch INTEGER NOT NULL DEFAULT 0;

ALTER TABLE lock_holders
  ADD COLUMN acquired_epoch INTEGER NOT NULL DEFAULT 0;

ALTER TABLE lock_holders
  ADD COLUMN owner_incarnation TEXT NOT NULL DEFAULT '__pre_v2__';

CREATE VIEW reader_fence_pending_count AS
  SELECT resource, COUNT(*) AS pending
    FROM reader_fence
   WHERE state = 'PENDING'
   GROUP BY resource;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (50, '0050_locks_v2', '', strftime('%s','now') * 1000);
