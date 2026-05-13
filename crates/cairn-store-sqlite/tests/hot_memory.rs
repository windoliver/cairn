//! `SQLite` hot-memory integration tests.

use cairn_core::contract::memory_store::{
    HotMemoryInvalidationScope, HotMemoryRequest, MemoryStore,
};
use cairn_core::hot_memory::{
    HotMemoryCacheInfo, HotMemoryCacheStatus, HotMemoryOptions, HotMemorySourceKind,
    assemble_hot_memory, assemble_hot_with_store, default_source_order,
};
use cairn_store_sqlite::{HotRecordSeed, SqliteMemoryStore};
use rusqlite::Connection;

fn request() -> HotMemoryRequest {
    HotMemoryRequest {
        session_id: Some("session-a".to_owned()),
        agent_id: Some("agent-a".to_owned()),
        budget_bytes: 4096,
        config_fingerprint: "config-a".to_owned(),
        god_node_weight: 0.3,
        source_kinds: default_source_order(),
    }
}

#[tokio::test]
async fn hot_memory_input_reads_vault_files_records_and_edges() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .write_vault_file("purpose.md", "purpose text")
        .expect("purpose");
    store
        .write_vault_file("index.md", "index text")
        .expect("index");
    store
        .write_vault_file("profile.md", "profile text")
        .expect("profile");
    store
        .insert_hot_record(
            HotRecordSeed::new(
                "01J0000000000000000000001",
                "user",
                "pinned text about ImportantType",
            )
            .tag("pinned")
            .salience(0.9),
        )
        .expect("pinned");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000002", "playbook", "playbook text")
                .salience(0.8),
        )
        .expect("playbook");
    store
        .insert_entity_edge(
            "ImportantType",
            "ContainsOnly",
            "uses",
            "src/important.rs",
            None,
        )
        .expect("edge");

    let input = store.hot_memory_input(&request()).await.expect("input");

    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Purpose && s.body.contains("purpose text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::ProjectState && s.body.contains("index text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Profile && s.body.contains("profile text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Pinned && s.body.contains("pinned text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Playbook && s.body.contains("playbook text"))
    );
    assert!(input.sources.iter().any(|s| s.centrality_score > 0.0));
}

#[tokio::test]
async fn hot_memory_input_respects_requested_source_kinds() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .write_vault_file("purpose.md", "purpose text")
        .expect("purpose");
    store
        .write_vault_file("index.md", "index text")
        .expect("index");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000001", "user", "pinned text")
                .tag("pinned")
                .salience(0.9),
        )
        .expect("pinned");
    let mut req = request();
    req.source_kinds = vec![HotMemorySourceKind::Purpose];

    let input = store.hot_memory_input(&req).await.expect("input");

    assert_eq!(input.sources.len(), 1);
    assert_eq!(input.sources[0].kind, HotMemorySourceKind::Purpose);
}

#[tokio::test]
async fn unmatched_hot_record_has_zero_centrality_when_edges_exist() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000001", "user", "unmatched pinned text")
                .tag("pinned")
                .salience(0.9),
        )
        .expect("pinned");
    store
        .insert_entity_edge(
            "ImportantType",
            "ContainsOnly",
            "uses",
            "src/important.rs",
            None,
        )
        .expect("edge");

    let input = store.hot_memory_input(&request()).await.expect("input");
    let pinned = input
        .sources
        .iter()
        .find(|source| source.record_id.as_deref() == Some("01J0000000000000000000001"))
        .expect("pinned source");

    assert!(pinned.centrality_score.abs() < f32::EPSILON);
}

#[tokio::test]
async fn source_revision_changes_when_ranking_scores_change() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000001", "user", "pinned text")
                .tag("pinned")
                .salience(0.8)
                .evidence(0.4),
        )
        .expect("pinned");

    let before = store
        .hot_memory_input(&request())
        .await
        .expect("input before")
        .source_revision;
    store
        .update_hot_record_scores("01J0000000000000000000001", 0.2, 0.9)
        .expect("update scores");
    let after = store
        .hot_memory_input(&request())
        .await
        .expect("input after")
        .source_revision;

    assert_ne!(before, after);
}

#[tokio::test]
async fn centrality_normalizes_across_candidate_sources_only() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .insert_hot_record(
            HotRecordSeed::new(
                "01J0000000000000000000001",
                "user",
                "pinned text about CandidateEntity",
            )
            .tag("pinned")
            .salience(0.9),
        )
        .expect("candidate");
    store
        .insert_entity_edge(
            "CandidateEntity",
            "RelevantNeighbor",
            "uses",
            "src/candidate.rs",
            None,
        )
        .expect("candidate edge");
    for i in 0..5 {
        store
            .insert_entity_edge(
                "UnrelatedHub",
                format!("UnrelatedNeighbor{i}"),
                "uses",
                format!("src/unrelated_{i}.rs"),
                None,
            )
            .expect("unrelated edge");
    }

    let input = store.hot_memory_input(&request()).await.expect("input");
    let candidate = input
        .sources
        .iter()
        .find(|source| source.record_id.as_deref() == Some("01J0000000000000000000001"))
        .expect("candidate source");

    assert!((candidate.centrality_score - 1.0).abs() < f32::EPSILON);
}

