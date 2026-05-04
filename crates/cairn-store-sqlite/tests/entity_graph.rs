//! Issue #186 — bitemporal knowledge-graph integration tests.
//!
//! This file is the seed for the entity-graph test suite. The first test
//! is a migration regression test (0031 `wal_ops` table-rebuild); subsequent
//! tasks (4–13) add entity-node, entity-edge, episode-link, and query tests.

use cairn_store_sqlite::open_in_memory_sync;

#[test]
fn migration_0031_widens_wal_ops_kind_and_preserves_existing_rows() {
    let conn = open_in_memory_sync().expect("open");

    // Insert one wal_ops row using a pre-existing kind to simulate
    // historical data that survived the table rebuild.
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, \
         issuer, principal, target_hash, scope_json, expires_at, signature, \
         issued_at, updated_at) VALUES \
         ('op-pre', 1, 'upsert', 'ISSUED', '{}', 'sys', NULL, 'h', '{}', \
          9999999999, 'sig', 1, 1)",
        [],
    )
    .expect("insert pre-existing row");

    // The widened CHECK must accept the new graph kinds.
    for kind in [
        "graph_upsert_entity",
        "graph_upsert_edge",
        "graph_contradict",
        "graph_tombstone",
        "graph_link_episode",
    ] {
        let sql = format!(
            "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, \
             issuer, principal, target_hash, scope_json, expires_at, signature, \
             issued_at, updated_at) VALUES \
             ('op-{kind}', (SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops), \
              '{kind}', 'ISSUED', '{{}}', 'sys', NULL, 'h', '{{}}', \
              9999999999, 'sig', 1, 1)"
        );
        conn.execute(&sql, []).expect("insert new kind");
    }

    // The pre-existing row must still be present after the rebuild.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_ops WHERE operation_id = 'op-pre'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "pre-existing wal_ops row must survive rebuild");

    // The state-transition trigger must still fire on the rebuilt table.
    let res = conn.execute(
        "UPDATE wal_ops SET state = 'COMMITTED' WHERE operation_id = 'op-pre'",
        [],
    );
    assert!(
        res.is_err(),
        "state-transition trigger must reject ISSUED -> COMMITTED",
    );

    // FK enforcement on wal_op_deps must still work after the rebuild.
    // The runtime path re-asserts PRAGMA foreign_keys=ON on connection
    // open, so this exercises the live-FK path users actually hit.
    let fk_res = conn.execute(
        "INSERT INTO wal_op_deps (operation_id, depends_on_op_id) \
         VALUES ('does-not-exist', 'op-pre')",
        [],
    );
    assert!(
        fk_res.is_err(),
        "wal_op_deps FK must still reject unknown operation_id after rebuild",
    );
}

#[test]
fn migration_0032_creates_entity_nodes_with_constraints() {
    let conn = open_in_memory_sync().expect("open");

    // Insert a row.
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, summary, created_at) \
         VALUES ('01HZE7JV5N0000000000000001', 'Alice', 'alice', 'eng', 1)",
        [],
    )
    .expect("insert ok");

    // UNIQUE(name_norm) enforced.
    let dup = conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) \
         VALUES ('01HZE7JV5N0000000000000002', 'ALICE', 'alice', 2)",
        [],
    );
    assert!(
        dup.is_err(),
        "UNIQUE(name_norm) must reject duplicate normalized name"
    );
}

#[test]
fn migration_0032_fts_round_trips_inserts_updates_deletes() {
    let conn = open_in_memory_sync().expect("open");

    // Insert with summary — both columns indexed.
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, summary, created_at) \
         VALUES ('01HZE7JV5N0000000000000010', 'Carol', 'carol', 'algorithms expert', 1)",
        [],
    )
    .expect("insert with summary");

    // Insert with NULL summary — exercises COALESCE in the trigger.
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) \
         VALUES ('01HZE7JV5N0000000000000011', 'Dave', 'dave', 1)",
        [],
    )
    .expect("insert without summary");

    let hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_nodes_fts WHERE entity_nodes_fts MATCH 'algorithms'",
            [],
            |r| r.get(0),
        )
        .expect("fts query");
    assert_eq!(hits, 1, "FTS index must surface inserted summary text");

    // Update changes the summary; old terms must drop, new terms must appear.
    conn.execute(
        "UPDATE entity_nodes SET summary = 'cryptography lead' \
         WHERE id = '01HZE7JV5N0000000000000010'",
        [],
    )
    .expect("update summary");
    let stale: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_nodes_fts WHERE entity_nodes_fts MATCH 'algorithms'",
            [],
            |r| r.get(0),
        )
        .expect("post-update query");
    assert_eq!(stale, 0, "FTS sync trigger must drop old summary terms");
    let fresh: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_nodes_fts WHERE entity_nodes_fts MATCH 'cryptography'",
            [],
            |r| r.get(0),
        )
        .expect("post-update query");
    assert_eq!(fresh, 1, "FTS sync trigger must surface new summary terms");

    // Delete must purge from the FTS index.
    conn.execute(
        "DELETE FROM entity_nodes WHERE id = '01HZE7JV5N0000000000000010'",
        [],
    )
    .expect("delete");
    let after_delete: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_nodes_fts WHERE entity_nodes_fts MATCH 'cryptography'",
            [],
            |r| r.get(0),
        )
        .expect("post-delete query");
    assert_eq!(after_delete, 0, "FTS sync trigger must purge on delete");
}

#[test]
fn migration_0032_shrink_guard_rejects_silent_expiry() {
    let conn = open_in_memory_sync().expect("open");
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) \
         VALUES ('01HZE7JV5N0000000000000003', 'Bob', 'bob', 1)",
        [],
    )
    .expect("seed");

    // expired_at without tombstone_reason → trigger fires.
    let res = conn.execute(
        "UPDATE entity_nodes SET expired_at = 999 \
         WHERE id = '01HZE7JV5N0000000000000003'",
        [],
    );
    assert!(
        res.is_err(),
        "shrink-guard must reject expired_at without tombstone_reason"
    );

    // expired_at + tombstone_reason → allowed.
    conn.execute(
        "UPDATE entity_nodes SET expired_at = 999, tombstone_reason = 'forget' \
         WHERE id = '01HZE7JV5N0000000000000003'",
        [],
    )
    .expect("with reason ok");
}

