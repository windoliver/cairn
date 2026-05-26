//! `SnapshotMetadataProvider` impl backed by direct sqlite access.
//!
//! Table name reference (verified against migrations/):
//!   - `records` (migration 0001): live rows; `tombstoned = 1` rows are tombstones.
//!   - `wal_ops` (migration 0002): WAL operations; `MAX(issued_seq)` is the frontier.
//!   - migration head: 69 (`0069_federation_consent_indexes` is the latest).

use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::snapshot_artifact::SnapshotMetadataProvider;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `SnapshotMetadataProvider` backed by a direct rusqlite connection to the
/// vault's `cairn.db`. All queries are read-only.
pub struct SqliteSnapshotMetadata {
    db_path: PathBuf,
    /// Vault id is typically constant per vault; passed in so this type
    /// has no startup cost. The CLI obtains it from `.cairn/vault.id` or
    /// from the `cairn_core::config` layer.
    vault_id: String,
}

impl SqliteSnapshotMetadata {
    /// Construct the metadata provider.
    ///
    /// `db_path` must point to a fully-migrated `cairn.db`.
    /// `vault_id` is the opaque vault identifier (e.g. a ULID or UUID
    /// stored in `.cairn/vault.id`).
    #[must_use]
    pub fn new(db_path: PathBuf, vault_id: String) -> Self {
        Self { db_path, vault_id }
    }
}

impl SnapshotMetadataProvider for SqliteSnapshotMetadata {
    /// Count of live (non-tombstoned, active) records.
    ///
    /// Uses the `records` table (migration 0001). `active = 1` rows
    /// are the live version of each lineage; `tombstoned = 0` excludes
    /// expired / forgotten rows.
    fn record_count(&self) -> Result<u64, StoreError> {
        crate::vec_ext::register_vec0();
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| Box::new(e) as StoreError)?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Count of tombstoned rows in the `records` table.
    ///
    /// Tombstones are rows with `tombstoned = 1` regardless of `active`.
    /// These represent expired, forgotten, or superseded record lineages.
    fn tombstone_count(&self) -> Result<u64, StoreError> {
        crate::vec_ext::register_vec0();
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| Box::new(e) as StoreError)?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE tombstoned = 1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// WAL frontier step: `"step:<N>"` where N is the highest `issued_seq`
    /// committed to `wal_ops`, or `"step:0"` for an empty vault.
    ///
    /// Uses `wal_ops` (migration 0002). `issued_seq` strictly advances
    /// with every WAL operation, making `MAX(issued_seq)` a deterministic
    /// replay cursor for the vault state at snapshot time.
    fn frontier_step(&self) -> Result<String, StoreError> {
        crate::vec_ext::register_vec0();
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| Box::new(e) as StoreError)?;
        // `wal_ops` may be absent on a very fresh DB that has not yet
        // run migration 0002. Fall back to step:0 gracefully.
        let table_exists: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='wal_ops')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        if table_exists == 0 {
            return Ok("step:0".to_string());
        }
        let max_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(issued_seq), 0) FROM wal_ops",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(format!("step:{max_seq}"))
    }

    fn vault_id(&self) -> Result<String, StoreError> {
        Ok(self.vault_id.clone())
    }

    /// Per-component migration heads. Returns the count of rows in
    /// `schema_migrations`, which equals the highest `migration_id`
    /// applied (the table is append-only and immutable once stamped).
    ///
    /// For v0.2 the only component is `"store"` (the embedded `SQLite`
    /// migration chain). The `"wal"` component has no separate migration
    /// table — WAL schema ships in the main chain — so it reports `0`.
    fn migration_heads(&self) -> Result<BTreeMap<String, u32>, StoreError> {
        crate::vec_ext::register_vec0();
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| Box::new(e) as StoreError)?;
        // Introspect the actual max migration_id so this stays accurate
        // as migrations accumulate.
        let max_id: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(migration_id), 0) FROM schema_migrations",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        let mut m = BTreeMap::new();
        m.insert(
            "store".to_string(),
            u32::try_from(max_id).unwrap_or(u32::MAX),
        );
        // WAL schema is embedded in the main store migration chain;
        // no separate migration table exists for it in v0.2.
        m.insert("wal".to_string(), 0);
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open an on-disk DB with all migrations applied, query metadata,
    /// and assert basic invariants.
    #[test]
    fn metadata_on_fresh_migrated_db() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let db_path = dir.path().join("cairn.db");

        // Apply all migrations to an on-disk DB.
        {
            crate::vec_ext::register_vec0();
            let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;",
            )
            .expect("pragmas");
            crate::migrations::migrations()
                .to_latest(&mut conn)
                .expect("migrations");
        }

        let meta =
            SqliteSnapshotMetadata::new(db_path.clone(), "vault-test-id".to_string());

        assert_eq!(meta.record_count().expect("record_count"), 0);
        assert_eq!(meta.tombstone_count().expect("tombstone_count"), 0);

        let step = meta.frontier_step().expect("frontier_step");
        assert!(
            step.starts_with("step:"),
            "frontier_step must start with 'step:', got: {step}"
        );

        assert_eq!(meta.vault_id().expect("vault_id"), "vault-test-id");

        let heads = meta.migration_heads().expect("migration_heads");
        assert!(heads.contains_key("store"), "must have 'store' key");
        assert!(
            *heads.get("store").expect("store head") > 0,
            "store migration head must be positive"
        );
        assert!(
            heads.contains_key("wal"),
            "must have 'wal' key"
        );
    }
}
