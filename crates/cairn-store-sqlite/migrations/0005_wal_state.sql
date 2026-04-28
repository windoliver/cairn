CREATE TABLE wal_ops (
  op_id      TEXT NOT NULL PRIMARY KEY,
  kind       TEXT NOT NULL,
  state      TEXT NOT NULL,
  payload    TEXT NOT NULL,
  pre_image  BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX wal_ops_state_idx ON wal_ops(state);

CREATE TABLE wal_steps (
  rowid_alias INTEGER PRIMARY KEY AUTOINCREMENT,
  op_id       TEXT NOT NULL,
  step_kind   TEXT NOT NULL,
  state       TEXT NOT NULL,
  payload     TEXT,
  at          TEXT NOT NULL,
  FOREIGN KEY (op_id) REFERENCES wal_ops(op_id) ON DELETE CASCADE
);

CREATE INDEX wal_steps_op_idx ON wal_steps(op_id);
