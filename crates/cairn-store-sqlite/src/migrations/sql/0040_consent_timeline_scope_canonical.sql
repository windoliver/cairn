-- Migration 0040: enforce canonical scope encoding on consent_timeline
-- rows. Issue #253, brief §14, loop 3 round 5.
--
-- The lint check joins records to grants by exact string equality
-- between `record.scope.canonical_wire()` and `consent_timeline.scope`.
-- Until now the column was raw TEXT, so a writer could persist a
-- non-canonical scope (different key ordering, lowercase variants,
-- whitespace) and lint would silently treat semantically-equivalent
-- grants as "no covering grant" -- a hard false negative on valid
-- data.
--
-- ScopeTuple::canonical_wire() emits only [A-Za-z0-9._:-] characters
-- (component alphabet from domain/scope.rs) plus the ':' separator.
-- The trigger below approximates that rule via SQLite GLOB: any
-- character outside the canonical alphabet is rejected. This is a
-- conservative check -- it rejects junk and obviously-non-canonical
-- forms but does not validate the prefix/component grammar (which
-- would require regex). Stricter validation lives in the writer
-- (Phase-B / #255), which constructs scope strings from typed
-- ScopeTuple values; this trigger is the schema-level safety net.
--
-- Combined with 0030's audit pass (which checks no preexisting row
-- contains characters outside the canonical alphabet), this closes
-- the encoding-drift attack surface.

CREATE TRIGGER consent_timeline_scope_canonical_chars
  BEFORE INSERT ON consent_timeline
  FOR EACH ROW
  WHEN length(NEW.scope) = 0
    OR NEW.scope GLOB '*[^A-Za-z0-9._:=,-]*'
BEGIN
  SELECT RAISE(ABORT,
    'consent_timeline: scope must be canonical wire form \
     (chars [A-Za-z0-9._:=,-] only, non-empty)');
END;

-- Audit existing rows: if anything in the table violates the rule,
-- abort the migration so an operator can investigate before lint
-- starts trusting these rows for authorization joins.
CREATE TEMP TABLE _migration_0040_audit (
  ok INTEGER NOT NULL CHECK (ok = 1)
);
INSERT INTO _migration_0040_audit (ok)
  SELECT CASE
    WHEN EXISTS (
      SELECT 1 FROM consent_timeline
       WHERE length(scope) = 0
          OR scope GLOB '*[^A-Za-z0-9._:=,-]*'
    ) THEN 0 ELSE 1
  END;
DROP TABLE _migration_0040_audit;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (40, '0040_consent_timeline_scope_canonical', '',
          strftime('%s','now') * 1000);
