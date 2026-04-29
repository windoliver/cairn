-- Migration 0016: quarantine unrecoverable pre-0009 rows.
--
-- Migration 0009 added the `record_json` column without a backfill —
-- there is no source of truth for the original `MemoryRecord` payload
-- of rows written before 0009. The post-migration legacy-row gate in
-- `open_blocking()` refused to open any database that still contained
-- such rows, which makes the normal upgrade path an operational
-- outage.
--
-- We cannot reconstruct the rows, but we also cannot silently delete
-- them: that would make rollback and forensic recovery materially
-- harder. Instead, move them verbatim into a quarantine table that
-- preserves every column we have. Operators can inspect, export, or
-- restore at their discretion; the runtime ignores the quarantine
-- table entirely.
--
-- After this migration, `records` contains no NULL-`record_json`
-- rows, so `open_blocking()`'s legacy-row gate no longer trips on
-- the normal upgrade path. The gate is retained as defense-in-depth
-- for direct schema tampering or partial migration runs.
CREATE TABLE IF NOT EXISTS records_legacy_quarantine AS
  SELECT * FROM records WHERE 0 = 1;

INSERT INTO records_legacy_quarantine
  SELECT * FROM records WHERE record_json IS NULL;

DELETE FROM records WHERE record_json IS NULL;
