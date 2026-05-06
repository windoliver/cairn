//! [`FixtureStore`] — `HashMap`-backed in-memory [`MemoryStore`] test double.
//!
//! Used by integration tests in place of the `SQLite` adapter so tests run
//! fast, in-process, with no file I/O. Never included as a non-dev
//! dependency — this crate is always a `dev-dependency`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion, StoreError, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::{BodyHash, RecordId, TargetId};

pub use cairn_core::domain::record::tests_export::{sample_record, sample_stored_record};

/// In-memory `MemoryStore` test double backed by a `HashMap`.
///
/// All async methods acquire a `std::sync::Mutex`, perform their operation,
/// and drop the lock before returning — the lock never spans an `.await`
/// point, which keeps the implementation correct under `current_thread` and
/// `multi_thread` runtimes alike.
///
/// Implements the full 9-method `MemoryStore` trait surface but only the
/// methods integration tests actually rely on are non-trivial: `upsert`,
/// `get`, `list`, `tombstone`, `versions`, `get_active_by_target`. Edges
/// and `search_keyword` return `Ok(empty)` / `Err("unimplemented")`.
#[derive(Debug, Default)]
pub struct FixtureStore {
    inner: Mutex<HashMap<String, RowEntry>>,
    index_stats_override: Mutex<Option<cairn_core::contract::memory_store::IndexStats>>,
    /// When true, `capabilities()` reports `per_record_consent_model = false`
    /// and `list_consent_models()` returns the "not supported" sentinel.
    /// Lets the §6.5 gate 2×2 matrix tests exercise the capability=false
    /// quadrants without needing a second `MemoryStore` impl.
    legacy_only_override: AtomicBool,
}

#[derive(Debug, Clone)]
struct RowEntry {
    record: MemoryRecord,
    version: u32,
    active: bool,
    tombstoned: bool,
    tombstone_reason: Option<TombstoneReason>,
    body_hash: BodyHash,
    schema_version: cairn_core::contract::version::SchemaVersion,
}

impl FixtureStore {
    /// Returns a new empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Force the next `index_stats` call to return these counts. Used by
    /// tests that seed deliberate `records_active` / `fts5_rows` drift
    /// for the lint `index_drift` check (#96 §6.7).
    #[allow(
        clippy::expect_used,
        reason = "Mutex poisoning in tests means a prior test panicked; surfacing it via expect is fine"
    )]
    pub fn set_index_stats_override(&self, stats: cairn_core::contract::memory_store::IndexStats) {
        *self
            .index_stats_override
            .lock()
            .expect("fixture store mutex poisoned") = Some(stats);
    }

    /// Flip the adapter into "capability=false" mode for §6.5 gate tests.
    /// Subsequent `capabilities()` calls report
    /// `per_record_consent_model = false` and `list_consent_models()`
    /// returns the not-supported sentinel.
    pub fn set_legacy_only_override(&self, on: bool) {
        self.legacy_only_override.store(on, Ordering::SeqCst);
    }
}

