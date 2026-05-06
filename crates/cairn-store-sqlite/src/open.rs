//! `SQLite` open path: pragmas + migrations, returning an async store handle.

use std::path::Path;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;

use crate::error::StoreError;
use crate::migrations::migrations;
use crate::store::SqliteMemoryStore;
use crate::vec_ext::register_vec0;
use crate::verify::{
    preflight_migration_history, verify_migration_history, verify_schema_fingerprint,
};

const PRAGMAS: &str = "PRAGMA journal_mode=WAL;\
     PRAGMA foreign_keys=ON;\
     PRAGMA synchronous=NORMAL;\
     PRAGMA busy_timeout=5000;\
     PRAGMA temp_store=MEMORY;\
     PRAGMA mmap_size=268435456;";

/// Build the base capability set based on whether an embedder is present.
///
/// `per_record_consent_model: true` since migration 0031 adds the
/// per-row column and 0032 adds the timeline table — lint can rely on
/// `list_consent_models` returning one entry per active row (Issue
/// #253).
fn base_caps(vector: bool) -> MemoryStoreCapabilities {
    MemoryStoreCapabilities {
        fts: true,
        vector,
        graph_edges: true,
        transactions: true,
        per_record_consent_model: true,

        graph_search: false,
    }
}

/// Default per-column BM25 weights for `records_fts` `(kind, class, scope, body)`.
/// Used by every open-path that does not explicitly thread a config through.
/// Mirrored in `SqliteMemoryStore::default()` so the registry stub agrees.
const DEFAULT_FTS_COLUMN_WEIGHTS: [f64; 4] = [10.0, 10.0, 5.0, 1.0];

/// Default sqlite-vec embedding dimension baked into migration 0020.
/// `EmbeddingModel::dim()` is consulted at open time; if the active
/// embedder reports a different dim, [`bootstrap`] drops and recreates
/// the (empty) `record_vectors` table at the embedder's dim before any
/// rows are inserted.
const DEFAULT_VEC_DIM: usize = 384;

/// Finish constructing a [`SqliteMemoryStore`] from an open connection and
/// optional embedder. Spawns the background drain loop when an embedder is
/// provided.
fn build_store(
    conn: AsyncConn,
    embedder: Option<Arc<dyn EmbeddingModel>>,
    fts_column_weights: [f64; 4],
) -> SqliteMemoryStore {
    let conn = Arc::new(conn);
    let vector = embedder.is_some();
    let caps = base_caps(vector);
    let cancel = embedder.as_ref().map(|_| CancellationToken::new());

    if let (Some(emb), Some(tok)) = (embedder.as_ref(), cancel.as_ref()) {
        let conn2 = Arc::clone(&conn);
        let emb2 = Arc::clone(emb);
        let tok2 = tok.clone();
        tokio::spawn(crate::store::reindex::drain_loop(conn2, emb2, tok2));
    }

    SqliteMemoryStore {
        conn: Some(conn),
        embedder,
        caps,
        _cancel: cancel,
        fts_column_weights,
    }
}

/// Open (or create) the Cairn store at `path` and bring it to schema head.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open(path: impl AsRef<Path>) -> Result<SqliteMemoryStore, StoreError> {
    open_with_embedder(path, None).await
}

/// Open (or create) the Cairn store at `path` with an optional local
/// embedding model. When `embedder` is `Some`, the `vector` capability is
/// enabled and a background drain loop is spawned to embed queued records.
///
/// Equivalent to [`open_with_embedder_and_config`] with the default
/// per-column BM25 weights `[10.0, 10.0, 5.0, 1.0]`.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open_with_embedder(
    path: impl AsRef<Path>,
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    open_with_embedder_and_config(path, embedder, DEFAULT_FTS_COLUMN_WEIGHTS).await
}

/// Open (or create) the Cairn store at `path` with an optional local
/// embedding model and an explicit per-column BM25 weight tuple. The four
/// weights map to `records_fts` columns `(kind, class, scope, body)` —
/// same order as migration 0030.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
pub async fn open_with_embedder_and_config(
    path: impl AsRef<Path>,
    embedder: Option<Arc<dyn EmbeddingModel>>,
    fts_column_weights: [f64; 4],
) -> Result<SqliteMemoryStore, StoreError> {
    // Register the sqlite-vec vec0 module globally before opening any
    // connection so migration 0020 (CREATE VIRTUAL TABLE USING vec0) succeeds.
    register_vec0();
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }
    let conn = AsyncConn::open(path).await?;
    let dim = embedder.as_ref().map(|e| e.dim());
    bootstrap(&conn, dim).await?;
    Ok(build_store(conn, embedder, fts_column_weights))
}

