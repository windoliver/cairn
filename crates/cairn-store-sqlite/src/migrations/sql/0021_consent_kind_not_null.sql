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
--                         trigger accepts it. Post-rebuild the trigger
--                         gating fires on this synthesized row (see the
--                         "trigger-gated data move" paragraph below) and
--                         must accept it; the sentinel is engineered to
--                         pass every 0011 trigger.
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
--                         step 0a below) ABORTs the migration when any
--                         legacy `rowid <= 0` row exists, leaving the
--                         operator to clean such rows manually before
--                         re-running the upgrade.
--
-- TRIGGER-GATED DATA MOVE (round-9 structural fix). Earlier rounds of
-- this migration tried to enumerate every 0011 invariant in an explicit
-- preflight (step 0d) so that pre-0011 event-kind rows violating any of
-- them would abort the migration cleanly. That approach kept finding new
-- gaps each review round (round-7: actor/payload/iso; round-8: subject
-- domains, sensor_id presence, hash-subject shape; round-9: payload
-- required-fields, payload shape-match) because step 0d was duplicating
-- 0011 trigger logic and falling out of sync. The structural fix is to
-- ATTACH the 0011 hardening triggers (and the 0009 iso/body-free + 0005
-- immutable/no_delete triggers) to `consent_journal_v2` BEFORE the
-- INSERT…SELECT data move, and let them gate the data move row-by-row.
-- Any pre-0011 event-kind row that violates ANY 0011 invariant aborts
-- the migration with the trigger's own error message — no enumeration
-- in this migration. After the rename, SQLite re-targets the triggers
-- onto the renamed table automatically, so they continue to gate
-- post-migration writes to `consent_journal` exactly as before.
--
-- Pre-rebuild fail-closed asserts (steps 0a–0c) STAY for the cases the
-- trigger gating cannot catch with a friendly message:
--   1. Legacy `rowid <= 0` (step 0a). The AFTER INSERT positive-rowid
--      trigger would fire on the data move, but renumbering would be
--      silent and reorder historical events. A friendly preflight
--      message points the operator at issue #267 instead.
--   2. Legacy unrenderable `decided_at` (step 0b). strftime returns
--      NULL, which would then trip the iso-required trigger with a
--      generic message. The preflight points at the data-shape issue
--      directly.
--   3. Legacy metadata domain (step 0c, legacy half). The metadata-
--      domain trigger would catch these, but legacy detection (`kind
--      IS NULL`) lets us route the operator to a legacy-data-repair
--      message instead of the generic trigger one.
--   4. Event-kind `decided_at_iso` text shape (step 0c, event half).
--      The 0011 iso trigger only checks non-null; SQL has no portable
--      RFC3339 parse. We do a structural sniff (length + separator
--      positions) to catch the gross-failure cases like
--      `'not-an-rfc3339'`. `consent::append` validates iso content via
--      `Rfc3339Timestamp::parse` for any post-0009 event write, so
--      this only matters for hand-rolled rows in test fixtures or
--      external tooling.
--
-- ROWID PRESERVATION: the async mirror in cairn-workflows tails
-- `consent_journal` by `rowid`. The rebuild preserves rowid 1:1 via
-- `INSERT INTO new(rowid, …) SELECT rowid, …`. Any cursor sidecar at
-- `.cairn/consent.cursor` written by an older incarnation continues to
-- point at the right row after upgrade.
--
-- Append-only triggers (0005's consent_journal_immutable +
-- consent_journal_no_delete) are dropped + re-attached unchanged. They
-- block UPDATE/DELETE, not INSERT, so they do not interfere with the
-- INSERT…SELECT data move.
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
--    of each step so subsequent migrations see a clean namespace.

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

