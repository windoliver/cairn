//! Graph test fixtures for `cairn-store-sqlite` integration tests.
//!
//! [`tiny_graph`] builds a minimal in-memory entity graph used by Tasks 3–10
//! of issue #190. It inserts four entity nodes, two scopes, two records, and
//! two edges so that every graph-query test can share a single fixture instead
//! of re-inserting the same rows.

use std::sync::Arc;

use cairn_core::domain::graph::{EntityId, EntityNode, normalize_entity_name};
use cairn_core::domain::scope::ScopeTuple;
use cairn_store_sqlite::SqliteMemoryStore;

// Raw SQL inserts need serde_json for serializing scope tuples.
use serde_json;

/// Fixed epoch used in every fixture row. All `valid_at`, `created_at`,
/// and `updated_at` timestamps are ≤ this value; `invalid_at` / `expired_at`
/// are `NULL` so they are live at this instant.
pub const FIXTURE_NOW: i64 = 1_700_000_000_000;

/// Result of [`timeline_fixture`]. All fields are `pub` so tests can reference
/// them directly without cloning.
pub struct TimelineGraphFixture {
    /// Connected in-memory store with all fixture rows inserted.
    pub store: Arc<SqliteMemoryStore>,
    /// The `now` timestamp to pass to [`cairn_store_sqlite::entity_graph::queries::GraphQueries::new`].
    pub now: i64,
    /// Scope that owns `node_a` and all three edges.
    pub scope_a: ScopeTuple,
    /// `EntityId` of the anchor node.
    pub node_a: String,
}

/// Build a timeline-focused in-memory entity graph.
///
/// Three edges are anchored on `node_a`:
/// - `edge-tl-active`:  `valid_at` < `now`, `invalid_at` NULL, `expired_at` NULL → active.
/// - `edge-tl-future`:  `valid_at` > `now` → future-dated.
/// - `edge-tl-expired`: `expired_at` NOT NULL, `tombstone_reason` set → expired.
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn timeline_fixture() -> TimelineGraphFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("timeline_fixture: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("tl-acme".to_owned()),
        ..ScopeTuple::default()
    };

    let scope_json = serde_json::to_string(&scope_a).expect("scope_a json");

    // Distinct stable IDs for nodes and edges.
    let id_node_a  = "01HZE7TL0000000000000000A1";
    let id_node_b  = "01HZE7TL0000000000000000B1";

    let now = FIXTURE_NOW;
    let past = now - 10_000;
    let future = now + 10_000;

    // Insert entity nodes.
    let nodes: &[(&str, &str)] = &[
        (id_node_a, "tl-alpha"),
        (id_node_b, "tl-beta"),
    ];
    for (id, name) in nodes {
        let node = EntityNode {
            id: EntityId::from(*id),
            name: (*name).to_owned(),
            name_norm: normalize_entity_name(name),
            summary: None,
            created_at: now,
            embedding_id: None,
        };
        store.upsert_entity(&node).await.expect("upsert_entity");
    }

    {
        let conn = Arc::clone(store.raw_conn().expect("raw_conn present"));

        let records_sql = format!(
            "INSERT INTO records \
             (record_id, target_id, version, path, kind, class, visibility, \
              scope, actor_chain, body, body_hash, created_at, updated_at, \
              active, tombstoned) \
             VALUES \
             ('rec-tl-scope-a', 'tgt-tl-scope-a', 1, 'p', 'fact', 'episodic', 'private', \
              '{scope_json}', '[]', '', 'h', {now}, {now}, 1, 0);"
        );

        // active: valid_at=past, invalid_at NULL, expired_at NULL
        // future: valid_at=future connecting the same two visible nodes (both
        //         reachable via the active edge, so visible_nodes includes them)
        // expired: valid_at=past, invalid_at NULL, expired_at=past, tombstone_reason set
        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id, \
              invalid_at, expired_at, tombstone_reason) \
             VALUES \
             ('edge-tl-active',  '{id_node_a}', '{id_node_b}', 'relates', 'EXTRACTED', 0.8, \
              {past}, {past}, X'A1', 'rec-tl-scope-a', NULL, NULL, NULL), \
             ('edge-tl-future',  '{id_node_b}', '{id_node_a}', 'relates', 'EXTRACTED', 0.7, \
              {future}, {past}, X'A2', 'rec-tl-scope-a', NULL, NULL, NULL), \
             ('edge-tl-expired', '{id_node_a}', '{id_node_b}', 'relates', 'EXTRACTED', 0.5, \
              {past}, {past}, X'A3', 'rec-tl-scope-a', NULL, {past}, 'test-tombstone');"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            Ok(())
        })
        .await
        .expect("timeline_fixture: seed SQL");
    }

    TimelineGraphFixture {
        store,
        now,
        scope_a,
        node_a: id_node_a.to_owned(),
    }
}

/// Result of [`tiny_graph`]. All fields are `pub` so tests can reference them
/// directly without cloning.
pub struct TinyGraphFixture {
    /// Connected in-memory store with all fixture rows inserted.
    pub store: Arc<SqliteMemoryStore>,
    /// The `now` timestamp to pass to [`cairn_store_sqlite::entity_graph::queries::GraphQueries::new`].
    pub now: i64,
    /// Scope that owns `node_a`, `node_b`, `node_c`, `node_auth_service`,
    /// and both edges.
    pub scope_a: ScopeTuple,
    /// A second, disjoint scope. No nodes or edges are exclusively owned
    /// under this scope, so queries scoped to `scope_b` should see nothing.
    pub scope_b: ScopeTuple,
    /// `EntityId` of the `"alpha"` node.
    pub node_a: String,
    /// `EntityId` of the `"beta"` node.
    pub node_b: String,
    /// `EntityId` of the `"gamma"` node.
    pub node_c: String,
    /// `EntityId` of the `"Auth Service (v2)"` node.
    pub node_auth_service: String,
}

