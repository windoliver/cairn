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

-- Receipt safety: forget_intent rows must not carry a payload that could
-- re-surface forgotten content. We bound the payload to a hashed-target
-- shape: payload_json must be NULL or a JSON object whose keys are a subset
-- of the §14 forget receipt allowlist {target_id_hash, op_id, purged_at,
-- reason_code}. Enforced via a defensive LIKE/length guard — full schema
-- validation lives in the core ConsentEvent type. The trigger here is the
-- last-line defense at the storage boundary.
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

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (7, '0007_consent_event', '', strftime('%s','now') * 1000);