-- 0a-bis. Abort if any kind-NULL row carries populated post-0009 event
--    fields (round-12 high finding, extended round-13). True 0005-era
--    legacy rows are kind-NULL AND have all of (actor, payload_json,
--    decided_at_iso, expires_at_iso, op_id, sensor_id) NULL — pre-0009
--    schema didn't have any of those columns. The 0009 event-shape
--    trigger only gated on `kind IS NOT NULL`, so a direct-SQL caller
--    could insert a row with kind=NULL but the other event fields
--    populated; the synthesis step below (`CASE WHEN kind IS NULL THEN
--    <sentinel> ELSE <existing> END`) would silently overwrite the real
--    authorship/payload/iso with sentinels while preserving any
--    orphaned expires_at_iso/op_id/sensor_id (which the synthesis
--    leaves untouched), yielding a structurally-corrupted row. Fail
--    closed on ANY non-NULL event-shape field instead so the operator
--    can repair the drift manually rather than losing audit data.
--    Round-13 high finding: the original guard missed expires_at_iso /
--    op_id / sensor_id; a kind-NULL row populated only with one of
--    those would slip past and produce a frankenrow.
CREATE TEMP TABLE __cairn_assert_legacy_drift (n INTEGER);
CREATE TEMP TRIGGER __cairn_assert_legacy_drift_trg
  BEFORE INSERT ON __cairn_assert_legacy_drift
  FOR EACH ROW WHEN NEW.n > 0
BEGIN
  SELECT RAISE(ABORT,
    'migration 0021: consent_journal contains kind-NULL row(s) with populated event fields (post-0009 direct-SQL drift); blanket-synthesizing legacy sentinels would destroy authorship/payload data. Resolve manually before re-running migration (issue #267).');
END;
INSERT INTO __cairn_assert_legacy_drift (n)
  SELECT COUNT(*) FROM consent_journal
   WHERE kind IS NULL
     AND (actor IS NOT NULL
       OR payload_json IS NOT NULL
       OR decided_at_iso IS NOT NULL
       OR expires_at_iso IS NOT NULL
       OR op_id IS NOT NULL
       OR sensor_id IS NOT NULL);
DROP TRIGGER __cairn_assert_legacy_drift_trg;
DROP TABLE __cairn_assert_legacy_drift;

-- 0c. Two-part metadata preflight:
--     (legacy half) Legacy rows' consent_id / scope / op_id / subject
--     domain classes — see the round-6 high finding. Pre-0011 schema
--     didn't enforce closed character classes; without this assert the
--     rebuild would copy unsanitized values into the new table. The
--     metadata-domain trigger we attach below would catch them too,
--     but its message is generic. We FAIL CLOSED with a legacy-aware
--     message so the operator routes to issue #267.
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
--
--     (event half) Event-kind rows' decided_at_iso AND expires_at_iso
--     (when present) must parse as a real datetime (semantic check via
--     SQLite's `datetime()` — returns NULL on unparseable input, so
--     e.g. '2026-99-99T99:99:99Z' is correctly rejected even though
--     it's structurally RFC3339-shaped). A structural sniff (length
--     20..=35 + separator positions) is layered in front for clearer
--     error messages on gross failures like `'not-an-rfc3339'`.
--     Round-10 High finding: structural-only sniff admitted
--     impossible-date values that later bricked
--     `Rfc3339Timestamp::parse` at decode.
--
--     CASE-INSENSITIVE: `Rfc3339Timestamp::parse` (in
--     cairn-core/src/domain/timestamp.rs) explicitly accepts both
--     `T`/`t` separators and `Z`/`z` UTC offsets. SQLite's `datetime()`
--     and naive `GLOB 'T'` are case-strict, so a legitimate row with
--     `'2026-04-22t14:02:11Z'` (lowercase t) would be rejected here
--     but accepted by `Rfc3339Timestamp::parse` post-migration. We
--     case-normalize via `[tT]` GLOB on the structural separator and
--     `upper()` on the `datetime()` argument to match the parser's
--     grammar exactly. Round-11 High finding.
--
--     (event half, actor) Event-kind rows' actor must match the
--     Identity wire format `<prefix>:<body>` where prefix ∈
--     {hmn, agt, snr} (see cairn-core/src/domain/identity/mod.rs)
--     and body ⊆ [A-Za-z0-9._:-]. Pre-0011 the actor column existed
--     (since 0009) but was domain-unchecked; the 0011
--     `consent_journal_event_requires_actor` trigger only catches
--     NULL. Round-10 High finding: a row with
--     `actor='not-an-identity'` would survive the rebuild and brick
--     `Identity::parse` in `decode_event_inner` after the cursor
--     reset.
CREATE TEMP TABLE __cairn_assert_metadata (n INTEGER);
CREATE TEMP TRIGGER __cairn_assert_metadata_trg
  BEFORE INSERT ON __cairn_assert_metadata
  FOR EACH ROW WHEN NEW.n > 0
