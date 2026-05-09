//! Schema drift + migration history verification.
//!
//! Two checks run on every `open`:
//! 1. **Migration history**: every applied migration in `schema_migrations`
//!    must match the compiled-in `(id, name, hash)` manifest. The first
//!    open after `to_latest()` stamps the hash for any `sql_hash = ''`
//!    rows; subsequent opens verify the on-disk hash matches the binary.
//! 2. **Schema fingerprint**: the set of app-owned objects in
//!    `sqlite_schema` (tables, indexes, triggers, views, excluding the
//!    `rusqlite_migration` meta table and FTS5 shadow tables) must equal
//!    the compiled-in expected set. Catches same-version drift such as a
//!    dropped trigger or index.

use std::collections::BTreeSet;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::migrations::MIGRATION_SOURCES;

/// App-owned object names that must exist after `to_latest()`. Any extra
/// or missing object at the same `user_version` is reported as drift.
///
/// Excludes:
/// - `rusqlite_migration`'s internal `_rusqlite_migration` table
/// - FTS5 shadow tables (`records_fts_*`) created by `SQLite`, not us
/// - `SQLite`-internal `sqlite_*` tables/indexes
const EXPECTED_OBJECTS: &[(&str, &str)] = &[
    // 0001_records
    ("table", "schema_migrations"),
    ("trigger", "schema_migrations_no_delete"),
    ("trigger", "schema_migrations_immutable"),
    ("table", "records"),
    ("index", "records_active_target_idx"),
    ("index", "records_path_idx"),
    ("index", "records_kind_idx"),
    ("index", "records_visibility_idx"),
    ("index", "records_scope_idx"),
    ("table", "records_fts"),
    ("trigger", "records_fts_ai"),
    ("trigger", "records_fts_ad"),
    ("trigger", "records_fts_au"),
    ("table", "edges"),
    ("trigger", "edges_updates_supersede_insert"),
    ("trigger", "edges_updates_immutable_after_insert"),
    ("trigger", "edges_updates_no_kind_flip"),
    ("view", "records_latest"),
    // 0002_wal
    ("table", "wal_ops"),
    ("index", "wal_ops_open_idx"),
    ("trigger", "wal_ops_issued_seq_must_advance"),
    ("trigger", "wal_ops_state_transition"),
    ("trigger", "wal_ops_envelope_immutable"),
    ("trigger", "wal_ops_terminal_immutable"),
    ("trigger", "wal_ops_no_delete"),
    ("table", "wal_op_deps"),
    ("index", "wal_op_deps_reverse_idx"),
    ("trigger", "wal_op_deps_must_be_acyclic"),
    ("trigger", "wal_op_deps_immutable"),
    ("trigger", "wal_op_deps_no_delete"),
    ("table", "wal_steps"),
    ("index", "wal_steps_resume_idx"),
    ("trigger", "wal_steps_state_transition"),
    ("trigger", "wal_steps_identity_immutable"),
    ("trigger", "wal_steps_no_delete"),
    // 0003_replay
    ("table", "used"),
    ("table", "issuer_seq"),
    ("table", "outstanding_challenges"),
    ("index", "outstanding_challenges_exp_idx"),
    ("trigger", "used_issuer_matches_wal"),
    ("trigger", "used_sequence_must_advance"),
    ("trigger", "used_advance_high_water"),
    ("trigger", "used_immutable"),
    ("trigger", "used_no_delete"),
    ("trigger", "issuer_seq_no_delete"),
    ("trigger", "issuer_seq_insert_must_match_ledger"),
    ("trigger", "issuer_seq_only_via_ledger"),
    // 0004_locks
    ("table", "locks"),
    ("table", "lock_holders"),
    ("index", "lock_holders_expiry_idx"),
    ("trigger", "lock_holders_exclusive_only_alone"),
    ("trigger", "lock_holders_shared_blocked_by_exclusive"),
    ("trigger", "lock_holders_keys_immutable"),
    ("trigger", "lock_holders_count_after_insert"),
    ("trigger", "lock_holders_count_after_delete"),
    ("table", "daemon_incarnation"),
    ("table", "reader_fence"),
    ("index", "reader_fence_pending_idx"),
    ("trigger", "reader_fence_state_transition"),
    ("trigger", "reader_fence_identity_immutable"),
    ("trigger", "reader_fence_no_direct_delete"),
    // 0050_locks_v2 — view added; column additions are not separate sqlite_schema rows
    ("view", "reader_fence_pending_count"),
    // 0051 + 0052_lock_acquisition_ulid — UNIQUE per-acquisition id +
    // sentinel-rejection trigger.
    ("index", "lock_holders_acquisition_ulid_idx"),
    ("trigger", "lock_holders_reject_sentinel_ulid"),
    // 0053_wal_payloads
    ("table", "wal_payloads"),
    ("trigger", "wal_payloads_kind_matches_wal"),
    ("trigger", "wal_payloads_immutable"),
    ("trigger", "wal_payloads_no_delete"),
    // 0005_consent
    ("table", "consent_journal"),
    ("index", "consent_journal_subject_scope_idx"),
    ("trigger", "consent_journal_immutable"),
    ("trigger", "consent_journal_no_delete"),
    // 0007_tombstone_reason
    ("index", "records_tombstoned_reason_idx"),
    // 0009_consent_event
    ("index", "consent_journal_op_idx"),
    ("index", "consent_journal_actor_idx"),
    ("index", "consent_journal_sensor_idx"),
    ("index", "consent_journal_kind_idx"),
    // 0021_consent_kind_not_null replaced the 0009 `consent_journal_kind_domain`
    // trigger with a column-level CHECK on `kind`. The trigger is gone; the
    // CHECK lives in the table definition (verified by the migration hash).
    ("trigger", "consent_journal_event_requires_iso"),
    ("trigger", "consent_journal_forget_receipt_body_free"),
    // 0010_ranking_indexes
    ("index", "records_confidence_idx"),
    ("index", "records_updated_at_idx"),
    // 0011_consent_event_hardening
    ("trigger", "consent_journal_event_requires_actor"),
    ("trigger", "consent_journal_event_requires_payload"),
    ("trigger", "consent_journal_payload_shape_matches_kind"),
    ("trigger", "consent_journal_payload_body_free"),
    ("trigger", "consent_journal_sensor_kind_requires_sensor_id"),
    ("trigger", "consent_journal_sensor_id_matches_payload"),
    (
        "trigger",
        "consent_journal_sensor_subject_matches_sensor_id",
    ),
    (
        "trigger",
        "consent_journal_non_sensor_kind_forbids_sensor_id",
    ),
    ("trigger", "consent_journal_hash_kind_subject_shape"),
    ("trigger", "consent_journal_hash_kind_target_id_hash_shape"),
    ("trigger", "consent_journal_payload_required_fields"),
    ("trigger", "consent_journal_payload_unknown_top_level_keys"),
    ("trigger", "consent_journal_event_requires_positive_rowid"),
    ("trigger", "consent_journal_payload_keys_match_shape"),
    ("trigger", "consent_journal_payload_scalar_domains"),
    ("trigger", "consent_journal_payload_no_duplicate_keys"),
    (
        "trigger",
        "consent_journal_subject_domain_for_non_hash_kinds",
    ),
    ("trigger", "consent_journal_event_metadata_domains"),
    ("trigger", "consent_journal_sensor_id_domain"),
    // 0013_edges_updates_dst_idx
    ("index", "edges_updates_dst_idx"),
    // 0014_sessions
    ("table", "sessions"),
    ("index", "sessions_active_lookup_idx"),
    ("index", "sessions_last_activity_idx"),
    // 0015_sessions_unique_active
    ("index", "sessions_one_active_per_identity_idx"),
    // 0016_sessions_unique_active_coalesce (drops + recreates the index
    // above; index name unchanged. Adds the empty-string guards.)
    ("trigger", "sessions_project_root_no_empty_insert"),
    ("trigger", "sessions_project_root_no_empty_update"),
    // 0020_workflow_jobs
    ("table", "workflow_jobs"),
    ("index", "workflow_jobs_ready_idx"),
    ("index", "workflow_jobs_queued_queue_key_idx"),
    ("index", "workflow_jobs_lease_expiry_idx"),
    ("index", "workflow_jobs_queue_key_leased_uniq"),
    ("index", "workflow_jobs_dedupe_uniq"),
    ("trigger", "workflow_jobs_identity_immutable"),
    ("trigger", "workflow_jobs_terminal_absorbing"),
    ("trigger", "workflow_jobs_state_transition"),
    ("trigger", "workflow_jobs_no_delete"),
    // 0021_consent_kind_not_null (mirror cursor reset marker — round-5
    // review follow-up; consumed by cairn-workflows::ConsentLogMaterializer
    // on first tick after upgrade so legacy promoted rows get replayed).
    ("table", "consent_mirror_resets"),
    // 0022_record_vectors (brief §3.0: sqlite-vec ANN + embedding backfill;
    // renumbered from 0020 on the bench-branch merge).
    ("table", "record_vectors"),
    ("table", "pending_embeddings"),
    ("index", "pending_embeddings_enqueued_idx"),
    ("trigger", "records_vector_cleanup"),
    // 0023_trace_links (issue #77, spec §6.1; renumbered from 0022 on
    // rebase since main shipped 0022_record_vectors first).
    ("index", "records_trace_event_id"),
    ("index", "records_trace_summary"),
    ("index", "records_trace_seq"),
    ("index", "records_trace_parent"),
    ("index", "records_trace_payload_hash"),
    // 0031_records_consent_model — Phase-A per-row consent gate (#253, brief §14).
    ("index", "records_consent_model_idx"),
    // 0032_consent_timeline — append-only receipt event log (#253, brief §14).
    ("table", "consent_timeline"),
    ("index", "consent_timeline_sensor_idx"),
    ("index", "consent_timeline_decided_idx"),
    ("trigger", "consent_timeline_immutable"),
    ("trigger", "consent_timeline_no_delete"),
    // 0033_consent_timeline_grant_immutable — per-consent_ref grant
    // immutability trigger (#253 round 2).
    ("trigger", "consent_timeline_grant_immutable"),
    // 0034_consent_timeline_lifecycle — first-event/seq-monotonic/
    // decided_at-non-decreasing triggers (#253 round 8).
    ("trigger", "consent_timeline_first_event_issued"),
    ("trigger", "consent_timeline_seq_strictly_monotonic"),
    ("trigger", "consent_timeline_decided_at_non_decreasing"),
    // 0035_consent_timeline_lifecycle_tighten — UTC-form + fresh-seq=1
    // guards that close the offset-form bypass and the seq!=1 first-row
    // hole left by 0034 (#253 round 9).
    ("trigger", "consent_timeline_decided_at_utc_only"),
    ("trigger", "consent_timeline_expires_at_utc_only"),
    ("trigger", "consent_timeline_fresh_must_start_at_seq_1"),
    // 0037_consent_timeline_canonical_nanos — pin timestamps to the
    // 30-char nanosecond form so lexical TEXT order matches
    // chronological order without losing sub-second precision (#253
    // round 10+2). 0037 drops 0036's seconds-only triggers.
    ("trigger", "consent_timeline_decided_at_canonical_nanos"),
    ("trigger", "consent_timeline_expires_at_canonical_nanos"),
    // 0040_consent_timeline_scope_canonical — restrict scope column to
    // the canonical wire alphabet so the lint exact-match join cannot
    // be defeated by encoding drift (#253 loop 3 round 5).
    ("trigger", "consent_timeline_scope_canonical_chars"),
    // 0042_entity_nodes (issue #186 §3.2: bitemporal knowledge-graph schema).
    // The implicit unique index from UNIQUE(name_norm) is auto-named
    // `sqlite_autoindex_*` and excluded by the `sqlite_%` filter below.
    ("table", "entity_nodes"),
    ("table", "entity_nodes_fts"),
    ("trigger", "entity_nodes_shrink_guard"),
    ("trigger", "entity_nodes_fts_ai"),
    ("trigger", "entity_nodes_fts_au"),
    ("trigger", "entity_nodes_fts_ad"),
    // 0043_entity_edges (issue #186 §3.3: bitemporal knowledge-graph schema).
    ("table", "entity_edges"),
    ("index", "entity_edges_live_triple"),
    ("index", "entity_edges_valid_at_idx"),
    ("index", "entity_edges_invalid_at_idx"),
    ("index", "entity_edges_source_relation_idx"),
    ("index", "entity_edges_target_relation_idx"),
    ("trigger", "entity_edges_shrink_guard"),
    // 0045_entity_edges_no_overlap_trigger (issue #186 round-9 fix:
    // schema-level defense in depth against bounded-overlap writes).
    ("trigger", "entity_edges_no_overlap_insert"),
    ("trigger", "entity_edges_no_overlap_update"),
    // 0044_entity_episodes (issue #186 §3.4: record ↔ entity link table).
    // The composite PK creates an implicit sqlite_autoindex_* which is
    // filtered by the `sqlite_%` clause — do NOT add it here.
    ("table", "entity_episodes"),
    ("index", "entity_episodes_entity_idx"),
    // 0046_records_schema_version — per-row schema-version stamp for the
    // §6.4 stale_schema lint (#258).
    ("index", "records_schema_version_idx"),
    // 0048_consent_journal_repair_audit (issue #267): operator-driven
    // recovery audit table for legacy consent_journal rows blocking 0021.
    ("table", "consent_journal_repair_audit"),
    ("trigger", "consent_journal_repair_audit_immutable"),
    ("trigger", "consent_journal_repair_audit_no_delete"),
];

