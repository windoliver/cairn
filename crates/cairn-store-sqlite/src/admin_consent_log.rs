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

/// `ConsentLog` implementation backed by two rusqlite databases: the live
/// vault's DB (source of currently-forgotten target hashes) and the
/// restored DB (whose targets should be purged to match the forget set).
pub struct SqliteConsentLog {
    live_db: PathBuf,
    restored_db: PathBuf,
}

impl SqliteConsentLog {
    /// `live_db` is the path to the running vault's `cairn.db`
    /// (the source of currently-forgotten target hashes). `restored_db`
    /// is the path to the swapped-in DB whose targets should be purged
    /// to match the forget set.
    #[must_use]
    pub fn new(live_db: PathBuf, restored_db: PathBuf) -> Self {
        Self {
            live_db,
            restored_db,
        }
    }
}

impl ConsentLog for SqliteConsentLog {
    fn forgotten_record_target_hashes(&self) -> Result<HashSet<String>, StoreError> {
        let hashes =
            current_record_forget_hashes(&self.live_db).map_err(|e| Box::new(e) as StoreError)?;
        Ok(hashes.into_iter().collect())
    }

    fn apply_post_restore_purge(&self) -> Result<u64, StoreError> {
        let forget_hashes =
            current_record_forget_hashes(&self.live_db).map_err(|e| Box::new(e) as StoreError)?;
        if forget_hashes.is_empty() {
            return Ok(0);
        }
        let targets =
            collect_target_ids(&self.restored_db).map_err(|e| Box::new(e) as StoreError)?;
        let to_purge: Vec<TargetId> = targets
            .into_iter()
            .filter(|t| forget_hashes.contains(&target_id_hash(t.as_str())))
            .collect();
        if to_purge.is_empty() {
            return Ok(0);
        }
        let count = u64::try_from(to_purge.len()).unwrap_or(u64::MAX);
        purge_targets(&self.restored_db, &to_purge).map_err(|e| Box::new(e) as StoreError)?;
        Ok(count)
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
}
