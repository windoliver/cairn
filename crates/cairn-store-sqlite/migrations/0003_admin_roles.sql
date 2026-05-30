-- crates/cairn-store-sqlite/migrations/0003_admin_roles.sql
-- Issue #161: admin role table for cairn.admin.v1 extension. See
--   docs/superpowers/specs/2026-05-26-issue-161-admin-v1-extension-design.md §6.5
-- Append-only contract: never mutate this file after merge.

CREATE TABLE admin_roles (
  identity_id TEXT NOT NULL,
  role        TEXT NOT NULL CHECK (role IN ('operator')),
  granted_at  TEXT NOT NULL,
  granted_by  TEXT NOT NULL,
  revoked_at  TEXT,
  PRIMARY KEY (identity_id, role)
);

CREATE INDEX admin_roles_active ON admin_roles(identity_id)
  WHERE revoked_at IS NULL;
