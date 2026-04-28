CREATE TABLE edges (
  from_id    TEXT NOT NULL,
  to_id      TEXT NOT NULL,
  kind       TEXT NOT NULL,
  weight     REAL NOT NULL,
  metadata   TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (from_id, to_id, kind)
) STRICT;

CREATE INDEX edges_to_idx ON edges(to_id);

CREATE TABLE edge_versions (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  from_id     TEXT NOT NULL,
  to_id       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  weight      REAL,
  metadata    TEXT,
  change_kind TEXT NOT NULL,
  at          TEXT NOT NULL
);

CREATE INDEX edge_versions_lookup_idx ON edge_versions(from_id, to_id, kind);