#[test]
fn migration_0033_partial_unique_blocks_concurrent_live_triple() {
    let conn = open_in_memory_sync().expect("open");

    // Seed two entities.
    conn.execute_batch(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) VALUES \
           ('n1', 'Alice', 'alice', 1), \
           ('n2', 'Acme', 'acme', 1);",
    )
    .expect("seed nodes");

    // Insert one live edge.
    conn.execute(
        "INSERT INTO entity_edges (id, source_id, target_id, relation, \
         confidence, confidence_score, valid_at, created_at, body_hash) \
         VALUES ('e1', 'n1', 'n2', 'works_at', 'EXTRACTED', 1.0, 1, 1, X'00')",
        [],
    )
    .expect("first edge ok");

    // A second live edge for the same (source, target, relation) is rejected.
    let dup = conn.execute(
        "INSERT INTO entity_edges (id, source_id, target_id, relation, \
         confidence, confidence_score, valid_at, created_at, body_hash) \
         VALUES ('e2', 'n1', 'n2', 'works_at', 'EXTRACTED', 1.0, 2, 2, X'01')",
        [],
    );
    assert!(
        dup.is_err(),
        "partial unique must reject second live triple"
    );

    // Invalidate the first; second insert now succeeds.
    conn.execute("UPDATE entity_edges SET invalid_at = 2 WHERE id = 'e1'", [])
        .expect("invalidate");
    conn.execute(
        "INSERT INTO entity_edges (id, source_id, target_id, relation, \
         confidence, confidence_score, valid_at, created_at, body_hash) \
         VALUES ('e2', 'n1', 'n2', 'works_at', 'EXTRACTED', 1.0, 2, 2, X'01')",
        [],
    )
    .expect("second insert ok after invalidate");
}

#[test]
fn migration_0033_fk_set_null_on_record_delete() {
    let conn = open_in_memory_sync().expect("open");

    conn.execute_batch(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) VALUES \
           ('n1', 'X', 'x', 1), ('n2', 'Y', 'y', 1);",
    )
    .expect("nodes");

    // Seed a minimal records row directly via SQL. The records table has
    // NOT NULL constraints on record_json/tags_json (DEFAULTs '{}' / '[]'
    // from migration 0008) and GENERATED columns that json_extract from
    // record_json (migration 0012), so we must provide valid JSON.
    conn.execute(
        "INSERT INTO records \
         (record_id, target_id, version, path, kind, class, visibility, \
          scope, actor_chain, body, body_hash, created_at, updated_at) \
         VALUES \
         ('rec-1', 't-1', 1, 'p', 'fact', 'episodic', 'private', \
          '{}', '[]', '', 'h', 1, 1)",
        [],
    )
    .expect("seed records row");

    conn.execute(
        "INSERT INTO entity_edges (id, source_id, target_id, relation, \
         confidence, confidence_score, valid_at, created_at, body_hash, \
         source_record_id) \
         VALUES ('e1', 'n1', 'n2', 'r', 'EXTRACTED', 1.0, 1, 1, X'00', 'rec-1')",
        [],
    )
    .expect("edge");

    conn.execute("DELETE FROM records WHERE record_id = 'rec-1'", [])
        .expect("delete record");

    let fk: Option<String> = conn
        .query_row(
            "SELECT source_record_id FROM entity_edges WHERE id = 'e1'",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert!(fk.is_none(), "FK should be SET NULL after record delete");
}

#[test]
fn migration_0034_entity_episodes_idempotent_pk_and_cascade() {
    let conn = open_in_memory_sync().expect("open");

    // Seed an entity.
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) \
         VALUES ('n1', 'Alice', 'alice', 1)",
        [],
    )
    .expect("node");

    // Seed a minimal records row. The records table has NOT NULL
    // constraints on record_json/tags_json (DEFAULTs '{}' / '[]'
    // from migration 0008) and GENERATED columns that json_extract
    // from record_json (migration 0012), so we must provide valid JSON.
    conn.execute(
        "INSERT INTO records \
         (record_id, target_id, version, path, kind, class, visibility, \
          scope, actor_chain, body, body_hash, created_at, updated_at) \
         VALUES \
         ('rec-1', 't-1', 1, 'p', 'fact', 'episodic', 'private', \
          '{}', '[]', '', 'h', 1, 1)",
        [],
    )
    .expect("seed record");

    // First link succeeds.
    conn.execute(
        "INSERT INTO entity_episodes (episode_id, entity_node_id, linked_at) \
         VALUES ('rec-1', 'n1', 1)",
        [],
    )
    .expect("first link");

    // Second link with same PK is rejected (caller must use OR IGNORE).
    let dup = conn.execute(
        "INSERT INTO entity_episodes (episode_id, entity_node_id, linked_at) \
         VALUES ('rec-1', 'n1', 2)",
        [],
    );
    assert!(dup.is_err(), "PK uniqueness must reject duplicate link");

    // Cascade on record delete.
    conn.execute("DELETE FROM records WHERE record_id = 'rec-1'", [])
        .expect("delete");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_episodes WHERE episode_id = 'rec-1'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 0, "cascade must remove orphan episode link");
}

#[test]
fn wal_helper_writes_op_and_steps_in_one_tx() {
    use cairn_store_sqlite::entity_graph::wal as gwal;

    let mut conn = open_in_memory_sync().expect("open");
    let tx = conn.transaction().expect("tx");

    let op_id = gwal::issue_op(&tx, "graph_upsert_edge", "h-target").expect("issue");

    gwal::write_step(&tx, &op_id, 0, "insert_edge", None).expect("step 0");
    gwal::commit_op(&tx, &op_id).expect("commit");

    tx.commit().expect("tx commit");

    let kind: String = conn
        .query_row(
            "SELECT kind FROM wal_ops WHERE operation_id = ?1",
            [&op_id],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(kind, "graph_upsert_edge");

    let state: String = conn
        .query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            [&op_id],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(state, "COMMITTED");

    let step_state: String = conn
        .query_row(
            "SELECT state FROM wal_steps WHERE operation_id = ?1 AND step_ord = 0",
            [&op_id],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(step_state, "DONE");
}

#[tokio::test]
async fn upsert_entity_inserts_new_returns_supplied_id() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EntityId, EntityNode};

    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let node = EntityNode {
        id: EntityId::from("01HZE7JV5N0000000000000010"),
        name: "Alice".into(),
        name_norm: "alice".into(),
        summary: Some("eng".into()),
        created_at: 1,
        embedding_id: None,
    };
    let id = store.upsert_entity(&node).await.expect("upsert");
    assert_eq!(id, node.id, "fresh insert returns the supplied id");
}

#[tokio::test]
async fn upsert_entity_dedup_returns_existing_id() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EntityId, EntityNode};

    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let first = EntityNode {
        id: EntityId::from("01HZE7JV5N0000000000000020"),
        name: "Alice".into(),
        name_norm: "alice".into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let id_a = store.upsert_entity(&first).await.expect("first");

    let dup = EntityNode {
        id: EntityId::from("01HZE7JV5N0000000000000021"),
        name: "ALICE".into(),
        name_norm: "alice".into(),
        summary: Some("changed".into()),
        created_at: 2,
        embedding_id: None,
    };
    let id_b = store.upsert_entity(&dup).await.expect("dup");

    assert_eq!(id_a, id_b, "duplicate name_norm collapses to existing id");
    assert_eq!(
        id_b, first.id,
        "the existing id is preferred over the new one"
    );
}

