//! Verify embed-on-write behaviour in upsert.

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};

use cairn_store_sqlite::open_in_memory_with_embedder;
use cairn_test_fixtures::sample_record;

#[tokio::test]
async fn upsert_with_embedder_writes_vector_row() {
    let embedder: Arc<dyn EmbeddingModel> =
        Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .unwrap();
    let r = sample_record(1);
    let outcome = store.upsert(&r).await.unwrap();
    assert!(outcome.content_changed);

    let rid = outcome.record_id.as_str().to_owned();
    let conn = store.raw_conn().unwrap().clone();
    let found: bool = conn
        .call(move |c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                rusqlite::params![rid],
                |row| row.get(0),
            )?;
            Ok::<_, tokio_rusqlite::Error>(n > 0)
        })
        .await
        .unwrap();
    assert!(
        found,
        "record_vectors must have a row after upsert with embedder"
    );
}

#[tokio::test]
async fn upsert_without_embedder_no_vector_row() {
    let store = open_in_memory_with_embedder(None).await.unwrap();
    let r = sample_record(2);
    let outcome = store.upsert(&r).await.unwrap();

    let rid = outcome.record_id.as_str().to_owned();
    let conn = store.raw_conn().unwrap().clone();
    let count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                rusqlite::params![rid],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(count, 0, "no vector row when no embedder");
}