BEGIN
  SELECT RAISE(ABORT, 'migration 0021: consent_journal contains legacy row(s) whose consent_id/subject/scope/op_id violate the 0011 domain classes, or event-kind row(s) whose decided_at_iso/expires_at_iso is not RFC3339 (parsed via SQLite datetime() with case-normalized t/z) or whose actor is not a valid Identity wire form (hmn:|agt:|snr: + [A-Za-z0-9._:-]); cannot promote without sanitization. Resolve manually before re-running migration (issue #267).');
END;
INSERT INTO __cairn_assert_metadata (n)
  SELECT COUNT(*) FROM consent_journal
   WHERE
     -- legacy half
     (kind IS NULL
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
      ))
  OR
     -- event half: decided_at_iso must be plausibly RFC3339-shaped
     -- (structural sniff for clear errors on gross failures) AND
     -- semantically parseable as a real datetime (round-10: catches
     -- impossible dates like '2026-99-99T99:99:99Z' that the
     -- structural sniff admits). datetime() returns NULL on
     -- unparseable input. Round-11: case-normalize `t`/`z` to match
     -- `Rfc3339Timestamp::parse` grammar (which accepts either case);
     -- `[tT]` glob + `upper()` on the datetime() arg.
     (kind IS NOT NULL
      AND decided_at_iso IS NOT NULL
      AND (length(decided_at_iso) < 20
           OR length(decided_at_iso) > 35
           OR substr(decided_at_iso, 5, 1) NOT GLOB '-'
           OR substr(decided_at_iso, 8, 1) NOT GLOB '-'
           OR substr(decided_at_iso, 11, 1) NOT GLOB '[tT]'
           OR substr(decided_at_iso, 14, 1) NOT GLOB ':'
           OR substr(decided_at_iso, 17, 1) NOT GLOB ':'
           OR datetime(upper(decided_at_iso)) IS NULL))
  OR
     -- event half: expires_at_iso must be plausibly RFC3339-shaped
     -- and semantically parseable when present (round-11 High
     -- finding: pre-0021 expires_at_iso was nullable and unchecked,
     -- so a malformed value like '2026-99-99T99:99:99Z' would
     -- survive into v2 and brick `Rfc3339Timestamp::parse` at decode
     -- after the cursor reset). Same case-insensitive grammar as
     -- decided_at_iso. Apply on event-kind rows only — pre-0009
     -- schema had no expires_at_iso column, so legacy rows always
     -- have NULL there; we still gate by `expires_at_iso IS NOT NULL`
     -- defensively so the predicate is well-defined under either kind.
     (kind IS NOT NULL
      AND expires_at_iso IS NOT NULL
      AND (length(expires_at_iso) < 20
           OR length(expires_at_iso) > 35
           OR substr(expires_at_iso, 5, 1) NOT GLOB '-'
           OR substr(expires_at_iso, 8, 1) NOT GLOB '-'
           OR substr(expires_at_iso, 11, 1) NOT GLOB '[tT]'
           OR substr(expires_at_iso, 14, 1) NOT GLOB ':'
           OR substr(expires_at_iso, 17, 1) NOT GLOB ':'
           OR datetime(upper(expires_at_iso)) IS NULL))
  OR
     -- legacy half: defensively gate expires_at_iso even on legacy
     -- (kind IS NULL) rows. Pre-0009 the column didn't exist, so
     -- legacy rows should always have NULL here, but we cover both
     -- halves so the preflight is robust to schema drift.
     (kind IS NULL
      AND expires_at_iso IS NOT NULL
      AND (length(expires_at_iso) < 20
           OR length(expires_at_iso) > 35
           OR substr(expires_at_iso, 5, 1) NOT GLOB '-'
           OR substr(expires_at_iso, 8, 1) NOT GLOB '-'
           OR substr(expires_at_iso, 11, 1) NOT GLOB '[tT]'
           OR substr(expires_at_iso, 14, 1) NOT GLOB ':'
           OR substr(expires_at_iso, 17, 1) NOT GLOB ':'
           OR datetime(upper(expires_at_iso)) IS NULL))
  OR
     -- event half: actor must match the Identity wire format
     -- (round-10): <prefix>:<body> where prefix ∈ {hmn, agt, snr}
     -- and body is non-empty in [A-Za-z0-9._:-]. The 0011
     -- requires-actor trigger only catches NULL; pre-0011 a row
     -- with a non-NULL but malformed actor would survive into v2
     -- and brick Identity::parse at decode.
     (kind IS NOT NULL
      AND actor IS NOT NULL
      AND NOT (
           (substr(actor, 1, 4) = 'hmn:' AND length(actor) > 4
              AND substr(actor, 5) NOT GLOB '*[^A-Za-z0-9._:-]*')
        OR (substr(actor, 1, 4) = 'agt:' AND length(actor) > 4
              AND substr(actor, 5) NOT GLOB '*[^A-Za-z0-9._:-]*')
        OR (substr(actor, 1, 4) = 'snr:' AND length(actor) > 4
              AND substr(actor, 5) NOT GLOB '*[^A-Za-z0-9._:-]*')
      ));