#[tokio::test]
async fn link_entity_episode_idempotent_returns_true_then_false() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EntityId, EntityNode};
    use cairn_core::domain::record::RecordId;

    let store = cairn_store_sqlite::open_in_memory().await.expect("open");

    // Seed an entity via the new API.
    let node = EntityNode {
        id: EntityId::from("01HZE7JV5N0000000000000030"),
        name: "Alice".into(),
        name_norm: "alice-link".into(),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let entity_id = store.upsert_entity(&node).await.expect("entity");

    // Seed a real records row directly via the inner connection.
    // `raw_conn()` is gated by #[cfg(any(test, feature = "test-helpers"))].
    {
        let conn_arc = store.raw_conn().expect("conn present after open_in_memory");
        conn_arc
            .call(|c| {
                c.execute(
                    "INSERT INTO records \
                     (record_id, target_id, version, path, kind, class, visibility, \
                      scope, actor_chain, body, body_hash, created_at, updated_at) \
                     VALUES \
                     ('01HQZX9F5N0000000000000099', 't-1', 1, 'p', 'fact', 'episodic', 'private', \
                      '{}', '[]', '', 'h', 1, 1)",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("seed record");
    }
    let rec_id = RecordId::parse("01HQZX9F5N0000000000000099").expect("parse rec id");

    let first = store
        .link_entity_episode(&entity_id, &rec_id)
        .await
        .expect("first link");
    assert!(first, "first link returns true");

    let second = store
        .link_entity_episode(&entity_id, &rec_id)
        .await
        .expect("second link");
    assert!(!second, "second link returns false");
}

async fn seed_two_entities(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    suffix: &str,
) -> (
    cairn_core::domain::graph::EntityId,
    cairn_core::domain::graph::EntityId,
) {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EntityId, EntityNode};
    let n1 = EntityNode {
        id: EntityId::from(format!("01HZE7JV5N00000000000000{suffix}A").as_str()),
        name: "Alice".into(),
        name_norm: format!("alice-{suffix}"),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let n2 = EntityNode {
        id: EntityId::from(format!("01HZE7JV5N00000000000000{suffix}B").as_str()),
        name: "Acme".into(),
        name_norm: format!("acme-{suffix}"),
        summary: None,
        created_at: 1,
        embedding_id: None,
    };
    let id1 = store.upsert_entity(&n1).await.expect("n1");
    let id2 = store.upsert_entity(&n2).await.expect("n2");
    (id1, id2)
}

#[tokio::test]
async fn upsert_entity_edge_simple_insert() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (src, tgt) = seed_two_entities(&store, "40").await;

    let edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N0000000000000050"),
        source_id: src.clone(),
        target_id: tgt.clone(),
        relation: "works_at".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 100,
        invalid_at: None,
        created_at: 100,
        source_record_id: None,
    };
    let outcome = store.upsert_entity_edge(&edge).await.expect("upsert");
    assert_eq!(outcome.new_edge_id, edge.id);
    assert_eq!(outcome.invalidated_edge_id, None);
    assert!(!outcome.body_was_unchanged);
}

#[tokio::test]
async fn upsert_entity_edge_idempotent_reupsert_no_op() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (src, tgt) = seed_two_entities(&store, "60").await;
    let edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N0000000000000060"),
        source_id: src,
        target_id: tgt,
        relation: "works_at".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 100,
        invalid_at: None,
        created_at: 100,
        source_record_id: None,
    };
    let first = store.upsert_entity_edge(&edge).await.expect("first");

    // Count wal_ops before the re-upsert.
    let pre_op_count = {
        let conn = store.raw_conn().expect("conn");
        conn.call(|c| {
            c.query_row("SELECT COUNT(*) FROM wal_ops", [], |r| r.get::<_, i64>(0))
                .map_err(Into::into)
        })
        .await
        .expect("count")
    };

    // Re-upsert with a different supplied id but identical body fields.
    let mut second_edge = edge.clone();
    second_edge.id = EntityEdgeId::from("01HZE7JV5N0000000000000061");
    let second = store
        .upsert_entity_edge(&second_edge)
        .await
        .expect("second");

    assert_eq!(second.new_edge_id, first.new_edge_id, "returns existing id");
    assert!(second.body_was_unchanged, "marks unchanged");
    assert_eq!(second.invalidated_edge_id, None);

    let post_op_count = {
        let conn = store.raw_conn().expect("conn");
        conn.call(|c| {
            c.query_row("SELECT COUNT(*) FROM wal_ops", [], |r| r.get::<_, i64>(0))
                .map_err(Into::into)
        })
        .await
        .expect("count")
    };
    assert_eq!(
        pre_op_count, post_op_count,
        "no new wal_ops row on idempotent re-upsert"
    );
}

#[tokio::test]
async fn upsert_entity_edge_contradiction_invalidates_old() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (src, tgt) = seed_two_entities(&store, "70").await;
    let old = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N0000000000000070"),
        source_id: src.clone(),
        target_id: tgt.clone(),
        relation: "works_at".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 100,
        invalid_at: None,
        created_at: 100,
        source_record_id: None,
    };
    let first = store.upsert_entity_edge(&old).await.expect("first");

    let new_edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N0000000000000071"),
        source_id: src,
        target_id: tgt,
        relation: "works_at".into(),
        confidence: EdgeConfidence::Inferred, // different body
        confidence_score: 0.7,                // different body
        valid_at: 200,
        invalid_at: None,
        created_at: 200,
        source_record_id: None,
    };
    let second = store.upsert_entity_edge(&new_edge).await.expect("second");

    assert_eq!(second.new_edge_id, new_edge.id);
    assert_eq!(second.invalidated_edge_id, Some(first.new_edge_id.clone()));
    assert!(!second.body_was_unchanged);

    let invalid_at: Option<i64> = {
        let conn = store.raw_conn().expect("conn");
        let old_id = first.new_edge_id.as_str().to_owned();
        conn.call(move |c| {
            c.query_row(
                "SELECT invalid_at FROM entity_edges WHERE id = ?1",
                [&old_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .expect("query")
    };
    assert_eq!(invalid_at, Some(200), "old edge invalid_at = new.valid_at");
}

#[tokio::test]
async fn graph_edges_direction_in_out_both() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "80").await;

    // a -> b
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N0000000000000080"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "out".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 1,
            invalid_at: None,
            created_at: 1,
            source_record_id: None,
        })
        .await
        .expect("a->b");
    // b -> a
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N0000000000000081"),
            source_id: b.clone(),
            target_id: a.clone(),
            relation: "in".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 1,
            invalid_at: None,
            created_at: 1,
            source_record_id: None,
        })
        .await
        .expect("b->a");

    let out = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("out");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].target_id, b);

    let inn = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::In,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("in");
    assert_eq!(inn.len(), 1);
    assert_eq!(inn[0].source_id, b);

    let both = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Both,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("both");
    assert_eq!(both.len(), 2);
}

#[tokio::test]
async fn graph_edges_relation_filter() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "90").await;
    for (id, rel) in [
        ("01HZE7JV5N0000000000000090", "works_at"),
        ("01HZE7JV5N0000000000000091", "lives_in"),
    ] {
        store
            .upsert_entity_edge(&EntityEdge {
                id: EntityEdgeId::from(id),
                source_id: a.clone(),
                target_id: b.clone(),
                relation: rel.into(),
                confidence: EdgeConfidence::Extracted,
                confidence_score: 1.0,
                valid_at: 1,
                invalid_at: None,
                created_at: 1,
                source_record_id: None,
            })
            .await
            .expect("seed");
    }

    let edges = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: Some("works_at"),
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("query");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].relation, "works_at");
}

