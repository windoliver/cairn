CREATE TABLE IF NOT EXISTS schema_migrations (
  id          INTEGER NOT NULL PRIMARY KEY,
  name        TEXT NOT NULL,
  checksum    TEXT NOT NULL,
  applied_at  TEXT NOT NULL
) STRICT;