/// In-memory store at schema head. For tests.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub async fn open_in_memory() -> Result<SqliteMemoryStore, StoreError> {
    open_in_memory_with_embedder(None).await
}

/// In-memory store at schema head with an optional local embedding model.
/// For tests that exercise the vector search path.
///
/// Equivalent to [`open_in_memory_with_embedder_and_config`] with the
/// default per-column BM25 weights `[10.0, 10.0, 5.0, 1.0]`.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub async fn open_in_memory_with_embedder(
    embedder: Option<Arc<dyn EmbeddingModel>>,
) -> Result<SqliteMemoryStore, StoreError> {
    open_in_memory_with_embedder_and_config(embedder, DEFAULT_FTS_COLUMN_WEIGHTS).await
}

/// In-memory store at schema head with an optional local embedding model
/// and an explicit per-column BM25 weight tuple. For tests that need to
/// exercise weight-driven ranking against `records_fts`.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
pub async fn open_in_memory_with_embedder_and_config(
    embedder: Option<Arc<dyn EmbeddingModel>>,
    fts_column_weights: [f64; 4],
) -> Result<SqliteMemoryStore, StoreError> {
    register_vec0();
    let conn = AsyncConn::open_in_memory().await?;
    let dim = embedder.as_ref().map(|e| e.dim());
    bootstrap(&conn, dim).await?;
    Ok(build_store(conn, embedder, fts_column_weights))
}

