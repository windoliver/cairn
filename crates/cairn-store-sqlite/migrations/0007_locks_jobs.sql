CREATE TABLE locks (
  scope_kind  TEXT NOT NULL,
  scope_key   TEXT NOT NULL,
  op_id       TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  PRIMARY KEY (scope_kind, scope_key)
) STRICT;

CREATE TABLE reader_fence (
  scope_kind TEXT NOT NULL,
  scope_key  TEXT NOT NULL,
  op_id      TEXT NOT NULL,
  state      TEXT NOT NULL,
  opened_at  TEXT NOT NULL,
  closed_at  TEXT,
  PRIMARY KEY (scope_kind, scope_key)
) STRICT;

CREATE TABLE jobs (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  workflow     TEXT NOT NULL,
  state        TEXT NOT NULL,
  payload      TEXT NOT NULL,
  scheduled_at TEXT NOT NULL,
  started_at   TEXT,
  finished_at  TEXT
);

CREATE INDEX jobs_state_idx ON jobs(state);
CREATE INDEX jobs_workflow_idx ON jobs(workflow);
