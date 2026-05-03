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
