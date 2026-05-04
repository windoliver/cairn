-- Migration 0035: tighten consent_timeline lifecycle invariants.
-- Round 9 adversarial-review (Issue #253, brief §14).
--
-- 0025 left two soft spots:
--
--   1. The decided_at non-decreasing trigger compared raw TEXT, but
--      Rfc3339Timestamp accepts offset forms ("...+01:00"). Lexical
--      order != chronological order across offsets, so a writer could
--      bypass the non-retroactivity guard by appending an offset-form
--      timestamp that sorts later than an existing UTC row but is
--      chronologically earlier (e.g. existing "2026-01-01T00:00:00Z"
--      is < new "2026-01-01T00:30:00+01:00" lexically, even though the
--      new one is 30 minutes earlier in UTC). That reopens the
--      retroactive-authorization hole 0025 was meant to close.
--
--   2. The first-event-issued + seq-strictly-monotonic triggers only
--      fire when (a) NEW.seq = 1, or (b) a row already exists. So a
--      fresh consent_ref could be opened at seq=99 with kind='revoked'
--      and neither trigger would catch it -- malformed timelines were
--      indistinguishable from legitimate ones.
--
-- This migration installs three additional BEFORE-INSERT triggers:
--
--   * consent_timeline_decided_at_utc_only: decided_at must end in 'Z'.
--     Together with the writer normalizing timestamps to UTC, this
--     turns lexical comparison into chronological comparison and
--     restores 0025's monotonicity guarantee.
--   * consent_timeline_expires_at_utc_only: same rule for expires_at.
--   * consent_timeline_fresh_must_start_at_seq_1: a brand-new
--     consent_ref must enter the table with seq=1, which forces
--     first_event_issued to evaluate kind on the first row.

CREATE TRIGGER consent_timeline_decided_at_utc_only
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN substr(NEW.decided_at, length(NEW.decided_at)) <> 'Z'
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: decided_at must be RFC3339 UTC (Z suffix); \
     writer must normalize before persist');
END;

CREATE TRIGGER consent_timeline_expires_at_utc_only
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN NEW.expires_at IS NOT NULL
   AND substr(NEW.expires_at, length(NEW.expires_at)) <> 'Z'
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: expires_at must be RFC3339 UTC (Z suffix) when set');
END;

CREATE TRIGGER consent_timeline_fresh_must_start_at_seq_1
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN NOT EXISTS (SELECT 1 FROM consent_timeline WHERE consent_ref = NEW.consent_ref)
   AND NEW.seq <> 1
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: first event for a consent_ref must use seq=1');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (35, '0035_consent_timeline_lifecycle_tighten', '',
          strftime('%s','now') * 1000);
