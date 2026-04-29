-- Migration 0013: replace the '__NULL__' sentinel idempotency index
-- with two partial unique indexes.
--
-- The previous index from 0011 used `COALESCE(target_id, '__NULL__')`
-- to make `(op_id, kind, target_id)` unique even when target_id is
-- NULL. That sentinel was an in-band string, so a real record whose
-- target_id is exactly '__NULL__' could collide with target-less
-- entries for the same `(op_id, kind)` and corrupt the audit trail.
--
-- Partial unique indexes give us the same idempotency guarantee
-- without an in-band sentinel.
DROP INDEX IF EXISTS consent_journal_idempotency_idx;

CREATE UNIQUE INDEX IF NOT EXISTS consent_journal_idempotency_targeted_idx
    ON consent_journal (op_id, kind, target_id)
    WHERE target_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS consent_journal_idempotency_untargeted_idx
    ON consent_journal (op_id, kind)
    WHERE target_id IS NULL;