#[tokio::test]
async fn upsert_embed_failure_queues_pending() {
    use cairn_embeddings_local::error::EmbeddingError;

    struct AlwaysFail;
    impl EmbeddingModel for AlwaysFail {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::BgeSmallEnV1_5
        }
        fn dim(&self) -> usize {
            384
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("simulated failure".into()))
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Err(EmbeddingError::Tokenizer("simulated failure".into()))
        }
    }

    let embedder: Arc<dyn EmbeddingModel> = Arc::new(AlwaysFail);
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .unwrap();
    let r = sample_record(3);
    // Upsert must SUCCEED even when embedding fails.
    let outcome = store.upsert(&r).await.unwrap();
    assert!(outcome.content_changed);

    let rid = outcome.record_id.as_str().to_owned();
    let conn = store.raw_conn().unwrap().clone();
    // Vector row must NOT exist.
    let vec_count: i64 = conn
        .call({
            let rid = rid.clone();
            move |c| {
                c.query_row(
                    "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                    rusqlite::params![rid],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            }
        })
        .await
        .unwrap();
    assert_eq!(vec_count, 0, "no vector row when embed failed");

    // Pending embeddings row MUST exist.
    let pending_count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM pending_embeddings WHERE record_id = ?",
                rusqlite::params![rid],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(
        pending_count, 1,
        "pending_embeddings must have a row when embed failed"
    );
}

/// Bootstrap resizes `record_vectors` when the embedder reports a dim
/// other than the migration default (384). Used by the `OpenAI` embedder
/// path to avoid silent dim mismatch on insert. Reads the actual `embedding`
/// column dimensionality off the schema string emitted by `SQLite`.
#[tokio::test]
async fn open_with_1536_dim_embedder_resizes_record_vectors() {
    use cairn_embeddings_local::error::EmbeddingError;

    struct OpenAiLike;
    impl EmbeddingModel for OpenAiLike {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large
        }
        fn dim(&self) -> usize {
            1536
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
    }

    let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiLike);
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .unwrap();

    // Inspect the live vec0 schema from sqlite_schema. The DDL string
    // reads `... embedding float[1536] ...` after the resize.
    let conn = store.raw_conn().unwrap().clone();
    let ddl: String = conn
        .call(|c| {
            c.query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'record_vectors'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert!(
        ddl.contains("float[1536]"),
        "expected vec0 dim 1536 after resize, got DDL: {ddl}",
    );

    // And: a 1536-dim vector now inserts cleanly via the upsert path.
    let r = sample_record(3);
    let outcome = store.upsert(&r).await.unwrap();
    assert!(outcome.content_changed);
    let rid = outcome.record_id.as_str().to_owned();
    let count: i64 = conn
        .call(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM record_vectors WHERE record_id = ?",
                rusqlite::params![rid],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(count, 1, "1536-dim vector row must persist after resize");
}

/// Regression for Codex round-1 finding #1: a persistent 1536-dim store
/// must reopen even after rows are present. Earlier versions of
/// `resize_record_vectors` refused any resize when the table was
/// non-empty, breaking persistent OpenAI-backed stores on the second
/// open. Now: if the existing dim already matches, the resize is a
/// no-op.
#[tokio::test]
async fn reopen_populated_1536_dim_store_is_idempotent() {
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_store_sqlite::open_with_embedder;
    use tempfile::tempdir;

    struct OpenAiLike;
    impl EmbeddingModel for OpenAiLike {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large
        }
        fn dim(&self) -> usize {
            1536
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
    }

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiLike);

    // First open: bootstrap resizes to 1536 and we insert one vector.
    {
        let store = open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
            .await
            .expect("first open");
        let r = sample_record(3);
        store.upsert(&r).await.expect("upsert");
    }

    // Second open with the SAME 1536-dim embedder: must succeed even
    // though `record_vectors` now holds rows. The pre-fix path would
    // SchemaDrift here.
    let store2 = open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
        .await
        .expect("reopen populated 1536-dim store");

    let conn = store2.raw_conn().unwrap().clone();
    let count: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "vector row from first open must survive the reopen"
    );
}

/// Regression for Codex round-2 finding #1: a populated 1536-dim store
/// must reopen even WITHOUT the embedder (e.g. a recovery / inspection
/// session where the `OpenAI` key isn't available). Bootstrap reads the
/// on-disk dim and computes the fingerprint against that, instead of
/// falling back to the migration default and tripping schema drift.
#[tokio::test]
async fn reopen_populated_1536_dim_store_without_embedder() {
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_store_sqlite::{open, open_with_embedder};
    use tempfile::tempdir;

    struct OpenAiLike;
    impl EmbeddingModel for OpenAiLike {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large
        }
        fn dim(&self) -> usize {
            1536
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
    }

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    {
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiLike);
        let store = open_with_embedder(&db_path, Some(embedder))
            .await
            .expect("first open with embedder");
        let r = sample_record(3);
        store.upsert(&r).await.expect("upsert");
    }

    // Reopen with NO embedder — pre-fix this would fail the fingerprint
    // check because verify_schema_fingerprint(_, None) re-applied
    // migrations to a fresh in-memory DB at the default 384-dim and
    // hashed the 384-dim DDL, while the on-disk DDL is 1536-dim.
    let store = open(&db_path).await.expect("reopen without embedder");
    let conn = store.raw_conn().unwrap().clone();
    let count: i64 = conn
        .call(|c| {
            c.query_row("SELECT COUNT(*) FROM record_vectors", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(count, 1, "vectors must remain readable without embedder");
}

/// Regression for Codex round-4 finding #1: an EMPTY 1536-dim store
/// must accept a 384-dim reopen (e.g. switching back to the local BGE
/// embedder after evaluating with `OpenAI`). Bootstrap previously only
/// resized when the requested dim differed from `DEFAULT_VEC_DIM`,
/// silently leaving the table at 1536 — and `verify_schema_fingerprint`
/// then tripped because the expected DDL was hashed at 384.
#[tokio::test]
async fn reopen_empty_1536_dim_store_with_384_dim_embedder_resizes_back() {
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_store_sqlite::open_with_embedder;
    use tempfile::tempdir;

    struct OpenAiLike;
    impl EmbeddingModel for OpenAiLike {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large
        }
        fn dim(&self) -> usize {
            1536
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
    }

    struct Bge384;
    impl EmbeddingModel for Bge384 {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::BgeSmallEnV1_5
        }
        fn dim(&self) -> usize {
            384
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 384])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 384])
        }
    }

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    // First open at 1536, do not insert anything → empty record_vectors at 1536-dim.
    {
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiLike);
        let _ = open_with_embedder(&db_path, Some(embedder))
            .await
            .expect("first open at 1536");
    }

    // Reopen with a 384-dim embedder. Pre-fix: bootstrap saw `dim ==
    // DEFAULT_VEC_DIM`, skipped the resize, and the fingerprint check
    // failed because the on-disk DDL was still `float[1536]`.
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(Bge384);
    let _ = open_with_embedder(&db_path, Some(embedder))
        .await
        .expect("reopen empty 1536-dim store at 384-dim must succeed");
}

