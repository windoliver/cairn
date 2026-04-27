-- Migration 0005: consent journal.
-- Brief source: §14 (privacy / consent).

CREATE TABLE consent_journal (
  consent_id    TEXT NOT NULL PRIMARY KEY,
  subject       TEXT NOT NULL,
  scope         TEXT NOT NULL,
  decision      TEXT NOT NULL CHECK (decision IN ('GRANT','REVOKE')),
  reason        TEXT,
  granted_by    TEXT NOT NULL,
  decided_at    INTEGER NOT NULL,
  expires_at    INTEGER
);

CREATE INDEX consent_journal_subject_scope_idx
  ON consent_journal(subject, scope, decided_at);

CREATE TRIGGER consent_journal_immutable
  BEFORE UPDATE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal rows are immutable');
END;

CREATE TRIGGER consent_journal_no_delete
  BEFORE DELETE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (5, '0005_consent', '', strftime('%s','now') * 1000);
