-- Migration 0039: audit existing consent_timeline rows against the
-- security invariants from 0024 (grant immutability) and 0025 (lifecycle
-- monotonicity). Issue #253, brief §14, Round 10+7.
--
-- 0024 and 0025 enforce their invariants only on INSERT. A vault that
-- was populated under earlier #253 revisions (before those triggers
-- existed) may already contain rows that violate them: scope/sensor
-- widening within a consent_ref, a non-`issued` first event, gaps in
-- seq, or backdated decided_at. 0028+0029 only check timestamp shape
-- on those rows; they don't replay the lifecycle rules. Lint then
-- treats whatever's persisted as authoritative for §6.5 decisions.
--
-- This migration runs four explicit audits and aborts the migration
-- if any fail. Fresh installs are no-ops (table empty). Vaults that
-- carry pre-trigger data refuse to open until rebuilt -- the same
-- posture 0029 takes for non-canonical timestamps.
--
-- Invariants checked:
--   1. (sensor_id, scope) is stable across every event for a single
--      consent_ref (0024).
--   2. The smallest seq for each consent_ref is 1 and that row's kind
--      is 'issued' (0025 first-event + fresh-seq=1).
--   3. seq numbers are contiguous: MAX(seq) = COUNT(*) per consent_ref
--      (0025 strictly monotonic + first row at 1).
--   4. decided_at is non-decreasing across seq within a consent_ref
--      (0025 + 0028 canonical-nanos make TEXT compare chronological).

CREATE TEMP TABLE _migration_0039_audit (
  ok INTEGER NOT NULL CHECK (ok = 1)
);

-- Invariant 1: stable (sensor_id, scope) per consent_ref.
INSERT INTO _migration_0039_audit (ok)
  SELECT CASE
    WHEN EXISTS (
      SELECT 1 FROM consent_timeline
       GROUP BY consent_ref
      HAVING COUNT(DISTINCT sensor_id) > 1
          OR COUNT(DISTINCT scope) > 1
    ) THEN 0 ELSE 1
  END;

-- Invariant 2: first event has seq=1 AND kind='issued'.
INSERT INTO _migration_0039_audit (ok)
  SELECT CASE
    WHEN EXISTS (
      SELECT 1 FROM consent_timeline ct1
       WHERE NOT EXISTS (
         SELECT 1 FROM consent_timeline ct2
          WHERE ct2.consent_ref = ct1.consent_ref AND ct2.seq < ct1.seq
       )
         AND (ct1.seq <> 1 OR ct1.kind <> 'issued')
    ) THEN 0 ELSE 1
  END;

-- Invariant 3: seq contiguity (max(seq) = count(*) per consent_ref).
INSERT INTO _migration_0039_audit (ok)
  SELECT CASE
    WHEN EXISTS (
      SELECT 1 FROM consent_timeline
       GROUP BY consent_ref
      HAVING MAX(seq) <> COUNT(*)
    ) THEN 0 ELSE 1
  END;

-- Invariant 4: decided_at non-decreasing across seq order.
INSERT INTO _migration_0039_audit (ok)
  SELECT CASE
    WHEN EXISTS (
      SELECT 1 FROM consent_timeline a, consent_timeline b
       WHERE a.consent_ref = b.consent_ref
         AND a.seq < b.seq
         AND a.decided_at > b.decided_at
    ) THEN 0 ELSE 1
  END;

DROP TABLE _migration_0039_audit;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (39, '0039_consent_timeline_audit_legacy_invariants', '',
          strftime('%s','now') * 1000);
