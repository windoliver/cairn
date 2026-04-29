-- Migration 0015: backfill `activated_at`/`activated_by` for rows
-- written before migration 0012 added those columns.
--
-- `version_history` now emits an `Update` event only when both audit
-- columns are populated. Without a backfill, every row created before
-- this branch's 0012 migration ran would silently lose its activation
-- event after upgrade.
--
-- For currently-active rows, the best signal we have is `created_at` /
-- `created_by` (the trusted writer; round-8 of the prior loop tied
-- those columns to the WAL executor). Using stage-time as a proxy for
-- activation-time is a known degradation — the contract guarantees an
-- `Update` event for every activated version, so a slightly imprecise
-- timestamp is preferable to dropping the event entirely. New writes
-- after 0012 use the real activation timestamp via
-- `activate_version`.
UPDATE records
SET activated_at = created_at,
    activated_by = created_by
WHERE active = 1
  AND activated_at IS NULL;