fn hash_hex(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

/// Pre-flight check: verify already-applied migration rows have hashes
/// matching the compiled-in manifest, BEFORE `to_latest` runs.
///
/// This catches a tampered `schema_migrations.sql_hash` on a pre-head
/// database — without it, `to_latest` would happily apply pending
/// migrations against an untrusted store and only the post-migration
/// `verify_migration_history` would refuse the open. Read-only:
/// performs no stamps, no mutations.
///
/// **Watermark semantics (Codex round-9 finding #1).** We compute the
/// applied-history watermark as `MAX(migration_id)` from
/// `schema_migrations`. For every `id <= watermark`:
///   - the row MUST exist (gaps from a DELETE are rejected);
///   - the name MUST match the manifest;
///   - the hash MUST be non-empty AND match the expected hash
///     (a blanked `''` row would otherwise let the post-check
///     re-stamp the expected hash and silently re-trust the row).
///
/// For `id > watermark`: the row is allowed to be absent — those are
/// genuinely pending migrations that `to_latest` will append.
pub(crate) fn preflight_migration_history(conn: &Connection) -> Result<(), StoreError> {
    // The table itself may not exist on a fresh DB — that's fine.
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema \
             WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |r| r.get::<_, i64>(0).map(|v| v == 1),
        )
        .unwrap_or(false);
    if !table_exists {
        return Ok(());
    }
    // Pre-0006 databases hold the hash in a column called `sql_blake3`;
    // migration 0006 renames it to `sql_hash`. Detect which one this
    // store is at so legitimate upgrades from older versions still pass
    // preflight (Codex round-10 finding #1).
    let hash_col: &str = if column_exists(conn, "schema_migrations", "sql_hash")? {
        "sql_hash"
    } else if column_exists(conn, "schema_migrations", "sql_blake3")? {
        "sql_blake3"
    } else {
        return Err(StoreError::SchemaDrift(
            "schema_migrations is missing both sql_hash and sql_blake3 columns".into(),
        ));
    };
    // Watermark: highest applied migration_id, or 0 if the table is empty.
    let watermark: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(migration_id), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // Distinguish "fresh post-to_latest, pre-stamp" from "tampered".
    // Right after migrations apply, every row has the hash column = ''
    // and we permit blank hashes. Once `verify_migration_history` has
    // run even once, NO row should have a blank hash (the stamp-once
    // trigger forbids reverting). So an open where SOME rows are
    // stamped and SOME are blank below the watermark indicates
    // tampering, and we treat blanks as drift.
    let all_below_blank: bool = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) = 0 FROM schema_migrations \
                 WHERE migration_id <= ? AND length({hash_col}) > 0"
            ),
            [watermark],
            |r| r.get::<_, i64>(0).map(|v| v == 1),
        )
        .unwrap_or(false);
    for &(id, name, sql) in MIGRATION_SOURCES {
        let expected = hash_hex(sql);
        let row: Option<(String, String)> = conn
            .query_row(
                &format!(
                    "SELECT name, {hash_col} FROM schema_migrations \
                     WHERE migration_id = ?"
                ),
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if id > watermark {
            // Genuinely pending — absence is fine, `to_latest` appends.
            // A row above the watermark is a contradiction in terms,
            // but if one somehow exists we still validate it below.
            let Some((on_disk_name, on_disk_hash)) = row else {
                continue;
            };
            if on_disk_name != name {
                return Err(StoreError::SchemaDrift(format!(
                    "migration {id} name mismatch above watermark: on-disk {on_disk_name:?} vs binary {name:?}",
                )));
            }
            if !on_disk_hash.is_empty() && on_disk_hash != expected {
                return Err(StoreError::SchemaDrift(format!(
                    "migration {id} ({name}) hash mismatch above watermark: \
                     on-disk {on_disk_hash} vs binary {expected}",
                )));
            }
            continue;
        }
        // id <= watermark: row MUST exist, hash MUST be non-empty,
        // hash MUST match. Anything else is tampering.
        let Some((on_disk_name, on_disk_hash)) = row else {
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} ({name}) missing from schema_migrations \
                 below watermark={watermark}: history has been tampered with",
            )));
        };
        if on_disk_name != name {
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} name mismatch: on-disk {on_disk_name:?} vs binary {name:?}",
            )));
        }
        if on_disk_hash.is_empty() {
            // Permit blanks ONLY in the all-blank "fresh post-to_latest,
            // pre-stamp" state. Mixed blanks-and-stamps below the
            // watermark indicates a stamp was undone — treat as drift.
            if all_below_blank {
                continue;
            }
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} ({name}) sql_hash blanked below watermark={watermark}: \
                 cannot re-stamp without applying the original migration body",
            )));
        }
        if on_disk_hash != expected {
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} ({name}) hash mismatch: \
                 on-disk {on_disk_hash} vs binary {expected}",
            )));
        }
    }
    Ok(())
}

