-- Migration 0064: v0.2 metadata for cold/session delete links.
-- Issue #109. Brief sources: §19 v0.2 in-place migration, §3.0 SQLite
-- authority, §5.6 forget_session.
--
-- Additive only: do not rewrite records.scope or record_json. Existing v0.1
-- rows remain the authoritative bodies; this table records deterministic,
-- auditable links derived from scope.session_id and trace.session_id.

CREATE TABLE record_session_links (
  record_id       TEXT NOT NULL PRIMARY KEY
                       REFERENCES records(record_id) ON DELETE CASCADE,
  target_id       TEXT NOT NULL,
  session_id      TEXT NOT NULL,
  tenant          TEXT,
  workspace       TEXT,
  link_source     TEXT NOT NULL CHECK (link_source IN ('scope', 'trace')),
  link_confidence TEXT NOT NULL CHECK (link_confidence IN ('explicit', 'derived')),
  created_at      INTEGER NOT NULL
);

CREATE INDEX record_session_links_session_idx
  ON record_session_links(session_id, tenant, workspace, target_id);

CREATE INDEX record_session_links_target_idx
  ON record_session_links(target_id, session_id);

CREATE TABLE record_link_review (
  record_id         TEXT NOT NULL PRIMARY KEY
                         REFERENCES records(record_id) ON DELETE CASCADE,
  reason            TEXT NOT NULL CHECK (reason IN ('session_id_mismatch')),
  scope_session_id  TEXT,
  trace_session_id  TEXT,
  detail_json       TEXT NOT NULL,
  created_at        INTEGER NOT NULL
);

CREATE INDEX record_link_review_reason_idx
  ON record_link_review(reason, record_id);

INSERT INTO record_link_review (
  record_id, reason, scope_session_id, trace_session_id, detail_json, created_at
)
SELECT
  record_id,
  'session_id_mismatch',
  json_extract(scope, '$.session_id'),
  trace_session_id,
  json_object(
    'scope_session_id', json_extract(scope, '$.session_id'),
    'trace_session_id', trace_session_id,
    'migration_id', 64
  ),
  strftime('%s','now') * 1000
FROM records
WHERE json_extract(scope, '$.session_id') IS NOT NULL
  AND trace_session_id IS NOT NULL
  AND json_extract(scope, '$.session_id') <> trace_session_id;

INSERT INTO record_session_links (
  record_id, target_id, session_id, tenant, workspace,
  link_source, link_confidence, created_at
)
SELECT
  record_id,
  target_id,
  COALESCE(json_extract(scope, '$.session_id'), trace_session_id),
  json_extract(scope, '$.tenant'),
  json_extract(scope, '$.workspace'),
  CASE
    WHEN json_extract(scope, '$.session_id') IS NOT NULL THEN 'scope'
    ELSE 'trace'
  END,
  CASE
    WHEN json_extract(scope, '$.session_id') IS NOT NULL THEN 'explicit'
    ELSE 'derived'
  END,
  strftime('%s','now') * 1000
FROM records
WHERE COALESCE(json_extract(scope, '$.session_id'), trace_session_id) IS NOT NULL
  AND NOT (
    json_extract(scope, '$.session_id') IS NOT NULL
    AND trace_session_id IS NOT NULL
    AND json_extract(scope, '$.session_id') <> trace_session_id
  );

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (64, '0064_v02_record_session_links', '', strftime('%s','now') * 1000);