/// Regression for Codex round-5 finding #1: after the 1536→384 resize,
/// the on-disk `record_vectors` is in the recreated DDL form. Subsequent
/// reopens (with a 384 embedder, or *without* one) must still pass the
/// fingerprint check — otherwise a one-time re-target would burn down
/// the store on its next launch.
///
/// Pre-fix the digest path branched on `dim != DEFAULT_VEC_DIM` and the
/// recreated-form text content disagreed with the migration form, so
/// the second reopen would trip schema drift even though nothing on
/// disk changed.
#[tokio::test]
async fn second_reopen_after_1536_to_384_resize_does_not_drift() {
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_store_sqlite::{open, open_with_embedder};
    use tempfile::tempdir;

    struct OpenAiLike;
    impl EmbeddingModel for OpenAiLike {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large
        }
        fn dim(&self) -> usize {
            1536
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
    }

    struct Bge384;
    impl EmbeddingModel for Bge384 {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::BgeSmallEnV1_5
        }
        fn dim(&self) -> usize {
            384
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 384])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 384])
        }
    }

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    {
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiLike);
        let _ = open_with_embedder(&db_path, Some(embedder))
            .await
            .expect("first open at 1536");
    }
    {
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(Bge384);
        let _ = open_with_embedder(&db_path, Some(embedder))
            .await
            .expect("second open resizes 1536→384");
    }

    // Third open at 384 with embedder: no resize runs (current==dim).
    {
        let embedder: Arc<dyn EmbeddingModel> = Arc::new(Bge384);
        let _ = open_with_embedder(&db_path, Some(embedder))
            .await
            .expect("third open at 384 must not drift");
    }
    // Fourth open with NO embedder — the inspection / recovery path.
    let _ = open(&db_path)
        .await
        .expect("fourth open without embedder must not drift");
}

/// Regression for Codex round-7 finding #1: bootstrap must verify
/// migration history BEFORE running `resize_record_vectors`. A DB
/// with a tampered `schema_migrations.sql_hash` reopened with a
/// different-dimension embedder used to commit a DROP/CREATE on
/// `record_vectors` and only then refuse the open — leaving the
/// untrusted store partially mutated.
#[tokio::test]
async fn tampered_migration_history_rejects_open_without_resizing_record_vectors() {
    use cairn_embeddings_local::error::EmbeddingError;
    use cairn_store_sqlite::open_with_embedder;
    use tempfile::tempdir;

    struct OpenAiLike;
    impl EmbeddingModel for OpenAiLike {
        fn kind(&self) -> EmbeddingModelKind {
            EmbeddingModelKind::OpenAiTextEmbedding3Large
        }
        fn dim(&self) -> usize {
            1536
        }
        fn embed_document(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
        fn embed_query(&self, _: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![0.0; 1536])
        }
    }

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    // Migrate to head at default 384-dim, no embedder.
    let _ = cairn_store_sqlite::open(&db_path)
        .await
        .expect("first open at 384");

    // Tamper a `sql_hash` directly. The immutable trigger only allows
    // the empty -> non-empty stamp transition, so we drop it for the
    // duration of the corruption write. (The reopen path doesn't run
    // migrations against this same connection, so the trigger stays
    // dropped — that's fine: we only care that bootstrap rejects the
    // tampered hash before mutating `record_vectors`.)
    {
        let conn = rusqlite::Connection::open(&db_path).expect("raw open");
        conn.execute_batch(
            "DROP TRIGGER schema_migrations_immutable; \
             UPDATE schema_migrations SET sql_hash = 'tampered' WHERE migration_id = 1;",
        )
        .expect("tamper migration history");
    }

    // Reopen with a different-dimension embedder. Pre-fix: bootstrap
    // resized record_vectors → 1536 BEFORE the migration history check
    // and only then errored. Post-fix: history check runs first; the
    // open errors with the table still at 384.
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(OpenAiLike);
    let err = open_with_embedder(&db_path, Some(embedder))
        .await
        .expect_err("tampered DB must refuse to open");
    let dbg = format!("{err:?}").to_lowercase();
    assert!(
        dbg.contains("hash") || dbg.contains("drift") || dbg.contains("tampered"),
        "expected schema-drift / hash-mismatch error, got: {err:?}",
    );

    // record_vectors must still be at the pre-resize 384 dim — the
    // reject path must not have committed the resize.
    let conn = rusqlite::Connection::open(&db_path).expect("raw reopen");
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema \
             WHERE type = 'table' AND name = 'record_vectors'",
            [],
            |r| r.get(0),
        )
        .expect("read record_vectors DDL");
    assert!(
        ddl.contains("float[384]"),
        "record_vectors must remain at 384-dim after rejected open; got {ddl}",
    );
}

