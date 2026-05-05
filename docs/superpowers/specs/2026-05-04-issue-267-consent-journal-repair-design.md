# Issue 267 Consent Journal Repair Design

## Context

Issue #267 covers vaults that cannot migrate through 0021 because legacy
`consent_journal` rows violate preflight checks added by PR #266. The blocking
rows are:

- true legacy rows (`kind IS NULL`) with `rowid <= 0`;
- true legacy rows whose `decided_at` or `expires_at` integer cannot be rendered
  by SQLite's RFC3339 synthesis expression;
- kind-NULL direct-SQL drift rows carrying post-0009 event-shape fields.

The design brief sections in scope are §3 (the SQLite DB is the authoritative
vault control plane), §5.6 (mutations must be transactionally auditable), and
§14 (the consent journal is append-only and the `.cairn/consent.log` mirror must
have no duplicates and no gaps). `CLAUDE.md` also makes the CLI the ground truth;
MCP and SDK surfaces must remain thin wrappers.

## Approaches Considered

1. **Deletion-only repair for blocked legacy rows.** The tool enumerates
   migration-blocking rows and can delete selected rows after an explicit
   operator confirmation. This is the recommended v1 because it avoids
   renumbering historical events and keeps the semantics easy to audit.

2. **Renumber malformed rowids.** This would preserve row contents, but it moves
   events to a new position in the mirror's `WHERE rowid > cursor ORDER BY rowid`
   stream. Even if the tool adds a mirror reset marker, the DB history itself has
   been reordered. That is too risky for the first repair path.

3. **Rewrite malformed timestamps.** This would require the operator to choose a
   replacement instant for each unrenderable value. It is useful eventually, but
   it changes the meaning of consent decisions and needs richer review/audit UI
   than issue #267 requires.

## Proposed CLI

Add `cairn repair consent-journal` as a top-level maintenance command:

```text
cairn repair consent-journal --vault <path-or-name>
cairn repair consent-journal --vault <path-or-name> --json
cairn repair consent-journal --vault <path-or-name> --delete-rowid <ROWID> --reason <TEXT> --yes
```

Without `--delete-rowid`, the command is read-only. It opens the vault database,
enumerates blockers, and prints row contents needed for triage:

- `rowid`;
- `consent_id`, `subject`, `scope`, `decision`, `granted_by`;
- `decided_at`, `expires_at`, and their renderability state;
- `kind`, `actor`, `payload_json`, `decided_at_iso`, `expires_at_iso`,
  `op_id`, `sensor_id`;
- a reason code such as `non_positive_rowid`, `unrenderable_decided_at`,
  `unrenderable_expires_at`, or `kind_null_event_field_drift`.

With `--delete-rowid`, the command requires `--reason` and `--yes`. It refuses
to delete rows that are not currently classified as blockers. It refuses to
renumber rows.

Exit codes:

- `0` when listing succeeds or a requested delete succeeds;
- `65` (`EX_DATAERR`) when a requested row is not repair-eligible;
- `74` (`EX_IOERR`) for filesystem or SQLite I/O errors;
- `78` (`EX_CONFIG`) for vault path/config resolution errors.

## Store API

Add a focused `cairn_store_sqlite::repair::consent_journal` module with
sync helpers over `rusqlite::Connection`:

- `list_blockers(conn) -> Result<Vec<ConsentJournalRepairRow>, StoreError>`;
- `delete_blocker(conn, rowid, reason, operator) -> Result<ConsentJournalRepairReceipt, StoreError>`.

The CLI opens the SQLite DB path directly for maintenance instead of going
through the normal store `open()` path, because `open()` may run migration 0021
and fail before the operator can repair. The repair helper applies conservative
pragmas (`foreign_keys=ON`, `busy_timeout=5000`) and uses `BEGIN IMMEDIATE` for
mutations. Because a vault blocked before 0021 cannot have later migrations
applied yet, the helper also creates the repair audit table idempotently during
maintenance. A normal append-only migration declares the same table for healthy
vaults so schema verification still knows about it.

## Controlled Trigger Bypass

`delete_blocker` performs exactly one direct mutation inside a single immediate
transaction:

1. Re-query and classify the row by `rowid`.
2. Abort if the row is not a blocker.
3. Record an audit row in a new `consent_journal_repair_audit` table. The audit
   row stores the repair action (`delete`), rowid, blocker codes, operator,
   reason, timestamp millis, and a JSON snapshot of the deleted row's metadata.
4. Drop `consent_journal_immutable` and `consent_journal_no_delete`.
5. Delete the selected `consent_journal` row by `rowid`.
6. Recreate both append-only triggers with the same bodies used by the migrated
   schema.
7. If migration 0021 has already created `consent_mirror_resets`, insert or
   replace a marker with a fresh nonce so live materializers rebuild
   `.cairn/consent.log` from rowid 0. On a DB still blocked before 0021, do not
   pre-create the table or marker; migration 0021 inserts its own reset marker
   once the repair has unblocked it.
8. Commit.

If any step fails, SQLite rolls back the transaction. The triggers therefore
return attached with the original row still present. The helper does not expose
generic SQL execution and does not accept arbitrary table names or predicates.

## Audit Table

Add a new append-only migration, and reuse the same SQL idempotently from the
maintenance helper:

```sql
CREATE TABLE consent_journal_repair_audit (
  repair_id        TEXT NOT NULL PRIMARY KEY,
  action           TEXT NOT NULL CHECK (action IN ('delete')),
  target_rowid     INTEGER NOT NULL,
  blocker_codes    TEXT NOT NULL CHECK (json_valid(blocker_codes) = 1),
  operator         TEXT NOT NULL,
  reason           TEXT NOT NULL,
  row_snapshot     TEXT NOT NULL CHECK (json_valid(row_snapshot) = 1),
  repaired_at      INTEGER NOT NULL
);

CREATE TRIGGER consent_journal_repair_audit_immutable
  BEFORE UPDATE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;

CREATE TRIGGER consent_journal_repair_audit_no_delete
  BEFORE DELETE ON consent_journal_repair_audit
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal_repair_audit is append-only');
END;
```

The audit table is separate from `consent_journal` because migration 0021 itself
may not be able to decode or insert legal post-0021 consent events yet. The
repair table is still in the authoritative SQLite DB and is append-only.

## MCP and SDK Wrappers

MCP/SDK wrappers expose the same two operations as the CLI: list blockers and
delete one eligible blocker. They accept the same fields and return the same
receipt structure. They do not implement their own SQL path.

## Tests

The implementation should be test-first.

Store integration tests:

- list returns a row for a v20 legacy row with `rowid = 0`;
- list returns a row for a v20 legacy row with unrenderable `decided_at`;
- list returns a row for kind-NULL direct-SQL drift with an event field;
- delete refuses a row not returned by the classifier;
- delete removes an eligible row despite append-only triggers;
- delete writes an immutable audit row;
- delete inserts a fresh `consent_mirror_resets` marker when the DB is already
  at or beyond migration 0021;
- after pre-0021 delete, migration 0021 can run successfully on the repaired DB
  and inserts its normal reset marker.

CLI tests:

- `cairn repair consent-journal --json` emits valid JSON with blockers;
- delete requires `--reason` and `--yes`;
- delete succeeds on an eligible row and reports the audit receipt;
- delete of a non-blocker exits `65`.

## Non-Goals

- No rowid renumbering.
- No timestamp rewriting.
- No broad SQL console or manual trigger bypass.
- No automatic repair during `open()`.
- No mutation unless the operator names one `rowid`, supplies a reason, and
  passes `--yes`.
