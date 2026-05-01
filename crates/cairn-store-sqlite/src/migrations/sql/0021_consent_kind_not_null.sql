-- Migration 0021: flip consent_journal.kind to NOT NULL + table-level
-- CHECK on the §14 domain. Completes Phase-B hardening of the consent
-- event log started in 0009 (additive nullable column + trigger gate)
-- and 0011 (direct-SQL hardening). Brief: §14 / §3 line 448. Issue #255.
--
-- WHY a table rebuild: SQLite cannot ALTER TABLE ADD CHECK, and the
-- existing kind-domain gate is a BEFORE INSERT trigger that fires only
-- when `kind IS NOT NULL`. Promoting the column to NOT NULL eliminates
-- the legacy null-kind path entirely; lifting the domain check from a
-- trigger to a column CHECK puts the constraint in the schema (visible
-- to migration verifiers, query planners, and any future tooling that
-- reads `sqlite_master`).
--
-- Legacy back-compat: rows written by migration 0005's pure GRANT/REVOKE
-- path have `kind IS NULL`. The rebuild backfills them from the existing
-- `decision` column: 'GRANT' -> 'grant', 'REVOKE' -> 'revoke'. The
-- broader 0011 hardening triggers gate INSERTs into the new table, but
-- they only fire when columns they require are populated. Backfilled
-- legacy rows lack `actor`/`payload_json`/`decided_at_iso` — those
-- triggers would fire and abort the rebuild. We therefore drop the
-- 0011 hardening triggers BEFORE the data move and re-attach them
-- AFTER, so backfilled legacy rows survive while every future INSERT
-- is gated normally.
--
-- ROWID PRESERVATION: the async mirror in cairn-workflows tails
-- `consent_journal` by `rowid`. The rebuild preserves rowid 1:1 via
-- `INSERT INTO new(rowid, …) SELECT rowid, …`. Any cursor sidecar at
-- `.cairn/consent.cursor` written by an older incarnation continues to
-- point at the right row after upgrade.
--
-- Append-only triggers (0005's consent_journal_immutable +
-- consent_journal_no_delete) are dropped + re-attached unchanged.
--
-- The 0009 `consent_journal_kind_domain` trigger is dropped permanently:
-- the new column-level CHECK supersedes it. Other 0009/0011 triggers
-- have their `WHEN NEW.kind IS NOT NULL AND …` guard stripped (kind is
-- now NOT NULL by column constraint, so the guard is tautological).

-- 1. Drop ALL triggers attached to the old consent_journal. They are
--    re-created at the end of this migration against the renamed table.
DROP TRIGGER IF EXISTS consent_journal_immutable;
DROP TRIGGER IF EXISTS consent_journal_no_delete;
DROP TRIGGER IF EXISTS consent_journal_kind_domain;
DROP TRIGGER IF EXISTS consent_journal_event_requires_iso;
DROP TRIGGER IF EXISTS consent_journal_forget_receipt_body_free;
DROP TRIGGER IF EXISTS consent_journal_event_requires_actor;
DROP TRIGGER IF EXISTS consent_journal_event_requires_payload;
DROP TRIGGER IF EXISTS consent_journal_payload_shape_matches_kind;
DROP TRIGGER IF EXISTS consent_journal_payload_body_free;
DROP TRIGGER IF EXISTS consent_journal_sensor_kind_requires_sensor_id;
DROP TRIGGER IF EXISTS consent_journal_sensor_id_matches_payload;
DROP TRIGGER IF EXISTS consent_journal_sensor_subject_matches_sensor_id;
DROP TRIGGER IF EXISTS consent_journal_non_sensor_kind_forbids_sensor_id;
DROP TRIGGER IF EXISTS consent_journal_hash_kind_subject_shape;
DROP TRIGGER IF EXISTS consent_journal_hash_kind_target_id_hash_shape;
DROP TRIGGER IF EXISTS consent_journal_payload_required_fields;
DROP TRIGGER IF EXISTS consent_journal_payload_unknown_top_level_keys;
DROP TRIGGER IF EXISTS consent_journal_payload_no_duplicate_keys;
DROP TRIGGER IF EXISTS consent_journal_subject_domain_for_non_hash_kinds;
DROP TRIGGER IF EXISTS consent_journal_event_requires_positive_rowid;
DROP TRIGGER IF EXISTS consent_journal_payload_keys_match_shape;
DROP TRIGGER IF EXISTS consent_journal_payload_scalar_domains;
DROP TRIGGER IF EXISTS consent_journal_event_metadata_domains;
DROP TRIGGER IF EXISTS consent_journal_sensor_id_domain;

