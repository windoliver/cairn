//! Integration tests for the hot-prefix cache + watermark tables (#83).

use cairn_core::contract::hot_prefix_cache::{CachedPrefix, HotPrefixCache};
use cairn_core::domain::Identity;
use cairn_core::domain::hot_prefix::{SourceClass, SourceWatermarks};
use cairn_store_sqlite::SqliteHotPrefixCache;

fn agent() -> Identity {
    Identity::parse("agt:cairn-cli:default:writer:v1").expect("identity")
}

async fn fresh_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // Bootstrap the SQLite store so migrations (including 0053) run.
    let _store = cairn_store_sqlite::open(dir.path().join("cairn.db"))
        .await
        .expect("open store");
    dir
}

#[tokio::test]
async fn migration_seeds_six_classes() {
    let vault = fresh_vault().await;
    let cache = SqliteHotPrefixCache::open(vault.path())
        .await
        .expect("open cache");
    let wm = cache.current_watermarks().await.expect("read watermarks");
    assert_eq!(wm, SourceWatermarks::default());
}

#[tokio::test]
async fn put_then_get_round_trips() {
    let vault = fresh_vault().await;
    let cache = SqliteHotPrefixCache::open(vault.path())
        .await
        .expect("open cache");
    let entry = CachedPrefix {
        prefix: b"hello".to_vec(),
        segments: vec![],
        bytes: 5,
        watermarks: SourceWatermarks::default(),
        assembled_at_ms: 1,
        assembly_latency_ms: 0,
    };
    cache.put(&agent(), "h1", &entry).await.expect("put");
    let got = cache.get(&agent(), "h1").await.expect("get").expect("Some");
    assert_eq!(got, entry);
}

#[tokio::test]
async fn upserting_user_record_bumps_profile_evidence() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::taxonomy::MemoryKind;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = cairn_store_sqlite::open(dir.path().join("cairn.db"))
        .await
        .expect("open store");
    let cache = SqliteHotPrefixCache::open(dir.path())
        .await
        .expect("open cache");

    let before = cache
        .current_watermarks()
        .await
        .expect("wm")
        .profile_evidence;

    let mut rec = cairn_test_fixtures::sample_record(0);
    rec.kind = MemoryKind::User;
    // Do NOT trigger the pinned predicate (tag "pinned" or
    // `extra_frontmatter["pinned"] == true`) — we only want ProfileEvidence
    // to bump, not Pinned.
    rec.tags.retain(|t| t != "pinned");

    store.upsert(&rec).await.expect("upsert");

    let after = cache
        .current_watermarks()
        .await
        .expect("wm")
        .profile_evidence;
    assert_eq!(
        after,
        before + 1,
        "ProfileEvidence watermark must bump on user record upsert"
    );
}

#[tokio::test]
async fn bump_invalidates_cached_row() {
    let vault = fresh_vault().await;
    let cache = SqliteHotPrefixCache::open(vault.path())
        .await
        .expect("open cache");
    let entry = CachedPrefix {
        prefix: b"x".to_vec(),
        segments: vec![],
        bytes: 1,
        watermarks: SourceWatermarks::default(),
        assembled_at_ms: 1,
        assembly_latency_ms: 0,
    };
    cache.put(&agent(), "h", &entry).await.expect("put");
    cache
        .bump(&[SourceClass::ProfileEvidence])
        .await
        .expect("bump");
    let got = cache.get(&agent(), "h").await.expect("get").expect("Some");
    let live = cache.current_watermarks().await.expect("wm");
    assert_ne!(
        got.watermarks, live,
        "bump must move live watermarks past the cached snapshot"
    );
}
