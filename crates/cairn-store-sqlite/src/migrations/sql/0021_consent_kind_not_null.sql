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
-- path have `kind IS NULL` and may also have NULL `actor`,
-- `payload_json`, `decided_at_iso` — none of which existed pre-0009.
-- The rebuild SYNTHESIZES every missing event-shape field at migration
-- time so that post-0021 readers can decode every row fail-closed,
-- without a structural NULL filter that would also hide ANY future
-- malformed row (and silently break the §14 audit invariant).
--
-- Legacy rows are NOT trusted on any nullable field — they get fixed
-- sentinel values regardless of what's in the column, because pre-0009
-- there were no domain checks on `granted_by` and pre-0011 there was no
-- shape gating on `payload_json` or `decided_at_iso`. Free-form pre-0009
-- values like `granted_by = 'tafeng'` (no `hmn:` prefix) would otherwise
-- survive the rebuild and brick `Identity::parse` at decode time. We
-- detect a "legacy row" structurally as `kind IS NULL` (the 0009
-- additive column is the only NULL-allowing column populated for every
-- post-0009 event row via the §14 append path).
--
-- Synthesis rules:
--   * `kind`            ← COALESCE(kind, decision -> grant/revoke). The
--                         only nullable field where COALESCE is safe:
--                         `decision` is gated by 0005's column CHECK,
--                         so it round-trips through the 0021 CHECK.
--   * `actor`           ← CASE WHEN kind IS NULL THEN 'hmn:legacy' ELSE
--                         actor END. Pre-0009 there was no `actor`
--                         column at all, so legacy rows have NULL there;
--                         we IGNORE `granted_by` (which is unconstrained
--                         pre-0009 and may carry free-form values like
--                         'tafeng'). `hmn:legacy` parses cleanly through
--                         `Identity::parse` (`hmn:` is one of the three
--                         §14 identity prefixes; body is non-empty).
--                         For event-kind rows (kind NOT NULL pre-0021),
--                         actor was written via `consent::append` which
--                         validates Identity shape, so we keep it as-is.
--   * `payload_json`    ← CASE WHEN kind IS NULL THEN body-free
--                         decision-shape sentinel ELSE payload_json END.
--                         Sentinel:
--                         `{"shape":"decision","subject_code":"legacy"}`.
--                         Shape pairs with the synthesized `grant`/
--                         `revoke` kind; keys match shape; subject_code
--                         is lower-snake; no body keys; no unknown
--                         top-level keys — i.e. every 0011 hardening
--                         trigger would accept it (although triggers do
--                         NOT fire on this rebuild INSERT — they are
--                         dropped + re-attached around the data move per
--                         the next paragraph). For event-kind rows,
--                         payload_json was written by `consent::append`
--                         under the 0011 hardening triggers, so we keep
--                         it as-is.
--   * `decided_at_iso`  ← CASE WHEN kind IS NULL THEN strftime(...) ELSE
--                         decided_at_iso END. The legacy UNIX-millis
--                         integer (0 or any value) is rendered as
--                         RFC3339 via strftime deterministically; e.g.
--                         `0` → `1970-01-01T00:00:00Z`. Seconds
--                         resolution is fine — nothing in the schema
--                         requires sub-sec precision and 0005 only
--                         stored UNIX millis. For event-kind rows,
--                         decided_at_iso is gated by the 0009
--                         `consent_journal_event_requires_iso` trigger,
--                         so we keep it as-is.
--   * `rowid`           ← preserved 1:1 unconditionally (mirror cursor
--                         stability AND replay-order preservation).
--                         Pathological `rowid <= 0` rows (only reachable
--                         on legacy pre-0011 paths) would have to be
--                         renumbered to satisfy the post-0021 positive-
--                         rowid invariant — but renumbering reorders
--                         historical events relative to the consent
--                         readers' `WHERE rowid > cursor ORDER BY rowid`
--                         tail. Rather than silently reorder events, we
--                         FAIL CLOSED: a pre-rebuild SQL assert (see
--                         step 0 below) ABORTs the migration when any
--                         legacy `rowid <= 0` row exists, leaving the
--                         operator to clean such rows manually before
--                         re-running the upgrade.
--
-- Pre-rebuild fail-closed asserts (step 0): two invariants on the
-- legacy data must hold before the rebuild proceeds, because the
-- rebuild SELECT cannot repair them without changing observable
-- semantics:
--   1. No legacy row may have `rowid <= 0` — see the rowid synthesis
--      rule above for why renumbering is unsafe.
--   2. Every legacy row's `decided_at` must be representable as RFC3339
--      via `strftime`. SQLite's strftime returns NULL for out-of-range
--      UNIX seconds (e.g. UNIX millis values past year 9999). A NULL
--      result would silently insert a row with `kind != NULL` AND
--      `decided_at_iso = NULL`; the event readers gate on
--      `kind IS NOT NULL` and would surface it; decode would then fail
--      on the missing iso field, bricking the consent mirror.
-- Both asserts use the temp-table-with-trigger idiom: a TEMP TABLE
-- guarded by a BEFORE INSERT trigger that RAISE(ABORT)s when a
-- non-zero count is inserted. Aborting before any DROP/CREATE/data-
-- move runs leaves the existing schema untouched (the migration
-- transaction rolls back). Operators must resolve the offending rows
-- manually before re-running the migration.
--
-- The broader 0011 hardening triggers gate INSERTs into the new table,
-- but they only fire when columns they require are populated. We drop
-- the 0011 hardening triggers BEFORE the data move and re-attach them
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