/// Regression for Codex round-8 finding #1: a *pre-head* DB with a
/// tampered already-applied migration must be rejected BEFORE
/// `to_latest` applies any pending migrations. Pre-fix the post-
/// migration check would catch the drift, but only after silently
/// upgrading an untrusted store.
#[tokio::test]
async fn pre_head_tampered_db_rejects_open_without_applying_pending_migrations() {
    use cairn_store_sqlite::open;
    use rusqlite_migration::{M, Migrations};
    use tempfile::tempdir;

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    // Apply migrations 1..=6 — migration 6 renames `sql_blake3` to
    // `sql_hash` and installs the stamp-once trigger we tamper with —
    // and leave 7..=N pending.
    {
        let mut conn = rusqlite::Connection::open(&db_path).expect("raw open");
        let m: Migrations = Migrations::new(vec![
            M::up(include_str!("../src/migrations/sql/0001_records.sql")),
            M::up(include_str!("../src/migrations/sql/0002_wal.sql")),
            M::up(include_str!("../src/migrations/sql/0003_replay.sql")),
            M::up(include_str!("../src/migrations/sql/0004_locks.sql")),
            M::up(include_str!("../src/migrations/sql/0005_consent.sql")),
            M::up(include_str!(
                "../src/migrations/sql/0006_drift_hardening.sql"
            )),
        ]);
        m.to_latest(&mut conn).expect("apply 1..=6");
        // Tamper migration 1's hash. Drop the immutable trigger first
        // (it would block the UPDATE under normal circumstances).
        conn.execute_batch(
            "DROP TRIGGER schema_migrations_immutable; \
             UPDATE schema_migrations SET sql_hash = 'tampered' WHERE migration_id = 1;",
        )
        .expect("tamper");
    }

    let pre_count: i64 = {
        let conn = rusqlite::Connection::open(&db_path).expect("raw reopen");
        conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .expect("count migrations")
    };
    assert_eq!(
        pre_count, 6,
        "test setup: must start at 6 applied migrations"
    );

    let err = open(&db_path)
        .await
        .expect_err("tampered pre-head DB must refuse to open");
    let dbg = format!("{err:?}").to_lowercase();
    assert!(
        dbg.contains("hash") || dbg.contains("drift") || dbg.contains("tampered"),
        "expected schema-drift / hash-mismatch error, got: {err:?}",
    );

    // No pending migrations were applied.
    let post_count: i64 = {
        let conn = rusqlite::Connection::open(&db_path).expect("raw reopen 2");
        conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .expect("count after")
    };
    assert_eq!(
        post_count, pre_count,
        "rejected open must not have applied pending migrations; \
         pre={pre_count} post={post_count}",
    );
}

/// Regression for Codex round-10 finding #1: a database stuck at
/// migrations 1..=5 still has the legacy `sql_blake3` column —
/// migration 0006 is what renames it to `sql_hash`. Preflight must
/// recognise the legacy shape and let `to_latest` upgrade through
/// 0006 instead of misreading every row as "missing" and rejecting
/// the open.
#[tokio::test]
async fn pre_0006_database_upgrades_through_to_latest() {
    use cairn_store_sqlite::open;
    use rusqlite_migration::{M, Migrations};
    use tempfile::tempdir;

    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");

    // Apply only migrations 1..=5 (legacy `sql_blake3` shape).
    {
        let mut conn = rusqlite::Connection::open(&db_path).expect("raw open");
        let m: Migrations = Migrations::new(vec![
            M::up(include_str!("../src/migrations/sql/0001_records.sql")),
            M::up(include_str!("../src/migrations/sql/0002_wal.sql")),
            M::up(include_str!("../src/migrations/sql/0003_replay.sql")),
            M::up(include_str!("../src/migrations/sql/0004_locks.sql")),
            M::up(include_str!("../src/migrations/sql/0005_consent.sql")),
        ]);
        m.to_latest(&mut conn).expect("apply 1..=5");
        // Sanity: the legacy column is in place.
        let col: String = conn
            .query_row(
                "SELECT name FROM pragma_table_info('schema_migrations') \
                 WHERE name IN ('sql_blake3','sql_hash')",
                [],
                |r| r.get(0),
            )
            .expect("read column name");
        assert_eq!(col, "sql_blake3", "test setup: must start at legacy shape");
    }

    // The current binary's `open` should preflight successfully (legacy
    // shape recognised), apply migrations 6..=N, then verify+stamp.
    let _ = open(&db_path).await.expect("upgrade through to_latest");

    // Post-condition: column has been renamed and the row count matches
    // the head of the manifest.
    let conn = rusqlite::Connection::open(&db_path).expect("raw reopen");
    let col: String = conn
        .query_row(
            "SELECT name FROM pragma_table_info('schema_migrations') \
             WHERE name IN ('sql_blake3','sql_hash')",
            [],
            |r| r.get(0),
        )
        .expect("read column name post-upgrade");
    assert_eq!(col, "sql_hash", "0006 must have renamed the column");
}
