-- Migration 0015: backfill `activated_at`/`activated_by` for rows
-- written before migration 0012 added those columns.
--
-- `version_history` now emits an `Update` event only when both audit
-- columns are populated. Without a backfill, every pre-0012 version
-- that was actually activated would silently lose its activation
-- event after upgrade.
--
-- Backfill scope: only versions we can *prove* were activated. In the
-- pre-0012 schema, that is:
--   1. Currently active rows (`active = 1`).
--   2. Superseded rows — versions whose target_id has a higher version
--      number recorded. Supersession is gated by `activate_version`,
--      so a version cannot be superseded without first being
--      activated.
--
-- Versions that are the latest (no higher version exists for their
-- target_id) AND are currently inactive (`active = 0`) are
-- ambiguous: they may have been staged-but-never-activated, or they
-- may have been activated and then tombstoned/expired in a way that
-- cleared `active`. We leave their audit columns NULL — the contract
-- already permits a missing `Update` event for versions that were
-- never activated, and falsely synthesizing one for genuine drafts is
-- audit corruption.
--
-- The signal used for the backfilled timestamp is `created_at` /
-- `created_by` (the trusted writer; round-8 of the prior loop tied
-- those columns to the WAL executor). Stage-time is a known
-- approximation of activation-time, but for proven-activated rows
-- the imprecision is preferable to dropping the event.
--
-- New writes after 0012 use the real activation timestamp via
-- `activate_version`.
UPDATE records AS r
SET activated_at = r.created_at,
    activated_by = r.created_by
WHERE r.activated_at IS NULL
  AND (
    r.active = 1
    OR EXISTS (
      SELECT 1
      FROM records AS r2
      WHERE r2.target_id = r.target_id
        AND r2.version > r.version
    )
  );