/// `true` iff `table` has a column named `column`. Used by
/// [`preflight_migration_history`] to switch between the legacy
/// `sql_blake3` column and the post-0006 `sql_hash` column.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, StoreError> {
    let mut stmt = conn.prepare("SELECT 1 FROM pragma_table_info(?) WHERE name = ? LIMIT 1")?;
    let found: Option<i64> = stmt
        .query_row(rusqlite::params![table, column], |r| r.get(0))
        .ok();
    Ok(found.is_some())
}

/// Stamp `sql_hash` for any rows still set to `''`, then verify every
/// row's hash matches the compiled-in manifest.
pub(crate) fn verify_migration_history(conn: &Connection) -> Result<(), StoreError> {
    for &(id, name, sql) in MIGRATION_SOURCES {
        let expected = hash_hex(sql);
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT name, sql_hash FROM schema_migrations WHERE migration_id = ?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((on_disk_name, on_disk_hash)) = row else {
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} ({name}) missing from schema_migrations",
            )));
        };
        if on_disk_name != name {
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} name mismatch: on-disk {on_disk_name:?} vs binary {name:?}",
            )));
        }
        if on_disk_hash.is_empty() {
            // Stamp once.
            conn.execute(
                "UPDATE schema_migrations SET sql_hash = ? WHERE migration_id = ?",
                rusqlite::params![&expected, id],
            )?;
        } else if on_disk_hash != expected {
            return Err(StoreError::SchemaDrift(format!(
                "migration {id} ({name}) hash mismatch: \
                 on-disk {on_disk_hash} vs binary {expected}",
            )));
        }
    }
    Ok(())
}