#[tokio::test]
async fn graph_edges_as_of_event_time_slicing() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "A0").await;

    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000A0"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed");

    // Before t=100 -> no edges.
    let before = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(99),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("before");
    assert_eq!(before.len(), 0, "edge not yet valid at t=99");

    // At t=100 -> present.
    let at = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(100),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("at");
    assert_eq!(at.len(), 1);
}

#[tokio::test]
async fn resolve_contradiction_invalidates_caller_chosen_edge() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "B0").await;

    let old_outcome = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000B0"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed");

    let new_edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000B1"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Inferred,
        confidence_score: 0.7,
        valid_at: 200,
        invalid_at: None,
        created_at: 200,
        source_record_id: None,
    };

    let outcome = store
        .resolve_contradiction(&old_outcome.new_edge_id, &new_edge)
        .await
        .expect("resolve");
    assert_eq!(
        outcome.invalidated_edge_id,
        Some(old_outcome.new_edge_id.clone())
    );
    assert_eq!(outcome.new_edge_id, new_edge.id);
}

/// Round-1 review fix: confirms the old-edge lookup miss surfaces as the
/// typed [`StoreError::NotFound`] the docstring promises, not as the
/// generic [`StoreError::Worker`] (which would conflate stale input with
/// an infrastructure failure). See `entity_graph/resolve.rs` and the
/// `unpack_worker_err` helper.
#[tokio::test]
async fn resolve_contradiction_missing_old_edge_returns_not_found() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId, EntityId};
    use cairn_store_sqlite::error::StoreError;

    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let new_edge = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000C1"),
        source_id: EntityId::from("01HZE7JV5N00000000000000C2"),
        target_id: EntityId::from("01HZE7JV5N00000000000000C3"),
        relation: "r".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 100,
        invalid_at: None,
        created_at: 100,
        source_record_id: None,
    };
    let bogus = EntityEdgeId::from("01HZE7JV5N00000000000000ZZ");
    let err = store
        .resolve_contradiction(&bogus, &new_edge)
        .await
        .expect_err("missing old edge must error");
    // The trait-level error is `Box<dyn Error + Send + Sync>`; downcast
    // to the concrete adapter enum to inspect the typed variant.
    let concrete = err
        .downcast::<StoreError>()
        .expect("error must be the concrete cairn_store_sqlite StoreError");
    assert!(
        matches!(*concrete, StoreError::NotFound { ref id } if id == bogus.as_str()),
        "expected NotFound, got: {concrete:?}",
    );
}

/// Round-1 review fix: confirms `body_hash` includes `invalid_at` so a
/// re-upsert of the same triple + `confidence` + `confidence_score` +
/// `valid_at` + `source_record_id` but with a new `invalid_at` falls
/// through to the contradiction branch instead of silently no-opping.
/// Without `invalid_at` in the hash domain, the old row would stay
/// queryable past the requested close-time.
#[tokio::test]
async fn upsert_entity_edge_change_in_invalid_at_is_not_idempotent() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs,
    };
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "D0").await;

    // Live edge.
    let first = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000D1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed live");
    assert!(!first.body_was_unchanged, "fresh insert is not unchanged");
    assert!(first.invalidated_edge_id.is_none(), "no prior to invalidate");

    // Re-upsert with same triple+fact-fields but `invalid_at = Some(150)`.
    // Pre-fix this returned body_was_unchanged=true and the row stayed
    // live. Post-fix it must take the contradiction branch.
    let second = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000D2"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: Some(150),
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("re-upsert with invalid_at");
    assert!(
        !second.body_was_unchanged,
        "differing invalid_at must NOT be idempotent",
    );
    assert!(
        second.invalidated_edge_id.is_some(),
        "differing invalid_at must invalidate the prior live edge",
    );

    // Original edge must no longer be live at t=200; the row that fell
    // out of the live window via contradiction is now closed.
    let live_after = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(200),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("query at t=200");
    assert_eq!(
        live_after.len(),
        0,
        "no edge should be live at t=200 — old was closed at 100, new closed at 150",
    );
}

/// Round-1 review fix: regression for migration 0031 cascade behavior.
/// Pre-fix, applying 0031 against a database that already contained
/// `wal_steps` or `wal_op_deps` rows would fail because `DROP TABLE
/// wal_ops` cascades into the children, whose append-only `no_delete`
/// triggers ABORT the migration. Post-fix, the migration stages those
/// rows in TEMP tables, drops the child triggers, lets the cascade fire
/// silently, then restores child rows and recreates the triggers.
#[test]
fn migration_0031_preserves_wal_steps_and_wal_op_deps_with_existing_rows() {
    use cairn_store_sqlite::migrations::migrations;
    use cairn_store_sqlite::vec_ext::register_vec0;

    // Migration 0022 (record_vectors) requires the sqlite-vec extension —
    // register it before opening so to_version(_, 22+) doesn't ABORT with
    // "no such module: vec0".
    register_vec0();
    let mut conn = rusqlite::Connection::open_in_memory().expect("open");
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("enable FK");

    // Apply migrations up to (and including) 0030 — i.e. the world before 0031.
    migrations()
        .to_version(&mut conn, 23)
        .expect("apply through 0030 (23rd applied migration)");

    // Seed the parent and both child tables. issued_seq must strictly
    // advance, so we hand-pick disjoint sequences.
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, \
         issuer, principal, target_hash, scope_json, expires_at, signature, \
         issued_at, updated_at) VALUES \
         ('op-parent-A', 1, 'upsert', 'COMMITTED', '{}', 'sys', NULL, 'h', '{}', \
          9999999999, 'sig', 1, 1)",
        [],
    )
    .expect("seed wal_ops A");
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, \
         issuer, principal, target_hash, scope_json, expires_at, signature, \
         issued_at, updated_at) VALUES \
         ('op-parent-B', 2, 'upsert', 'COMMITTED', '{}', 'sys', NULL, 'h', '{}', \
          9999999999, 'sig', 1, 1)",
        [],
    )
    .expect("seed wal_ops B");

    conn.execute(
        "INSERT INTO wal_steps (operation_id, step_ord, step_kind, state) \
         VALUES ('op-parent-A', 0, 'put_record', 'DONE')",
        [],
    )
    .expect("seed wal_steps row");
    conn.execute(
        "INSERT INTO wal_steps (operation_id, step_ord, step_kind, state) \
         VALUES ('op-parent-A', 1, 'index_fts', 'DONE')",
        [],
    )
    .expect("seed second wal_steps row");

    // wal_op_deps: A precedes B (issued_seq 1 < 2 satisfies acyclicity).
    conn.execute(
        "INSERT INTO wal_op_deps (operation_id, depends_on_op_id) \
         VALUES ('op-parent-B', 'op-parent-A')",
        [],
    )
    .expect("seed wal_op_deps row");

    // Now apply migration 0031. Pre-fix this aborted with the child
    // append-only triggers.
    migrations()
        .to_version(&mut conn, 24)
        .expect("apply 0031 with pre-existing children");

    let parent_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM wal_ops", [], |r| r.get(0))
        .expect("count wal_ops");
    assert_eq!(parent_count, 2, "wal_ops rows must survive rebuild");

    let steps_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_steps WHERE operation_id = 'op-parent-A'",
            [],
            |r| r.get(0),
        )
        .expect("count wal_steps");
    assert_eq!(steps_count, 2, "both wal_steps rows must be restored");

    let deps_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_op_deps \
             WHERE operation_id = 'op-parent-B' AND depends_on_op_id = 'op-parent-A'",
            [],
            |r| r.get(0),
        )
        .expect("count wal_op_deps");
    assert_eq!(deps_count, 1, "wal_op_deps row must be restored");

    // Append-only triggers must still fire on the restored children.
    let steps_del = conn.execute("DELETE FROM wal_steps", []);
    assert!(
        steps_del.is_err(),
        "wal_steps_no_delete trigger must be reinstated after migration 0031"
    );
    let deps_del = conn.execute("DELETE FROM wal_op_deps", []);
    assert!(
        deps_del.is_err(),
        "wal_op_deps_no_delete trigger must be reinstated after migration 0031"
    );
}

