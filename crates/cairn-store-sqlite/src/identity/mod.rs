//! [`IdentityRegistry`] `SQLite` adapter (issue #50).
//!
//! This module owns [`SqliteIdentityRegistry`], which backs
//! [`cairn_core::contract::identity_registry::IdentityRegistry`] against a
//! real `SQLite` database via `rusqlite`.  Schema is applied by embedding
//! `migrations/0002_identity.sql` at compile time; no external migration
//! runner is required.
//!
//! Submodule layout:
//! - [`queries`] — read-path SQL helpers (lands in C4+).
//! - [`wal`]     — `identity_wal` insert helper (lands in C4+).

mod queries;
mod wal;

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::Connection;

use cairn_core::contract::identity_registry::{
    IdentityRegistry, IdentityVisibility, PurgeAcknowledgement, PurgeReason, RegistryError,
};
use cairn_core::domain::DomainError;
use cairn_core::domain::identity::{
    Identity, IdentityKind,
    keys::{KeyVersion, VaultId, WitnessHash},
    receipts::{RevocationReceipt, RotationReceipt},
    records::{
        FirstBindState, IdentityKeyEntry, PendingEvictionEntry, PendingIdentityEntry,
        PendingKeyDisableEntry, PublicIdentityRecord, PurgePendingEntry, ReceiptId,
        RevokePendingEntry,
    },
};

use rusqlite::OptionalExtension as _;

use queries::{
    in_placeholders, kind_str, row_to_identity_key_entry, row_to_pending_identity_entry,
    row_to_public_identity_record, visibility_states,
};

/// Compiled-in `0002_identity.sql` migration DDL.
const MIGRATION_0002: &str = include_str!("../../migrations/0002_identity.sql");

/// Convert a [`DomainError`] into a [`RegistryError::Backend`] boxed error.
///
/// Used wherever `VaultId::parse` (or another domain constructor) is called
/// on data read back from the database — parse failures are unexpected and
/// treated as backend corruption.
fn domain_err(e: DomainError) -> RegistryError {
    RegistryError::Backend(Box::new(e))
}

/// `SQLite`-backed implementation of [`cairn_core::contract::identity_registry::IdentityRegistry`].
///
/// Each instance wraps a single `rusqlite` [`Connection`] behind an
/// [`Arc`]`<`[`Mutex`]`>` so it can be shared across `async` tasks via
/// `spawn_blocking` (the mutex is `parking_lot` — never held across an
/// `.await` point).
///
/// Construct with [`SqliteIdentityRegistry::open_in_memory`] (tests) or
/// [`SqliteIdentityRegistry::open`] (production).
pub struct SqliteIdentityRegistry {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteIdentityRegistry {
    /// Open an in-memory `SQLite` database and apply the identity migration.
    ///
    /// Intended for unit and integration tests; the database is discarded when
    /// the returned value is dropped.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] if the connection or migration fails.
    pub fn open_in_memory() -> Result<Self, RegistryError> {
        let conn = Connection::open_in_memory().map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open (or create) a `SQLite` database at `db_path` and apply the
    /// identity migration.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] if the connection or migration fails.
    pub fn open(db_path: &Path) -> Result<Self, RegistryError> {
        let conn = Connection::open(db_path).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Apply [`MIGRATION_0002`] to `conn` in a single `execute_batch` call.
    fn run_migrations(conn: &Connection) -> Result<(), RegistryError> {
        conn.execute_batch(MIGRATION_0002)
            .map_err(|e| RegistryError::Backend(Box::new(e)))
    }
}

// ── Helper functions placed alongside the impl ────────────────────────────────

/// Insert a row into `identities` inside an open transaction.
///
/// # Errors
/// Returns [`RegistryError::Backend`] on SQL failure.
pub(super) fn insert_identity_row(
    tx: &rusqlite::Transaction<'_>,
    record: &PublicIdentityRecord,
) -> Result<(), RegistryError> {
    let kind_str = match record.kind {
        IdentityKind::Human => "human",
        IdentityKind::Agent => "agent",
        IdentityKind::Sensor => "sensor",
    };
    tx.execute(
        "INSERT INTO identities \
         (id, kind, current_key_version, provisioning_state, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            record.id.as_str(),
            kind_str,
            record.current_key_version.as_u32(),
            "pending",
            record.created_at.to_rfc3339(),
        ],
    )
    .map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}

/// Insert a row into `identity_keys` inside an open transaction.
///
/// # Errors
/// Returns [`RegistryError::Backend`] on SQL failure.
pub(super) fn insert_identity_key_row(
    tx: &rusqlite::Transaction<'_>,
    key: &IdentityKeyEntry,
) -> Result<(), RegistryError> {
    tx.execute(
        "INSERT INTO identity_keys \
         (identity_id, key_version, public_key, signed_predecessor, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            key.identity_id.as_str(),
            key.key_version.as_u32(),
            &key.public_key[..],
            key.signed_predecessor.as_deref(),
            key.created_at.to_rfc3339(),
        ],
    )
    .map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}

// ── IdentityRegistry trait implementation ────────────────────────────────────

/// `#[allow(clippy::unimplemented)]` is intentional: methods marked
/// `unimplemented!("Task CN")` will be filled in incrementally per the
/// C4-C12 plan tasks.  Each stub carries its target task label so reviewers
/// know exactly where the implementation will land.
#[allow(
    clippy::unimplemented,
    reason = "filled in incrementally per plan tasks C5-C10"
)]
#[async_trait]
impl IdentityRegistry for SqliteIdentityRegistry {
    // ── First-bind transaction (C4) ───────────────────────────────────────────

