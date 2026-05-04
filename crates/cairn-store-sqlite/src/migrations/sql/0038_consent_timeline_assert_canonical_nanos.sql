-- Migration 0038: assert every consent_timeline row is in the canonical
-- 30-char nanoseconds form. Issue #253, brief §14, Round 10+5.
--
-- Round 10+4 attempted an in-place rewrite of pre-0028 ...SSZ rows by
-- blindly appending '.000000000'. The reviewer correctly flagged that
-- as a silent authorization false-positive: 0027 had previously
-- *truncated* to whole-second form, so an issued grant whose original
-- decided_at was 12:00:00.900Z became 12:00:00Z; promoting that to
-- 12:00:00.000000000Z makes the resolver treat records written at
-- 12:00:00.100Z as consent-covered, even though they actually
-- predated the grant. The lost fractional precision is unrecoverable
-- from the table alone.
--
-- Likewise, vaults populated under 0023..0025 (before 0026 added the
-- UTC-only guard) may carry legitimate offset-form rows like
-- '2026-01-01T00:00:00+01:00' that SQL can't safely re-canonicalize
-- without an RFC3339 parser.
--
-- This migration therefore takes the conservative path: refuse to
-- open vaults whose `consent_timeline` carries any non-canonical row,
-- and route the operator to a deliberate rebuild. Fresh installs are
-- no-ops (the table is empty). Vaults populated under earlier #253
-- revisions need:
--
--   * Drop and re-create from scratch, OR
--   * Rebuild .cairn/ via a controlled pipeline that re-derives
--     consent_timeline rows with full nanosecond precision and the
--     required UTC normalization.
--
-- Silently rewriting truncated rows would convert a real upgrade
-- hazard into a silent authorization bug; we prefer the loud failure.

CREATE TEMP TABLE _migration_0038_check (
  ok INTEGER NOT NULL CHECK (ok = 1)
);

INSERT INTO _migration_0038_check (ok)
  SELECT CASE
    WHEN EXISTS (
      SELECT 1 FROM consent_timeline
       WHERE length(decided_at) <> 30
          OR substr(decided_at, 30, 1) <> 'Z'
          OR substr(decided_at, 20, 1) <> '.'
          OR (
            expires_at IS NOT NULL
            AND (
              length(expires_at) <> 30
              OR substr(expires_at, 30, 1) <> 'Z'
              OR substr(expires_at, 20, 1) <> '.'
            )
          )
    )
    THEN 0
    ELSE 1
  END;

DROP TABLE _migration_0038_check;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (38, '0038_consent_timeline_assert_canonical_nanos', '',
          strftime('%s','now') * 1000);