-- 0. Pre-rebuild fail-closed asserts. MUST run before any DROP /
--    CREATE / data-move SQL so an aborted assert leaves the existing
--    schema fully intact (SQLite rolls back the migration transaction).
--    The temp-table + BEFORE INSERT trigger pattern is pure SQL: the
--    trigger fires on the INSERT, sees a non-zero count, and RAISEs
--    ABORT with a stable message that the migration test asserts on.
--    TEMP objects are scoped to the connection and dropped at the end
--    of step 0 so subsequent migrations see a clean namespace.

-- 0a. Abort if any legacy row has rowid <= 0 (Finding 1).
CREATE TEMP TABLE __cairn_assert_legacy_rowid (n INTEGER);
CREATE TEMP TRIGGER __cairn_assert_legacy_rowid_trg
  BEFORE INSERT ON __cairn_assert_legacy_rowid
  FOR EACH ROW WHEN NEW.n > 0
BEGIN
  SELECT RAISE(ABORT, 'migration 0021: consent_journal contains legacy row(s) with rowid <= 0; cannot promote without changing replay order. Repair tool tracked in issue #267; until then, resolve manually before re-running migration.');
END;
INSERT INTO __cairn_assert_legacy_rowid (n)
  SELECT COUNT(*) FROM consent_journal WHERE kind IS NULL AND rowid <= 0;
DROP TRIGGER __cairn_assert_legacy_rowid_trg;
DROP TABLE __cairn_assert_legacy_rowid;

-- 0b. Abort if any legacy row's decided_at can't render as RFC3339
--     (Finding 2). strftime returns NULL for out-of-range UNIX seconds.
CREATE TEMP TABLE __cairn_assert_legacy_iso (n INTEGER);
CREATE TEMP TRIGGER __cairn_assert_legacy_iso_trg
  BEFORE INSERT ON __cairn_assert_legacy_iso
  FOR EACH ROW WHEN NEW.n > 0
BEGIN
  SELECT RAISE(ABORT, 'migration 0021: consent_journal contains legacy row(s) whose decided_at cannot be rendered as RFC3339 (out-of-range UNIX millis). Repair tool tracked in issue #267; until then, resolve manually before re-running migration.');
END;
INSERT INTO __cairn_assert_legacy_iso (n)
  SELECT COUNT(*) FROM consent_journal
   WHERE kind IS NULL
     AND strftime('%Y-%m-%dT%H:%M:%SZ', decided_at / 1000, 'unixepoch') IS NULL;
DROP TRIGGER __cairn_assert_legacy_iso_trg;
DROP TABLE __cairn_assert_legacy_iso;

-- 0c. Abort if any legacy row's metadata violates the 0011 domain classes
--     (Finding: round-6 high). Pre-0011 schema didn't enforce closed
--     character classes on consent_id / subject / scope / op_id, so
--     historical rows can carry free-form text. Without this assert, the
--     rebuild would copy unsanitized values into the new table and the
--     async mirror (which now resets its cursor at 0021 per round 5) would
--     replay them into `consent.log`. `decode_event_inner` checks event
--     shape but not the metadata domain, so out-of-domain values reach
--     consumers. We FAIL CLOSED here so the operator repairs the data
--     manually before re-running migration (issue #267 tracks the repair
--     tool).
--
--     Domain classes (mirror of 0011's
--     consent_journal_event_metadata_domains and
--     consent_journal_subject_domain_for_non_hash_kinds for grant/revoke,
--     which is what every legacy row becomes via the kind-coalesce in
--     step 4):
--       consent_id : 1..=64,  [A-Za-z0-9._:-]
--       scope      : 1..=256, [a-z0-9._:=,-]
--       op_id      : 1..=128, [A-Za-z0-9._:-] (NULL allowed)
--       subject    : 1..=128, [a-z0-9._:-], first char [a-z]
CREATE TEMP TABLE __cairn_assert_legacy_metadata (n INTEGER);
CREATE TEMP TRIGGER __cairn_assert_legacy_metadata_trg
  BEFORE INSERT ON __cairn_assert_legacy_metadata
  FOR EACH ROW WHEN NEW.n > 0
