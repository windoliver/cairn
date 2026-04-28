-- 0011: idempotency key on consent_journal so retried operations cannot
-- accumulate duplicate audit rows. The brief mandates one consent
-- entry per (op_id, kind, target_id) tuple; without this index a
-- replayed WAL entry would silently double-count.
--
-- target_id is nullable; SQLite UNIQUE INDEX on a NULL column treats
-- NULL as distinct, so the COALESCE pins NULLs to a sentinel string
-- and pulls them into the uniqueness check.
CREATE UNIQUE INDEX consent_journal_idempotency_idx
  ON consent_journal(op_id, kind, COALESCE(target_id, '__NULL__'));
