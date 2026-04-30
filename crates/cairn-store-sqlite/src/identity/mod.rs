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
    keys::{IdentityRevision, KeyVersion, VaultId, WitnessHash},
    receipts::{RevocationReceipt, RotationReceipt},
    records::{
        FirstBindState, IdentityKeyEntry, PendingEvictionEntry, PendingIdentityEntry,
        PendingKeyDisableEntry, ProvisioningState, PublicIdentityRecord, PurgePendingEntry,
        ReceiptId, RevokePendingEntry,
    },
};

use rusqlite::OptionalExtension as _;

use queries::{
    in_placeholders, kind_str, parse_kind, parse_state, row_to_identity_key_entry,
    row_to_pending_identity_entry, row_to_public_identity_record, visibility_states,
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

    /// Apply [`MIGRATION_0002`] to `conn` if it has not already been applied.
    ///
    /// Uses the `SQLite` `user_version` pragma as a cheap schema-version guard
    /// so that opening an already-migrated database is idempotent. The version
    /// is set to `2` after the migration commits.
    fn run_migrations(conn: &Connection) -> Result<(), RegistryError> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        if version >= 2 {
            // Migration already applied — skip to avoid "table already exists".
            return Ok(());
        }
        conn.execute_batch(MIGRATION_0002)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        conn.execute_batch("PRAGMA user_version = 2")
            .map_err(|e| RegistryError::Backend(Box::new(e)))
    }

    /// Return a locked [`Connection`] guard for use in integration tests that
    /// need to execute raw SQL to verify schema-level constraints.
    ///
    /// This method is intentionally `pub` so that tests in the `tests/`
    /// directory can call it, but it is `#[doc(hidden)]` to signal that it
    /// is not part of the stable public API.
    #[doc(hidden)]
    pub fn test_connection(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }
}

// ── Private helpers for get_first_bind_state ─────────────────────────────────

