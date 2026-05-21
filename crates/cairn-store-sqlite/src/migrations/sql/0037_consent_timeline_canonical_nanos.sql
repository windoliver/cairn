-- Migration 0037: store consent_timeline timestamps with nanosecond precision.
-- Round 10+2 adversarial-review (Issue #253, brief §14).
--
-- 0027 forced timestamps to a 20-char seconds-only form so lexical
-- TEXT compare matched chronological compare. That over-corrected:
-- `records.provenance.created_at` is a full RFC3339 timestamp with
-- fractional-second support, so an issued event at 12:00:00.900Z and
-- a record at 12:00:00.100Z would both collapse to 12:00:00Z and
-- `CoveringGrant::resolve` would treat the grant as in force even
-- though the record actually predates consent. That is an
-- authorization bug, not just precision loss.
--
-- Fix: persist consent timestamps in the canonical 30-char
-- nanosecond UTC form (YYYY-MM-DDTHH:MM:SS.NNNNNNNNNZ). With fixed
-- width and full ns resolution, lexical TEXT order remains
-- chronological AND the per-event compare is at least as fine-grained
-- as `records.created_at` (which never exceeds nanosecond precision
-- under chrono). Writers must normalize timestamps to that form before
-- persistence -- the trigger refuses anything else.
--
-- This supersedes 0027's truncated-seconds triggers (dropped here to
-- avoid both rules firing).

DROP TRIGGER IF EXISTS consent_timeline_decided_at_canonical_seconds;
DROP TRIGGER IF EXISTS consent_timeline_expires_at_canonical_seconds;

CREATE TRIGGER consent_timeline_decided_at_canonical_nanos
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN length(NEW.decided_at) <> 30
    OR substr(NEW.decided_at, 30, 1) <> 'Z'
    OR substr(NEW.decided_at, 20, 1) <> '.'
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: decided_at must be canonical RFC3339 UTC \
     nanoseconds form (YYYY-MM-DDTHH:MM:SS.NNNNNNNNNZ, 30 chars)');
END;

CREATE TRIGGER consent_timeline_expires_at_canonical_nanos
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN NEW.expires_at IS NOT NULL
   AND (length(NEW.expires_at) <> 30
        OR substr(NEW.expires_at, 30, 1) <> 'Z'
        OR substr(NEW.expires_at, 20, 1) <> '.')
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: expires_at must be canonical RFC3339 UTC nanoseconds form when set');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (37, '0037_consent_timeline_canonical_nanos', '',
          strftime('%s','now') * 1000);
