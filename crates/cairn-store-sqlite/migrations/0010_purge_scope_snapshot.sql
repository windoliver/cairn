-- 0010: snapshot scope/taxonomy at purge time so version_history can
-- evaluate rebac on `Purge` markers. Without this, an unprivileged
-- caller can probe purge_target_id to learn timing/actor for records
-- they were never allowed to read.

ALTER TABLE record_purges ADD COLUMN scope_snapshot TEXT;
ALTER TABLE record_purges ADD COLUMN taxonomy_snapshot TEXT;
