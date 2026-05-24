-- Migration 0068: extend consent_journal.kind CHECK constraint to allow
-- federation_grant / federation_accept / federation_revoke kinds, and update
-- the companion triggers to validate and accept federation payloads.
-- Brief §12.a (federation propagation audit trail).
--
-- Background: migration 0021 replaced the consent_journal_kind_domain
-- trigger (0009) with a column-level CHECK. Extending a column CHECK in
-- SQLite requires a full table rebuild.
--
-- Triggers updated in this migration:
--   consent_journal_payload_shape_matches_kind   -- add federation shape mappings
--   consent_journal_payload_required_fields       -- add federation required fields
--   consent_journal_payload_unknown_top_level_keys -- allow new federation keys
--   consent_journal_payload_keys_match_shape       -- add federation shape->key sets
--   consent_journal_payload_scalar_domains         -- add federation scalar validation
--   consent_journal_subject_domain_for_non_hash_kinds -- add federation subject class
-- All other triggers are reproduced verbatim from 0021.
--
-- NOTE: Triggers must be dropped from consent_journal BEFORE creating
-- same-named triggers on consent_journal_v2 (SQLite uses a global
-- namespace for trigger names within a database).

-- 1. Drop indexes and all triggers attached to consent_journal.
DROP INDEX IF EXISTS consent_journal_subject_scope_idx;
DROP INDEX IF EXISTS consent_journal_op_idx;
DROP INDEX IF EXISTS consent_journal_actor_idx;
DROP INDEX IF EXISTS consent_journal_sensor_idx;
DROP INDEX IF EXISTS consent_journal_kind_idx;

DROP TRIGGER IF EXISTS consent_journal_immutable;
DROP TRIGGER IF EXISTS consent_journal_no_delete;
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

-- 2. Build the new table with the extended kind CHECK.
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
    'source_forget',
    'grant',
    'revoke',
    'promote_receipt',
    'federation_grant',
    'federation_accept',
    'federation_revoke'
  )),
  sensor_id       TEXT,
  actor           TEXT,
  payload_json    TEXT,
  decided_at_iso  TEXT,
  expires_at_iso  TEXT
);

-- 3. Attach all triggers to consent_journal_v2 BEFORE the data move so
--    they gate the INSERT...SELECT row-by-row. After the rename in step 5,
--    SQLite automatically re-targets these triggers onto consent_journal.

-- 3a. Append-only guards (verbatim from 0021).
CREATE TRIGGER consent_journal_immutable
  BEFORE UPDATE ON consent_journal_v2
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal rows are immutable');
END;

CREATE TRIGGER consent_journal_no_delete
  BEFORE DELETE ON consent_journal_v2
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal is append-only');
END;

-- 3b. Event-shape triggers (verbatim from 0021/0009).
CREATE TRIGGER consent_journal_event_requires_iso
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.decided_at_iso IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require decided_at_iso (RFC3339)');
END;

CREATE TRIGGER consent_journal_forget_receipt_body_free
  BEFORE INSERT ON consent_journal_v2
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
  SELECT RAISE(ABORT, 'forget_intent payload must be body-free (s14)');
END;

-- 3c. Hardening triggers. Unchanged ones verbatim from 0021; updated
--     ones are annotated with the change.

CREATE TRIGGER consent_journal_event_requires_actor
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.actor IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require actor');
END;

CREATE TRIGGER consent_journal_event_requires_payload
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN (NEW.payload_json IS NULL OR json_valid(NEW.payload_json) = 0)
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require valid JSON payload');
END;

-- Updated: federation shape->kind mappings added.
CREATE TRIGGER consent_journal_payload_shape_matches_kind
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.kind IN (
        'sensor_enable',
        'sensor_disable',
        'policy_change',
        'remember_intent',
        'forget_intent',
        'source_forget',
        'grant',
        'revoke',
        'promote_receipt',
        'federation_grant',
        'federation_accept',
        'federation_revoke'
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
     OR (NEW.kind IN ('remember_intent', 'forget_intent', 'source_forget')
           AND json_extract(NEW.payload_json, '$.shape') = 'intent_receipt')
     OR (NEW.kind IN ('grant', 'revoke')
           AND json_extract(NEW.payload_json, '$.shape') = 'decision')
     OR (NEW.kind = 'promote_receipt'
           AND json_extract(NEW.payload_json, '$.shape') = 'promote_receipt')
     OR (NEW.kind = 'federation_grant'
           AND json_extract(NEW.payload_json, '$.shape') = 'federation_grant')
     OR (NEW.kind = 'federation_accept'
           AND json_extract(NEW.payload_json, '$.shape') = 'federation_accept')
     OR (NEW.kind = 'federation_revoke'
           AND json_extract(NEW.payload_json, '$.shape') = 'federation_revoke')
        )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload shape must match kind');
END;

CREATE TRIGGER consent_journal_payload_body_free
  BEFORE INSERT ON consent_journal_v2
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
  SELECT RAISE(ABORT, 'consent_journal payload must be body-free (s14)');
END;

CREATE TRIGGER consent_journal_sensor_kind_requires_sensor_id
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor kinds require sensor_id');
END;

CREATE TRIGGER consent_journal_sensor_id_matches_payload
  BEFORE INSERT ON consent_journal_v2
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

CREATE TRIGGER consent_journal_sensor_subject_matches_sensor_id
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
   AND (NEW.subject IS NULL OR NEW.subject IS NOT ('snr:' || NEW.sensor_id))
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor subject must be `snr:` + sensor_id');
END;

CREATE TRIGGER consent_journal_non_sensor_kind_forbids_sensor_id
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.kind NOT IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal non-sensor kinds must not carry sensor_id');
END;

