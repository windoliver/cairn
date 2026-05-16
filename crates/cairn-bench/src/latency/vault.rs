//! Vault-seeding helper for latency benches.
//!
//! Builds a tempdir-rooted `SQLite` vault with the `MockEmbedder` wired so
//! FTS5 keyword and vector (semantic/hybrid) indexes are both populated.
//! The constructor is synchronous so criterion's setup phase can call it
//! without an outer `#[tokio::main]`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tempfile::TempDir;

use cairn_embeddings_local::{EmbeddingModelKind, MockEmbedder};

/// A seeded test vault ready for SDK verb calls.
///
/// Keep this value alive for the duration of the bench — dropping it
/// removes the underlying tempdir and closes the store.
pub struct SeededVault {
    /// Tempdir handle; held so the directory is not deleted until `SeededVault` drops.
    pub dir: TempDir,
    /// Vault root inside the tempdir.
    pub root: PathBuf,
    /// Opened `SQLite` store with FTS5 + vector indexes populated.
    pub store: Arc<cairn_store_sqlite::SqliteMemoryStore>,
}

impl SeededVault {
    /// Build a vault seeded with 20 records, drain the embedding queue so
    /// semantic and hybrid searches have vectors, and return the harness.
    ///
    /// Uses a current-thread tokio runtime so criterion's synchronous iter
    /// setup can call this without an outer `#[tokio::main]`.
    ///
    /// # Errors
    ///
    /// Returns an error if the tokio runtime, tempdir, store, or seeding
    /// fails.
    #[allow(clippy::expect_used)]
    pub fn new() -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        rt.block_on(async {
            use cairn_core::contract::memory_store::MemoryStore as _;
            use cairn_core::domain::{RecordId, TargetId};

            let dir = TempDir::new()?;
            let root = dir.path().to_path_buf();
            let cairn_dir = root.join(".cairn");
            std::fs::create_dir_all(&cairn_dir)?;
            // Write vault.id sentinel so vault-binding gates accept this fixture.
            std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")?;
            let db_path = cairn_dir.join("cairn.db");

            let kind = EmbeddingModelKind::default();
            let embedder: Arc<dyn cairn_embeddings_local::EmbeddingModel> =
                Arc::new(MockEmbedder::new(kind));
            let store = cairn_store_sqlite::open_with_embedder(
                &db_path,
                Some(Arc::clone(&embedder)),
            )
            .await?;

            for i in 0u32..20 {
                let mut r =
                    cairn_core::domain::record::tests_export::sample_record();
                // Deterministic ULID: 01HQZX9F5N00000000000000{i:02X}
                let suffix = format!("{i:02X}");
                let id_str = format!("01HQZX9F5N00000000000000{suffix}");
                r.id = RecordId::parse(id_str.clone())
                    .map_err(|e| anyhow::anyhow!("bad seed id: {e}"))?;
                r.target_id = TargetId::parse(id_str)
                    .map_err(|e| anyhow::anyhow!("bad seed target: {e}"))?;
                r.body = format!(
                    "seeded bench record {i} — Rust memory safety, \
                     async runtimes, SQLite FTS5, vector embeddings, \
                     agent workflows, hot memory assembly, latency gates"
                );
                store.upsert(&r).await.map_err(|e| anyhow::anyhow!("upsert: {e}"))?;
            }

            // Drain embedding queue so semantic / hybrid have vectors.
            if let Some(conn) = store.raw_conn_for_admin() {
                let conn = Arc::clone(conn);
                for _ in 0..1000 {
                    let stats =
                        cairn_store_sqlite::drain_once(Arc::clone(&conn), Arc::clone(&embedder))
                            .await?;
                    if stats.remaining == 0 {
                        break;
                    }
                }
            }

            Ok(SeededVault {
                dir,
                root,
                store: Arc::new(store),
            })
        })
    }

    /// Return the record ID of the first seeded record (index 0).
    ///
    /// The ID is generated deterministically:
    ///   `format!("01HQZX9F5N00000000000000{i:02X}")` for `i = 0`
    /// → `"01HQZX9F5N0000000000000000"` (26 chars).
    #[must_use]
    pub fn first_record_id() -> &'static str {
        "01HQZX9F5N0000000000000000"
    }
}
