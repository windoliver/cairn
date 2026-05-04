-- Migration 0046: append-only audit table for consent_journal repair.
-- Brief §3 / §5.6 / §14. Issue #267.

CREATE TABLE IF NOT EXISTS consent_journal_repair_audit (
  repair_id        TEXT NOT NULL PRIMARY KEY,
  action           TEXT NOT NULL CHECK (action IN ('delete')),
  target_rowid     INTEGER NOT NULL,
  blocker_codes    TEXT NOT NULL CHECK (json_valid(blocker_codes) = 1),
  operator         TEXT NOT NULL,
  reason           TEXT NOT NULL,
  row_snapshot     TEXT NOT NULL CHECK (json_valid(row_snapshot) = 1),
  repaired_at      INTEGER NOT NULL
);

CREATE TRIGGER IF NOT EXISTS consent_journal_repair_audit_immutable
  BEFORE UPDATE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;

CREATE TRIGGER IF NOT EXISTS consent_journal_repair_audit_no_delete
  BEFORE DELETE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (46, '0046_consent_journal_repair_audit', '', strftime('%s','now') * 1000);