/// Round-3 adversarial-review (Medium) follow-up for #255: `EXPECTED_OBJECTS`
/// only enumerates *named* schema objects (tables, indexes, triggers, views).
/// Migration 0021 replaced the named `consent_journal_kind_domain` trigger
/// with a column-level CHECK on `consent_journal.kind` — a constraint that
/// lives inside the table's CREATE TABLE text, not under its own name.
/// `verify_schema_fingerprint`'s DDL-digest does cover the CREATE TABLE text
/// transitively, but a digest-mismatch error message would not point at the
/// CHECK; a future schema modification that drops the CHECK would surface
/// only as opaque drift. This explicit substring check pins the
/// load-bearing §14 invariant — `kind TEXT NOT NULL` plus the closed-set
/// `CHECK (kind IN (...))` — and reports it by name.
///
/// Returns [`StoreError::SchemaDrift`] if either substring is missing.
fn verify_consent_journal_kind_check(conn: &Connection) -> Result<(), StoreError> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_schema \
         WHERE type = 'table' AND name = 'consent_journal'",
        [],
        |r| r.get(0),
    )?;

    // Whitespace-tolerant: SQLite preserves the original CREATE TABLE
    // text (including line breaks + indentation) so a contains() on the
    // exact substring our migration emits is robust enough. If the
    // migration text ever reformats either substring (e.g. `kind\nTEXT`
    // or `CHECK(kind IN`), update both this check AND the migration
    // together — they are co-load-bearing.
    let normalized = canonicalize_ddl(&sql);
    if !normalized.contains("kind TEXT NOT NULL") {
        return Err(StoreError::SchemaDrift(
            "consent_journal.kind NOT NULL constraint missing or modified \
             (expected `kind TEXT NOT NULL` in CREATE TABLE)"
                .to_owned(),
        ));
    }
    if !normalized.contains("CHECK (kind IN (") {
        return Err(StoreError::SchemaDrift(
            "consent_journal kind CHECK constraint missing or modified \
             (expected `CHECK (kind IN (...))` in CREATE TABLE)"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Verify that the set of app-owned schema objects matches `EXPECTED_OBJECTS`.
///
/// `vec_dim`: when `Some(n)` and `n != 384`, the expected DDL digest is
/// re-computed against a connection where `record_vectors` has been
/// resized to dim `n` — same operation `crate::open::record_vectors_create_sql`
/// applies on the live connection. Without this, an `OpenAI`
/// (`dim()=1536`) embedder would fail the digest check immediately on
/// open even though the resized table is exactly what we asked for.
pub(crate) fn verify_schema_fingerprint(
    conn: &Connection,
    vec_dim: Option<usize>,
) -> Result<(), StoreError> {
    // Run the named-substring check for `consent_journal.kind` BEFORE
    // the EXPECTED_OBJECTS / DDL-digest sweep so that a missing CHECK
    // surfaces under its specific error message rather than as part of
    // an opaque digest mismatch. (Round-3 review follow-up; #255.)
    verify_consent_journal_kind_check(conn)?;

    // Exclude SQLite-internal objects, the rusqlite_migration meta table,
    // and FTS5 shadow tables/indexes. The shadow set is identified by
    // (type IN ('table','index') AND name LIKE 'records_fts_%') — our
    // user-defined triggers (records_fts_ai/ad/au) share the prefix but
    // are type='trigger' and must not be excluded.
    let mut stmt = conn.prepare(
        "SELECT type, name, sql FROM sqlite_schema \
         WHERE name NOT LIKE 'sqlite_%' \
           AND name <> '_rusqlite_migration' \
           AND NOT (name LIKE 'records_fts_%' AND type IN ('table','index')) \
           AND NOT (name LIKE 'record_vectors_%' AND type IN ('table','index')) \
           AND NOT (name LIKE 'entity_nodes_fts_%' AND type IN ('table','index')) \
           AND type IN ('table','index','trigger','view') \
           AND sql IS NOT NULL",
    )?;
    let mut on_disk_names: BTreeSet<(String, String)> = BTreeSet::new();
    let mut on_disk_digest = Sha256::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    // BTreeMap for stable iteration order before hashing.
    let mut by_name: std::collections::BTreeMap<(String, String), String> =
        std::collections::BTreeMap::new();
    for row in rows {
        let (ty, name, sql) = row?;
        by_name.insert((ty.clone(), name.clone()), sql);
        on_disk_names.insert((ty, name));
    }
    for ((ty, name), sql) in &by_name {
        on_disk_digest.update(ty.as_bytes());
        on_disk_digest.update(b"|");
        on_disk_digest.update(name.as_bytes());
        on_disk_digest.update(b"|");
        on_disk_digest.update(canonicalize_ddl(sql).as_bytes());
        on_disk_digest.update(b"\n");
    }

    let expected: BTreeSet<(String, String)> = EXPECTED_OBJECTS
        .iter()
        .map(|(t, n)| ((*t).to_string(), (*n).to_string()))
        .collect();

    let missing: Vec<_> = expected.difference(&on_disk_names).collect();
    let extra: Vec<_> = on_disk_names.difference(&expected).collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(StoreError::SchemaDrift(format!(
            "schema fingerprint mismatch: missing={missing:?} extra={extra:?}",
        )));
    }

    // Compare the on-disk DDL digest against an expected digest computed
    // from the same database immediately after `to_latest()` on a fresh
    // in-memory store. This catches same-name drift (e.g. a trigger
    // dropped and recreated with weaker predicates).
    let expected_digest = expected_ddl_digest(vec_dim)?;
    let actual_digest = finalize_hex(on_disk_digest);
    if actual_digest != expected_digest {
        return Err(StoreError::SchemaDrift(format!(
            "schema DDL digest mismatch: on-disk {actual_digest} vs expected {expected_digest}",
        )));
    }
    Ok(())
}

/// Normalize whitespace in DDL so cosmetic differences (e.g. `PRAGMA`
/// reformat) don't trip the digest. Collapses runs of whitespace to a
/// single space and trims.
fn canonicalize_ddl(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last_was_ws = true;
    for c in sql.chars() {
        if c.is_whitespace() {
            if !last_was_ws {
                out.push(' ');
                last_was_ws = true;
            }
        } else {
            out.push(c);
            last_was_ws = false;
        }
    }
    out.trim().to_string()
}

fn finalize_hex(digest: Sha256) -> String {
    let bytes = digest.finalize();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

/// Compute the expected DDL digest by applying the migration set to a
/// fresh in-memory store. This is the canonical fingerprint the binary
/// claims, derived from the compiled-in SQL without hand-maintained tables.
fn expected_ddl_digest(vec_dim: Option<usize>) -> Result<String, StoreError> {
    use rusqlite::Connection;
    // Register sqlite-vec before opening the connection so migration 0020
    // (CREATE VIRTUAL TABLE USING vec0) can succeed on the digest-path.
    crate::vec_ext::register_vec0();
    let mut conn = Connection::open_in_memory()?;
    crate::migrations::migrations().to_latest(&mut conn)?;
    // Apply the same resize bootstrap performs so the digest reflects
    // the post-bootstrap schema, not the migration-default 384-dim one.
    //
    // `Some(d)` means "the live `record_vectors` is in *recreated* form at
    // dim `d`" — including `d == 384` after a 1536→384 resize, where the
    // on-disk DDL string is the bare CREATE we emit and not the comment-
    // bearing migration 0020 form. Always replay the recreate when `Some(_)`.
    if let Some(dim) = vec_dim {
        conn.execute_batch("DROP TABLE record_vectors;")?;
        conn.execute_batch(&crate::open::record_vectors_create_sql(dim))?;
    }

    let mut stmt = conn.prepare(
        "SELECT type, name, sql FROM sqlite_schema \
         WHERE name NOT LIKE 'sqlite_%' \
           AND name <> '_rusqlite_migration' \
           AND NOT (name LIKE 'records_fts_%' AND type IN ('table','index')) \
           AND NOT (name LIKE 'record_vectors_%' AND type IN ('table','index')) \
           AND NOT (name LIKE 'entity_nodes_fts_%' AND type IN ('table','index')) \
           AND type IN ('table','index','trigger','view') \
           AND sql IS NOT NULL",
    )?;
    let mut by_name: std::collections::BTreeMap<(String, String), String> =
        std::collections::BTreeMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (ty, name, sql) = row?;
        by_name.insert((ty, name), sql);
    }
    let mut digest = Sha256::new();
    for ((ty, name), sql) in &by_name {
        digest.update(ty.as_bytes());
        digest.update(b"|");
        digest.update(name.as_bytes());
        digest.update(b"|");
        digest.update(canonicalize_ddl(sql).as_bytes());
        digest.update(b"\n");
    }
    Ok(finalize_hex(digest))
}