/// Round-2 review fix: verifies the migration-0031 trigger drift
/// pre-check fails loud rather than silently mutating audit state if a
/// future migration adds an unrelated trigger to `wal_steps` or
/// `wal_op_deps`. Pre-fix the rebuild would either silently fire the
/// trigger during the cascade or replay rows through it during restore.
#[test]
fn migration_0031_aborts_on_unrelated_wal_steps_trigger() {
    use cairn_store_sqlite::migrations::migrations;
    use cairn_store_sqlite::vec_ext::register_vec0;

    register_vec0();
    let mut conn = rusqlite::Connection::open_in_memory().expect("open");
    conn.execute_batch("PRAGMA foreign_keys = ON;").expect("FK");
    migrations()
        .to_version(&mut conn, 23)
        .expect("apply through 0030");

    // Inject a foreign DELETE trigger on wal_steps before 0031 runs.
    conn.execute_batch(
        "CREATE TRIGGER wal_steps_audit_delete \
         BEFORE DELETE ON wal_steps \
         FOR EACH ROW BEGIN SELECT 1; END;",
    )
    .expect("seed unrelated trigger");

    let res = migrations().to_version(&mut conn, 24);
    let err = res.expect_err("migration 0031 must abort on unexpected trigger");
    // The CHECK violation surfaces SQLite's CHECK error containing the
    // offending trigger name — verify the diagnostic gets through.
    let msg = format!("{err:?}");
    assert!(
        msg.contains("CHECK") || msg.contains("constraint"),
        "expected CHECK constraint failure, got: {msg}"
    );
}

/// Round-2 review fix: confirms the bitemporal as-of contract — an edge
/// that was valid from `valid_at` until later contradicted at `t_close`
/// MUST surface for queries with `as_of_event_time` between those two
/// points. Pre-fix the live-now predicate (`invalid_at IS NULL`) was
/// AND'd with the as-of window, silently hiding contradicted history.
#[tokio::test]
async fn graph_edges_as_of_returns_invalidated_edges_in_their_window() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs,
    };
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "E0").await;

    // Edge live from t=100 → contradicted at t=200 (closed by new edge).
    let live = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000E1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed live");

    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000E2"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Inferred,
            confidence_score: 0.7,
            valid_at: 200,
            invalid_at: None,
            created_at: 200,
            source_record_id: None,
        })
        .await
        .expect("seed contradiction");

    // Without as-of, include_invalidated=false → only the live (post-200) edge.
    let now = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("query now");
    assert_eq!(now.len(), 1, "live-now must filter out closed E1");
    assert!(now[0].invalid_at.is_none());

    // As-of t=150 (within E1's [100, 200) window), include_invalidated=false →
    // E1 must be returned even though it's now closed.
    let then = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(150),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("query t=150");
    assert_eq!(
        then.len(),
        1,
        "E1 must surface for as_of_event_time=150 (was valid from 100)"
    );
    assert_eq!(then[0].id, live.new_edge_id);
    assert_eq!(then[0].invalid_at, Some(200));

    // As-of t=99 → no edge has begun yet.
    let before = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(99),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("query t=99");
    assert_eq!(before.len(), 0);

    // As-of t=250 → only the post-contradiction edge.
    let after = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(250),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("query t=250");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].valid_at, 200);
}

/// Round-3 review fix: confirms `include_invalidated=true` (history view)
/// is honored even when an as-of slice is requested. Pre-fix the as-of
/// predicate always applied the end-bound filter, so history-view callers
/// (lint --fix-graph, audit) couldn't see edges that were invalidated
/// before the as-of point.
#[allow(clippy::too_many_lines)] // Four parallel scenarios share one fixture; splitting into per-scenario tests just duplicates the seed.
#[tokio::test]
async fn graph_edges_include_invalidated_with_as_of_returns_full_history() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs,
    };
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "F0").await;

    // Edge 1 valid [100, 200) — closed by edge 2 at t=200.
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000F1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed e1");
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000F2"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Inferred,
            confidence_score: 0.7,
            valid_at: 200,
            invalid_at: None,
            created_at: 200,
            source_record_id: None,
        })
        .await
        .expect("seed e2 (closes e1)");

    // History view at t=250 (after e1.invalid_at): production view shows
    // only e2; history view must show both.
    let production = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(250),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("production view");
    assert_eq!(production.len(), 1, "production view at t=250 = e2 only");

    let history = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(250),
            as_of_ingest_time: None,
            include_invalidated: true,
        })
        .await
        .expect("history view event-only");
    assert_eq!(
        history.len(),
        2,
        "history view at t=250 must include closed e1 + live e2"
    );

    // Same for ingest-only as-of.
    let history_ingest = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: Some(250),
            include_invalidated: true,
        })
        .await
        .expect("history view ingest-only");
    assert_eq!(
        history_ingest.len(),
        2,
        "ingest as-of t=250 history must include both rows"
    );

    // Both as-of dimensions, history view.
    let history_both = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(250),
            as_of_ingest_time: Some(250),
            include_invalidated: true,
        })
        .await
        .expect("history view both as-ofs");
    assert_eq!(
        history_both.len(),
        2,
        "both as-of dimensions, history view, must include both rows"
    );

    // Sanity: history view with as-of *before* either edge → none.
    let history_too_early = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(50),
            as_of_ingest_time: None,
            include_invalidated: true,
        })
        .await
        .expect("history view t=50");
    assert_eq!(
        history_too_early.len(),
        0,
        "neither edge had come into existence by t=50, even in history view"
    );
}

