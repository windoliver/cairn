-- Migration 0059: add `mutation_seq` to `session_metadata_audit` so
-- multiple session-metadata patches against the same session in a
-- single flush plan can all be audited (issue #289 re-loop r7).
--
-- The previous PK (operation_id, session_id) collided on the second
-- INSERT when one plan patched the same session twice; the whole
-- apply tx would abort. Replace the PK with (operation_id, session_id,
-- mutation_seq) where mutation_seq is the 0-based index of the
-- mutation within the plan.

CREATE TABLE session_metadata_audit_v2 (
  operation_id TEXT NOT NULL,
  session_id   TEXT NOT NULL,
  mutation_seq INTEGER NOT NULL,
  pre_state    TEXT NOT NULL,
  post_state   TEXT NOT NULL,
  applied_at   INTEGER NOT NULL,
  PRIMARY KEY (operation_id, session_id, mutation_seq)
) WITHOUT ROWID;

INSERT INTO session_metadata_audit_v2
  (operation_id, session_id, mutation_seq, pre_state, post_state, applied_at)
  SELECT operation_id, session_id, 0, pre_state, post_state, applied_at
  FROM session_metadata_audit;

DROP INDEX IF EXISTS session_metadata_audit_by_session;
DROP TABLE session_metadata_audit;

ALTER TABLE session_metadata_audit_v2 RENAME TO session_metadata_audit;

CREATE INDEX session_metadata_audit_by_session
  ON session_metadata_audit (session_id, applied_at);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (59, '0059_session_metadata_audit_seq', '', strftime('%s','now') * 1000);
