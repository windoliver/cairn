-- crates/cairn-store-sqlite/src/migrations/sql/0071_connector_state.sql
-- Issue #161: connector enable/disable state for cairn.admin.v1. See
--   docs/superpowers/specs/2026-05-26-issue-161-admin-v1-extension-design.md §6.5
-- Append-only contract: never mutate this file after merge.

CREATE TABLE connector_state (
  connector_name  TEXT PRIMARY KEY,
  enabled         INTEGER NOT NULL DEFAULT 1,
  last_changed_at TEXT NOT NULL,
  last_changed_by TEXT NOT NULL,
  reason          TEXT
);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (71, '0071_connector_state', '', strftime('%s','now') * 1000);