BEGIN
  SELECT RAISE(ABORT, 'migration 0021: consent_journal contains legacy row(s) whose consent_id/subject/scope/op_id violate the 0011 domain classes; cannot promote without sanitization. Resolve manually before re-running migration (issue #267).');
END;
INSERT INTO __cairn_assert_legacy_metadata (n)
  SELECT COUNT(*) FROM consent_journal
   WHERE kind IS NULL
     AND (
          consent_id IS NULL
       OR length(consent_id) < 1
       OR length(consent_id) > 64
       OR consent_id GLOB '*[^A-Za-z0-9._:-]*'
       OR scope IS NULL
       OR length(scope) < 1
       OR length(scope) > 256
       OR scope GLOB '*[^a-z0-9._:=,-]*'
       OR (op_id IS NOT NULL
            AND (length(op_id) < 1
                 OR length(op_id) > 128
                 OR op_id GLOB '*[^A-Za-z0-9._:-]*'))
       OR subject IS NULL
       OR length(subject) < 1
       OR length(subject) > 128
       OR subject GLOB '*[^a-z0-9._:-]*'
       OR substr(subject, 1, 1) NOT GLOB '[a-z]'
     );
DROP TRIGGER __cairn_assert_legacy_metadata_trg;
DROP TABLE __cairn_assert_legacy_metadata;

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

-- 4. Move data. Synthesize every event-shape field for legacy 0005 rows
--    so every post-0021 row decodes as a fully-formed ConsentEvent. See
--    the head-of-file synthesis-rules comment for justification of each
--    expression and for why this is preferable to a structural NULL
--    filter in the readers (which would also hide future drift).
--
--    Legacy detection: `kind IS NULL` (the 0009 additive column is the
--    only NULL-allowing column populated for every post-0009 event row).
--    For legacy rows we use `CASE WHEN kind IS NULL THEN <sentinel> ELSE
--    <existing-column> END` to UNCONDITIONALLY override pre-existing
--    nullable columns with sentinels — NOT `COALESCE(<existing>,
--    <sentinel>)`, because COALESCE would trust whatever non-NULL value
--    sits in the column and pre-0009/pre-0011 there were no domain
--    checks to gate those values. `kind` itself is the one exception:
--    `COALESCE(kind, decision -> grant/revoke)` is safe because
--    `decision` is gated by 0005's column CHECK to GRANT|REVOKE.
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
  sensor_id,
  CASE WHEN kind IS NULL THEN 'hmn:legacy' ELSE actor END AS actor,
  CASE
    WHEN kind IS NULL
      THEN '{"shape":"decision","subject_code":"legacy"}'
    ELSE payload_json
  END AS payload_json,
  CASE
    WHEN kind IS NULL
      THEN strftime('%Y-%m-%dT%H:%M:%SZ', decided_at / 1000, 'unixepoch')
    ELSE decided_at_iso
  END AS decided_at_iso,
  expires_at_iso
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

-- 10. Mirror cursor reset marker (round-5 review follow-up). Existing
--     vaults' .cairn/consent.cursor sidecar may already point ABOVE the
--     legacy `kind IS NULL` rowids that this migration just promoted to
--     event-shape rows (the async mirror in cairn-workflows tailed only
--     event-kind rows pre-0021, so legacy rowids were silently skipped).
--     Without intervention, the next mirror tick would still skip them
--     because `WHERE rowid > cursor` filters them out.
--
--     Rather than reach across crate boundaries to delete the sidecar
--     file, we leave a marker in the DB. The mirror's tick() reads the
--     unconsumed markers, calls rebuild_from_db() (which atomically
--     replaces the on-disk log by replaying from rowid 0), then marks
--     the row consumed. Idempotent under repeated ticks; safe under
--     concurrent mirror processes (the mirror holds the consent.lock
--     advisory file lock during the rebuild).
--
--     `consumed` is an INTEGER (0/1) rather than BOOLEAN because SQLite
--     stores booleans as integers anyway and the column-affinity rules
--     are clearer. UPDATEs on this table are unconstrained — the
--     append-only triggers attach to consent_journal only.
CREATE TABLE IF NOT EXISTS consent_mirror_resets (
  migration_id INTEGER NOT NULL PRIMARY KEY,
  applied_at   INTEGER NOT NULL,
  consumed     INTEGER NOT NULL DEFAULT 0
);

INSERT INTO consent_mirror_resets (migration_id, applied_at, consumed)
  VALUES (21, strftime('%s','now') * 1000, 0);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (21, '0021_consent_kind_not_null', '', strftime('%s','now') * 1000);
