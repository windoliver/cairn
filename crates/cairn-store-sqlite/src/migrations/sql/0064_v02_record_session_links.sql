-- Migration 0064: v0.2 metadata for cold/session delete links.
-- Issue #109. Brief sources: §19 v0.2 in-place migration, §3.0 SQLite
-- authority, §5.6 forget_session.
--
-- Additive only: do not rewrite records.scope or record_json. Existing v0.1
-- rows remain the authoritative bodies; these tables record deterministic,
-- auditable links derived from scope/session traces, summary frontmatter, and
-- DB-owned markdown projection columns.

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

CREATE TABLE record_summary_links (
  summary_record_id TEXT NOT NULL
                         REFERENCES records(record_id) ON DELETE CASCADE,
  source_record_id  TEXT NOT NULL
                         REFERENCES records(record_id) ON DELETE CASCADE,
  summary_kind      TEXT NOT NULL CHECK (summary_kind IN ('turn_summary', 'consolidation')),
  link_source       TEXT NOT NULL CHECK (link_source IN ('trace', 'consolidation')),
  created_at        INTEGER NOT NULL,
  PRIMARY KEY (summary_record_id, source_record_id, summary_kind)
);

CREATE INDEX record_summary_links_source_idx
  ON record_summary_links(source_record_id, summary_record_id);

CREATE INDEX record_summary_links_summary_idx
  ON record_summary_links(summary_record_id, source_record_id);

CREATE TABLE record_projection_links (
  record_id       TEXT NOT NULL PRIMARY KEY
                       REFERENCES records(record_id) ON DELETE CASCADE,
  target_id       TEXT NOT NULL,
  projection_kind TEXT NOT NULL CHECK (projection_kind IN ('markdown')),
  path            TEXT NOT NULL,
  body_hash       TEXT NOT NULL,
  created_at      INTEGER NOT NULL
);

CREATE INDEX record_projection_links_target_idx
  ON record_projection_links(target_id, projection_kind);

CREATE INDEX record_projection_links_path_idx
  ON record_projection_links(path);

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

INSERT INTO record_summary_links (
  summary_record_id, source_record_id, summary_kind, link_source, created_at
)
SELECT
  summary.record_id,
  source.value,
  'turn_summary',
  'trace',
  strftime('%s','now') * 1000
FROM records AS summary
JOIN json_each(json_extract(summary.extra_frontmatter, '$.trace.member_event_ids')) AS source
JOIN records AS source_record
  ON source_record.record_id = source.value
WHERE summary.trace_event = 'turn_summary'
  AND source.type = 'text';

INSERT INTO record_summary_links (
  summary_record_id, source_record_id, summary_kind, link_source, created_at
)
SELECT
  summary.record_id,
  source.value,
  'consolidation',
  'consolidation',
  strftime('%s','now') * 1000
FROM records AS summary
JOIN json_each(json_extract(summary.extra_frontmatter, '$.consolidation.source_record_ids')) AS source
JOIN records AS source_record
  ON source_record.record_id = source.value
WHERE json_extract(summary.extra_frontmatter, '$.consolidation') IS NOT NULL
  AND source.type = 'text'
ON CONFLICT(summary_record_id, source_record_id, summary_kind) DO NOTHING;

INSERT INTO record_projection_links (
  record_id, target_id, projection_kind, path, body_hash, created_at
)
SELECT
  record_id,
  target_id,
  'markdown',
  path,
  body_hash,
  strftime('%s','now') * 1000
FROM records
WHERE path IS NOT NULL
  AND path <> ''
  AND body_hash IS NOT NULL
  AND body_hash <> '';

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (64, '0064_v02_record_session_links', '', strftime('%s','now') * 1000);