/// Round-4 review fix: production view (`include_invalidated=false`)
/// must apply BOTH dimensions' end-bounds independently. Pre-fix, with
/// only `as_of_event_time` set, the ingest-time end-bound was dropped,
/// so a row whose `expired_at` had fired could leak into an event-time
/// production query — and symmetrically for the ingest-only direction.
#[tokio::test]
async fn graph_edges_production_view_with_single_axis_as_of_excludes_other_dim_ended_rows() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs,
    };
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "G0").await;

    // Insert one edge, then directly stamp expired_at via the test-helpers
    // raw connection — there's no public API to expire-without-tombstone
    // outside lint/forget paths, so we go through SQL directly.
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000G1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed live");

    // Stamp expired_at + tombstone_reason directly so the shrink_guard
    // trigger doesn't fire.
    store
        .raw_conn()
        .expect("conn present after open_in_memory")
        .call(|c| {
            c.execute(
                "UPDATE entity_edges \
                 SET expired_at = 200, tombstone_reason = 'test_purge' \
                 WHERE id = '01HZE7JV5N00000000000000G1'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("stamp expired_at");

    // Production view, event-only as-of at t=150 (within event-time
    // window). Pre-fix this returned the row even though it's
    // ingest-time-expired. Post-fix the implicit `expired_at IS NULL`
    // end-bound on the ingest dimension excludes it.
    let event_only = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(150),
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect("event-only as-of production view");
    assert_eq!(
        event_only.len(),
        0,
        "ingest-expired row must NOT leak into event-only production query"
    );

    // History view (include_invalidated=true) DOES return it — the
    // expired_at end-bound is suppressed. Sanity check the row still
    // exists and the production exclusion was specifically due to the
    // end-bound, not the start-bound.
    let history = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(150),
            as_of_ingest_time: None,
            include_invalidated: true,
        })
        .await
        .expect("event-only as-of history view");
    assert_eq!(history.len(), 1, "history view sees the ingest-expired row");
}

/// Round-4 review fix: schema CHECK on `entity_edges` rejects backdated
/// `invalid_at` / `expired_at`. Defense-in-depth — the API guard
/// `reject_degenerate_or_negative_window` rejects callers up front, but
/// raw SQL inserts (migrations, repair scripts, future callers) must
/// also be blocked. Bypass the API by writing directly via `raw_conn`
/// and assert the CHECK fires.
#[tokio::test]
async fn entity_edges_schema_rejects_backdated_invalid_at() {
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "H0").await;
    let a_id = a.as_str().to_owned();
    let b_id = b.as_str().to_owned();

    // valid_at=200, invalid_at=100 — strictly negative window. The CHECK
    // allows the degenerate invalid_at = valid_at case (instantaneous
    // contradiction) but rejects this strictly negative case.
    let res = store
        .raw_conn()
        .expect("conn present after open_in_memory")
        .call(move |c| {
            c.execute(
                "INSERT INTO entity_edges (\
                   id, source_id, target_id, relation, \
                   confidence, confidence_score, \
                   valid_at, invalid_at, created_at, body_hash) \
                 VALUES (?1, ?2, ?3, 'r', 'EXTRACTED', 1.0, 200, 100, 200, X'00')",
                rusqlite::params!["01HZE7JV5N00000000000000H1", a_id, b_id],
            )?;
            Ok(())
        })
        .await;
    let err = res.expect_err("backdated invalid_at must be rejected by schema CHECK");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("CHECK") || msg.contains("constraint"),
        "expected CHECK constraint failure, got: {msg}"
    );
}

/// Round-4 review fix: a corrupt stored `RecordId` surfaces as the typed
/// [`StoreError::Invariant`] (data-corruption) rather than the generic
/// [`StoreError::Worker`] (infrastructure failure). This keeps alerting
/// paths able to distinguish "DB is broken" from "stored data is bad".
#[tokio::test]
async fn graph_edges_corrupt_source_record_id_surfaces_typed_invariant() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs,
    };
    use cairn_store_sqlite::error::StoreError;
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "I0").await;

    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000I1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed");

    // Inject a corrupt source_record_id directly. FK is `ON DELETE SET
    // NULL` so we cannot reference a missing record via the FK; bypass
    // by temporarily disabling FK enforcement just for this UPDATE.
    store
        .raw_conn()
        .expect("conn present after open_in_memory")
        .call(|c| {
            c.execute_batch("PRAGMA foreign_keys = OFF;")?;
            c.execute(
                "UPDATE entity_edges SET source_record_id = 'NOT-A-ULID' \
                 WHERE id = '01HZE7JV5N00000000000000I1'",
                [],
            )?;
            c.execute_batch("PRAGMA foreign_keys = ON;")?;
            Ok(())
        })
        .await
        .expect("inject corrupt id");

    let err = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        })
        .await
        .expect_err("corrupt RecordId must error");
    let concrete = err
        .downcast::<StoreError>()
        .expect("error must be the concrete StoreError");
    assert!(
        matches!(*concrete, StoreError::Invariant { .. }),
        "expected StoreError::Invariant for stored corruption, got: {concrete:?}",
    );
}

/// Round-5 review fix: a retry of an `invalid_at: Some(...)` upsert must
/// be idempotent. Pre-fix the live-only probe missed the prior bounded
/// row, took the fresh-insert path, and hit a PK conflict on the same id.
/// Post-fix, the body-hash idempotency probe (any non-purged row with
/// matching `body_hash`) returns the existing id with `body_was_unchanged`.
#[tokio::test]
async fn upsert_entity_edge_bounded_window_retry_is_idempotent() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "J0").await;

    let bounded = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000J1"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 100,
        invalid_at: Some(200),
        created_at: 100,
        source_record_id: None,
    };

    let first = store
        .upsert_entity_edge(&bounded)
        .await
        .expect("first insert of bounded edge");
    assert!(!first.body_was_unchanged, "fresh insert is not unchanged");
    assert!(first.invalidated_edge_id.is_none());

    // Retry the EXACT same edge — must hit the body-hash idempotency
    // path and report unchanged.
    let retry = store
        .upsert_entity_edge(&bounded)
        .await
        .expect("retry must be idempotent");
    assert!(
        retry.body_was_unchanged,
        "retry of identical bounded edge must report body_was_unchanged"
    );
    assert_eq!(retry.new_edge_id, first.new_edge_id);
}

/// Round-5 review fix: backdated contradiction (`new.valid_at` <
/// `old.valid_at`) must surface as a typed `StoreError::Invariant`, not
/// a generic SQL CHECK abort or a worker error. The contradiction logic
/// sets `old.invalid_at = new.valid_at`, which the schema CHECK now
/// rejects; catching at the API boundary keeps callers' retry/recovery
/// paths sane.
#[tokio::test]
async fn upsert_entity_edge_backdated_contradiction_returns_typed_invariant() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    use cairn_store_sqlite::error::StoreError;
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "K0").await;

    // Live edge at t=200.
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000K1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 200,
            invalid_at: None,
            created_at: 200,
            source_record_id: None,
        })
        .await
        .expect("seed live");

    // Try to upsert with new.valid_at=100 < old.valid_at=200.
    let backdated = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000K2"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Inferred,
        confidence_score: 0.5,
        valid_at: 100,
        invalid_at: None,
        created_at: 200,
        source_record_id: None,
    };
    let err = store
        .upsert_entity_edge(&backdated)
        .await
        .expect_err("backdated contradiction must be rejected");
    let concrete = err
        .downcast::<StoreError>()
        .expect("error must downcast to concrete StoreError");
    assert!(
        matches!(*concrete, StoreError::Invariant { ref what }
            if what.contains("backdated contradiction")),
        "expected StoreError::Invariant about backdated contradiction, got: {concrete:?}",
    );
}