#[tokio::test]
async fn live_graph_edge_changes_hot_memory_source_revision() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .insert_hot_record(
            HotRecordSeed::new(
                "01J0000000000000000000001",
                "user",
                "pinned text about ImportantType",
            )
            .tag("pinned")
            .salience(0.9),
        )
        .expect("pinned");

    let before = store
        .hot_memory_input(&request())
        .await
        .expect("input before")
        .source_revision;
    store
        .insert_entity_edge(
            "ImportantType",
            "ContainsOnly",
            "uses",
            "src/important.rs",
            None,
        )
        .expect("edge");
    let after = store
        .hot_memory_input(&request())
        .await
        .expect("input after")
        .source_revision;

    assert_ne!(before, after);
}

#[tokio::test]
async fn hot_memory_input_filters_records_by_agent_scope() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000001", "user", "agent a pinned text")
                .agent("agent-a")
                .tag("pinned")
                .salience(0.9),
        )
        .expect("agent a");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000002", "user", "agent b pinned text")
                .agent("agent-b")
                .tag("pinned")
                .salience(0.9),
        )
        .expect("agent b");

    let input = store.hot_memory_input(&request()).await.expect("input");

    assert!(
        input
            .sources
            .iter()
            .any(|s| s.body == "agent a pinned text")
    );
    assert!(
        !input
            .sources
            .iter()
            .any(|s| s.body == "agent b pinned text")
    );
}

#[test]
fn persistent_store_migration_is_idempotent_on_reopen() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    SqliteMemoryStore::open(tempdir.path()).expect("first open");
    SqliteMemoryStore::open(tempdir.path()).expect("second open");

    let conn = Connection::open(tempdir.path().join(".cairn/cairn.db")).expect("db");
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user version");

    assert_eq!(version, 1);
}

#[test]
fn opening_future_schema_version_returns_store_error() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tempdir.path().join(".cairn")).expect("cairn dir");
    let conn = Connection::open(tempdir.path().join(".cairn/cairn.db")).expect("db");
    conn.execute_batch("PRAGMA user_version = 2;")
        .expect("future version");

    let Err(err) = SqliteMemoryStore::open(tempdir.path()) else {
        panic!("future schema should be rejected");
    };

    assert!(err.to_string().contains("newer sqlite schema version"));
}

#[test]
fn opening_partial_zero_version_schema_returns_clear_migration_error() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tempdir.path().join(".cairn")).expect("cairn dir");
    let conn = Connection::open(tempdir.path().join(".cairn/cairn.db")).expect("db");
    conn.execute_batch(
        "CREATE TABLE records (
            record_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            body TEXT NOT NULL
        );",
    )
    .expect("partial schema");

    let Err(err) = SqliteMemoryStore::open(tempdir.path()) else {
        panic!("partial schema should be rejected");
    };

    assert!(err.to_string().contains("migration incomplete"));
}

#[tokio::test]
async fn hot_memory_cache_hits_and_invalidates_by_session() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .write_vault_file("purpose.md", "purpose text")
        .expect("purpose");
    let req = request();
    let input = store.hot_memory_input(&req).await.expect("input");
    let key = store.hot_memory_cache_key(&req, &input).expect("key");
    let output = assemble_hot_memory(
        &input,
        HotMemoryOptions {
            budget_bytes: req.budget_bytes,
            god_node_weight: req.god_node_weight,
            cache: HotMemoryCacheInfo::refreshed(key.clone()),
            source_order: req.source_kinds.clone(),
        },
    );
    store
        .store_hot_memory_cache(&key, &output)
        .await
        .expect("store cache");
    assert!(
        store
            .load_hot_memory_cache(&key)
            .await
            .expect("load cache")
            .is_some()
    );
    let deleted = store
        .invalidate_hot_memory_cache(HotMemoryInvalidationScope::Session("session-a".to_owned()))
        .await
        .expect("invalidate");
    assert_eq!(deleted, 1);
    assert!(
        store
            .load_hot_memory_cache(&key)
            .await
            .expect("load cache")
            .is_none()
    );
}

#[tokio::test]
async fn hot_memory_cache_is_scoped_to_requested_source_order() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .write_vault_file("purpose.md", "purpose text")
        .expect("purpose");
    store
        .write_vault_file("profile.md", "profile text")
        .expect("profile");

    let mut purpose_first = request();
    purpose_first.source_kinds = vec![HotMemorySourceKind::Purpose, HotMemorySourceKind::Profile];
    let purpose_output = assemble_hot_with_store(&store, &purpose_first)
        .await
        .expect("purpose first");
    assert_eq!(purpose_output.cache.status, HotMemoryCacheStatus::Refreshed);

    let mut profile_first = request();
    profile_first.source_kinds = vec![HotMemorySourceKind::Profile, HotMemorySourceKind::Purpose];
    let profile_output = assemble_hot_with_store(&store, &profile_first)
        .await
        .expect("profile first");

    let purpose_index = profile_output
        .prefix
        .find("## purpose")
        .expect("purpose section");
    let profile_index = profile_output
        .prefix
        .find("## profile")
        .expect("profile section");
    assert!(profile_index < purpose_index);
    assert_eq!(profile_output.cache.status, HotMemoryCacheStatus::Refreshed);
    assert_ne!(profile_output.cache.key, purpose_output.cache.key);
}
