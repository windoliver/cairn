-- Migration 0002: WAL operations + steps + dependency DAG.
-- Brief source: §5.6.

CREATE TABLE wal_ops (
  operation_id   TEXT NOT NULL PRIMARY KEY,
  issued_seq     INTEGER NOT NULL UNIQUE,
  kind           TEXT NOT NULL CHECK (kind IN (
                   'upsert','delete','promote','expire',
                   'forget_session','forget_record','evolve')),
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

CREATE TABLE wal_op_deps (
  operation_id     TEXT NOT NULL,
  depends_on_op_id TEXT NOT NULL,
  PRIMARY KEY (operation_id, depends_on_op_id),
  CHECK (operation_id != depends_on_op_id),
  FOREIGN KEY (operation_id)     REFERENCES wal_ops(operation_id) ON DELETE CASCADE,
  FOREIGN KEY (depends_on_op_id) REFERENCES wal_ops(operation_id)
);
CREATE INDEX wal_op_deps_reverse_idx ON wal_op_deps(depends_on_op_id);

CREATE TRIGGER wal_op_deps_must_be_acyclic
  BEFORE INSERT ON wal_op_deps
  FOR EACH ROW
  WHEN (SELECT issued_seq FROM wal_ops WHERE operation_id = NEW.depends_on_op_id)
       >= (SELECT issued_seq FROM wal_ops WHERE operation_id = NEW.operation_id)
BEGIN
  SELECT RAISE(ABORT, 'wal_op_deps.depends_on_op_id must have a strictly smaller issued_seq');
END;

CREATE TRIGGER wal_op_deps_immutable
  BEFORE UPDATE ON wal_op_deps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_op_deps rows are immutable');
END;

CREATE TRIGGER wal_op_deps_no_delete
  BEFORE DELETE ON wal_op_deps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_op_deps is append-only');
END;

CREATE TABLE wal_steps (
  operation_id  TEXT NOT NULL,
  step_ord      INTEGER NOT NULL CHECK (step_ord >= 0),
  step_kind     TEXT NOT NULL,
  state         TEXT NOT NULL CHECK (state IN ('PENDING','DONE','FAILED','COMPENSATED')),
  attempts      INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  last_error    TEXT,
  pre_image     BLOB,
  started_at    INTEGER,
  finished_at   INTEGER,
  PRIMARY KEY (operation_id, step_ord),
  FOREIGN KEY (operation_id) REFERENCES wal_ops(operation_id) ON DELETE CASCADE
);

CREATE INDEX wal_steps_resume_idx
  ON wal_steps(operation_id, state, step_ord);

CREATE TRIGGER wal_steps_state_transition
  BEFORE UPDATE OF state ON wal_steps
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (
        (OLD.state = 'PENDING' AND NEW.state IN ('DONE','FAILED'))
     OR (OLD.state = 'FAILED'  AND NEW.state IN ('PENDING','COMPENSATED'))
     OR (OLD.state = 'DONE'    AND NEW.state = 'COMPENSATED')
   )
BEGIN
  SELECT RAISE(ABORT, 'wal_steps.state transition not allowed');
END;

CREATE TRIGGER wal_steps_identity_immutable
  BEFORE UPDATE ON wal_steps
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.step_ord     IS NOT OLD.step_ord
    OR NEW.step_kind    IS NOT OLD.step_kind
BEGIN
  SELECT RAISE(ABORT, 'wal_steps identity is immutable');
END;

CREATE TRIGGER wal_steps_no_delete
  BEFORE DELETE ON wal_steps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_steps is append-only; DELETE not permitted');
END;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (2, '0002_wal', '', strftime('%s','now') * 1000);
