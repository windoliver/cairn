-- Migration 0031: widen wal_ops.kind CHECK to admit graph mutations.
-- Issue #186 — bitemporal knowledge-graph schema.
--
-- SQLite CHECK constraints are immutable, so we table-rebuild wal_ops:
-- copy data into a new table with the widened CHECK, then swap names.
-- All triggers, indexes, and FKs (from wal_op_deps and wal_steps) are
-- recreated verbatim.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;
PRAGMA defer_foreign_keys = ON;

CREATE TABLE wal_ops_new (
  operation_id   TEXT NOT NULL PRIMARY KEY,
  issued_seq     INTEGER NOT NULL UNIQUE,
  kind           TEXT NOT NULL CHECK (kind IN (
                   'upsert','delete','promote','expire',
                   'forget_session','forget_record','evolve',
                   'graph_upsert_entity','graph_upsert_edge',
                   'graph_contradict','graph_tombstone','graph_link_episode')),
  state          TEXT NOT NULL CHECK (state IN (
                   'ISSUED','PREPARED','COMMITTED','ABORTED','REJECTED')),
  envelope       TEXT NOT NULL,
  issuer         TEXT NOT NULL,
  principal      TEXT,
  target_hash    TEXT NOT NULL,
  scope_json     TEXT NOT NULL,
  plan_ref       TEXT,
  expires_at     INTEGER NOT NULL,
  signature      TEXT NOT NULL,
  issued_at      INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  reason         TEXT
);

INSERT INTO wal_ops_new SELECT * FROM wal_ops;

DROP TRIGGER IF EXISTS wal_ops_issued_seq_must_advance;
DROP TRIGGER IF EXISTS wal_ops_state_transition;
DROP TRIGGER IF EXISTS wal_ops_envelope_immutable;
DROP TRIGGER IF EXISTS wal_ops_terminal_immutable;
DROP TRIGGER IF EXISTS wal_ops_no_delete;
DROP INDEX IF EXISTS wal_ops_open_idx;
DROP TABLE wal_ops;
ALTER TABLE wal_ops_new RENAME TO wal_ops;

CREATE INDEX wal_ops_open_idx
  ON wal_ops(state, issued_at)
  WHERE state IN ('ISSUED','PREPARED');

CREATE TRIGGER wal_ops_issued_seq_must_advance
  BEFORE INSERT ON wal_ops
  FOR EACH ROW
  WHEN NEW.issued_seq <= COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0)
BEGIN
  SELECT RAISE(ABORT, 'wal_ops.issued_seq must strictly advance MAX(issued_seq)');
END;

CREATE TRIGGER wal_ops_state_transition
  BEFORE UPDATE OF state ON wal_ops
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (
        (OLD.state = 'ISSUED'   AND NEW.state IN ('PREPARED','REJECTED'))
     OR (OLD.state = 'PREPARED' AND NEW.state IN ('COMMITTED','ABORTED'))
   )
BEGIN
  SELECT RAISE(ABORT, 'wal_ops.state transition not allowed by §5.6 FSM');
END;

CREATE TRIGGER wal_ops_envelope_immutable
  BEFORE UPDATE ON wal_ops
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.issued_seq   IS NOT OLD.issued_seq
    OR NEW.kind         IS NOT OLD.kind
    OR NEW.envelope     IS NOT OLD.envelope
    OR NEW.issuer       IS NOT OLD.issuer
    OR NEW.principal    IS NOT OLD.principal
    OR NEW.target_hash  IS NOT OLD.target_hash
    OR NEW.scope_json   IS NOT OLD.scope_json
    OR NEW.plan_ref     IS NOT OLD.plan_ref
    OR NEW.expires_at   IS NOT OLD.expires_at
    OR NEW.signature    IS NOT OLD.signature
    OR NEW.issued_at    IS NOT OLD.issued_at
BEGIN
  SELECT RAISE(ABORT, 'wal_ops envelope columns are immutable');
END;

CREATE TRIGGER wal_ops_terminal_immutable
  BEFORE UPDATE ON wal_ops
  FOR EACH ROW
  WHEN OLD.state IN ('COMMITTED', 'ABORTED', 'REJECTED')
BEGIN
  SELECT RAISE(ABORT, 'wal_ops terminal-state rows are fully immutable');
END;

CREATE TRIGGER wal_ops_no_delete
  BEFORE DELETE ON wal_ops
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_ops is append-only; DELETE not permitted');
END;

PRAGMA foreign_keys = ON;
PRAGMA legacy_alter_table = OFF;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (31, '0031_wal_kind_widening', '', strftime('%s','now') * 1000);
