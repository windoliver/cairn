-- 0023_trace_links — generated columns + unique indices for trace records
-- (issue #77, spec §6.1).

ALTER TABLE records ADD COLUMN trace_event TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace_event')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_session_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.session_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_turn_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.turn_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_sequence INTEGER
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.sequence')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_capture_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.capture_event_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_parent_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.parent_event_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_payload_hash TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.payload_hash')) VIRTUAL;

-- Idempotency: same CaptureEvent may not produce two rows.
CREATE UNIQUE INDEX records_trace_event_id
  ON records(trace_capture_event_id)
  WHERE trace_capture_event_id IS NOT NULL;

-- Exactly one summary per (session_id, turn_id).
CREATE UNIQUE INDEX records_trace_summary
  ON records(trace_session_id, trace_turn_id)
  WHERE trace_event = 'turn_summary';

-- Sequence monotonicity within a turn (excluding summary rows).
CREATE UNIQUE INDEX records_trace_seq
  ON records(trace_session_id, trace_turn_id, trace_sequence)
  WHERE trace_event IS NOT NULL AND trace_event != 'turn_summary';

CREATE INDEX records_trace_parent
  ON records(trace_parent_event_id)
  WHERE trace_parent_event_id IS NOT NULL;

CREATE INDEX records_trace_payload_hash
  ON records(trace_payload_hash)
  WHERE trace_payload_hash IS NOT NULL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (23, '0023_trace_links', '', strftime('%s','now') * 1000);