    async fn reserve_first_identity(
        &self,
        vault_id: &VaultId,
        record: &PublicIdentityRecord,
        key: &IdentityKeyEntry,
        witness_hash: WitnessHash,
        binding_path: &Path,
    ) -> Result<(), RegistryError> {
        use sha2::{Digest, Sha256};

        // 1. Verify the binding file contents match the supplied witness hash.
        let bytes = std::fs::read(binding_path).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        let actual_hash: [u8; 32] = Sha256::digest(&bytes).into();
        if &actual_hash != witness_hash.as_bytes() {
            return Err(RegistryError::WitnessMismatch);
        }

        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // 2. Idempotent resume: if vault_meta already exists, check for mismatch.
        let existing: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT vault_id, witness_sha256 FROM vault_meta WHERE rowid = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        if let Some((stored_id, stored_hash)) = existing {
            let stored_arr: [u8; 32] = stored_hash
                .as_slice()
                .try_into()
                .map_err(|_| RegistryError::Backend("vault_meta hash has wrong length".into()))?;
            // If the vault_id or hash differ, this is a mismatch.
            if stored_id != vault_id.as_str() || stored_arr != *witness_hash.as_bytes() {
                return Err(RegistryError::FirstBindMismatch {
                    stored: VaultId::parse(stored_id).map_err(domain_err)?,
                    attempted: vault_id.clone(),
                });
            }
            // Same vault_id — idempotent resume path.
            // If the identity row is already present, we're done.
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM identities WHERE id = ?1",
                    rusqlite::params![record.id.as_str()],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if exists {
                return Ok(());
            }
            // vault_meta committed but identity row absent — treat as
            // FirstBindAlreadyCommitted (rare crash window after vault_meta
            // insert but before identity insert; caller should start fresh).
            return Err(RegistryError::FirstBindAlreadyCommitted);
        }

        // 3. Fresh first-bind: insert vault_meta + identity + key + WAL row atomically.
        tx.execute(
            "INSERT INTO vault_meta \
             (rowid, vault_id, witness_sha256, binding_path, witness_created_at) \
             VALUES (1, ?1, ?2, ?3, ?4)",
            rusqlite::params![
                vault_id.as_str(),
                &witness_hash.as_bytes()[..],
                binding_path.display().to_string(),
                record.created_at.to_rfc3339(),
            ],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        insert_identity_row(&tx, record)?;
        insert_identity_key_row(&tx, key)?;

        let payload =
            serde_json::to_vec(record).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "reserve_first_identity", record.id.as_str(), &payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn get_first_bind_state(
        &self,
        _vault_id: &VaultId,
    ) -> Result<FirstBindState, RegistryError> {
        unimplemented!("Task C10")
    }

    // ── vault_meta read (C4) ──────────────────────────────────────────────────

    async fn read_vault_meta(&self) -> Result<Option<(VaultId, WitnessHash)>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT vault_id, witness_sha256 FROM vault_meta WHERE rowid = 1")
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        if let Some(row) = rows
            .next()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
        {
            let id: String = row
                .get(0)
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            let hash: Vec<u8> = row
                .get(1)
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            let arr: [u8; 32] = hash
                .as_slice()
                .try_into()
                .map_err(|_| RegistryError::Backend("vault_meta hash has wrong length".into()))?;
            Ok(Some((
                VaultId::parse(id).map_err(domain_err)?,
                WitnessHash::from_bytes(arr),
            )))
        } else {
            Ok(None)
        }
    }

    // ── Provisioning state machine stubs (C5) ────────────────────────────────

    async fn reserve_identity(
        &self,
        record: &PublicIdentityRecord,
        key: &IdentityKeyEntry,
    ) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Require vault_meta to exist (first-bind must have committed).
        let meta_exists: bool = tx
            .query_row("SELECT 1 FROM vault_meta WHERE rowid = 1", [], |_| Ok(true))
            .unwrap_or(false);
        if !meta_exists {
            return Err(RegistryError::VaultMetaMissing);
        }

        // Check whether the identity is already known.
        let existing_state: Option<String> = tx
            .query_row(
                "SELECT provisioning_state FROM identities WHERE id = ?1",
                rusqlite::params![record.id.as_str()],
                |r| r.get(0),
            )
            .ok();

        if let Some(state_str) = existing_state {
            return if state_str == "pending" {
                Err(RegistryError::ProvisioningInFlight {
                    id: record.id.clone(),
                })
            } else {
                Err(RegistryError::IdentityExists {
                    id: record.id.clone(),
                })
            };
        }

        // Insert the pending identity row + key row.
        insert_identity_row(&tx, record)?;
        insert_identity_key_row(&tx, key)?;

        let payload =
            serde_json::to_vec(record).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "reserve_identity", record.id.as_str(), &payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn activate_identity(
        &self,
        id: &Identity,
        key_version: KeyVersion,
    ) -> Result<(), RegistryError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows_changed = tx
            .execute(
                "UPDATE identities \
                 SET provisioning_state = 'active', activated_at = ?1 \
                 WHERE id = ?2 \
                   AND current_key_version = ?3 \
                   AND provisioning_state = 'pending'",
                rusqlite::params![now, id.as_str(), key_version.as_u32()],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            // Distinguish: does the row exist at all?
            let existing: Option<(String, u32)> = tx
                .query_row(
                    "SELECT provisioning_state, current_key_version \
                     FROM identities WHERE id = ?1",
                    rusqlite::params![id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();

            return match existing {
                None => Err(RegistryError::NotFound),
                Some((state, _)) if state != "pending" => {
                    Err(RegistryError::Backend("invalid state transition".into()))
                }
                Some((_, stored_kv)) => {
                    use std::num::NonZeroU32;
                    let nz = NonZeroU32::new(stored_kv)
                        .ok_or_else(|| RegistryError::Backend("stored key_version is 0".into()))?;
                    Err(RegistryError::KeyVersionConflict {
                        existing: KeyVersion::new(nz),
                        attempted: key_version,
                    })
                }
            };
        }

        let payload = id.as_str().as_bytes();
        wal::wal_insert(&tx, "activate_identity", id.as_str(), payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn delete_pending(
        &self,
        id: &Identity,
        key_version: KeyVersion,
    ) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Delete the identity row when it is pending; ON DELETE CASCADE removes
        // the identity_keys row automatically.  We key on id + provisioning_state
        // so we never accidentally delete an active/revoked identity.
        let rows_changed = tx
            .execute(
                "DELETE FROM identities WHERE id = ?1 AND provisioning_state = 'pending'",
                rusqlite::params![id.as_str()],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            // The row either doesn't exist or isn't pending — treat as NotFound.
            return Err(RegistryError::NotFound);
        }

        let payload = serde_json::to_vec(&key_version.as_u32())
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "delete_pending", id.as_str(), &payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn list_pending(&self) -> Result<Vec<PendingIdentityEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT ik.identity_id, ik.key_version, ik.public_key, ik.created_at \
                 FROM identity_keys ik \
                 WHERE ik.identity_id IN \
                       (SELECT id FROM identities WHERE provisioning_state = 'pending')",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let entries = stmt
            .query_map([], row_to_pending_identity_entry)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        Ok(entries)
    }

    async fn list_pending_by_identity(
        &self,
        id: &Identity,
    ) -> Result<Vec<PendingIdentityEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT ik.identity_id, ik.key_version, ik.public_key, ik.created_at \
                 FROM identity_keys ik \
                 WHERE ik.identity_id IN \
                       (SELECT id FROM identities WHERE provisioning_state = 'pending') \
                   AND ik.identity_id = ?1",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let entries = stmt
            .query_map(
                rusqlite::params![id.as_str()],
                row_to_pending_identity_entry,
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        Ok(entries)
    }

    // ── Read paths / visibility (C5) ─────────────────────────────────────────

    async fn get_identity(
        &self,
        id: &Identity,
        visibility: IdentityVisibility,
    ) -> Result<Option<PublicIdentityRecord>, RegistryError> {
        let states = visibility_states(visibility);
        let placeholders = in_placeholders(states.len(), 2); // ?2, ?3, …
        let sql = format!(
            "SELECT id, kind, current_key_version, provisioning_state, \
                    created_at, activated_at, revoked_at, purge_requested_at, purged_at \
             FROM identities \
             WHERE id = ?1 AND provisioning_state IN ({placeholders})"
        );

        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Bind id as first param, then each state string.
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(states.len() + 1);
        params.push(Box::new(id.as_str().to_owned()));
        for s in states {
            params.push(Box::new(*s));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(std::convert::AsRef::as_ref).collect();

        let result = stmt
            .query_row(params_refs.as_slice(), row_to_public_identity_record)
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        Ok(result)
    }

    async fn list_identities(
        &self,
        kind: Option<IdentityKind>,
        visibility: IdentityVisibility,
    ) -> Result<Vec<PublicIdentityRecord>, RegistryError> {
        let states = visibility_states(visibility);

        // Build the kind filter dynamically.
        let kind_clause = if kind.is_some() {
            format!(" AND kind = ?{}", states.len() + 2)
        } else {
            String::new()
        };

        let placeholders = in_placeholders(states.len(), 1);
        let sql = format!(
            "SELECT id, kind, current_key_version, provisioning_state, \
                    created_at, activated_at, revoked_at, purge_requested_at, purged_at \
             FROM identities \
             WHERE provisioning_state IN ({placeholders}){kind_clause}"
        );

        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(states.len() + 1);
        for s in states {
            params.push(Box::new(*s));
        }
        if let Some(k) = kind {
            params.push(Box::new(kind_str(k)));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(std::convert::AsRef::as_ref).collect();

        let rows = stmt
            .query_map(params_refs.as_slice(), row_to_public_identity_record)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        Ok(rows)
    }

    async fn list_keys(&self, id: &Identity) -> Result<Vec<IdentityKeyEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT identity_id, key_version, public_key, signed_predecessor, \
                        created_at, superseded_at \
                 FROM identity_keys WHERE identity_id = ?1",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows = stmt
            .query_map(rusqlite::params![id.as_str()], row_to_identity_key_entry)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        Ok(rows)
    }

    async fn count_keys(&self) -> Result<u64, RegistryError> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM identity_keys", [], |r| r.get(0))
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(count.unsigned_abs())
    }

    async fn list_all_keys(&self) -> Result<Vec<IdentityKeyEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT identity_id, key_version, public_key, signed_predecessor, \
                        created_at, superseded_at \
                 FROM identity_keys",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows = stmt
            .query_map([], row_to_identity_key_entry)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        Ok(rows)
    }

    // ── Rotation stubs (C6) ───────────────────────────────────────────────────

    async fn apply_rotation(
        &self,
        _receipt: &RotationReceipt,
        _expected_current: KeyVersion,
    ) -> Result<(), RegistryError> {
        unimplemented!("Task C6")
    }

    async fn insert_pending_rotation(
        &self,
        _identity: &Identity,
        _planned_version: KeyVersion,
        _planned_handle: &str,
    ) -> Result<(), RegistryError> {
        unimplemented!("Task C6")
    }

    async fn delete_pending_rotation(
        &self,
        _identity: &Identity,
        _planned_version: KeyVersion,
    ) -> Result<(), RegistryError> {
        unimplemented!("Task C6")
    }

    async fn list_pending_rotations(
        &self,
        _identity: &Identity,
    ) -> Result<Vec<(KeyVersion, String)>, RegistryError> {
        unimplemented!("Task C6")
    }

    // ── Revocation two-phase tombstone stubs (C7) ─────────────────────────────

    async fn begin_revocation(&self, _receipt: &RevocationReceipt) -> Result<(), RegistryError> {
        unimplemented!("Task C7")
    }

    async fn finalise_revocation(&self, _id: &Identity) -> Result<(), RegistryError> {
        unimplemented!("Task C7")
    }

    async fn list_revoke_pending(&self) -> Result<Vec<RevokePendingEntry>, RegistryError> {
        unimplemented!("Task C7")
    }

    // ── Two-phase purge tombstone stubs (C8) ──────────────────────────────────

    async fn mark_purge_pending(
        &self,
        _id: &Identity,
        _ack: &PurgeAcknowledgement,
        _reason: PurgeReason,
    ) -> Result<(), RegistryError> {
        unimplemented!("Task C8")
    }

    async fn finalise_purge(&self, _id: &Identity) -> Result<(), RegistryError> {
        unimplemented!("Task C8")
    }

    async fn list_purge_pending(&self) -> Result<Vec<PurgePendingEntry>, RegistryError> {
        unimplemented!("Task C8")
    }

    // ── Receipt reconciliation flag-clear stubs (C9) ──────────────────────────

    async fn clear_pending_eviction(&self, _receipt_id: &ReceiptId) -> Result<(), RegistryError> {
        unimplemented!("Task C9")
    }

    async fn list_pending_evictions(&self) -> Result<Vec<PendingEvictionEntry>, RegistryError> {
        unimplemented!("Task C9")
    }

    async fn clear_pending_key_disable(
        &self,
        _receipt_id: &ReceiptId,
    ) -> Result<(), RegistryError> {
        unimplemented!("Task C9")
    }

    async fn list_pending_key_disables(
        &self,
    ) -> Result<Vec<PendingKeyDisableEntry>, RegistryError> {
        unimplemented!("Task C9")
    }
}
