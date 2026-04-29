-- Migration 0007: extend consent_journal for the broader event log.
-- Brief source: §14 (privacy / consent), §3 line 448 (consent_journal columns),
-- issue #94 (queryable by operation, identity, sensor, scope; mirror feed).
--
-- Adds the columns needed by the async `.cairn/consent.log` materializer:
--   * op_id        — links the event to wal_ops.operation_id (when applicable)
--   * kind         — event type (sensor toggle, policy change, intent, …)
--   * sensor_id    — populated when kind ∈ {sensor_enable, sensor_disable}
--   * actor        — principal that authored the event (HumanIdentity/Agent)
--   * payload_json — body-free structured metadata, validated by callers
--
-- All new columns are nullable so rows written by migration 0005 stay valid.
-- New writers MUST set kind; the kind-domain CHECK is enforced by trigger.
-- Migration 0005's append-only triggers (consent_journal_immutable,
-- consent_journal_no_delete) continue to apply unchanged.

ALTER TABLE consent_journal ADD COLUMN op_id           TEXT;
ALTER TABLE consent_journal ADD COLUMN kind            TEXT;
ALTER TABLE consent_journal ADD COLUMN sensor_id       TEXT;
ALTER TABLE consent_journal ADD COLUMN actor           TEXT;
ALTER TABLE consent_journal ADD COLUMN payload_json    TEXT;
-- RFC3339 mirror of decided_at — written by the new event-kind path so the
-- async materializer can emit human-readable timestamps without an in-store
-- millis↔RFC3339 conversion. Legacy GRANT/REVOKE rows leave this NULL.
ALTER TABLE consent_journal ADD COLUMN decided_at_iso  TEXT;
-- Optional RFC3339 mirror of expires_at for the same reason.
ALTER TABLE consent_journal ADD COLUMN expires_at_iso  TEXT;

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
  ON consent_journal(kind, decided_at)
  WHERE kind IS NOT NULL;

-- Domain check on `kind`. ALTER TABLE cannot add CHECK constraints, so we
-- gate at INSERT time with a trigger. Old rows (kind IS NULL) are exempt;
-- new rows MUST set a recognized event type.
CREATE TRIGGER consent_journal_kind_domain
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND NEW.kind NOT IN (
        'sensor_enable',
        'sensor_disable',
        'policy_change',
        'remember_intent',
        'forget_intent',
        'grant',
        'revoke',
        'promote_receipt'
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal.kind not in §14 domain');
END;

-- Event-kind rows must carry the RFC3339 decided_at mirror so the async
-- materializer can emit a human-readable consent.log without needing a
-- millis↔ISO converter at the storage boundary. Legacy rows (kind IS NULL)
-- are exempt.
CREATE TRIGGER consent_journal_event_requires_iso
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind IS NOT NULL
   AND NEW.decided_at_iso IS NULL
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require decided_at_iso (RFC3339)');
END;

-- Event-kind rows must carry actor + payload_json + valid JSON. Without
-- the actor, the mirror's `decode_event_inner` raises SchemaDrift; without
-- a valid payload, the mirror cannot decode the row at all and would
-- block forever on append-only retention.
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

-- Payload `shape` discriminator must be present and match the kind.
-- Without this, a direct-SQL writer could insert a row with an empty
-- object `{}` or a wrong shape; serde would reject it at decode time
-- and the append-only row would permanently block the mirror cursor.
--
-- This trigger is gated on `kind` being in the §14 domain so the kind
-- domain trigger remains the canonical violation when both could fire
-- (SQLite does not guarantee BEFORE INSERT trigger fire order).
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
   AND NOT (
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
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload shape must match kind');
END;

-- Receipt safety: every event-kind row must be body-free. The full
-- validator lives in the core `ConsentEvent::validate`; this trigger is
-- the last-line defense at the storage boundary against any direct-SQL
-- writer that bypasses the domain type. Round-2 hardening: we walk the
-- decoded keys via SQLite's JSON1 `json_tree`, so JSON-escaped key names
-- (`"body"` → `"body"`) cannot bypass the check.
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

-- Forget-intent rows get a kind-specific error message so operators can
-- tell at a glance the violation came from a forget receipt. Same rule
-- as the general body-free trigger; redundant on purpose.
CREATE TRIGGER consent_journal_forget_receipt_body_free
  BEFORE INSERT ON consent_journal
  FOR EACH ROW
  WHEN NEW.kind = 'forget_intent'
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
  SELECT RAISE(ABORT, 'forget_intent payload must be body-free (§14)');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (7, '0007_consent_event', '', strftime('%s','now') * 1000);