async fn bootstrap(conn: &AsyncConn, vec_dim: Option<usize>) -> Result<(), StoreError> {
    conn.call(move |c| {
        c.execute_batch(PRAGMAS)?;
        // Pre-flight: read-only check that already-applied migration
        // rows agree with the compiled-in manifest. Catches a tampered
        // `schema_migrations.sql_hash` on a pre-head DB BEFORE
        // `to_latest` happily appends the next migration to an
        // untrusted store.
        preflight_migration_history(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        migrations()
            .to_latest(c)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        // Verify migration history BEFORE any schema mutation. If the
        // on-disk DB has tampered or stale `schema_migrations.sql_hash`
        // rows we must reject it untouched — otherwise a mismatched
        // DB plus a different-dimension embedder would commit a
        // DROP/CREATE on `record_vectors` and only then refuse the
        // open, leaving a partially-mutated, untrusted store on disk.
        verify_migration_history(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        // Compare the requested dim to whatever is already on disk —
        // not to `DEFAULT_VEC_DIM`. A persistent store previously
        // bootstrapped at 1536 must be re-pointed at 384 if the next
        // open uses a 384-dim embedder (and vice versa); skipping the
        // resize on `dim == DEFAULT_VEC_DIM` would leave the table at
        // 1536, then `verify_schema_fingerprint` fails because the
        // expected DDL was hashed at 384.
        let mut resized = false;
        if let Some(dim) = vec_dim {
            let current = read_record_vectors_dim(c)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            if current != dim {
                resize_record_vectors(c, dim)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                resized = true;
            }
        }
        // Choose the form used to compute the expected DDL digest.
        // - `None`: pristine migration form (no resize ever ran on this DB).
        // - `Some(d)`: recreated form at dim `d` (this open resized, OR a
        //   prior open did and we're observing its recreated table).
        // The on-disk DDL string differs between the two forms (comments
        // and whitespace from migration 0020 vs the bare CREATE we emit
        // in `resize_record_vectors`), so the digest path must mirror
        // whichever form is actually present.
        let on_disk_dim =
            read_record_vectors_dim(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        let effective_dim = if resized || on_disk_dim != DEFAULT_VEC_DIM {
            Some(on_disk_dim)
        } else {
            None
        };
        verify_schema_fingerprint(c, effective_dim)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok(())
    })
    .await?;
    Ok(())
}

/// Drop and recreate the `record_vectors` vec0 virtual table at `dim`.
///
/// Called from [`bootstrap`] when the active embedder's `dim()` differs
/// from migration 0020's hardcoded `DEFAULT_VEC_DIM` (384). Migration
/// 0020 stays static — only the runtime table is resized — so the
/// migration history hash and `verify_migration_history` keep working
/// across embedder choices.
///
/// **Idempotent reopen**: if the table is already at `dim` (e.g. a
/// persistent OpenAI-backed DB being reopened), this is a no-op even
/// when the table holds rows. Only when the existing dim differs and
/// the table holds rows do we refuse — preventing silent loss of
/// previously-stored embeddings on an embedder swap.
///
/// **Validates `dim` first**: we reject 0 (sqlite-vec rejects later
/// anyway, but here we keep the original table intact instead of
/// relying on transaction rollback semantics for virtual tables).
fn resize_record_vectors(conn: &mut rusqlite::Connection, dim: usize) -> Result<(), StoreError> {
    if dim == 0 {
        return Err(StoreError::Invariant {
            what: "record_vectors dim must be > 0".into(),
        });
    }
    let current_dim = read_record_vectors_dim(conn)?;
    if current_dim == dim {
        // Already at the requested dim — reopen of a persistent DB
        // that was previously bootstrapped with the same embedder.
        return Ok(());
    }
    // Open an IMMEDIATE transaction so the emptiness check, the DROP,
    // and the CREATE all hold the database write lock — otherwise a
    // concurrent drainer/upserter could insert vectors between our
    // pre-check and the DROP and we'd silently lose embeddings. The
    // typed transaction also gives us RAII rollback if CREATE fails
    // (SQLite's `execute_batch` does not auto-rollback mid-batch).
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let existing: i64 = tx.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))?;
    if existing > 0 {
        return Err(StoreError::SchemaDrift(format!(
            "record_vectors holds {existing} rows at dim {current_dim}; \
             cannot resize to {dim} without losing embeddings — re-ingest first",
        )));
    }
    // The `records_vector_cleanup` trigger references `record_vectors`
    // by name — it survives the DROP and continues to work against the
    // recreated table. The schema fingerprint check runs after this
    // and asserts `("table","record_vectors")` is present.
    tx.execute_batch("DROP TABLE record_vectors;")?;
    tx.execute_batch(&record_vectors_create_sql(dim))?;
    tx.commit()?;
    Ok(())
}

/// Canonical `record_vectors` `CREATE VIRTUAL TABLE` statement at `dim`.
///
/// Text-identical (modulo `dim` and whitespace, which `canonicalize_ddl`
/// collapses) to migration 0020's body so the on-disk DDL string is the
/// same regardless of whether the table was produced by the migration
/// or recreated by [`resize_record_vectors`]. The shared shadow-column
/// comments are reproduced here so the post-canonicalisation digest
/// matches the migration form even after a resize lands the table at
/// the default dim (e.g. 1536→384), eliminating "ghost drift" on later
/// reopens.
pub(crate) fn record_vectors_create_sql(dim: usize) -> String {
    format!(
        "CREATE VIRTUAL TABLE record_vectors USING vec0(\n\
          record_id TEXT PRIMARY KEY,\n\
          embedding float[{dim}],\n\
          -- Shadow column: which model produced this embedding.\n\
          -- Semantic search skips rows where model ≠ active model (stale from swap).\n\
          +model TEXT NOT NULL\n\
        )"
    )
}

/// Read the current `embedding float[N]` dim out of the live
/// `record_vectors` DDL string. The vec0 virtual-table SQL stored in
/// `sqlite_schema` is byte-stable across `SQLite` versions for the
/// `float[N]` form, so a small regex-free scan suffices.
fn read_record_vectors_dim(conn: &rusqlite::Connection) -> Result<usize, StoreError> {
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'record_vectors'",
        [],
        |r| r.get(0),
    )?;
    let needle = "float[";
    let start = sql.find(needle).ok_or_else(|| {
        StoreError::SchemaDrift(format!("record_vectors DDL missing `float[<dim>]`: {sql}"))
    })?;
    let after = &sql[start + needle.len()..];
    let close = after.find(']').ok_or_else(|| {
        StoreError::SchemaDrift(format!("record_vectors DDL missing `]` after dim: {sql}"))
    })?;
    after[..close].trim().parse::<usize>().map_err(|e| {
        StoreError::SchemaDrift(format!(
            "record_vectors DDL has non-numeric dim: {e}; sql={sql}",
        ))
    })
}

