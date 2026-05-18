-- Migration 0058: session_metadata_audit — durable rollback evidence for
-- in-place session-metadata patches (issue #289 re-loop r6).
--
-- `apply_session_patch` mutates `sessions` rows in place (the live
-- session-resolution path requires a single live row per identity), so
-- pre-mutation values were lost on success. This append-only audit
-- captures the (operation_id, session_id, pre/post JSON) tuple inside
-- the same SQLite transaction as the UPDATE, so recovery tooling can
-- reconstruct the overwrite without out-of-band logs.
--
-- The table is keyed by (operation_id, session_id) so a single flush
-- plan touching multiple sessions still records one row per target.

CREATE TABLE session_metadata_audit (
  operation_id TEXT NOT NULL,
  session_id   TEXT NOT NULL,
  pre_state    TEXT NOT NULL,
  post_state   TEXT NOT NULL,
  applied_at   INTEGER NOT NULL,
  PRIMARY KEY (operation_id, session_id)
) WITHOUT ROWID;

CREATE INDEX session_metadata_audit_by_session
  ON session_metadata_audit (session_id, applied_at);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (58, '0058_session_metadata_audit', '', strftime('%s','now') * 1000);