-- 2. Drop indexes that reference the old name.
DROP INDEX IF EXISTS consent_journal_subject_scope_idx;
DROP INDEX IF EXISTS consent_journal_op_idx;
DROP INDEX IF EXISTS consent_journal_actor_idx;
DROP INDEX IF EXISTS consent_journal_sensor_idx;
DROP INDEX IF EXISTS consent_journal_kind_idx;

-- 3. Create the new table with NOT NULL + CHECK on `kind`. Mirrors the
--    0005 + 0009 columns exactly otherwise.
CREATE TABLE consent_journal_v2 (
  consent_id      TEXT NOT NULL PRIMARY KEY,
  subject         TEXT NOT NULL,
  scope           TEXT NOT NULL,
  decision        TEXT NOT NULL CHECK (decision IN ('GRANT','REVOKE')),
  reason          TEXT,
  granted_by      TEXT NOT NULL,
  decided_at      INTEGER NOT NULL,
  expires_at      INTEGER,
  op_id           TEXT,
  kind            TEXT NOT NULL CHECK (kind IN (
    'sensor_enable',
    'sensor_disable',
    'policy_change',
    'remember_intent',
    'forget_intent',
    'grant',
    'revoke',
    'promote_receipt'
  )),
  sensor_id       TEXT,
  actor           TEXT,
  payload_json    TEXT,
  decided_at_iso  TEXT,
  expires_at_iso  TEXT
);

-- 4. Move data, preserving rowid. Backfill kind from decision for any
--    legacy null-kind rows (decision is already CHECK-constrained to
--    'GRANT'/'REVOKE'). The CASE here is total over the legal decision
--    domain.
INSERT INTO consent_journal_v2 (
  rowid,
  consent_id, subject, scope, decision, reason, granted_by,
  decided_at, expires_at, op_id, kind, sensor_id, actor,
  payload_json, decided_at_iso, expires_at_iso
)
SELECT
  rowid,
  consent_id, subject, scope, decision, reason, granted_by,
  decided_at, expires_at, op_id,
  COALESCE(
    kind,
    CASE decision WHEN 'GRANT' THEN 'grant' WHEN 'REVOKE' THEN 'revoke' END
  ) AS kind,
  sensor_id, actor,
  payload_json, decided_at_iso, expires_at_iso
FROM consent_journal;

-- 5. Drop old, rename new.
DROP TABLE consent_journal;
ALTER TABLE consent_journal_v2 RENAME TO consent_journal;

-- 6. Recreate indexes (identical to 0005 + 0009, except kind_idx no
--    longer needs the `WHERE kind IS NOT NULL` partial-index predicate).
CREATE INDEX consent_journal_subject_scope_idx
  ON consent_journal(subject, scope, decided_at);

CREATE INDEX consent_journal_op_idx
  ON consent_journal(op_id)
  WHERE op_id IS NOT NULL;

CREATE INDEX consent_journal_actor_idx
  ON consent_journal(actor, decided_at)
  WHERE actor IS NOT NULL;

CREATE INDEX consent_journal_sensor_idx
  ON consent_journal(sensor_id, decided_at)
  WHERE sensor_id IS NOT NULL;

CREATE INDEX consent_journal_kind_idx
  ON consent_journal(kind, decided_at);

-- 7. Re-attach 0005 append-only triggers verbatim.
CREATE TRIGGER consent_journal_immutable
  BEFORE UPDATE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal rows are immutable');
END;

CREATE TRIGGER consent_journal_no_delete
  BEFORE DELETE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal is append-only');
END;

-- 8. Re-attach 0009 event-shape triggers (kind-domain trigger is now
--    redundant with the column CHECK; permanently dropped above).
--    `WHEN NEW.kind IS NOT NULL AND …` guards are stripped — kind is
--    NOT NULL by column constraint.
CREATE TRIGGER consent_journal_event_requires_iso
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.decided_at_iso IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require decided_at_iso (RFC3339)');
END;

