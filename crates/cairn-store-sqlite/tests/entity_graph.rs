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
