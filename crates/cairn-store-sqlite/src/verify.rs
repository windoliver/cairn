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
    // 0005_consent
    ("table", "consent_journal"),
    ("index", "consent_journal_subject_scope_idx"),
    ("trigger", "consent_journal_immutable"),
    ("trigger", "consent_journal_no_delete"),
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

/// Verify that the set of app-owned schema objects matches `EXPECTED_OBJECTS`.
pub(crate) fn verify_schema_fingerprint(conn: &Connection) -> Result<(), StoreError> {
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
    let expected_digest = expected_ddl_digest()?;
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
fn expected_ddl_digest() -> Result<String, StoreError> {
    use rusqlite::Connection;
    let mut conn = Connection::open_in_memory()?;
    crate::migrations::migrations().to_latest(&mut conn)?;

    let mut stmt = conn.prepare(
        "SELECT type, name, sql FROM sqlite_schema \
         WHERE name NOT LIKE 'sqlite_%' \
           AND name <> '_rusqlite_migration' \
           AND NOT (name LIKE 'records_fts_%' AND type IN ('table','index')) \
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