/// Build a [`PublicIdentityRecord`] from raw column values read from the
/// `identities` table.  Called only from [`IdentityRegistry::get_first_bind_state`].
fn first_bind_record(
    id_str: String,
    kind_str_val: &str,
    kv_u32: u32,
    state_str_val: &str,
    created_str: &str,
) -> Result<(Identity, ProvisioningState, PublicIdentityRecord), RegistryError> {
    let identity = Identity::parse(id_str).map_err(domain_err)?;
    let kind = parse_kind(kind_str_val)
        .map_err(|_| RegistryError::Backend("unknown identity kind".into()))?;
    let nz = std::num::NonZeroU32::new(kv_u32)
        .ok_or_else(|| RegistryError::Backend("current_key_version is 0".into()))?;
    let current_key_version = KeyVersion::new(nz);
    let provisioning_state = parse_state(state_str_val)?;
    let created_at = chrono::DateTime::parse_from_rfc3339(created_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;
    let record = PublicIdentityRecord {
        id: identity.clone(),
        kind,
        current_key_version,
        revision: IdentityRevision::FIRST,
        provisioning_state,
        created_at,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    Ok((identity, provisioning_state, record))
}

/// Build an [`IdentityKeyEntry`] from raw column values read from the
/// `identity_keys` table.  Called only from [`IdentityRegistry::get_first_bind_state`].
fn first_bind_key(
    identity: Identity,
    kv_u32_k: u32,
    pk_bytes: &[u8],
    signed_pred: Option<Vec<u8>>,
    key_created_str: &str,
    key_superseded_str: Option<&str>,
) -> Result<IdentityKeyEntry, RegistryError> {
    let nz_k = std::num::NonZeroU32::new(kv_u32_k)
        .ok_or_else(|| RegistryError::Backend("key_version is 0 in key row".into()))?;
    let key_version = KeyVersion::new(nz_k);
    let public_key: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| RegistryError::Backend("public_key must be 32 bytes".into()))?;
    let created_at = chrono::DateTime::parse_from_rfc3339(key_created_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;
    let superseded_at = key_superseded_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| RegistryError::Backend(Box::new(e)))
        })
        .transpose()?;
    Ok(IdentityKeyEntry {
        identity_id: identity,
        key_version,
        public_key,
        signed_predecessor: signed_pred,
        created_at,
        superseded_at,
    })
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
        vault_id: &VaultId,
    ) -> Result<FirstBindState, RegistryError> {
        let conn = self.conn.lock();

        // Read vault_meta. If absent (or different vault), this vault is Absent.
        let meta: Option<(String, Vec<u8>)> = conn
            .query_row(
                "SELECT vault_id, witness_sha256 FROM vault_meta WHERE rowid = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let Some((stored_id_str, witness_bytes)) = meta else {
            return Ok(FirstBindState::Absent);
        };
        if stored_id_str != vault_id.as_str() {
            return Ok(FirstBindState::Absent);
        }

        let witness_arr: [u8; 32] = witness_bytes
            .as_slice()
            .try_into()
            .map_err(|_| RegistryError::Backend("vault_meta witness_sha256 wrong length".into()))?;
        let witness_hash = WitnessHash::from_bytes(witness_arr);

        // Read the earliest identity row (the first-bind identity).
        let first_row: Option<(String, String, u32, String, String)> = conn
            .query_row(
                "SELECT id, kind, current_key_version, provisioning_state, created_at \
                 FROM identities ORDER BY created_at ASC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let Some((id_str, kind_str_val, kv_u32, state_str_val, created_str)) = first_row else {
            return Ok(FirstBindState::Absent);
        };

        let (identity, provisioning_state, record) =
            first_bind_record(id_str, &kind_str_val, kv_u32, &state_str_val, &created_str)?;

        // Fetch the identity's earliest key entry.
        let key_row: Option<(u32, Vec<u8>, Option<Vec<u8>>, String, Option<String>)> = conn
            .query_row(
                "SELECT key_version, public_key, signed_predecessor, created_at, superseded_at \
                 FROM identity_keys WHERE identity_id = ?1 ORDER BY key_version ASC LIMIT 1",
                rusqlite::params![identity.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let (kv_u32_k, pk_bytes, signed_pred, key_created_str, key_superseded_str) = key_row
            .ok_or_else(|| RegistryError::Backend("first-bind identity has no key row".into()))?;

        let key = first_bind_key(
            identity,
            kv_u32_k,
            &pk_bytes,
            signed_pred,
            &key_created_str,
            key_superseded_str.as_deref(),
        )?;

        match provisioning_state {
            ProvisioningState::Pending => Ok(FirstBindState::Reserved {
                record,
                key,
                witness_hash,
            }),
            ProvisioningState::Active | ProvisioningState::Revoked => {
                Ok(FirstBindState::Activated { record, key })
            }
            // Any other state (revoke_pending, purge_pending, purged) is
            // unexpected for the first-bind identity — treat as Absent for safety.
            _ => Ok(FirstBindState::Absent),
        }
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
        // States occupy placeholders ?1..?states.len(); kind is appended next
        // at ?{states.len()+1} (the `+ 2` in earlier revisions was an off-by-one).
        let kind_clause = if kind.is_some() {
            format!(" AND kind = ?{}", states.len() + 1)
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

    // ── Rotation (C6) ────────────────────────────────────────────────────────

    async fn apply_rotation(
        &self,
        receipt: &RotationReceipt,
        expected_current: KeyVersion,
        new_key: &IdentityKeyEntry,
    ) -> Result<ReceiptId, RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let new_v = receipt.payload.new_key_version.ok_or_else(|| {
            RegistryError::Backend("rotation receipt missing new_key_version".into())
        })?;
        let target = receipt.payload.target.as_str();

        // CAS: advance current_key_version only if it still equals expected_current.
        let updated = tx
            .execute(
                "UPDATE identities SET current_key_version = ?1 \
                 WHERE id = ?2 AND current_key_version = ?3",
                rusqlite::params![new_v.as_u32(), target, expected_current.as_u32()],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if updated == 0 {
            // Read the actual stored version to populate KeyVersionConflict.
            let actual: u32 = tx
                .query_row(
                    "SELECT current_key_version FROM identities WHERE id = ?1",
                    rusqlite::params![target],
                    |r| r.get(0),
                )
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            let nz = std::num::NonZeroU32::new(actual).ok_or_else(|| {
                RegistryError::Backend("invalid current_key_version stored".into())
            })?;
            return Err(RegistryError::KeyVersionConflict {
                existing: KeyVersion::new(nz),
                attempted: expected_current,
            });
        }

        // Stamp predecessor's superseded_at.
        tx.execute(
            "UPDATE identity_keys SET superseded_at = ?1 \
             WHERE identity_id = ?2 AND key_version = ?3",
            rusqlite::params![
                chrono::Utc::now().to_rfc3339(),
                target,
                expected_current.as_u32()
            ],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Insert the new key row in the same transaction (load-bearing: spec gap fix).
        insert_identity_key_row(&tx, new_key)?;

        // Delete the matching pending_rotations row (if any).
        tx.execute(
            "DELETE FROM pending_rotations WHERE identity_id = ?1 AND planned_version = ?2",
            rusqlite::params![target, new_v.as_u32()],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Insert the receipt row.
        let payload_bytes = receipt
            .payload
            .canonical_json()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        tx.execute(
            "INSERT INTO identity_receipts \
             (op_kind, target_identity, signer_identity, signer_key_version, \
              old_key_version, new_key_version, issued_at, signed_payload, \
              signature, pending_eviction) \
             VALUES ('rotation', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                target,
                receipt.payload.signer.as_str(),
                receipt.payload.signer_key_version.as_u32(),
                receipt.payload.old_key_version.map(KeyVersion::as_u32),
                new_v.as_u32(),
                receipt.payload.issued_at.to_rfc3339(),
                payload_bytes,
                &receipt.signature[..],
                i32::from(receipt.pending_eviction),
            ],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let receipt_id = ReceiptId(tx.last_insert_rowid());

        let wal_payload = serde_json::to_vec(&receipt.payload)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "apply_rotation", target, &wal_payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(receipt_id)
    }

    async fn insert_pending_rotation(
        &self,
        identity: &Identity,
        planned_version: KeyVersion,
        planned_handle: &str,
    ) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        tx.execute(
            "INSERT INTO pending_rotations \
             (identity_id, planned_version, planned_handle, intended_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                identity.as_str(),
                planned_version.as_u32(),
                planned_handle,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let payload = serde_json::json!({
            "identity": identity.as_str(),
            "planned_version": planned_version.as_u32(),
            "planned_handle": planned_handle,
        });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(
            &tx,
            "insert_pending_rotation",
            identity.as_str(),
            &payload_bytes,
        )?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn delete_pending_rotation(
        &self,
        identity: &Identity,
        planned_version: KeyVersion,
    ) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows_changed = tx
            .execute(
                "DELETE FROM pending_rotations \
                 WHERE identity_id = ?1 AND planned_version = ?2",
                rusqlite::params![identity.as_str(), planned_version.as_u32()],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            return Err(RegistryError::NotFound);
        }

        let payload = serde_json::json!({
            "identity": identity.as_str(),
            "planned_version": planned_version.as_u32(),
        });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(
            &tx,
            "delete_pending_rotation",
            identity.as_str(),
            &payload_bytes,
        )?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn list_pending_rotations(
        &self,
        identity: &Identity,
    ) -> Result<Vec<(KeyVersion, String)>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT planned_version, planned_handle \
                 FROM pending_rotations \
                 WHERE identity_id = ?1 \
                 ORDER BY planned_version",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows = stmt
            .query_map(rusqlite::params![identity.as_str()], |row| {
                let v: u32 = row.get(0)?;
                let h: String = row.get(1)?;
                Ok((v, h))
            })
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let mut result = Vec::with_capacity(rows.len());
        for (v, h) in rows {
            let nz = std::num::NonZeroU32::new(v)
                .ok_or_else(|| RegistryError::Backend("invalid planned_version stored".into()))?;
            result.push((KeyVersion::new(nz), h));
        }
        Ok(result)
    }

    // ── Revocation two-phase tombstone (C7) ──────────────────────────────────

    async fn begin_revocation(&self, receipt: &RevocationReceipt) -> Result<(), RegistryError> {
        let now = Utc::now().to_rfc3339();
        let target_str = receipt.payload.target.as_str().to_owned();

        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Phase 1: transition identities row active → revoke_pending.
        let rows_changed = tx
            .execute(
                "UPDATE identities \
                 SET provisioning_state = 'revoke_pending', revoked_at = ?1 \
                 WHERE id = ?2 AND provisioning_state = 'active'",
                rusqlite::params![now, &target_str],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            // Distinguish NotFound from bad-state.
            let existing_state: Option<String> = tx
                .query_row(
                    "SELECT provisioning_state FROM identities WHERE id = ?1",
                    rusqlite::params![&target_str],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;

            return match existing_state.as_deref() {
                None => Err(RegistryError::NotFound),
                Some(_) => Err(RegistryError::AlreadyRevoked {
                    id: receipt.payload.target.clone(),
                }),
            };
        }

        // Insert the receipt row with pending_key_disable=1.
        let payload_bytes = receipt
            .payload
            .canonical_json()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        tx.execute(
            "INSERT INTO identity_receipts \
             (op_kind, target_identity, signer_identity, signer_key_version, \
              old_key_version, new_key_version, issued_at, signed_payload, \
              signature, pending_key_disable) \
             VALUES ('revocation', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            rusqlite::params![
                &target_str,
                receipt.payload.signer.as_str(),
                receipt.payload.signer_key_version.as_u32(),
                receipt.payload.old_key_version.map(KeyVersion::as_u32),
                receipt.payload.new_key_version.map(KeyVersion::as_u32),
                receipt.payload.issued_at.to_rfc3339(),
                payload_bytes,
                &receipt.signature[..],
            ],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let wal_payload = serde_json::to_vec(&receipt.payload)
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "begin_revocation", &target_str, &wal_payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn finalise_revocation(&self, id: &Identity) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Phase 2: transition revoke_pending → revoked.
        let rows_changed = tx
            .execute(
                "UPDATE identities \
                 SET provisioning_state = 'revoked' \
                 WHERE id = ?1 AND provisioning_state = 'revoke_pending'",
                rusqlite::params![id.as_str()],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            return Err(RegistryError::NotFound);
        }

        // Clear the pending_key_disable flag on the matching receipt row(s).
        tx.execute(
            "UPDATE identity_receipts \
             SET pending_key_disable = 0 \
             WHERE target_identity = ?1 AND op_kind = 'revocation' AND pending_key_disable = 1",
            rusqlite::params![id.as_str()],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let payload = id.as_str().as_bytes();
        wal::wal_insert(&tx, "finalise_revocation", id.as_str(), payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn list_revoke_pending(&self) -> Result<Vec<RevokePendingEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT ir.target_identity, i.revoked_at, ir.rowid \
                 FROM identity_receipts ir \
                 JOIN identities i ON i.id = ir.target_identity \
                 WHERE i.provisioning_state = 'revoke_pending' \
                   AND ir.op_kind = 'revocation' \
                   AND ir.pending_key_disable = 1",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let entries = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let revoked_str: String = row.get(1)?;
                let rowid: i64 = row.get(2)?;
                Ok((id_str, revoked_str, rowid))
            })
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let mut result = Vec::with_capacity(entries.len());
        for (id_str, revoked_str, rowid) in entries {
            let identity = Identity::parse(id_str).map_err(domain_err)?;
            let revoked_at = chrono::DateTime::parse_from_rfc3339(&revoked_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            result.push(RevokePendingEntry {
                identity,
                revoked_at,
                receipt_id: ReceiptId(rowid),
            });
        }
        Ok(result)
    }

    // ── Two-phase purge tombstone (C8) ────────────────────────────────────────

    async fn mark_purge_pending(
        &self,
        id: &Identity,
        _ack: &PurgeAcknowledgement,
        reason: PurgeReason,
    ) -> Result<(), RegistryError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Validate the current state before transitioning.
        let existing_state: Option<String> = tx
            .query_row(
                "SELECT provisioning_state FROM identities WHERE id = ?1",
                rusqlite::params![id.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let state_str_val = existing_state.ok_or(RegistryError::NotFound)?;
        let current_state = parse_state(&state_str_val)?;

        // Purge is allowed from: pending, active, revoked — not from
        // revoke_pending, purge_pending, or purged.
        match current_state {
            ProvisioningState::Pending | ProvisioningState::Active | ProvisioningState::Revoked => {
            }
            invalid => {
                return Err(RegistryError::InvalidPurgeStartState {
                    id: id.clone(),
                    state: invalid,
                });
            }
        }

        tx.execute(
            "UPDATE identities \
             SET provisioning_state = 'purge_pending', \
                 purge_requested_at = ?1, \
                 purge_reason = ?2 \
             WHERE id = ?3",
            rusqlite::params![now, &reason.0, id.as_str()],
        )
        .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let payload = serde_json::json!({
            "identity": id.as_str(),
            "reason": reason.0,
        });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "mark_purge_pending", id.as_str(), &payload_bytes)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn finalise_purge(&self, id: &Identity) -> Result<(), RegistryError> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows_changed = tx
            .execute(
                "UPDATE identities \
                 SET provisioning_state = 'purged', purged_at = ?1 \
                 WHERE id = ?2 AND provisioning_state = 'purge_pending'",
                rusqlite::params![now, id.as_str()],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            return Err(RegistryError::NotFound);
        }

        let payload = id.as_str().as_bytes();
        wal::wal_insert(&tx, "finalise_purge", id.as_str(), payload)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn list_purge_pending(&self) -> Result<Vec<PurgePendingEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, purge_requested_at, purge_reason \
                 FROM identities \
                 WHERE provisioning_state = 'purge_pending'",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let purge_req_str: String = row.get(1)?;
                let purge_reason: String = row.get(2)?;
                Ok((id_str, purge_req_str, purge_reason))
            })
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let mut result = Vec::with_capacity(rows.len());
        for (id_str, purge_req_str, purge_reason) in rows {
            let identity = Identity::parse(id_str).map_err(domain_err)?;
            let purge_requested_at = chrono::DateTime::parse_from_rfc3339(&purge_req_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            result.push(PurgePendingEntry {
                identity,
                purge_requested_at,
                purge_reason,
            });
        }
        Ok(result)
    }

    // ── Receipt reconciliation flag-clear stubs (C9) ──────────────────────────

    async fn clear_pending_eviction(&self, receipt_id: &ReceiptId) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Read the target_identity before we clear the flag (needed for WAL).
        let target: Option<String> = tx
            .query_row(
                "SELECT target_identity FROM identity_receipts WHERE rowid = ?1",
                rusqlite::params![receipt_id.0],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let target = target.ok_or(RegistryError::NotFound)?;

        // Idempotent: clearing an already-cleared flag is a successful no-op.
        // Reserve `NotFound` for missing receipt rows; recovery code re-runs
        // `clear_pending_*` on retry and must converge cleanly.
        let rows_changed = tx
            .execute(
                "UPDATE identity_receipts SET pending_eviction = 0 \
                 WHERE rowid = ?1 AND pending_eviction = 1",
                rusqlite::params![receipt_id.0],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            // Receipt row exists (target was Some above) but flag is already
            // clear → idempotent success; skip the WAL write.
            tx.commit()
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            return Ok(());
        }

        let payload = serde_json::json!({ "receipt_id": receipt_id.0 });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "clear_pending_eviction", &target, &payload_bytes)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn list_pending_evictions(&self) -> Result<Vec<PendingEvictionEntry>, RegistryError> {
        // Per spec §3.6: a pending_eviction receipt records that the OLD key
        // version (the predecessor that was just superseded) still needs to be
        // deleted from the keystore. Returning `new_key_version` here would
        // make repair/reconcile delete the freshly active key — a data-loss
        // bug. Always source `evict_version` from `old_key_version`.
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT rowid, target_identity, old_key_version, issued_at \
                 FROM identity_receipts \
                 WHERE pending_eviction = 1",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows = stmt
            .query_map([], |row| {
                let rowid: i64 = row.get(0)?;
                let target_str: String = row.get(1)?;
                let old_kv_u32: Option<u32> = row.get(2)?;
                let issued_str: String = row.get(3)?;
                Ok((rowid, target_str, old_kv_u32, issued_str))
            })
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let mut result = Vec::with_capacity(rows.len());
        for (rowid, target_str, old_kv_u32, issued_str) in rows {
            let identity = Identity::parse(target_str).map_err(domain_err)?;
            let evict_version_raw = old_kv_u32.ok_or_else(|| {
                RegistryError::Backend("pending_eviction receipt has NULL old_key_version".into())
            })?;
            let nz = std::num::NonZeroU32::new(evict_version_raw).ok_or_else(|| {
                RegistryError::Backend("old_key_version is 0 in eviction receipt".into())
            })?;
            let evict_version = KeyVersion::new(nz);
            let rotated_at = chrono::DateTime::parse_from_rfc3339(&issued_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            result.push(PendingEvictionEntry {
                receipt_id: ReceiptId(rowid),
                identity,
                evict_version,
                rotated_at,
            });
        }
        Ok(result)
    }

    async fn clear_pending_key_disable(&self, receipt_id: &ReceiptId) -> Result<(), RegistryError> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // Read the target_identity before we clear the flag (needed for WAL).
        let target: Option<String> = tx
            .query_row(
                "SELECT target_identity FROM identity_receipts WHERE rowid = ?1",
                rusqlite::params![receipt_id.0],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let target = target.ok_or(RegistryError::NotFound)?;

        // Idempotent: clearing an already-cleared flag is a successful no-op.
        // See `clear_pending_eviction` for rationale.
        let rows_changed = tx
            .execute(
                "UPDATE identity_receipts SET pending_key_disable = 0 \
                 WHERE rowid = ?1 AND pending_key_disable = 1",
                rusqlite::params![receipt_id.0],
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        if rows_changed == 0 {
            tx.commit()
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            return Ok(());
        }

        let payload = serde_json::json!({ "receipt_id": receipt_id.0 });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        wal::wal_insert(&tx, "clear_pending_key_disable", &target, &payload_bytes)?;

        tx.commit()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn list_pending_key_disables(
        &self,
    ) -> Result<Vec<PendingKeyDisableEntry>, RegistryError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT rowid, target_identity, issued_at \
                 FROM identity_receipts \
                 WHERE pending_key_disable = 1",
            )
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        let rows = stmt
            .query_map([], |row| {
                let rowid: i64 = row.get(0)?;
                let target_str: String = row.get(1)?;
                let issued_str: String = row.get(2)?;
                Ok((rowid, target_str, issued_str))
            })
            .map_err(|e| RegistryError::Backend(Box::new(e)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RegistryError::Backend(Box::new(e)))?;

        // For each entry, fetch active key versions as retained_versions.
        let mut result = Vec::with_capacity(rows.len());
        for (rowid, target_str, issued_str) in rows {
            let identity = Identity::parse(target_str.clone()).map_err(domain_err)?;
            let revoked_at = chrono::DateTime::parse_from_rfc3339(&issued_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;

            // Retained versions = keys NOT yet superseded for this identity.
            let mut kv_stmt = conn
                .prepare(
                    "SELECT key_version FROM identity_keys \
                     WHERE identity_id = ?1 AND superseded_at IS NULL \
                     ORDER BY key_version",
                )
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;
            let kv_rows = kv_stmt
                .query_map(rusqlite::params![&target_str], |r| {
                    let v: u32 = r.get(0)?;
                    Ok(v)
                })
                .map_err(|e| RegistryError::Backend(Box::new(e)))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| RegistryError::Backend(Box::new(e)))?;

            let mut retained_versions = Vec::with_capacity(kv_rows.len());
            for v in kv_rows {
                let nz = std::num::NonZeroU32::new(v)
                    .ok_or_else(|| RegistryError::Backend("key_version is 0".into()))?;
                retained_versions.push(KeyVersion::new(nz));
            }

            result.push(PendingKeyDisableEntry {
                receipt_id: ReceiptId(rowid),
                identity,
                revoked_at,
                retained_versions,
            });
        }
        Ok(result)
    }
}