/// Build and return a tiny in-memory entity graph.
///
/// Schema after return:
///
/// ```text
/// entity_nodes:
///   01HZE7JV5N0000000000000001  alpha            (scope_a)
///   01HZE7JV5N0000000000000002  beta             (scope_a)
///   01HZE7JV5N0000000000000003  gamma            (scope_a)
///   01HZE7JV5N0000000000000004  Auth Service (v2) (scope_a)
///
/// records:
///   rec-scope-a  (target_id=tgt-scope-a, active=1, tombstoned=0, scope=scope_a JSON)
///   rec-scope-b  (target_id=tgt-scope-b, active=1, tombstoned=0, scope=scope_b JSON)
///
/// entity_edges:
///   edge-ab  alpha → beta   calls  confidence=0.9  source_record_id=rec-scope-a
///   edge-ac  alpha → gamma  calls  confidence=0.6  source_record_id=rec-scope-a
/// ```
///
/// `scope_a = { tenant: "acme" }`, `scope_b = { tenant: "other" }`.
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn tiny_graph() -> TinyGraphFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("tiny_graph: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_b = ScopeTuple {
        tenant: Some("other".to_owned()),
        ..ScopeTuple::default()
    };

    // Serialize scopes to JSON strings for raw SQL inserts.
    let primary_scope_json = serde_json::to_string(&scope_a).expect("scope_a json");
    let secondary_scope_json = serde_json::to_string(&scope_b).expect("scope_b json");

    // Node IDs (stable ULID-format strings, no real randomness needed).
    let id_alpha = "01HZE7JV5N0000000000000001";
    let id_beta = "01HZE7JV5N0000000000000002";
    let id_gamma = "01HZE7JV5N0000000000000003";
    let id_auth = "01HZE7JV5N0000000000000004";

    // Insert entity nodes via the MemoryStore trait (same path as production code).
    let nodes: &[(&str, &str)] = &[
        (id_alpha, "alpha"),
        (id_beta, "beta"),
        (id_gamma, "gamma"),
        (id_auth, "Auth Service (v2)"),
    ];
    for (id, name) in nodes {
        let node = EntityNode {
            id: EntityId::from(*id),
            name: (*name).to_owned(),
            name_norm: normalize_entity_name(name),
            summary: None,
            created_at: FIXTURE_NOW,
            embedding_id: None,
        };
        store.upsert_entity(&node).await.expect("upsert_entity");
    }

    // Insert records and edges via raw SQL so we can control scope, active,
    // tombstoned, source_record_id, and confidence_score exactly.
    {
        // raw_conn() is gated by cfg(any(test, feature = "test-helpers"));
        // this crate enables that feature on cairn-store-sqlite.
        let conn = Arc::clone(store.raw_conn().expect("raw_conn present"));

        // Build the two INSERT statements before entering the blocking closure
        // so we can move owned Strings in without cloning in the hot path.
        let records_sql = format!(
            "INSERT INTO records \
             (record_id, target_id, version, path, kind, class, visibility, \
              scope, actor_chain, body, body_hash, created_at, updated_at, \
              active, tombstoned) \
             VALUES \
             ('rec-scope-a', 'tgt-scope-a', 1, 'p', 'fact', 'episodic', 'private', \
              '{primary_scope_json}', '[]', '', 'h', {FIXTURE_NOW}, {FIXTURE_NOW}, 1, 0), \
             ('rec-scope-b', 'tgt-scope-b', 1, 'p', 'fact', 'episodic', 'private', \
              '{secondary_scope_json}', '[]', '', 'h', {FIXTURE_NOW}, {FIXTURE_NOW}, 1, 0);"
        );
        // Two edges in scope_a: alpha→beta (calls, 0.9) and alpha→gamma (calls, 0.6).
        // entity_edges columns: id, source_id, target_id, relation,
        //   confidence, confidence_score, valid_at, created_at, body_hash,
        //   source_record_id, [invalid_at NULL, expired_at NULL]
        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id) \
             VALUES \
             ('edge-ab', '{id_alpha}', '{id_beta}', 'calls', 'EXTRACTED', 0.9, \
              {FIXTURE_NOW}, {FIXTURE_NOW}, X'01', 'rec-scope-a'), \
             ('edge-ac', '{id_alpha}', '{id_gamma}', 'calls', 'EXTRACTED', 0.6, \
              {FIXTURE_NOW}, {FIXTURE_NOW}, X'02', 'rec-scope-a');"
        );
        // Auth Service (v2) has no edges; link it via entity_episodes so it
        // appears in visible_nodes under scope_a (episode arm of the CTE).
        let episodes_sql = format!(
            "INSERT INTO entity_episodes (episode_id, entity_node_id, linked_at) \
             VALUES ('rec-scope-a', '{id_auth}', {FIXTURE_NOW});"
        );

        conn.call(move |c| {
            // Two records — one per scope. The records table requires:
            //   NOT NULL: record_id, target_id, version, path, kind, class,
            //             visibility, scope, actor_chain, body, body_hash,
            //             created_at, updated_at
            // `active` defaults to 0 per the migration; supply 1 explicitly so
            // the CTE EXISTS predicate fires.
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            c.execute_batch(&episodes_sql)?;
            Ok(())
        })
        .await
        .expect("tiny_graph: seed SQL");
    }

    TinyGraphFixture {
        store,
        now: FIXTURE_NOW,
        scope_a,
        scope_b,
        node_a: id_alpha.to_owned(),
        node_b: id_beta.to_owned(),
        node_c: id_gamma.to_owned(),
        node_auth_service: id_auth.to_owned(),
    }
}