DROP TRIGGER __cairn_assert_metadata_trg;
DROP TABLE __cairn_assert_metadata;

-- 1. Drop ALL triggers attached to the old consent_journal. The
--    immutable + no_delete + 0009 iso/body-free + 0011 hardening
--    triggers will be re-created against `consent_journal_v2` BEFORE
--    the data move (so they gate it row-by-row). The 0009
--    `consent_journal_kind_domain` trigger is permanently retired —
--    the new column CHECK supersedes it.
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

-- 4. Attach all triggers to consent_journal_v2 BEFORE the data move so
--    they gate the INSERT…SELECT row-by-row. After the rename in step
--    6, SQLite automatically re-targets these triggers onto the renamed
--    `consent_journal` table; the trigger names stay stable and match
--    the verify.rs schema fingerprint.
--
--    The 0005 immutable + no_delete triggers gate UPDATE / DELETE only
--    (not INSERT), so they do not interfere with the data move.

-- 4a. 0005 append-only triggers verbatim.
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

-- 4b. 0009 event-shape triggers (kind-domain trigger is now redundant
--     with the column CHECK; permanently dropped above). `WHEN NEW.kind
--     IS NOT NULL AND …` guards are stripped — kind is NOT NULL by
--     column constraint.
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
  SELECT RAISE(ABORT, 'forget_intent payload must be body-free (§14)');
END;

-- 4c. 0011 hardening triggers (verbatim except for stripped
--     `WHEN NEW.kind IS NOT NULL AND …` guards, which became
--     tautological once the column went NOT NULL).

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

CREATE TRIGGER consent_journal_payload_shape_matches_kind
  BEFORE INSERT ON consent_journal_v2
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
  SELECT RAISE(ABORT, 'consent_journal payload must be body-free (§14)');
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

CREATE TRIGGER consent_journal_hash_kind_target_id_hash_shape
  BEFORE INSERT ON consent_journal_v2
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

CREATE TRIGGER consent_journal_event_requires_positive_rowid
  AFTER INSERT ON consent_journal_v2
  FOR EACH ROW
  WHEN NEW.rowid <= 0
BEGIN
  SELECT RAISE(ABORT, 'consent_journal event rows require positive rowid');
END;

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
        )
   )
BEGIN
  SELECT RAISE(ABORT, 'consent_journal payload key not allowed for its shape');
END;

CREATE TRIGGER consent_journal_payload_scalar_domains
  BEFORE INSERT ON consent_journal_v2
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

