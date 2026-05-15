-- Migration 0062: workflow dead-letter columns + completion timestamps.
-- Issue #92. Spec: docs/superpowers/specs/2026-05-15-workflow-recovery-design.md
-- Brief sources: §5.6 WAL, §10 Continuous Learning.
--
-- Adds three nullable columns to workflow_jobs:
--   * failure_class     — last FailureClass stamped by the worker on fail()
--   * dead_letter_at_ms — wall-clock when the row transitioned to state='failed'
--   * completed_at_ms   — wall-clock when the row transitioned to state='done'
--
-- All nullable for backward compat with existing 0020 rows.

ALTER TABLE workflow_jobs ADD COLUMN failure_class    TEXT;
ALTER TABLE workflow_jobs ADD COLUMN dead_letter_at_ms INTEGER;
ALTER TABLE workflow_jobs ADD COLUMN completed_at_ms   INTEGER;

-- Lint hot-path: enumerate dead-letter rows.
CREATE INDEX workflow_jobs_dead_letter_idx
  ON workflow_jobs(dead_letter_at_ms)
  WHERE dead_letter_at_ms IS NOT NULL;

-- Lint hot-path: last-success lookup per kind.
CREATE INDEX workflow_jobs_kind_completed_idx
  ON workflow_jobs(kind, completed_at_ms);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (62, '0062_workflow_dead_letter', '', strftime('%s','now') * 1000);
