-- Migration 0036: pin consent_timeline timestamps to canonical UTC seconds form.
-- Round 10+1 adversarial-review (Issue #253, brief §14).
--
-- 0026 required the trailing 'Z' to make TEXT comparison match
-- chronological order. That assumption breaks for fractional UTC forms
-- accepted by Rfc3339Timestamp (e.g. "2026-01-01T00:00:00.100Z"):
-- lexically, "00.100Z" sorts BEFORE "00Z" even though it is later in
-- time. So an exact-second event can still be appended after a later
-- fractional event and pass the 0025 monotonicity trigger,
-- reintroducing the retroactive-authorization hole.
--
-- Fix: require timestamps in this column to be the canonical 20-char
-- RFC3339 UTC seconds form (YYYY-MM-DDTHH:MM:SSZ). With fixed width
-- and no fractional component, lexical TEXT order equals chronological
-- order, and 0025's `decided_at > NEW.decided_at` comparison holds.

CREATE TRIGGER consent_timeline_decided_at_canonical_seconds
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN length(NEW.decided_at) <> 20
    OR substr(NEW.decided_at, 20, 1) <> 'Z'
    OR instr(NEW.decided_at, '.') > 0
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: decided_at must be canonical RFC3339 UTC seconds form \
     (YYYY-MM-DDTHH:MM:SSZ; no fractional seconds, no offset)');
END;

CREATE TRIGGER consent_timeline_expires_at_canonical_seconds
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN NEW.expires_at IS NOT NULL
   AND (length(NEW.expires_at) <> 20
        OR substr(NEW.expires_at, 20, 1) <> 'Z'
        OR instr(NEW.expires_at, '.') > 0)
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: expires_at must be canonical RFC3339 UTC seconds form when set');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (36, '0036_consent_timeline_canonical_utc', '',
          strftime('%s','now') * 1000);