CREATE TRIGGER consent_journal_forget_receipt_body_free
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind = 'forget_intent'
   AND NEW.payload_json IS NOT NULL
   AND (
        NEW.payload_json LIKE '%"body"%'
     OR NEW.payload_json LIKE '%"text"%'
     OR NEW.payload_json LIKE '%"content"%'
     OR NEW.payload_json LIKE '%"raw"%'
     OR NEW.payload_json LIKE '%"snippet"%'
     OR NEW.payload_json LIKE '%"command"%'
     OR NEW.payload_json LIKE '%"url"%'
     OR NEW.payload_json LIKE '%"title"%'
     OR NEW.payload_json LIKE '%"file_path"%'
     OR NEW.payload_json LIKE '%"input"%'
   )
BEGIN
  SELECT RAISE(ABORT, 'forget_intent payload must be body-free (§14)');
END;

-- 9. Re-attach 0011 hardening triggers (verbatim except for stripped
--    `WHEN NEW.kind IS NOT NULL AND …` guards, which became tautological
--    once the column went NOT NULL). Each trigger keeps its
--    `DROP TRIGGER IF EXISTS` prelude defensively.

DROP TRIGGER IF EXISTS consent_journal_event_requires_actor;
CREATE TRIGGER consent_journal_event_requires_actor
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.actor IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require actor');
END;