CREATE TRIGGER consent_journal_event_metadata_domains
  BEFORE INSERT ON consent_journal_v2
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

-- 5. Move data. Synthesize every event-shape field for legacy 0005 rows
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
--
--    This INSERT is gated by every trigger attached in step 4 — any
--    pre-0011 event-kind row violating any 0011 invariant aborts the
--    migration with the trigger's own RAISE message, no enumeration in
--    this migration.
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

-- 6. Drop old, rename new. SQLite re-targets the triggers attached to
--    consent_journal_v2 onto the renamed `consent_journal` table; the
--    trigger names stay stable (matches verify.rs schema fingerprint).
DROP TABLE consent_journal;
ALTER TABLE consent_journal_v2 RENAME TO consent_journal;

-- 7. Recreate indexes (identical to 0005 + 0009, except kind_idx no
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

-- 8. Mirror cursor reset marker (round-5 review follow-up). Existing
--    vaults' .cairn/consent.cursor sidecar may already point ABOVE the
--    legacy `kind IS NULL` rowids that this migration just promoted to
--    event-shape rows (the async mirror in cairn-workflows tailed only
--    event-kind rows pre-0021, so legacy rowids were silently skipped).
--    Without intervention, the next mirror tick would still skip them
--    because `WHERE rowid > cursor` filters them out.
--
--    Rather than reach across crate boundaries to delete the sidecar
--    file, we leave a marker in the DB. The mirror's tick() reads the
--    unconsumed markers, calls rebuild_from_db() (which atomically
--    replaces the on-disk log by replaying from rowid 0), then marks
--    the row consumed. Idempotent under repeated ticks; safe under
--    concurrent mirror processes (the mirror holds the consent.lock
--    advisory file lock during the rebuild).
--
--    `consumed` is RESERVED-FOR-NO-USE as of Phase-B finding 2: the DB
--    row is keyed only on `migration_id`, so multiple mirrors (different
--    vault_dirs sharing one DB) would race for the marker — first
--    consumer wins, peers stay stale. Consumption tracking moved to a
--    per-mirror sidecar at `.cairn/consent.mirror_resets_consumed`. The
--    column stays in the schema for fingerprint stability (verify.rs
--    enumerates table columns); the workflow code no longer reads or
--    writes it. Originally `consumed` was an INTEGER (0/1) rather than
--    BOOLEAN because SQLite stores booleans as integers anyway and the
--    column-affinity rules are clearer. UPDATEs on this table are
--    unconstrained — the append-only triggers attach to consent_journal
--    only.
--    Round-12 medium finding: `applied_at = strftime('%s','now') * 1000`
--    is second-resolution and is preserved verbatim by DB copies. Two
--    DBs migrated in the same second collide on (migration_id,
--    applied_at), and a backup restore preserves the original
--    applied_at — so a stale sidecar from the source DB would silently
--    suppress replay against the restored DB. We add a per-DB UUID
--    column `db_nonce` (32 hex chars from `randomblob(16)`) and bind
--    sidecar consumption to that instead of (in addition to)
--    applied_at. randomblob() produces fresh bytes on every INSERT so
--    two DBs migrated in the same instant get different nonces, and a
--    backup restore preserves the source nonce so that a sidecar
--    written against the source still matches — that's intentional:
--    the nonce identifies the DB schema instance, and the audit log
--    rebuilt against the restored DB is identical to the source's by
--    construction (rebuild from rowid 0 is deterministic).
CREATE TABLE IF NOT EXISTS consent_mirror_resets (
  migration_id INTEGER NOT NULL PRIMARY KEY,
  applied_at   INTEGER NOT NULL,
  consumed     INTEGER NOT NULL DEFAULT 0,
  db_nonce     TEXT NOT NULL
);

INSERT INTO consent_mirror_resets (migration_id, applied_at, consumed, db_nonce)
  VALUES (21, strftime('%s','now') * 1000, 0, lower(hex(randomblob(16))));

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (21, '0021_consent_kind_not_null', '', strftime('%s','now') * 1000);