#[allow(
    clippy::expect_used,
    reason = "Mutex poisoning in tests means a prior test panicked; surfacing it via expect is fine"
)]
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
            per_record_consent_model: true,

            graph_search: false,
        };
        static CAPS_LEGACY_ONLY: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
            per_record_consent_model: false,

            graph_search: false,
        };
        if self.legacy_only_override.load(Ordering::SeqCst) {
            &CAPS_LEGACY_ONLY
        } else {
            &CAPS
        }
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            CONTRACT_VERSION,
            ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
        )
    }

    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        record
            .validate()
            .map_err(|e| -> StoreError { e.to_string().into() })?;
        let body_hash = BodyHash::compute(&record.body);

        let mut guard = self.inner.lock().expect("fixture store mutex poisoned");

        // Find the active row for this target.
        let prior = guard
            .iter()
            .find(|(_, e)| e.record.target_id == record.target_id && e.active)
            .map(|(k, e)| (k.clone(), e.clone()));

        if let Some((_, ref entry)) = prior
            && entry.body_hash == body_hash
        {
            return Ok(UpsertOutcome {
                record_id: entry.record.id.clone(),
                target_id: entry.record.target_id.clone(),
                version: entry.version,
                content_changed: false,
                prior_hash: Some(entry.body_hash.clone()),
            });
        }

        let (version, prior_hash) = match prior.as_ref() {
            Some((prior_id, entry)) => {
                let next = entry.version + 1;
                let mut deactivated = entry.clone();
                deactivated.active = false;
                guard.insert(prior_id.clone(), deactivated);
                (next, Some(entry.body_hash.clone()))
            }
            None => (1, None),
        };

        let mut row_record = record.clone();
        if prior.is_some() {
            // Mint a fresh record_id so the HashMap key stays unique per
            // version (mirrors the SQLite store's `record_id` synthesis).
            let new_id = format!(
                "01HQZX9F5N0{}",
                ulid::Ulid::new()
                    .to_string()
                    .chars()
                    .take(15)
                    .collect::<String>()
            );
            row_record.id =
                RecordId::parse(new_id).map_err(|e| -> StoreError { e.to_string().into() })?;
        }

        let entry = RowEntry {
            record: row_record.clone(),
            version,
            active: true,
            tombstoned: false,
            tombstone_reason: None,
            body_hash: body_hash.clone(),
            schema_version: cairn_core::contract::version::SchemaVersion::current(),
        };
        let outcome_id = entry.record.id.clone();
        let target_id = entry.record.target_id.clone();
        guard.insert(outcome_id.as_str().to_owned(), entry);

        Ok(UpsertOutcome {
            record_id: outcome_id,
            target_id,
            version,
            content_changed: true,
            prior_hash,
        })
    }

    async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        let guard = self.inner.lock().expect("fixture store mutex poisoned");
        Ok(guard
            .get(id.as_str())
            .filter(|e| !e.tombstoned)
            .map(|e| e.record.clone()))
    }

    async fn list(&self, args: &ListArgs) -> Result<ListPage, StoreError> {
        let guard = self.inner.lock().expect("fixture store mutex poisoned");
        let mut records: Vec<_> = guard
            .values()
            .filter(|e| e.active && !e.tombstoned)
            .filter(|e| args.kind.is_none_or(|k| e.record.kind == k))
            .filter(|e| args.class.is_none_or(|c| e.record.class == c))
            .filter(|e| {
                args.visibility_allowlist.is_empty()
                    || args.visibility_allowlist.contains(&e.record.visibility)
            })
            .map(|e| e.record.clone())
            .collect();
        // Stable order so tests are deterministic; mirror SQLite's
        // `ORDER BY updated_at DESC, record_id DESC`.
        records.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
        // `limit == 0` is the sentinel for "use the adapter's own page
        // size" — for the in-memory fixture, that means "everything".
        if args.limit > 0 {
            records.truncate(args.limit);
        }
        Ok(ListPage {
            records,
            next_cursor: None,
        })
    }

    async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError> {
        let mut guard = self.inner.lock().expect("fixture store mutex poisoned");
        if let Some(entry) = guard.get_mut(id.as_str()) {
            entry.tombstoned = true;
            entry.tombstone_reason = Some(reason);
        }
        Ok(())
    }

    async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        let guard = self.inner.lock().expect("fixture store mutex poisoned");
        let mut out: Vec<_> = guard
            .values()
            .filter(|e| e.record.target_id == *target)
            .map(|e| RecordVersion {
                record_id: e.record.id.clone(),
                target_id: e.record.target_id.clone(),
                version: e.version,
                created_at: 0,
                updated_at: 0,
                active: e.active,
                tombstoned: e.tombstoned,
                tombstone_reason: e.tombstone_reason,
                body_hash: e.body_hash.clone(),
                schema_version: Some(e.schema_version),
            })
            .collect();
        out.sort_by_key(|v| v.version);
        Ok(out)
    }

    async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
        Ok(())
    }

    async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
        Ok(false)
    }

    async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        Ok(vec![])
    }

    async fn index_stats(
        &self,
    ) -> Result<cairn_core::contract::memory_store::IndexStats, StoreError> {
        use cairn_core::contract::memory_store::IndexStats;
        if let Some(s) = *self
            .index_stats_override
            .lock()
            .expect("fixture store mutex poisoned")
        {
            return Ok(s);
        }
        let guard = self.inner.lock().expect("fixture store mutex poisoned");
        let active = guard.values().filter(|e| e.active && !e.tombstoned).count();
        let active = u64::try_from(active).map_err(|e| -> StoreError { e.to_string().into() })?;
        Ok(IndexStats::new(active, active))
    }

    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        Err("FixtureStore: search_keyword is not implemented (capability fts=false)".into())
    }

    /// Test fixtures certify per-row consent state authoritatively:
    /// every active record is `LegacyEvent`. Returning a populated
    /// map (rather than the `Ok(empty)` escape hatch) matches the
    /// fail-closed posture lint requires for non-empty vaults under
    /// `per_record_consent_model = true`.
    async fn list_consent_models(
        &self,
    ) -> Result<
        std::collections::HashMap<RecordId, cairn_core::domain::consent_timeline::ConsentModel>,
        StoreError,
    > {
        if self.legacy_only_override.load(Ordering::SeqCst) {
            return Err("list_consent_models: not supported by this store adapter".into());
        }
        let guard = self.inner.lock().expect("fixture store mutex poisoned");
        Ok(guard
            .values()
            .filter(|e| e.active)
            .map(|e| {
                (
                    e.record.id.clone(),
                    cairn_core::domain::consent_timeline::ConsentModel::LegacyEvent,
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixture_store_get_upsert_round_trip() {
        let store = FixtureStore::default();
        let record = sample_record();
        let id = record.id.clone();

        let outcome = store.upsert(&record).await.unwrap();
        assert_eq!(outcome.version, 1);
        assert_eq!(outcome.record_id, id);
        assert!(outcome.content_changed);

        let fetched = store.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.id, id);
    }

    #[tokio::test]
    async fn fixture_store_upsert_increments_version_on_body_change() {
        let store = FixtureStore::default();
        let mut record = sample_record();
        store.upsert(&record).await.unwrap();

        record.body = "updated body".to_owned();
        let outcome = store.upsert(&record).await.unwrap();
        assert_eq!(outcome.version, 2);
        assert!(outcome.content_changed);

        let active = store
            .get_active_by_target(&record.target_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.version, 2);
        assert_eq!(active.record.body, "updated body");
    }

    #[tokio::test]
    async fn fixture_store_idempotent_on_same_body() {
        let store = FixtureStore::default();
        let record = sample_record();
        store.upsert(&record).await.unwrap();
        let outcome = store.upsert(&record).await.unwrap();
        assert_eq!(outcome.version, 1);
        assert!(!outcome.content_changed);
    }

    #[tokio::test]
    async fn fixture_store_get_missing_returns_none() {
        let store = FixtureStore::default();
        let id = RecordId::parse("01HQZX9F5N0000000000000099".to_owned()).unwrap();
        let result = store.get(&id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fixture_store_capabilities_and_version() {
        let store = FixtureStore::new();
        assert_eq!(store.name(), "fixture");
        let caps = store.capabilities();
        assert!(!caps.fts);
        assert!(!caps.vector);
        assert!(
            store
                .supported_contract_versions()
                .accepts(CONTRACT_VERSION)
        );
    }

    #[tokio::test]
    async fn fixture_store_is_dyn_compatible() {
        let store: Box<dyn MemoryStore> = Box::new(FixtureStore::default());
        assert_eq!(store.name(), "fixture");
        let result = store.list(&ListArgs::default()).await.unwrap();
        assert!(result.records.is_empty());
    }

    #[tokio::test]
    async fn fixture_index_stats_matches_active_record_count_by_default() {
        use cairn_core::contract::memory_store::MemoryStore;
        let store = FixtureStore::default();
        let r = sample_record();
        store.upsert(&r).await.expect("upsert");
        let stats = store.index_stats().await.expect("index_stats");
        assert_eq!(stats.records_active, 1);
        assert_eq!(stats.fts5_rows, 1);
    }

    #[tokio::test]
    async fn fixture_index_stats_can_be_overridden_for_drift_tests() {
        use cairn_core::contract::memory_store::{IndexStats, MemoryStore};
        let store = FixtureStore::default();
        let r = sample_record();
        store.upsert(&r).await.expect("upsert");
        store.set_index_stats_override(IndexStats::new(5, 4));
        let stats = store.index_stats().await.expect("index_stats");
        assert_eq!(stats.records_active, 5);
        assert_eq!(stats.fts5_rows, 4);
    }
}