DROP TRIGGER IF EXISTS consent_journal_event_requires_payload;
CREATE TRIGGER consent_journal_event_requires_payload
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN (NEW.payload_json IS NULL OR json_valid(NEW.payload_json) = 0)
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require valid JSON payload');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_shape_matches_kind;
CREATE TRIGGER consent_journal_payload_shape_matches_kind
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN (
        'sensor_enable',
        'sensor_disable',
        'policy_change',
        'remember_intent',
        'forget_intent',
        'grant',
        'revoke',
        'promote_receipt'
   )
   AND NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND (
        json_type(NEW.payload_json, '$.shape') IS NOT 'text'
     OR NOT (
        (NEW.kind IN ('sensor_enable', 'sensor_disable')
           AND json_extract(NEW.payload_json, '$.shape') = 'sensor_toggle')
     OR (NEW.kind = 'policy_change'
           AND json_extract(NEW.payload_json, '$.shape') = 'policy_delta')
     OR (NEW.kind IN ('remember_intent', 'forget_intent')
           AND json_extract(NEW.payload_json, '$.shape') = 'intent_receipt')
     OR (NEW.kind IN ('grant', 'revoke')
           AND json_extract(NEW.payload_json, '$.shape') = 'decision')
     OR (NEW.kind = 'promote_receipt'
           AND json_extract(NEW.payload_json, '$.shape') = 'promote_receipt')
        )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload shape must match kind');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_body_free;
CREATE TRIGGER consent_journal_payload_body_free
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND EXISTS (
     SELECT 1 FROM json_tree(NEW.payload_json)
      WHERE key IN (
        'body', 'text', 'content', 'raw', 'snippet', 'command',
        'url', 'title', 'file_path', 'input',
        'payload_text', 'user_input', 'message'
      )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload must be body-free (§14)');
END;

DROP TRIGGER IF EXISTS consent_journal_sensor_kind_requires_sensor_id;
CREATE TRIGGER consent_journal_sensor_kind_requires_sensor_id
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor kinds require sensor_id');
END;

DROP TRIGGER IF EXISTS consent_journal_sensor_id_matches_payload;
CREATE TRIGGER consent_journal_sensor_id_matches_payload
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
   AND NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND (
        json_type(NEW.payload_json, '$.sensor_label') IS NOT 'text'
     OR json_extract(NEW.payload_json, '$.sensor_label') IS NOT NEW.sensor_id
   )
BEGIN
  SELECT RAISE(ABORT,
    'consent_journal sensor_id must equal payload.sensor_label (and payload.sensor_label must be text)');
END;

DROP TRIGGER IF EXISTS consent_journal_sensor_subject_matches_sensor_id;
CREATE TRIGGER consent_journal_sensor_subject_matches_sensor_id
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
   AND (NEW.subject IS NULL OR NEW.subject IS NOT ('snr:' || NEW.sensor_id))
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor subject must be `snr:` + sensor_id');
END;

DROP TRIGGER IF EXISTS consent_journal_non_sensor_kind_forbids_sensor_id;
CREATE TRIGGER consent_journal_non_sensor_kind_forbids_sensor_id
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind NOT IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal non-sensor kinds must not carry sensor_id');
END;

DROP TRIGGER IF EXISTS consent_journal_hash_kind_subject_shape;
CREATE TRIGGER consent_journal_hash_kind_subject_shape
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('forget_intent', 'remember_intent', 'promote_receipt')
   AND NEW.subject IS NOT NULL
   AND NOT (
        (substr(NEW.subject, 1, 7) = 'sha256:'
           AND length(NEW.subject) = 71
           AND substr(NEW.subject, 8) NOT GLOB '*[^0-9a-f]*')
     OR (substr(NEW.subject, 1, 5) = 'hash:'
           AND length(NEW.subject) BETWEEN 37 AND 133
           AND substr(NEW.subject, 6) NOT GLOB '*[^0-9a-f]*')
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal hash-kind subject must be sha256:64hex or hash:32..128hex');
END;

DROP TRIGGER IF EXISTS consent_journal_hash_kind_target_id_hash_shape;
CREATE TRIGGER consent_journal_hash_kind_target_id_hash_shape
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('forget_intent', 'remember_intent', 'promote_receipt')
   AND NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND (
        json_type(NEW.payload_json, '$.target_id_hash') IS NOT 'text'
     OR NOT (
        (substr(json_extract(NEW.payload_json, '$.target_id_hash'), 1, 7) = 'sha256:'
           AND length(json_extract(NEW.payload_json, '$.target_id_hash')) = 71
           AND substr(json_extract(NEW.payload_json, '$.target_id_hash'), 8)
                 NOT GLOB '*[^0-9a-f]*')
     OR (substr(json_extract(NEW.payload_json, '$.target_id_hash'), 1, 5) = 'hash:'
           AND length(json_extract(NEW.payload_json, '$.target_id_hash')) BETWEEN 37 AND 133
           AND substr(json_extract(NEW.payload_json, '$.target_id_hash'), 6)
                 NOT GLOB '*[^0-9a-f]*')
        )
   )
BEGIN
  SELECT RAISE(ABORT,
    'consent_journal hash-kind payload.target_id_hash must be sha256:64hex or hash:32..128hex (text)');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_required_fields;
CREATE TRIGGER consent_journal_payload_required_fields
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND (
        (NEW.kind IN ('sensor_enable', 'sensor_disable')
           AND json_type(NEW.payload_json, '$.reason_code') IS NOT 'text')
     OR (NEW.kind = 'policy_change'
           AND (
                json_type(NEW.payload_json, '$.key') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.from_code') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.to_code') IS NOT 'text'
           ))
     OR (NEW.kind IN ('remember_intent', 'forget_intent')
           AND (
                json_type(NEW.payload_json, '$.scope_tier') IS NOT 'text'
             OR json_extract(NEW.payload_json, '$.scope_tier') NOT IN
                  ('private', 'session', 'project', 'team', 'org', 'public')
             OR json_type(NEW.payload_json, '$.reason_code') IS NOT 'text'
           ))
     OR (NEW.kind IN ('grant', 'revoke')
           AND (
                json_type(NEW.payload_json, '$.subject_code') IS NOT 'text'
             OR (json_type(NEW.payload_json, '$.policy_code') IS NOT 'text'
                  AND json_type(NEW.payload_json, '$.policy_code') IS NOT 'null'
                  AND json_type(NEW.payload_json, '$.policy_code') IS NOT NULL)
           ))
     OR (NEW.kind = 'promote_receipt'
           AND (
                json_type(NEW.payload_json, '$.from_tier') IS NOT 'text'
             OR json_extract(NEW.payload_json, '$.from_tier') NOT IN
                  ('private', 'session', 'project', 'team', 'org', 'public')
             OR json_type(NEW.payload_json, '$.to_tier') IS NOT 'text'
             OR json_extract(NEW.payload_json, '$.to_tier') NOT IN
                  ('private', 'session', 'project', 'team', 'org', 'public')
             OR json_type(NEW.payload_json, '$.receipt_id') IS NOT 'text'
           ))
   )
BEGIN
  SELECT RAISE(ABORT,
    'consent_journal payload missing or malformed required field for its shape');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_unknown_top_level_keys;
CREATE TRIGGER consent_journal_payload_unknown_top_level_keys
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND json_type(NEW.payload_json) = 'object'
   AND EXISTS (
     SELECT 1 FROM json_each(NEW.payload_json)
      WHERE key NOT IN (
        'shape',
        'sensor_label', 'reason_code',
        'key', 'from_code', 'to_code',
        'target_id_hash', 'scope_tier',
        'subject_code', 'policy_code',
        'from_tier', 'to_tier', 'receipt_id'
      )
        AND key NOT IN (
        'body', 'text', 'content', 'raw', 'snippet', 'command',
        'url', 'title', 'file_path', 'input',
        'payload_text', 'user_input', 'message'
      )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload has unknown top-level key');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_no_duplicate_keys;
CREATE TRIGGER consent_journal_payload_no_duplicate_keys
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND json_type(NEW.payload_json) = 'object'
   AND EXISTS (
     SELECT 1 FROM json_each(NEW.payload_json)
      GROUP BY key
     HAVING count(*) > 1
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload has duplicate top-level keys');
END;

DROP TRIGGER IF EXISTS consent_journal_subject_domain_for_non_hash_kinds;
CREATE TRIGGER consent_journal_subject_domain_for_non_hash_kinds
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.subject IS NOT NULL
   AND (
        (NEW.kind = 'policy_change'
           AND (
                length(NEW.subject) < 1
             OR length(NEW.subject) > 128
             OR NEW.subject GLOB '*[^a-z0-9._-]*'
           ))
     OR (NEW.kind IN ('grant', 'revoke')
           AND (
                length(NEW.subject) < 1
             OR length(NEW.subject) > 128
             OR NEW.subject GLOB '*[^a-z0-9._:-]*'
             OR substr(NEW.subject, 1, 1) NOT GLOB '[a-z]'
           ))
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal subject out of domain class for its kind');
END;

DROP TRIGGER IF EXISTS consent_journal_event_requires_positive_rowid;
CREATE TRIGGER consent_journal_event_requires_positive_rowid
  AFTER INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.rowid <= 0
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require positive rowid');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_keys_match_shape;
CREATE TRIGGER consent_journal_payload_keys_match_shape
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND json_type(NEW.payload_json) = 'object'
   AND json_type(NEW.payload_json, '$.shape') = 'text'
   AND EXISTS (
     SELECT 1 FROM json_each(NEW.payload_json) AS j
      WHERE j.key NOT IN (
        'body', 'text', 'content', 'raw', 'snippet', 'command',
        'url', 'title', 'file_path', 'input',
        'payload_text', 'user_input', 'message'
      )
        AND NOT (
          (json_extract(NEW.payload_json, '$.shape') = 'sensor_toggle'
             AND j.key IN ('shape', 'sensor_label', 'reason_code'))
       OR (json_extract(NEW.payload_json, '$.shape') = 'policy_delta'
             AND j.key IN ('shape', 'key', 'from_code', 'to_code'))
       OR (json_extract(NEW.payload_json, '$.shape') = 'intent_receipt'
             AND j.key IN ('shape', 'target_id_hash', 'scope_tier', 'reason_code'))
       OR (json_extract(NEW.payload_json, '$.shape') = 'decision'
             AND j.key IN ('shape', 'subject_code', 'policy_code'))
       OR (json_extract(NEW.payload_json, '$.shape') = 'promote_receipt'
             AND j.key IN ('shape', 'target_id_hash', 'from_tier', 'to_tier', 'receipt_id'))
        )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload key not allowed for its shape');
END;

DROP TRIGGER IF EXISTS consent_journal_payload_scalar_domains;
CREATE TRIGGER consent_journal_payload_scalar_domains
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND (
        (NEW.kind IN ('sensor_enable', 'sensor_disable',
                      'remember_intent', 'forget_intent')
           AND json_type(NEW.payload_json, '$.reason_code') = 'text'
           AND (
                length(json_extract(NEW.payload_json, '$.reason_code')) > 64
             OR length(json_extract(NEW.payload_json, '$.reason_code')) < 1
             OR json_extract(NEW.payload_json, '$.reason_code')
                  GLOB '*[^a-z0-9_-]*'
             OR substr(json_extract(NEW.payload_json, '$.reason_code'), 1, 1)
                  NOT GLOB '[a-z]'
           ))
     OR (NEW.kind = 'policy_change'
           AND (
                (json_type(NEW.payload_json, '$.key') = 'text'
                  AND (length(json_extract(NEW.payload_json, '$.key')) < 1
                       OR length(json_extract(NEW.payload_json, '$.key')) > 128
                       OR json_extract(NEW.payload_json, '$.key')
                            GLOB '*[^a-z0-9_.-]*'))
             OR (json_type(NEW.payload_json, '$.from_code') = 'text'
                  AND (length(json_extract(NEW.payload_json, '$.from_code')) > 64
                       OR json_extract(NEW.payload_json, '$.from_code')
                            GLOB '*[^a-z0-9_-]*'
                       OR substr(json_extract(NEW.payload_json, '$.from_code'), 1, 1)
                            NOT GLOB '[a-z]'))
             OR (json_type(NEW.payload_json, '$.to_code') = 'text'
                  AND (length(json_extract(NEW.payload_json, '$.to_code')) > 64
                       OR json_extract(NEW.payload_json, '$.to_code')
                            GLOB '*[^a-z0-9_-]*'
                       OR substr(json_extract(NEW.payload_json, '$.to_code'), 1, 1)
                            NOT GLOB '[a-z]'))
           ))
     OR (NEW.kind IN ('grant', 'revoke')
           AND (
                (json_type(NEW.payload_json, '$.subject_code') = 'text'
                  AND (length(json_extract(NEW.payload_json, '$.subject_code')) > 128
                       OR json_extract(NEW.payload_json, '$.subject_code')
                            GLOB '*[^a-z0-9._:-]*'
                       OR substr(json_extract(NEW.payload_json, '$.subject_code'), 1, 1)
                            NOT GLOB '[a-z]'))
             OR (json_type(NEW.payload_json, '$.policy_code') = 'text'
                  AND (length(json_extract(NEW.payload_json, '$.policy_code')) > 128
                       OR json_extract(NEW.payload_json, '$.policy_code')
                            GLOB '*[^a-z0-9._:-]*'
                       OR substr(json_extract(NEW.payload_json, '$.policy_code'), 1, 1)
                            NOT GLOB '[a-z]'))
           ))
     OR (NEW.kind = 'promote_receipt'
           AND json_type(NEW.payload_json, '$.receipt_id') = 'text'
           AND (
                length(json_extract(NEW.payload_json, '$.receipt_id')) > 128
             OR length(json_extract(NEW.payload_json, '$.receipt_id')) < 1
             OR json_extract(NEW.payload_json, '$.receipt_id')
                  GLOB '*[^A-Za-z0-9._:-]*'
           ))
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload scalar out of domain class');
END;

DROP TRIGGER IF EXISTS consent_journal_event_metadata_domains;
CREATE TRIGGER consent_journal_event_metadata_domains
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN (NEW.consent_id IS NULL
           OR length(NEW.consent_id) < 1
           OR length(NEW.consent_id) > 64
           OR NEW.consent_id GLOB '*[^A-Za-z0-9._:-]*')
     OR (NEW.scope IS NULL
           OR length(NEW.scope) < 1
           OR length(NEW.scope) > 256
           OR NEW.scope GLOB '*[^a-z0-9._:=,-]*')
     OR (NEW.op_id IS NOT NULL
           AND (length(NEW.op_id) < 1
                OR length(NEW.op_id) > 128
                OR NEW.op_id GLOB '*[^A-Za-z0-9._:-]*'))
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event metadata out of domain class');
END;

DROP TRIGGER IF EXISTS consent_journal_sensor_id_domain;
CREATE TRIGGER consent_journal_sensor_id_domain
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
   AND (
        length(NEW.sensor_id) < 1
     OR length(NEW.sensor_id) > 128
     OR NEW.sensor_id GLOB '*[^A-Za-z0-9._:-]*'
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor_id out of domain class');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (21, '0021_consent_kind_not_null', '', strftime('%s','now') * 1000);