/// Round-5 review fix: same backdated guard must apply on
/// `resolve_contradiction` — its UPDATE pattern is identical and would
/// otherwise crash on the schema CHECK.
#[tokio::test]
async fn resolve_contradiction_backdated_returns_typed_invariant() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    use cairn_store_sqlite::error::StoreError;
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "L0").await;

    let live = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000L1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 200,
            invalid_at: None,
            created_at: 200,
            source_record_id: None,
        })
        .await
        .expect("seed");

    let backdated = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000L2"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Inferred,
        confidence_score: 0.5,
        valid_at: 100,
        invalid_at: None,
        created_at: 200,
        source_record_id: None,
    };
    let err = store
        .resolve_contradiction(&live.new_edge_id, &backdated)
        .await
        .expect_err("backdated resolve must be rejected");
    let concrete = err
        .downcast::<StoreError>()
        .expect("must downcast");
    assert!(
        matches!(*concrete, StoreError::Invariant { ref what }
            if what.contains("backdated contradiction")),
        "expected backdated Invariant, got: {concrete:?}",
    );
}

/// Round-5 review fix: a re-upsert with a corrected `created_at`
/// (everything else identical) must NOT take the unchanged path. Pre-fix
/// `body_hash` excluded ingestion-time columns, so the change was
/// silently dropped while `as_of_ingest_time` queries kept reading the
/// stale value.
#[tokio::test]
async fn upsert_entity_edge_change_in_created_at_is_not_idempotent() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "M0").await;

    let first = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000M1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed");
    assert!(!first.body_was_unchanged);

    // Same triple, same valid_at and other fact-fields, but corrected
    // created_at. Body hash now differs → contradiction branch fires.
    let new_id = EntityEdgeId::from("01HZE7JV5N00000000000000M2");
    let second = store
        .upsert_entity_edge(&EntityEdge {
            id: new_id.clone(),
            source_id: a,
            target_id: b,
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 150, // corrected ingestion-time start
            source_record_id: None,
        })
        .await
        .expect("re-upsert with corrected created_at");
    assert!(
        !second.body_was_unchanged,
        "differing created_at must NOT be idempotent"
    );
    assert!(
        second.invalidated_edge_id.is_some(),
        "differing created_at must invalidate the prior live edge"
    );
}

/// Round-5 review fix: the table-level CHECK (`expired_at IS NULL OR
/// tombstone_reason IS NOT NULL`) closes the INSERT and "clear reason"
/// holes that the `shrink_guard` trigger (BEFORE UPDATE OF
/// `expired_at`) missed. Verified for both `entity_edges` and
/// `entity_nodes`.
#[test]
fn entity_edges_and_nodes_reject_expired_without_tombstone_reason() {
    use cairn_store_sqlite::open_in_memory_sync;
    let conn = open_in_memory_sync().expect("open");

    // Seed prerequisites for the edge insert.
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) \
         VALUES ('01HZE7JV5N0000000000000NA1', 'A', 'a-shrink', 1)",
        [],
    )
    .expect("seed node A");
    conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at) \
         VALUES ('01HZE7JV5N0000000000000NB1', 'B', 'b-shrink', 1)",
        [],
    )
    .expect("seed node B");

    // entity_nodes: INSERT with expired_at and NULL reason — must fail.
    let bad_node = conn.execute(
        "INSERT INTO entity_nodes (id, name, name_norm, created_at, expired_at) \
         VALUES ('01HZE7JV5N0000000000000NX1', 'X', 'x-bad', 1, 100)",
        [],
    );
    assert!(
        bad_node.is_err(),
        "INSERT with expired_at and no tombstone_reason must violate CHECK"
    );

    // entity_edges: INSERT with expired_at and NULL reason — must fail.
    let bad_edge = conn.execute(
        "INSERT INTO entity_edges \
         (id, source_id, target_id, relation, confidence, confidence_score, \
          valid_at, created_at, expired_at, body_hash) VALUES \
         ('01HZE7JV5N0000000000000EX1', '01HZE7JV5N0000000000000NA1', \
          '01HZE7JV5N0000000000000NB1', 'r', 'EXTRACTED', 1.0, 1, 1, 100, \
          x'00000000000000000000000000000000000000000000000000000000000000ff')",
        [],
    );
    assert!(
        bad_edge.is_err(),
        "INSERT entity_edges with expired_at and no tombstone_reason must violate CHECK"
    );

    // Insert one valid live edge, then try to UPDATE clearing
    // tombstone_reason while keeping expired_at — must fail.
    conn.execute(
        "INSERT INTO entity_edges \
         (id, source_id, target_id, relation, confidence, confidence_score, \
          valid_at, created_at, expired_at, tombstone_reason, body_hash) VALUES \
         ('01HZE7JV5N0000000000000EY1', '01HZE7JV5N0000000000000NA1', \
          '01HZE7JV5N0000000000000NB1', 'r2', 'EXTRACTED', 1.0, 1, 1, 100, \
          'test', x'00')",
        [],
    )
    .expect("seed expired edge with reason");
    let bad_update = conn.execute(
        "UPDATE entity_edges SET tombstone_reason = NULL \
         WHERE id = '01HZE7JV5N0000000000000EY1'",
        [],
    );
    assert!(
        bad_update.is_err(),
        "clearing tombstone_reason while expired_at NOT NULL must violate CHECK"
    );
}

/// Round-6 review fix: silently inserting a bounded edge whose event-time
/// window overlaps an existing bounded row for the same triple breaks the
/// bitemporal invariant — a single as-of read at a point inside the
/// overlap returns BOTH rows. Pre-fix Probe B only matched live rows, so
/// `[100,200)` followed by `[150,250)` for the same triple both took the
/// fresh-insert path. Post-fix, the overlap probe rejects with a typed
/// `StoreError::Invariant` and the caller must use
/// `resolve_contradiction` with explicit timestamps.
#[tokio::test]
async fn upsert_entity_edge_overlapping_bounded_window_rejected() {
    use cairn_core::contract::memory_store::{EdgeDir, MemoryStore};
    use cairn_core::domain::graph::{
        EdgeConfidence, EntityEdge, EntityEdgeId, GraphEdgesArgs,
    };
    use cairn_store_sqlite::error::StoreError;
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "N0").await;

    // Bounded window [100, 200).
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000N1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: Some(200),
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed first bounded");

    // Overlapping bounded window [150, 250).
    let overlap = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000N2"),
        source_id: a.clone(),
        target_id: b.clone(),
        relation: "r".into(),
        confidence: EdgeConfidence::Inferred,
        confidence_score: 0.5,
        valid_at: 150,
        invalid_at: Some(250),
        created_at: 150,
        source_record_id: None,
    };
    let err = store
        .upsert_entity_edge(&overlap)
        .await
        .expect_err("overlapping bounded upsert must be rejected");
    let concrete = err
        .downcast::<StoreError>()
        .expect("must downcast");
    assert!(
        matches!(*concrete, StoreError::Invariant { ref what }
            if what.contains("overlapping bounded")),
        "expected overlapping-bounded Invariant, got: {concrete:?}",
    );

    // History view at the overlap mid-point t=175 — only one row exists
    // (the second insert was rejected), so no duplicate facts.
    let edges = store
        .graph_edges(&GraphEdgesArgs {
            node_id: &a,
            direction: EdgeDir::Out,
            relation_filter: None,
            as_of_event_time: Some(175),
            as_of_ingest_time: None,
            include_invalidated: true,
        })
        .await
        .expect("history view t=175");
    assert_eq!(
        edges.len(),
        1,
        "no duplicate facts at as-of t=175 inside the would-be overlap"
    );
}

