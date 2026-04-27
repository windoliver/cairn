//! [`FixtureStore`] — `HashMap`-backed in-memory [`MemoryStore`] test double.
//!
//! Used by integration tests in place of the `SQLite` adapter so tests run
//! fast, in-process, with no file I/O. Never included as a non-dev
//! dependency — this crate is always a `dev-dependency`.

use std::collections::HashMap;
use std::sync::Mutex;

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities, StoredRecord, StoreError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
pub use sample_helpers::{sample_record, sample_stored_record};

/// Helpers that build canonical test fixtures for `MemoryRecord` and
/// `StoredRecord`. Exposed at crate level so downstream integration tests
/// don't have to duplicate the construction.
mod sample_helpers {
    use std::collections::BTreeMap;

    use cairn_core::contract::memory_store::StoredRecord;
    use cairn_core::domain::{
        ActorChainEntry, ChainRole, Identity, Provenance, Rfc3339Timestamp, ScopeTuple,
    };
    use cairn_core::domain::record::{Ed25519Signature, MemoryRecord, RecordId};
    use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
    use cairn_core::domain::EvidenceVector;

    /// Returns a deterministic [`MemoryRecord`] for use in tests.
    ///
    /// Mirrors the `sample_record` helper in `cairn-core` (which is
    /// `#[cfg(test)]` and therefore inaccessible from outside that crate).
    #[must_use]
    #[allow(clippy::expect_used)] // test fixture construction: panics are bugs, not runtime errors
    pub fn sample_record() -> MemoryRecord {
        let user_id = Identity::parse("usr:tafeng").expect("valid identity");
        MemoryRecord {
            id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid ULID"),
            kind: MemoryKind::User,
            class: MemoryClass::Semantic,
            visibility: MemoryVisibility::Private,
            scope: ScopeTuple {
                user: Some("usr:tafeng".to_owned()),
                ..ScopeTuple::default()
            },
            body: "user prefers dark mode".to_owned(),
            provenance: Provenance {
                source_sensor: Identity::parse("snr:local:hook:cc-session:v1")
                    .expect("valid sensor identity"),
                created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z")
                    .expect("valid timestamp"),
                originating_agent_id: user_id.clone(),
                source_hash: format!("sha256:{}", "a".repeat(64)),
                consent_ref: "consent:01HQZ".to_owned(),
                llm_id_if_any: None,
            },
            updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z")
                .expect("valid timestamp"),
            evidence: EvidenceVector::default(),
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: user_id,
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid timestamp"),
            }],
            signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128)))
                .expect("valid signature"),
            tags: vec!["pref".to_owned()],
            extra_frontmatter: BTreeMap::new(),
        }
    }

    /// Returns a [`StoredRecord`] wrapping [`sample_record`] at the given version.
    #[must_use]
    pub fn sample_stored_record(version: u32) -> StoredRecord {
        StoredRecord { record: sample_record(), version }
    }
}

/// In-memory `MemoryStore` test double backed by a `HashMap`.
///
/// All three async methods acquire a `std::sync::Mutex`, perform their
/// operation, and drop the lock before returning — the lock never spans an
/// `.await` point, which keeps the implementation correct under
/// `current_thread` and `multi_thread` runtimes alike.
#[derive(Debug, Default)]
pub struct FixtureStore {
    inner: Mutex<HashMap<String, StoredRecord>>,
}

impl FixtureStore {
    /// Returns a new empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl MemoryStore for FixtureStore {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        // Accepts exactly the current contract version up to (but not
        // including) the next minor bump, following the same pattern
        // used by the StubStore in the trait definition tests.
        VersionRange::new(
            CONTRACT_VERSION,
            ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
        )
    }

    async fn get(&self, target_id: &str) -> Result<Option<StoredRecord>, StoreError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| StoreError::Internal(e.to_string()))?;
        Ok(guard.get(target_id).cloned())
    }

    async fn upsert(&self, record: MemoryRecord) -> Result<StoredRecord, StoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| StoreError::Internal(e.to_string()))?;
        let id = record.id.as_str().to_owned();
        let version = guard.get(&id).map_or(1, |s| s.version + 1);
        let stored = StoredRecord { record, version };
        guard.insert(id, stored.clone());
        Ok(stored)
    }

    async fn list_active(&self) -> Result<Vec<StoredRecord>, StoreError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| StoreError::Internal(e.to_string()))?;
        Ok(guard.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixture_store_get_upsert_round_trip() {
        let store = FixtureStore::default();
        let record = sample_record();
        let id = record.id.as_str().to_owned();

        // upsert returns stored record with version 1
        let stored = store.upsert(record.clone()).await.unwrap();
        assert_eq!(stored.version, 1);
        assert_eq!(stored.record.id, record.id);

        // get returns the same record
        let fetched = store.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.record.id, record.id);
        assert_eq!(fetched.version, 1);
    }

    #[tokio::test]
    async fn fixture_store_upsert_increments_version() {
        let store = FixtureStore::default();
        let mut record = sample_record();
        let id = record.id.as_str().to_owned();
        store.upsert(record.clone()).await.unwrap();

        // second upsert with same id → version 2
        record.body = "updated body".to_owned();
        let stored = store.upsert(record).await.unwrap();
        assert_eq!(stored.version, 2);

        let fetched = store.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.version, 2);
        assert_eq!(fetched.record.body, "updated body");
    }

    #[tokio::test]
    async fn fixture_store_get_missing_returns_none() {
        let store = FixtureStore::default();
        let result = store.get("nonexistent-id").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fixture_store_list_active_returns_all() {
        let store = FixtureStore::default();
        let r1 = sample_record();
        // sample_record() always produces the same id, so a single insert
        // is the minimal meaningful test for list_active.
        store.upsert(r1.clone()).await.unwrap();
        let active = store.list_active().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].record.id, r1.id);
    }

    #[tokio::test]
    async fn fixture_store_capabilities_and_version() {
        let store = FixtureStore::new();
        assert_eq!(store.name(), "fixture");
        let caps = store.capabilities();
        assert!(!caps.fts);
        assert!(!caps.vector);
        assert!(store
            .supported_contract_versions()
            .accepts(CONTRACT_VERSION));
    }

    #[tokio::test]
    async fn fixture_store_is_dyn_compatible() {
        let store: Box<dyn MemoryStore> = Box::new(FixtureStore::default());
        assert_eq!(store.name(), "fixture");
        let result = store.list_active().await.unwrap();
        assert!(result.is_empty());
    }
}
