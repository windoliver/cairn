//! `ConsentLog` impl backed by direct rusqlite access. Wraps the
//! live consent journal + the restored DB so that
//! `verbs::admin::restore` can ask "is target T currently forgotten?"
//! and then "purge all matches."
//!
//! This is the v0.2 implementation of forget-replay-on-restore. It moves
//! the helpers that previously lived in
//! `crates/cairn-cli/src/verbs/admin_snapshot.rs` (`current_record_forget_hashes`,
//! `collect_target_ids`, `purge_targets`, `target_id_hash`) into the store
//! crate so `cairn-core::verbs::admin::restore::run` can drive them via a
//! trait, keeping `cairn-core` I/O-free.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::snapshot_artifact::ConsentLog;
use cairn_core::domain::{ConsentKind, ConsentPayload, TargetId};
use rusqlite::Connection;

use crate::error::StoreError as SqliteStoreError;

/// `ConsentLog` implementation backed by rusqlite. Holds the live vault's DB
/// path (the source of currently-forgotten target hashes); the purge TARGET is
/// passed per call so the restore verb can purge the snapshot's STAGED database
/// before it is swapped in.
///
/// ## Pre-swap purge (no cache needed)
///
/// `restore::run` calls `apply_post_restore_purge(staged_db)` BEFORE
/// `swap_in()`. Because the live DB is untouched at that point, the forget-hash
/// set is read straight from it — no pre-capture/cache is required (this
/// supersedes the round-2 swap-safe cache, which existed only because purge
/// used to run after the swap had overwritten the live path).
pub struct SqliteConsentLog {
    live_db: PathBuf,
}

impl SqliteConsentLog {
    /// `live_db` is the path to the running vault's `cairn.db` — the source of
    /// the currently-forgotten target hashes. The purge target (the staged or
    /// restored DB) is supplied to [`ConsentLog::apply_post_restore_purge`].
    #[must_use]
    pub fn new(live_db: PathBuf) -> Self {
        Self { live_db }
    }

    /// Purge from `target_db` every record whose `sha256(target_id)` appears in
    /// `forget_hashes`, returning the count removed.
    ///
    /// Lets a caller that has ALREADY captured the live forget set apply it to
    /// the restored database — e.g. the CLI, which must read the live vault's
    /// forgets BEFORE a filesystem copy overwrites that vault (round-6 review
    /// #1). The trait [`ConsentLog::apply_post_restore_purge`] is the
    /// read-live-then-purge convenience built on top of this.
    ///
    /// # Errors
    /// Returns `StoreError` if `target_db` cannot be read or written.
    pub fn purge_targets_matching(
        &self,
        target_db: &Path,
        forget_hashes: &HashSet<String>,
    ) -> Result<u64, StoreError> {
        if forget_hashes.is_empty() {
            return Ok(0);
        }
        let targets = collect_target_ids(target_db).map_err(|e| Box::new(e) as StoreError)?;
        let to_purge: Vec<TargetId> = targets
            .into_iter()
            .filter(|t| forget_hashes.contains(&target_id_hash(t.as_str())))
            .collect();
        if to_purge.is_empty() {
            return Ok(0);
        }
        let count = u64::try_from(to_purge.len()).unwrap_or(u64::MAX);
        purge_targets(target_db, &to_purge).map_err(|e| Box::new(e) as StoreError)?;
        Ok(count)
    }
}

impl ConsentLog for SqliteConsentLog {
    fn forgotten_record_target_hashes(&self) -> Result<HashSet<String>, StoreError> {
        Ok(current_record_forget_hashes(&self.live_db)
            .map_err(|e| Box::new(e) as StoreError)?
            .into_iter()
            .collect())
    }

    fn apply_post_restore_purge(&self, target_db: &Path) -> Result<u64, StoreError> {
        let forget_hashes: HashSet<String> = current_record_forget_hashes(&self.live_db)
            .map_err(|e| Box::new(e) as StoreError)?
            .into_iter()
            .collect();
        self.purge_targets_matching(target_db, &forget_hashes)
    }
}

// --- helpers ported from cairn-cli/src/verbs/admin_snapshot.rs ----------

/// Returns `sha256:<hex>` hashes for every record currently in the
/// consent journal's forget set.
fn current_record_forget_hashes(db_path: &Path) -> Result<BTreeSet<String>, SqliteStoreError> {
    if !db_path.exists() {
        return Ok(BTreeSet::new());
    }

    crate::vec_ext::register_vec0();
    let conn = Connection::open(db_path)?;
    if !table_exists(&conn, "consent_journal")? {
        return Ok(BTreeSet::new());
    }

    let events = crate::consent::read_since_rowid(&conn, 0)?;
    let mut hashes = BTreeSet::new();
    for (_, event) in events {
        if event.kind != ConsentKind::ForgetIntent {
            continue;
        }
        if let ConsentPayload::IntentReceipt {
            target_id_hash,
            reason_code,
            ..
        } = &event.payload
            && reason_code == "record_forget"
        {
            hashes.insert(target_id_hash.clone());
        }
    }

    Ok(hashes)
}

