CREATE TABLE records (
  record_id      TEXT NOT NULL PRIMARY KEY,
  target_id      TEXT NOT NULL,
  version        INTEGER NOT NULL,
  active         INTEGER NOT NULL DEFAULT 0,
  tombstoned     INTEGER NOT NULL DEFAULT 0,
  created_at     TEXT NOT NULL,
  created_by     TEXT NOT NULL,
  tombstoned_at  TEXT,
  tombstoned_by  TEXT,
  expired_at     TEXT,
  body           TEXT NOT NULL,
  provenance     TEXT NOT NULL,
  actor_chain    TEXT NOT NULL,
  evidence       TEXT NOT NULL,
  scope          TEXT NOT NULL,
  taxonomy       TEXT NOT NULL,
  confidence     REAL NOT NULL,
  salience       REAL NOT NULL,
  UNIQUE (target_id, version)
) STRICT;

CREATE UNIQUE INDEX records_active_target_idx
  ON records(target_id) WHERE active = 1;

CREATE INDEX records_target_idx ON records(target_id);

CREATE TABLE record_purges (
  target_id        TEXT NOT NULL,
  op_id            TEXT NOT NULL,
  purged_at        TEXT NOT NULL,
  purged_by        TEXT NOT NULL,
  body_hash_salt   TEXT NOT NULL,
  PRIMARY KEY (target_id, op_id)
) STRICT;
