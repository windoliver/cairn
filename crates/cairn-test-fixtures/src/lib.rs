//! Shared test helpers for Cairn crates.
//!
//! Only ever pulled in as a `dev-dependency`. `cairn-core` does not depend on
//! this crate — core tests stay pure so the boundary check remains trivially
//! sound.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod fake_consent_lookup;
pub mod flush_plan;
pub mod graph;
pub mod hybrid_vault;
pub mod intent;
pub mod keystore;
pub mod mcp;
pub mod source_links;
pub mod store;
pub use fake_consent_lookup::FakeConsentLookup;
pub use hybrid_vault::{HybridTestVault, RecordSpec, build_hybrid_test_vault};
pub use keystore::MemoryKeystore;
pub use store::FixtureStore;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Absolute path to the workspace-level `fixtures/` directory.
///
/// Resolves at runtime from `CARGO_MANIFEST_DIR` (this crate's dir) and walks
/// up to the workspace root. Cached after first call.
#[must_use]
// `expect` is appropriate here: a broken project layout (crate not two levels
// below the workspace root) is a programmer error that should panic loudly.
#[allow(clippy::expect_used)]
pub fn fixtures_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // CARGO_MANIFEST_DIR is this crate: <workspace>/crates/cairn-test-fixtures
        // Walk up two levels to the workspace root, then into `fixtures/`.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .ancestors()
            .nth(2)
            .expect("crates/cairn-test-fixtures must be two levels below the workspace root");
        workspace_root.join("fixtures")
    })
    .as_path()
}

/// Absolute path to the versioned P0 fixture directory (`fixtures/v0/`).
#[must_use]
pub fn fixture_v0_dir() -> std::path::PathBuf {
    fixtures_dir().join("v0")
}

use cairn_core::domain::{MemoryRecord, RecordId, SourceId, TargetId};
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::TempDir;

/// Deterministic [`MemoryRecord`] keyed off `seed`. Body, id, and target are
/// derived from the seed so distinct seeds always produce distinct rows.
///
/// # Panics
/// Panics if the seed-derived ULID strings fail to parse — should never
/// happen because the format is fixed and uses only Crockford-valid hex.
#[must_use]
#[allow(clippy::expect_used)]
pub fn sample_record(seed: u64) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    // ULID layout in this fixture: 11-char prefix `01HQZX9F5N0` + a
    // 15-char seed-derived suffix = 26 chars. 15 hex digits hold 60
    // bits, so distinct seeds in the low-60-bit space always produce
    // distinct ids; seeds whose top 4 bits differ but low 60 match
    // collapse onto the same id. Document and accept — every test
    // suite using this helper passes seeds in the small-integer range.
    //
    // Earlier form (`{seed:020X}`, take `[..15]`) was wrong: 20-pad
    // is left-padded with zeros, so the first 15 chars were always
    // `"000000000000000"` for any seed < 16^15, collapsing every
    // small seed onto the same id and silently breaking
    // FixtureStore's target-id-keyed dedupe.
    let masked = seed & ((1u64 << 60) - 1);
    let suffix = format!("{masked:015X}");
    debug_assert_eq!(suffix.len(), 15);
    let id_str = format!("01HQZX9F5N0{suffix}");
    r.id = RecordId::parse(id_str.clone()).expect("seed-derived id");
    r.target_id = TargetId::parse(id_str).expect("seed-derived target");
    r.source_ids = vec![SourceId::parse("01HQZX9F5N0000000000000001").expect("valid")];
    r.body = format!("seeded body {seed}");
    r
}

/// File-backed store in a fresh temp dir. Caller keeps `TempDir` alive
/// for the duration of the test.
///
/// # Panics
/// Panics if the temp dir or store cannot be created.
#[allow(clippy::expect_used)]
pub async fn tempstore() -> (TempDir, SqliteMemoryStore) {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("cairn.db");
    let store = cairn_store_sqlite::open(path).await.expect("open");
    (dir, store)
}

/// In-memory store. For fast tests that don't need a path on disk.
///
/// # Panics
/// Panics if the in-memory store cannot be opened.
#[allow(clippy::expect_used)]
pub async fn memstore() -> SqliteMemoryStore {
    cairn_store_sqlite::open_in_memory()
        .await
        .expect("memstore")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_seeds_yield_distinct_ids() {
        // Regression: previous implementation truncated the suffix
        // to the first 15 chars of a 20-pad zero-prefixed hex form,
        // collapsing every seed in `0..16^15` onto the same id.
        // FixtureStore dedupes by `target_id`, so collisions caused
        // multi-record fixtures to silently lose all but one row.
        let ids: Vec<_> = (0..16u64).map(|s| sample_record(s).id).collect();
        let mut sorted = ids.clone();
        sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "distinct seeds must yield distinct ids"
        );
    }
}