fn collect_target_ids(db_path: &Path) -> Result<Vec<TargetId>, SqliteStoreError> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    crate::vec_ext::register_vec0();
    let conn = Connection::open(db_path)?;
    if !table_exists(&conn, "records")? {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare("SELECT DISTINCT target_id FROM records ORDER BY target_id ASC")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    let mut targets = Vec::new();
    for row in rows {
        let target = row?;
        targets.push(TargetId::parse(target.clone()).map_err(|e| {
            SqliteStoreError::VaultPath(format!(
                "parse target id `{target}` from {path}: {e}",
                path = db_path.display()
            ))
        })?);
    }
    Ok(targets)
}

fn purge_targets(db_path: &Path, targets: &[TargetId]) -> Result<(), SqliteStoreError> {
    if targets.is_empty() || !db_path.exists() {
        return Ok(());
    }

    crate::vec_ext::register_vec0();
    let mut conn = Connection::open(db_path)?;
    if !table_exists(&conn, "records")? {
        return Ok(());
    }

    let tx = conn.transaction()?;
    for target in targets {
        tx.execute(
            "DELETE FROM edges \
              WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
                 OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
            [target.as_str()],
        )?;
        tx.execute(
            "DELETE FROM records WHERE target_id = ?1",
            [target.as_str()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn target_id_hash(target_id: &str) -> String {
    use sha2::{Digest, Sha256};

    format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()))
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, SqliteStoreError> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_id_hash_is_sha256_of_input() {
        // sha256("hmn:alice/rec/1") should be deterministic.
        // Format: "sha256:" (7 chars) + 64 hex chars = 71 chars total.
        let h = target_id_hash("hmn:alice/rec/1");
        assert_eq!(
            h.len(),
            71,
            "expected 'sha256:' + 64 hex = 71 chars, got: {h}"
        );
        assert!(
            h.starts_with("sha256:"),
            "hash must start with 'sha256:' prefix"
        );
        assert_eq!(
            target_id_hash("hmn:alice/rec/1"),
            h,
            "hash must be deterministic"
        );
        assert_ne!(target_id_hash("other"), h, "hash must be input-sensitive");
    }

    /// Round-6 review #1: `purge_targets_matching` removes exactly the records
    /// whose target-id hash is in the supplied forget set (the mechanism the
    /// CLI uses after capturing the LIVE vault's forgets) and leaves the rest.
    #[test]
    fn purge_targets_matching_removes_only_forgotten_targets() {
        // Valid 26-char ULID target ids (collect_target_ids parses via TargetId).
        const KEEP_TID: &str = "01HQZX9F5N0000000000000001";
        const DROP_TID: &str = "01HQZX9F5N0000000000000002";

        let dir = tempfile::tempdir().expect("tmpdir");
        let db = dir.path().join("cairn.db");
        {
            let conn = crate::open::open_sync(&db).expect("open db");
            seed_record(&conn, "01HQZX9F5N0000000000000011", KEEP_TID);
            seed_record(&conn, "01HQZX9F5N0000000000000012", DROP_TID);
            conn.close().expect("close");
        }

        let consent = SqliteConsentLog::new(db.clone());
        // Forget only the DROP target.
        let forgets: HashSet<String> = std::iter::once(target_id_hash(DROP_TID)).collect();
        let purged = consent
            .purge_targets_matching(&db, &forgets)
            .expect("purge");
        assert_eq!(purged, 1, "exactly the one forgotten target is purged");

        let conn = crate::open::open_sync(&db).expect("reopen db");
        let mut stmt = conn
            .prepare("SELECT target_id FROM records ORDER BY target_id")
            .expect("prepare");
        let remaining: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(
            remaining,
            vec![KEEP_TID.to_string()],
            "only the kept target survives the purge"
        );
    }

    fn seed_record(conn: &Connection, rid: &str, tid: &str) {
        conn.execute(
            "INSERT INTO records \
               (record_id, target_id, version, path, kind, class, visibility, \
                scope, actor_chain, body, body_hash, created_at, updated_at, \
                active, tombstoned, is_static) \
             VALUES (?1, ?2, 1, 'test/x', 'user', 'semantic', 'private', \
                     '{\"user\":\"hmn:t\"}', '[]', 'body', 'h', 1, 1, 1, 0, 0)",
            rusqlite::params![rid, tid],
        )
        .expect("insert record");
    }
}
