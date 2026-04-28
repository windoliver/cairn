//! Shared test helpers for Cairn crates.
//!
//! Only ever pulled in as a `dev-dependency`. `cairn-core` does not depend on
//! this crate — core tests stay pure so the boundary check remains trivially
//! sound.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

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

use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
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
    let suffix = format!("{seed:020X}");
    let id_str = format!("01HQZX9F5N0{}", &suffix[..15]);
    r.id = RecordId::parse(id_str.clone()).expect("seed-derived id");
    r.target_id = TargetId::parse(id_str).expect("seed-derived target");
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
