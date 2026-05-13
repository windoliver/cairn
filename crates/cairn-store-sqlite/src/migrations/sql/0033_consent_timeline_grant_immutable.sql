-- Migration 0033: enforce per-consent_ref grant immutability.
-- Round 2 adversarial-review (Issue #253, brief §14).
--
-- Without this trigger, the only column-level constraint on
-- consent_timeline was (consent_ref, seq) uniqueness. A writer could
-- therefore append an `issued` row under an existing consent_ref but
-- with a different (sensor_id, scope) — silently widening the grant
-- across sensors or scopes. The lint `consent` check resolves grants
-- by joining (sensor_id, scope) on a record's provenance to events
-- under a candidate consent_ref, so a widened reference would let a
-- record claim coverage from a grant that was originally issued for
-- a different sensor or scope.
--
-- Invariant enforced here: every event for a given consent_ref shares
-- the same (sensor_id, scope) tuple. The trigger fires BEFORE INSERT
-- and rejects any row that disagrees with the first event already
-- recorded under the same consent_ref. The first row for a fresh
-- consent_ref always passes (NOT EXISTS short-circuits).
--
-- A change of sensor or scope therefore requires a new consent_ref —
-- which is the documented model: one consent_ref = one stable grant.

CREATE TRIGGER consent_timeline_grant_immutable
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1 FROM consent_timeline
     WHERE consent_ref = NEW.consent_ref
       AND (sensor_id <> NEW.sensor_id OR scope <> NEW.scope)
  )
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: (sensor_id, scope) is immutable per consent_ref; \
     emit a new consent_ref for any sensor or scope change');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (33, '0033_consent_timeline_grant_immutable', '',
          strftime('%s','now') * 1000);