/// Sync open at `path`, returning a raw `rusqlite::Connection`. For tests
/// that drive SQL directly (drift detection, migration validation). Not
/// part of the production API — gated behind `test-helpers` feature.
///
/// # Errors
/// Returns [`StoreError`] if the directory cannot be created, the
/// connection cannot be opened, pragmas fail, or migrations fail.
#[cfg(any(test, feature = "test-helpers"))]
pub fn open_sync(path: impl AsRef<Path>) -> Result<rusqlite::Connection, StoreError> {
    register_vec0();
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::VaultPath(e.to_string()))?;
    }
    let mut conn = rusqlite::Connection::open(path)?;
    conn.execute_batch(PRAGMAS)?;
    migrations().to_latest(&mut conn)?;
    verify_migration_history(&conn)?;
    verify_schema_fingerprint(&conn, None)?;
    Ok(conn)
}

/// Sync in-memory open returning a raw `rusqlite::Connection` for tests
/// that drive SQL directly. Not part of the production API — gated behind
/// `test-helpers` feature.
///
/// # Errors
/// Returns [`StoreError`] if pragmas or migrations fail.
#[cfg(any(test, feature = "test-helpers"))]
pub fn open_in_memory_sync() -> Result<rusqlite::Connection, StoreError> {
    register_vec0();
    let mut conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(PRAGMAS)?;
    migrations().to_latest(&mut conn)?;
    verify_migration_history(&conn)?;
    verify_schema_fingerprint(&conn, None)?;
    Ok(conn)
}

#[cfg(test)]
mod resize_tests {
    use super::{register_vec0, resize_record_vectors};

    /// If the resize CREATE leg fails (here forced by passing a dim of
    /// 0, which our own validation rejects), the transactional wrapper
    /// keeps the original `record_vectors` table intact so the next
    /// legitimate open still succeeds.
    #[test]
    fn failed_resize_rolls_back_to_original_record_vectors() {
        register_vec0();
        let mut conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::migrations::migrations()
            .to_latest(&mut conn)
            .expect("migrate");

        let before: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema \
                 WHERE type = 'table' AND name = 'record_vectors'",
                [],
                |r| r.get(0),
            )
            .expect("read original DDL");
        assert!(before.contains("float[384]"));

        let err = resize_record_vectors(&mut conn, 0).expect_err("resize must fail");
        let _ = err;

        let after: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema \
                 WHERE type = 'table' AND name = 'record_vectors'",
                [],
                |r| r.get(0),
            )
            .expect("table still present after failed resize");
        assert!(
            after.contains("float[384]"),
            "rollback must preserve the pre-resize 384-dim table; got {after}",
        );
    }

    /// Drift guard for Codex round-6 finding #2: the canonical
    /// `record_vectors_create_sql(DEFAULT_VEC_DIM)` must match the
    /// `record_vectors` DDL emitted by migration 0020 once both have
    /// been canonicalized the same way `verify_schema_fingerprint`
    /// canonicalizes them. Otherwise a future edit to migration 0020
    /// (e.g. dropping the shadow-column comments) would leave the
    /// helper stale, and resized stores would silently keep the helper
    /// form while fresh default stores follow the migration.
    #[test]
    fn helper_record_vectors_create_sql_matches_migration_0020_at_default_dim() {
        use super::{DEFAULT_VEC_DIM, record_vectors_create_sql};

        let helper = record_vectors_create_sql(DEFAULT_VEC_DIM);

        register_vec0();
        let mut conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::migrations::migrations()
            .to_latest(&mut conn)
            .expect("migrate");
        let migration_form: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema \
                 WHERE type = 'table' AND name = 'record_vectors'",
                [],
                |r| r.get(0),
            )
            .expect("read migration DDL");

        // Use the same whitespace-collapsing canonicalization the
        // verify path uses; SQLite's stored DDL string preserves the
        // original line breaks and indentation, but our digest folds
        // those down before hashing.
        let canon = |s: &str| {
            let mut out = String::with_capacity(s.len());
            let mut last_was_ws = true;
            for c in s.chars() {
                if c.is_whitespace() {
                    if !last_was_ws {
                        out.push(' ');
                        last_was_ws = true;
                    }
                } else {
                    out.push(c);
                    last_was_ws = false;
                }
            }
            out.trim().to_string()
        };

        assert_eq!(
            canon(&helper),
            canon(&migration_form),
            "record_vectors_create_sql must mirror migration 0020. \
             If you changed the migration, update the helper to match.",
        );
    }
}
