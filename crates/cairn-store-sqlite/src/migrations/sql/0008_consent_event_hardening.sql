-- Migration 0008: harden the 0007 consent_journal event log against
-- direct-SQL writers that bypass `cairn_core::domain::ConsentEvent::validate`.
-- Brief source: §14 (privacy / consent), §15 (forget receipts).
-- Issue source: #94, adversarial-review rounds 2..5.
--
-- 0007 added the columns + the kind-domain / iso / forget-body-free
-- triggers. This migration ADDs the rest of the journal-side invariants
-- so a direct INSERT cannot brick the async mirror by writing a row
-- whose payload `serde_json::from_str::<ConsentPayload>` cannot decode
-- (the consent_journal is append-only — a single bad row blocks the
-- mirror cursor forever).
--
-- WHY a separate migration: 0007 was applied in earlier branch states.
-- Per CLAUDE.md §6.11, applied migrations are immutable; new schema
-- belongs in a new migration file. The verifier in `verify.rs` hashes
-- compiled migration text against `schema_migrations.sql_hash`, so
-- mutating 0007 would surface as `SchemaDrift` on every existing vault.

-- Event-kind rows must carry actor + payload_json. Without the actor,
-- the mirror's `decode_event_inner` raises SchemaDrift; without a valid
-- payload, the mirror cannot decode the row at all.
CREATE TRIGGER consent_journal_event_requires_actor
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL AND NEW.actor IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require actor');
END;

CREATE TRIGGER consent_journal_event_requires_payload
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND (NEW.payload_json IS NULL OR json_valid(NEW.payload_json) = 0)
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require valid JSON payload');
END;

-- Payload `shape` discriminator must be present, of JSON text type, and
-- match the kind. A missing JSON path makes `json_extract` return NULL,
-- and a NULL WHEN clause does NOT fire, so we guard explicitly on
-- `json_type(...)` returning the literal `'text'`.
--
-- Gated on `kind` being in the §14 domain so the kind-domain trigger
-- (0007) remains the canonical violation when both could fire (SQLite
-- does not guarantee BEFORE INSERT trigger fire order).
CREATE TRIGGER consent_journal_payload_shape_matches_kind
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND NEW.kind IN (
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

-- General body-free: every event-kind row's payload must be free of
-- body-bearing keys at any depth (json_tree walks the decoded keys, so
-- JSON-escaped key names like `"body"` cannot bypass the check).
CREATE TRIGGER consent_journal_payload_body_free
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND NEW.payload_json IS NOT NULL
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

-- Sensor kinds require a non-NULL `sensor_id`. Without this, a direct-
-- SQL writer could insert a `sensor_enable` row with `sensor_id IS
-- NULL`, which `query_by_sensor` (a `sensor_id IS NOT NULL` index
-- predicate) would silently miss.
CREATE TRIGGER consent_journal_sensor_kind_requires_sensor_id
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor kinds require sensor_id');
END;

-- Sensor kinds must carry `payload.sensor_label` as a JSON text value
-- equal to `sensor_id`. Fires on missing / non-text values too — without
-- that branch, serde would fail to decode the row at mirror time.
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

-- Subject body must equal the sensor identity (`snr:` + label) for
-- sensor kinds. Together with the trigger above, the journal cannot
-- carry a sensor row whose subject points at a different sensor.
CREATE TRIGGER consent_journal_sensor_subject_matches_sensor_id
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
   AND (NEW.subject IS NULL OR NEW.subject IS NOT ('snr:' || NEW.sensor_id))
BEGIN
  SELECT RAISE(ABORT, 'consent_journal sensor subject must be `snr:` + sensor_id');
END;

-- Non-sensor kinds must NOT carry a sensor_id. Without this, a
-- direct-SQL writer could pin a non-sensor event to a sensor index,
-- polluting `query_by_sensor` results.
CREATE TRIGGER consent_journal_non_sensor_kind_forbids_sensor_id
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND NEW.kind NOT IN ('sensor_enable', 'sensor_disable')
   AND NEW.sensor_id IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal non-sensor kinds must not carry sensor_id');
END;

-- Hash-shape invariant for kinds whose `subject` MUST be a salted /
-- cryptographic digest (matches `validate_hash` in
-- `cairn-core::domain::consent`). Accepts `sha256:` + 64 lowercase hex
-- or `hash:` + 32..=128 lowercase hex. Note: SQLite GLOB uses `^` for
-- negated character classes, not `!`.
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

-- Hash-kind payloads MUST carry `target_id_hash` as a JSON text value
-- of the canonical hash shape. Fires on missing or non-text values too.
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

-- Required-field invariants for every payload variant. Without these,
-- a direct-SQL writer could ship a row whose JSON `shape` matches the
-- kind but whose required serde fields are missing — `read_since_rowid`
-- would then fail to decode the append-only row, blocking the mirror.
--
-- Required fields per shape (mirrors `cairn-core::domain::consent`):
--   sensor_toggle    : sensor_label (covered above), reason_code
--   policy_delta     : key, from_code, to_code
--   intent_receipt   : target_id_hash (covered above), scope_tier, reason_code
--   decision         : subject_code  (policy_code is optional)
--   promote_receipt  : target_id_hash (covered above), from_tier, to_tier, receipt_id
CREATE TRIGGER consent_journal_payload_required_fields
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND NEW.payload_json IS NOT NULL
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
             OR json_type(NEW.payload_json, '$.reason_code') IS NOT 'text'
           ))
     OR (NEW.kind IN ('grant', 'revoke')
           AND json_type(NEW.payload_json, '$.subject_code') IS NOT 'text')
     OR (NEW.kind = 'promote_receipt'
           AND (
                json_type(NEW.payload_json, '$.from_tier') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.to_tier') IS NOT 'text'
             OR json_type(NEW.payload_json, '$.receipt_id') IS NOT 'text'
           ))
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload missing a required field for its shape');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (8, '0008_consent_event_hardening', '', strftime('%s','now') * 1000);