CREATE TRIGGER consent_journal_hash_kind_subject_shape
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.kind IN ('forget_intent', 'remember_intent', 'source_forget', 'promote_receipt')
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

CREATE TRIGGER consent_journal_hash_kind_target_id_hash_shape
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.kind IN ('forget_intent', 'remember_intent', 'source_forget', 'promote_receipt')
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

-- Updated: federation required fields added.
CREATE TRIGGER consent_journal_payload_required_fields
  BEFORE INSERT ON consent_journal_v2
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
     OR (NEW.kind IN ('remember_intent', 'forget_intent', 'source_forget')
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
     OR (NEW.kind = 'federation_grant'
           AND (
                json_type(NEW.payload_json, '$.link_id') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.grant_tier') IS NOT 'text'
             OR json_extract(NEW.payload_json, '$.grant_tier') NOT IN
                  ('private', 'session', 'project', 'team', 'org', 'public')
             OR json_type(NEW.payload_json, '$.peer_code') IS NOT 'text'
           ))
     OR (NEW.kind = 'federation_accept'
           AND (
                json_type(NEW.payload_json, '$.link_id') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.grant_tier') IS NOT 'text'
             OR json_extract(NEW.payload_json, '$.grant_tier') NOT IN
                  ('private', 'session', 'project', 'team', 'org', 'public')
             OR json_type(NEW.payload_json, '$.applied_id_hashes') IS NOT 'array'
           ))
     OR (NEW.kind = 'federation_revoke'
           AND (
                json_type(NEW.payload_json, '$.link_id') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.reason_code') IS NOT 'text'
           ))
   )
BEGIN
  SELECT RAISE(ABORT,
    'consent_journal payload missing or malformed required field for its shape');
END;

-- Updated: federation fields added to allowed key set.
CREATE TRIGGER consent_journal_payload_unknown_top_level_keys
  BEFORE INSERT ON consent_journal_v2
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
        'from_tier', 'to_tier', 'receipt_id',
        'link_id', 'grant_tier', 'peer_code',
        'grantee_id_hash', 'applied_id_hashes'
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

CREATE TRIGGER consent_journal_payload_no_duplicate_keys
  BEFORE INSERT ON consent_journal_v2
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

-- Updated: federation subject class added (share_link: style like grant/revoke).
CREATE TRIGGER consent_journal_subject_domain_for_non_hash_kinds
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.subject IS NOT NULL
   AND (
        (NEW.kind = 'policy_change'
           AND (
                length(NEW.subject) < 1
             OR length(NEW.subject) > 128
             OR NEW.subject GLOB '*[^a-z0-9._-]*'
           ))
     OR (NEW.kind IN ('grant', 'revoke',
                      'federation_grant', 'federation_accept', 'federation_revoke')
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

CREATE TRIGGER consent_journal_event_requires_positive_rowid
  AFTER INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.rowid <= 0
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require positive rowid');
END;

-- Updated: federation shape->key sets added.
CREATE TRIGGER consent_journal_payload_keys_match_shape
  BEFORE INSERT ON consent_journal_v2
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
       OR (json_extract(NEW.payload_json, '$.shape') = 'federation_grant'
             AND j.key IN ('shape', 'link_id', 'grant_tier', 'peer_code', 'grantee_id_hash'))
       OR (json_extract(NEW.payload_json, '$.shape') = 'federation_accept'
             AND j.key IN ('shape', 'link_id', 'grant_tier', 'applied_id_hashes'))
       OR (json_extract(NEW.payload_json, '$.shape') = 'federation_revoke'
             AND j.key IN ('shape', 'link_id', 'reason_code'))
        )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload key not allowed for its shape');
END;

-- Updated: federation scalar domain checks added for peer_code.
CREATE TRIGGER consent_journal_payload_scalar_domains
  BEFORE INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.payload_json IS NOT NULL
   AND json_valid(NEW.payload_json) = 1
   AND (
        (NEW.kind IN ('sensor_enable', 'sensor_disable',
                      'remember_intent', 'forget_intent', 'source_forget',
                      'federation_revoke')
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
     OR (NEW.kind = 'federation_grant'
           AND (
                (json_type(NEW.payload_json, '$.peer_code') = 'text'
                  AND (length(json_extract(NEW.payload_json, '$.peer_code')) < 1
                       OR length(json_extract(NEW.payload_json, '$.peer_code')) > 64
                       OR json_extract(NEW.payload_json, '$.peer_code')
                            GLOB '*[^a-z0-9_-]*'
                       OR substr(json_extract(NEW.payload_json, '$.peer_code'), 1, 1)
                            NOT GLOB '[a-z]'))
           ))
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload scalar out of domain class');
END;

CREATE TRIGGER consent_journal_event_metadata_domains
  BEFORE INSERT ON consent_journal_v2
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

CREATE TRIGGER consent_journal_sensor_id_domain
  BEFORE INSERT ON consent_journal_v2
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

-- 4. Copy all rows. Every row already satisfies 0021 constraints; only
--    new rows may carry the new kinds.
INSERT INTO consent_journal_v2 (
  rowid,
  consent_id, subject, scope, decision, reason, granted_by,
  decided_at, expires_at, op_id, kind, sensor_id, actor,
  payload_json, decided_at_iso, expires_at_iso
)
SELECT
  rowid,
  consent_id, subject, scope, decision, reason, granted_by,
  decided_at, expires_at, op_id, kind, sensor_id, actor,
  payload_json, decided_at_iso, expires_at_iso
FROM consent_journal;

-- 5. Drop old table and rename new.
DROP TABLE consent_journal;
ALTER TABLE consent_journal_v2 RENAME TO consent_journal;

-- 6. Recreate indexes.
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

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (68, '0068_federation_consent_kinds', '', strftime('%s','now') * 1000);
