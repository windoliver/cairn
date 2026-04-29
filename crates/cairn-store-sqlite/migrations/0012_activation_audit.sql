-- Migration 0012: persist activation lifecycle in the records table.
--
-- `version_history` previously synthesized an `Update` event for every
-- staged version using `created_at`/`created_by`, which conflated stage
-- time with activation time and falsely surfaced staged-but-never-active
-- rows as "updated". With these columns set only by `activate_version`,
-- the read path can emit `Update` events that reflect real activations.
ALTER TABLE records ADD COLUMN activated_at TEXT;
ALTER TABLE records ADD COLUMN activated_by TEXT;
