-- Migration 0060: widen consent_journal scope character class to include uppercase.
--
-- The `consent_journal_event_metadata_domains` trigger (landed in 0011) used
-- `[^a-z0-9._:=,-]` for the `scope` column, accepting only lowercase letters.
-- However `ScopeTuple::canonical_wire()` emits ULID-valued dimensions (e.g.
-- `session_id=01ARZ3NDEKTSV4RRFFQ69G5FAV`) which contain uppercase Crockford
-- base32 characters. The Rust `validate_scope` function (the authoritative
-- definition in `cairn-core::domain::consent`) has always allowed
-- `[A-Za-z0-9._:=,-]`.  The SQL trigger was narrower by mistake.
--
-- Fix: DROP the old trigger and CREATE a replacement with
-- `[^A-Za-z0-9._:=,-]` (uppercase added to the scope GLOB).  The
-- consent_id and op_id patterns were already correct in 0011 and are
-- reproduced here unchanged.

DROP TRIGGER IF EXISTS consent_journal_event_metadata_domains;
CREATE TRIGGER consent_journal_event_metadata_domains
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND (
        (NEW.consent_id IS NULL
           OR length(NEW.consent_id) < 1
           OR length(NEW.consent_id) > 64
           OR NEW.consent_id GLOB '*[^A-Za-z0-9._:-]*')
     OR (NEW.scope IS NULL
           OR length(NEW.scope) < 1
           OR length(NEW.scope) > 256
           OR NEW.scope GLOB '*[^A-Za-z0-9._:=,-]*')
     OR (NEW.op_id IS NOT NULL
           AND (length(NEW.op_id) < 1
                OR length(NEW.op_id) > 128
                OR NEW.op_id GLOB '*[^A-Za-z0-9._:-]*'))
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event metadata out of domain class');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (60, '0060_consent_scope_uppercase', '', strftime('%s','now') * 1000);