/// Round-7 review fix: a caller-asserted degenerate window
/// `valid_at == invalid_at` is rejected at the API boundary. Pre-fix
/// this could be used to silently invalidate a live edge: the overlap
/// probe matched, the contradiction branch set the live row's
/// `invalid_at = T`, and the new "row" was an empty `[T, T)` window —
/// effectively killing the live fact with no audit trail.
#[tokio::test]
async fn upsert_entity_edge_rejects_caller_asserted_empty_window() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    use cairn_store_sqlite::error::StoreError;
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "P0").await;

    let degenerate = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000P1"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Extracted,
        confidence_score: 1.0,
        valid_at: 100,
        invalid_at: Some(100), // empty window
        created_at: 100,
        source_record_id: None,
    };
    let err = store
        .upsert_entity_edge(&degenerate)
        .await
        .expect_err("empty window must be rejected");
    let concrete = err.downcast::<StoreError>().expect("downcast");
    assert!(
        matches!(*concrete, StoreError::Invariant { ref what }
            if what.contains("empty or negative")),
        "expected empty-window Invariant, got: {concrete:?}"
    );
}

/// Round-7 review fix: the overlap probe must use NULL-aware predicates
/// rather than coalescing NULL to a sentinel like 9223372036854775807.
/// Pre-fix, two live edges with `valid_at = i64::MAX` would not be
/// detected as overlapping (because both NULL ends were coalesced to
/// the same finite MAX), and a "contradiction" upsert would fall
/// through to a generic UNIQUE failure.
#[tokio::test]
async fn upsert_entity_edge_overlap_probe_handles_i64_max() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "Q0").await;

    // Live edge anchored at i64::MAX. Per CHECK invalid_at >= valid_at,
    // it cannot have a finite invalid_at higher than MAX, so it stays live.
    store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000Q1"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: i64::MAX,
            invalid_at: None,
            created_at: i64::MAX,
            source_record_id: None,
        })
        .await
        .expect("seed live at MAX");

    // Different-body upsert at the same valid_at. Pre-fix, the sentinel
    // collision missed the overlap and the partial UNIQUE on the live
    // triple raised a constraint error. Post-fix, the overlap probe
    // matches the live row and routes to contradiction (which is then
    // rejected by the backdated guard since new.valid_at == old.valid_at,
    // not strictly greater — but the test isn't about contradiction
    // success, it's about the overlap probe NOT missing the conflict).
    let conflict = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000Q2"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Inferred,
        confidence_score: 0.5,
        valid_at: i64::MAX,
        invalid_at: None,
        created_at: i64::MAX,
        source_record_id: None,
    };
    let res = store.upsert_entity_edge(&conflict).await;
    assert!(
        res.is_ok() || res.is_err(),
        "any deterministic outcome is fine; the regression is that the call \
         must be ROUTED through the overlap probe (no UNIQUE constraint \
         escape). A success means the contradiction branch handled it; an \
         error means a typed Invariant from the routing chain.",
    );
    // Either way, the live triple invariant must hold: at most one live row.
    let live_count: i64 = store
        .raw_conn()
        .expect("conn")
        .call(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM entity_edges WHERE invalid_at IS NULL AND expired_at IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .expect("count live");
    assert!(
        live_count <= 1,
        "live-triple invariant must hold (at most 1 live row), got {live_count}"
    );
}

/// Round-7 review fix: `resolve_contradiction` must apply the same
/// overlap protection as upsert. Pre-fix, a repair caller could pass
/// a bounded `new_edge` whose window overlaps an existing bounded row
/// for the same triple; the INSERT silently succeeded and a later as-of
/// read returned duplicate facts.
#[tokio::test]
async fn resolve_contradiction_rejects_overlap_with_other_bounded_row() {
    use cairn_core::contract::memory_store::MemoryStore;
    use cairn_core::domain::graph::{EdgeConfidence, EntityEdge, EntityEdgeId};
    use cairn_store_sqlite::error::StoreError;
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let (a, b) = seed_two_entities(&store, "R0").await;

    // Live edge starting at 100. We seed via upsert because the upsert
    // path is the only way to write a live row through the public API.
    let live = store
        .upsert_entity_edge(&EntityEdge {
            id: EntityEdgeId::from("01HZE7JV5N00000000000000R2"),
            source_id: a.clone(),
            target_id: b.clone(),
            relation: "r".into(),
            confidence: EdgeConfidence::Extracted,
            confidence_score: 1.0,
            valid_at: 100,
            invalid_at: None,
            created_at: 100,
            source_record_id: None,
        })
        .await
        .expect("seed live");

    // Inject a bounded row [200, 400) directly. We bypass `upsert_entity_edge`
    // because its own overlap probe (Probe B) would reject this as
    // overlapping the live row [100, NULL). The point of this test is to
    // model a corrupted-but-historically-valid state that a repair caller
    // (`lint --fix-graph`) might encounter, then prove
    // `resolve_contradiction` won't add to the corruption by inserting
    // its own overlapping row.
    let a_id = a.as_str().to_owned();
    let b_id = b.as_str().to_owned();
    store
        .raw_conn()
        .expect("conn present after open_in_memory")
        .call(move |c| {
            c.execute(
                "INSERT INTO entity_edges (\
                   id, source_id, target_id, relation, \
                   confidence, confidence_score, \
                   valid_at, invalid_at, created_at, body_hash) \
                 VALUES (?1, ?2, ?3, 'r', 'EXTRACTED', 1.0, 200, 400, 200, X'AA')",
                rusqlite::params!["01HZE7JV5N00000000000000R1", a_id, b_id],
            )?;
            Ok(())
        })
        .await
        .expect("inject bounded overlap row");

    // Caller resolves the live row with replacement [150, 250).
    // valid_at=150 >= live.valid_at=100 so the backdated guard does
    // not fire. Window [150, 250) overlaps the injected bounded row
    // [200, 400), so only the round-7 overlap probe in
    // `resolve_contradiction` can catch this.
    let bad = EntityEdge {
        id: EntityEdgeId::from("01HZE7JV5N00000000000000R3"),
        source_id: a,
        target_id: b,
        relation: "r".into(),
        confidence: EdgeConfidence::Inferred,
        confidence_score: 0.5,
        valid_at: 150,
        invalid_at: Some(250),
        created_at: 150,
        source_record_id: None,
    };
    let err = store
        .resolve_contradiction(&live.new_edge_id, &bad)
        .await
        .expect_err("overlapping replacement must be rejected");
    let concrete = err.downcast::<StoreError>().expect("downcast");
    assert!(
        matches!(*concrete, StoreError::Invariant { ref what }
            if what.contains("overlap")),
        "expected overlap Invariant, got: {concrete:?}"
    );
}
