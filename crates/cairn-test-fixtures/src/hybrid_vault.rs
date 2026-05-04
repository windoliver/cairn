//! `build_hybrid_test_vault` — produce a tempdir-rooted vault with the
//! `MockEmbedder` wired, an open `SQLite` store, and N seeded records ready
//! for `cairn search` integration tests.
//!
//! Used by Task 15 CLI snapshot tests.

use std::path::PathBuf;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::{RecordId, TargetId};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::TempDir;

/// Spec for one seeded record.
pub struct RecordSpec {
    /// Body indexed by FTS5 + embedded.
    pub body: String,
}

impl RecordSpec {
    /// Construct from a body string.
    #[must_use]
    pub fn from_body(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }
}

/// Returned harness; drop the `dir` to clean up.
pub struct HybridTestVault {
    /// Tempdir (drop = cleanup).
    pub dir: TempDir,
    /// Vault root inside the tempdir.
    pub root: PathBuf,
    /// Path to `.cairn/cairn.db`.
    pub db_path: PathBuf,
    /// The opened store, with embedder wired.
    pub store: SqliteMemoryStore,
    /// The embedder used. Useful for callers that want to drive `drain_once`.
    pub embedder: Arc<dyn EmbeddingModel>,
}

/// Build a vault, seed `records`, drain the embedding queue, return the
/// harness.
///
/// # Panics
///
/// Panics on any setup error — these helpers are for tests.
#[allow(clippy::expect_used)]
pub async fn build_hybrid_test_vault(records: &[RecordSpec]) -> HybridTestVault {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().to_path_buf();
    let cairn = root.join(".cairn");
    std::fs::create_dir_all(&cairn).expect("mkdir .cairn");
    // Write the vault.id sentinel so `cairn search`'s vault-binding gate
    // accepts this fixture as a real vault. Without it the CLI exits 78
    // (EX_CONFIG) before opening the store. The id is a deterministic
    // ULID — fixtures don't need a per-test value.
    std::fs::write(cairn.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id sentinel");
    let db_path = cairn.join("cairn.db");

    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = cairn_store_sqlite::open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
        .await
        .expect("open store with embedder");

    for (i, spec) in records.iter().enumerate() {
        let mut r = cairn_core::domain::record::tests_export::sample_record();
        // Distinct ULIDs per record so they don't overwrite each other.
        let suffix = format!("{i:02X}");
        let id_str = format!("01HQZX9F5N00000000000000{suffix}");
        r.id = RecordId::parse(id_str.clone()).expect("seed id");
        r.target_id = TargetId::parse(id_str).expect("seed target");
        spec.body.clone_into(&mut r.body);
        store.upsert(&r).await.expect("upsert");
    }

    // Drain the queue so semantic / hybrid have vectors when the test runs.
    if let Some(conn) = store.raw_conn_for_admin() {
        let conn = Arc::clone(conn);
        for _ in 0..1000 {
            let stats = cairn_store_sqlite::drain_once(Arc::clone(&conn), Arc::clone(&embedder))
                .await
                .expect("drain");
            if stats.remaining == 0 {
                break;
            }
        }
    }

    HybridTestVault {
        dir,
        root,
        db_path,
        store,
        embedder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn build_vault_with_three_records() {
        let vault = build_hybrid_test_vault(&[
            RecordSpec::from_body("the quick brown fox"),
            RecordSpec::from_body("jumps over the lazy dog"),
            RecordSpec::from_body("rust is memory safe"),
        ])
        .await;
        assert!(vault.db_path.exists());
        // Capabilities sanity.
        assert!(vault.store.capabilities().vector, "vector cap should be on");
    }
}
