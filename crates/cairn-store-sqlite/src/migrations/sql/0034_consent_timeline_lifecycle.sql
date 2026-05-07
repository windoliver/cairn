-- Migration 0034: lifecycle invariants on consent_timeline.
-- Round 8 adversarial-review (Issue #253, brief §14).
--
-- Rounds 2 (immutability per consent_ref) and 7 (replay by seq, not
-- decided_at) closed two attack surfaces but left the door open for
-- retroactive authorization:
--
--   - A writer could append a fresh `issued` event with a backdated
--     `decided_at` and silently make previously unauthorized records
--     appear covered on the next lint run, because the resolver's
--     time-window filter (`decided_at <= record.created_at`) admits
--     any event whose decided_at is in the past.
--   - The schema did not require `seq` to be strictly contiguous or
--     the first event to be `issued`, so a malformed timeline could
--     lead with a `revoked` row or skip seq numbers.
--
-- This migration installs three BEFORE-INSERT triggers per consent_ref:
--
-- 1. `consent_timeline_first_event_issued`: the first event for any
--    consent_ref must be `kind='issued'`.
-- 2. `consent_timeline_seq_strictly_monotonic`: each new event must
--    have seq = MAX(existing seq) + 1.
-- 3. `consent_timeline_decided_at_non_decreasing`: each new event's
--    `decided_at` must be >= MAX(existing decided_at). This is the
--    load-bearing one for §6.5 -- without it, retroactive issued
--    events can fool the resolver.
--
-- Together these constrain the timeline to a forward-only,
-- non-retroactive lifecycle, which is what the resolver implicitly
-- assumes.

CREATE TRIGGER consent_timeline_first_event_issued
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN NEW.seq = 1 AND NEW.kind <> 'issued'
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: first event for a consent_ref must be issued');
END;

CREATE TRIGGER consent_timeline_seq_strictly_monotonic
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN EXISTS (SELECT 1 FROM consent_timeline WHERE consent_ref = NEW.consent_ref)
   AND NEW.seq <> (SELECT MAX(seq) + 1 FROM consent_timeline
                    WHERE consent_ref = NEW.consent_ref)
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: seq must be MAX(seq)+1 within a consent_ref');
END;

CREATE TRIGGER consent_timeline_decided_at_non_decreasing
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1 FROM consent_timeline
     WHERE consent_ref = NEW.consent_ref
       AND decided_at > NEW.decided_at
  )
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: decided_at must be non-decreasing per consent_ref \
     (no retroactive backfills)');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (34, '0034_consent_timeline_lifecycle', '',
          strftime('%s','now') * 1000);
