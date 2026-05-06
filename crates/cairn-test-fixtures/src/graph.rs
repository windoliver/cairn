//! Graph test fixtures for `cairn-store-sqlite` integration tests.
//!
//! [`tiny_graph`] builds a minimal in-memory entity graph used by Tasks 3–10
//! of issue #190. It inserts four entity nodes, two scopes, two records, and
//! two edges so that every graph-query test can share a single fixture instead
//! of re-inserting the same rows.
//!
//! Task 21 adds five adversarial fixtures:
//! - [`endpoint_tombstone_fixture`] — node B is tombstoned; edges to B hidden.
//! - [`lineage_rescope_fixture`]    — immutable provenance: scope_a sees edge,
//!   scope_b (active head) does not.
//! - [`future_provenance_fixture`]  — edge only valid in the future.
//! - [`episode_tombstone_fixture`]  — entity reachable only via tombstoned record.
//! - [`scope_user_fixture`]         — user-scoped record; `user=None` must not match.

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

/// Result of [`surprise_fixture`]. All fields are `pub` so tests can reference
/// them directly without cloning.
pub struct SurpriseFixture {
    /// Connected in-memory store with all fixture rows inserted.
    pub store: Arc<SqliteMemoryStore>,
    /// The `now` timestamp.
    pub now: i64,
    /// Primary scope — owns 3 edges.
    pub scope_a: ScopeTuple,
    /// Secondary scope — owns 1 cross-scope edge (the outlier).
    pub scope_b: ScopeTuple,
    /// `EntityId` of node A.
    pub node_a: String,
    /// `EntityId` of node B.
    pub node_b: String,
    /// `EntityId` of node C.
    pub node_c: String,
    /// Record id whose `scope` JSON matches `scope_b` — the cross-scope edge
    /// is authored against this record.
    pub rec_b: String,
}

/// Build a surprise-connections in-memory entity graph.
///
/// Schema after return:
///
/// ```text
/// 3 edges in scope_a: node_a→node_b, node_b→node_c, node_a→node_c
/// 1 edge  in scope_b: node_b→node_c  (the cross-scope outlier)
/// Both scopes are in the caller's allowed_scopes so all edges are visible.
/// Modal scope is scope_a (3 edges vs 1); the scope_b edge scores higher.
/// ```
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn surprise_fixture() -> SurpriseFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("surprise_fixture: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("sp-acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_b = ScopeTuple {
        tenant: Some("sp-other".to_owned()),
        ..ScopeTuple::default()
    };

    let primary_scope_json = serde_json::to_string(&scope_a).expect("scope_a json");
    let secondary_scope_json = serde_json::to_string(&scope_b).expect("scope_b json");

    let id_node_a = "01HZE7SP0000000000000000A1";
    let id_node_b = "01HZE7SP0000000000000000B1";
    let id_node_c = "01HZE7SP0000000000000000C1";

    let now = FIXTURE_NOW;

    let nodes: &[(&str, &str)] = &[
        (id_node_a, "sp-alpha"),
        (id_node_b, "sp-beta"),
        (id_node_c, "sp-gamma"),
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

    let rec_b_id = "rec-sp-scope-b";

    {
        let conn = Arc::clone(store.raw_conn().expect("raw_conn present"));

        let records_sql = format!(
            "INSERT INTO records \
             (record_id, target_id, version, path, kind, class, visibility, \
              scope, actor_chain, body, body_hash, created_at, updated_at, \
              active, tombstoned) \
             VALUES \
             ('rec-sp-scope-a', 'tgt-sp-scope-a', 1, 'p', 'fact', 'episodic', 'private', \
              '{primary_scope_json}', '[]', '', 'h', {now}, {now}, 1, 0), \
             ('{rec_b_id}', 'tgt-sp-scope-b', 1, 'p', 'fact', 'episodic', 'private', \
              '{secondary_scope_json}', '[]', '', 'h', {now}, {now}, 1, 0);"
        );

        // 3 edges in scope_a, 1 cross-scope edge in scope_b (node_b→node_c).
        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id) \
             VALUES \
             ('edge-sp-ab', '{id_node_a}', '{id_node_b}', 'relates', 'EXTRACTED', 0.5, \
              {now}, {now}, X'B1', 'rec-sp-scope-a'), \
             ('edge-sp-bc', '{id_node_b}', '{id_node_c}', 'relates', 'EXTRACTED', 0.5, \
              {now}, {now}, X'B2', 'rec-sp-scope-a'), \
             ('edge-sp-ac', '{id_node_a}', '{id_node_c}', 'relates', 'EXTRACTED', 0.5, \
              {now}, {now}, X'B3', 'rec-sp-scope-a'), \
             ('edge-sp-bc-outlier', '{id_node_b}', '{id_node_c}', 'calls', 'EXTRACTED', 0.5, \
              {now}, {now}, X'B4', '{rec_b_id}');"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            Ok(())
        })
        .await
        .expect("surprise_fixture: seed SQL");
    }

    SurpriseFixture {
        store,
        now,
        scope_a,
        scope_b,
        node_a: id_node_a.to_owned(),
        node_b: id_node_b.to_owned(),
        node_c: id_node_c.to_owned(),
        rec_b: rec_b_id.to_owned(),
    }
}

// ── Task 21: adversarial cross-tool fixtures ──────────────────────────────────

/// Result of [`endpoint_tombstone_fixture`].
pub struct EndpointTombstoneFixture {
    /// In-memory store with all rows inserted.
    pub store: Arc<SqliteMemoryStore>,
    /// `now` timestamp.
    pub now: i64,
    /// Scope that owns the edge.
    pub scope_a: ScopeTuple,
    /// Live node A — source of the edge.
    pub node_a: String,
    /// Tombstoned node B — target of the edge; must be hidden from all tools.
    pub tombstoned_b: String,
}

/// Fixture: node B has `expired_at` set (entity-level tombstone).
///
/// One edge connects A→B under scope_a, but B's `expired_at` is NOT NULL.
/// `visible_nodes` filters out expired nodes, so no edge involving B must
/// appear in any tool's output.
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn endpoint_tombstone_fixture() -> EndpointTombstoneFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("endpoint_tombstone_fixture: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("et-acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_json = serde_json::to_string(&scope_a).expect("scope json");

    let id_node_a = "01HZET0000000000000000A001";
    let id_node_b = "01HZET0000000000000000B001";

    let now = FIXTURE_NOW;

    // Insert node A (live) via MemoryStore trait.
    for (id, name) in &[(id_node_a, "et-alpha"), (id_node_b, "et-beta")] {
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

        // Tombstone node B: set expired_at + tombstone_reason (CHECK constraint
        // requires both or neither).
        let tombstone_sql = format!(
            "UPDATE entity_nodes \
             SET expired_at = {now}, tombstone_reason = 'test-tombstone' \
             WHERE id = '{id_node_b}';"
        );

        let records_sql = format!(
            "INSERT INTO records \
             (record_id, target_id, version, path, kind, class, visibility, \
              scope, actor_chain, body, body_hash, created_at, updated_at, \
              active, tombstoned) \
             VALUES \
             ('rec-et-a', 'tgt-et-a', 1, 'p', 'fact', 'episodic', 'private', \
              '{scope_json}', '[]', '', 'h', {now}, {now}, 1, 0);"
        );

        // Edge A→B in scope_a; node B is tombstoned so the edge must be hidden.
        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id) \
             VALUES \
             ('edge-et-ab', '{id_node_a}', '{id_node_b}', 'calls', 'EXTRACTED', 0.9, \
              {now}, {now}, X'E1', 'rec-et-a');"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            c.execute_batch(&tombstone_sql)?;
            Ok(())
        })
        .await
        .expect("endpoint_tombstone_fixture: seed SQL");
    }

    EndpointTombstoneFixture {
        store,
        now,
        scope_a,
        node_a: id_node_a.to_owned(),
        tombstoned_b: id_node_b.to_owned(),
    }
}

/// Result of [`lineage_rescope_fixture`].
pub struct LineageRescopeFixture {
    /// In-memory store.
    pub store: Arc<SqliteMemoryStore>,
    /// `now` timestamp.
    pub now: i64,
    /// Original record scope; source_record_id of the edge.
    pub scope_a: ScopeTuple,
    /// Active-head scope (promoted record); NOT the edge provenance.
    pub scope_b: ScopeTuple,
    /// Node A — edge source.
    pub node_a: String,
    /// Node B — edge target.
    pub node_b: String,
}

/// Fixture: immutable provenance — scope_a sees the edge, scope_b does not.
///
/// `r_orig` has `scope=scope_a, active=0`; the edge's `source_record_id`
/// points to `r_orig`.  `r_new` has `scope=scope_b, active=1, tombstoned=0`
/// and shares `target_id` with `r_orig`.
///
/// scope_a query: r_src = r_orig (scope_a ✓) → r_active = r_new (active=1) → edge visible.
/// scope_b query: r_src = r_orig (scope != scope_b) → no match → edge hidden.
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn lineage_rescope_fixture() -> LineageRescopeFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("lineage_rescope_fixture: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("lr-acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_b = ScopeTuple {
        tenant: Some("lr-other".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_a_json = serde_json::to_string(&scope_a).expect("scope_a json");
    let scope_b_json = serde_json::to_string(&scope_b).expect("scope_b json");

    let id_node_a = "01HZLR0000000000000000A001";
    let id_node_b = "01HZLR0000000000000000B001";
    let now = FIXTURE_NOW;

    for (id, name) in &[(id_node_a, "lr-alpha"), (id_node_b, "lr-beta")] {
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

        // r_orig: scope_a, active=0 — the immutable provenance record.
        // r_new:  scope_b, active=1 — the promoted active head, same target_id.
        let records_sql = format!(
            "INSERT INTO records \
             (record_id, target_id, version, path, kind, class, visibility, \
              scope, actor_chain, body, body_hash, created_at, updated_at, \
              active, tombstoned) \
             VALUES \
             ('rec-lr-orig', 'tgt-lr-shared', 1, 'p', 'fact', 'episodic', 'private', \
              '{scope_a_json}', '[]', '', 'h', {now}, {now}, 0, 0), \
             ('rec-lr-new',  'tgt-lr-shared', 2, 'p', 'fact', 'episodic', 'private', \
              '{scope_b_json}', '[]', '', 'h', {now}, {now}, 1, 0);"
        );

        // Edge sourced from r_orig (scope_a, active=0).
        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id) \
             VALUES \
             ('edge-lr-ab', '{id_node_a}', '{id_node_b}', 'relates', 'EXTRACTED', 0.8, \
              {now}, {now}, X'F1', 'rec-lr-orig');"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            Ok(())
        })
        .await
        .expect("lineage_rescope_fixture: seed SQL");
    }

    LineageRescopeFixture {
        store,
        now,
        scope_a,
        scope_b,
        node_a: id_node_a.to_owned(),
        node_b: id_node_b.to_owned(),
    }
}

/// Result of [`future_provenance_fixture`].
pub struct FutureProvenanceFixture {
    /// In-memory store.
    pub store: Arc<SqliteMemoryStore>,
    /// `now` timestamp; the edge is NOT visible at this instant.
    pub now: i64,
    /// Scope that owns the future edge.
    pub scope_a: ScopeTuple,
    /// Node whose only edge has `valid_at > now`.
    pub node_future: String,
    /// Another node connected via the future edge.
    pub node_anchor: String,
    /// `valid_at` of the future edge; visible only at `now >= future_valid_at`.
    pub future_valid_at: i64,
}

/// Fixture: an edge whose `valid_at` is in the future relative to `now`.
///
/// At `now` the edge is not in `visible_edges_raw` (the CTE requires
/// `valid_at <= now`).  Advancing `now` to `future_valid_at + 1` makes it
/// visible.
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn future_provenance_fixture() -> FutureProvenanceFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("future_provenance_fixture: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("fp-acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_json = serde_json::to_string(&scope_a).expect("scope json");

    let id_node_anchor = "01HZFP0000000000000000A001";
    let id_node_future = "01HZFP0000000000000000F001";
    let now = FIXTURE_NOW;
    let future_valid_at = now + 86_400_000; // +1 day

    for (id, name) in &[
        (id_node_anchor, "fp-anchor"),
        (id_node_future, "fp-future"),
    ] {
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
             ('rec-fp-a', 'tgt-fp-a', 1, 'p', 'fact', 'episodic', 'private', \
              '{scope_json}', '[]', '', 'h', {now}, {now}, 1, 0);"
        );

        // Edge with valid_at = future_valid_at (not visible at `now`).
        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id) \
             VALUES \
             ('edge-fp-af', '{id_node_anchor}', '{id_node_future}', 'relates', \
              'EXTRACTED', 0.9, {future_valid_at}, {now}, X'C1', 'rec-fp-a');"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            Ok(())
        })
        .await
        .expect("future_provenance_fixture: seed SQL");
    }

    FutureProvenanceFixture {
        store,
        now,
        scope_a,
        node_future: id_node_future.to_owned(),
        node_anchor: id_node_anchor.to_owned(),
        future_valid_at,
    }
}

/// Result of [`episode_tombstone_fixture`].
pub struct EpisodeTombstoneFixture {
    /// In-memory store.
    pub store: Arc<SqliteMemoryStore>,
    /// `now` timestamp.
    pub now: i64,
    /// Scope in which we query.
    pub scope_a: ScopeTuple,
    /// Node reachable only via a tombstoned episode record; must be hidden.
    pub node_via_episode: String,
}

/// Fixture: a node linked only via an `entity_episodes` row whose
/// `episode_id` points to a tombstoned record.
///
/// The `visible_nodes` episode arm requires `r_active.tombstoned = 0`.
/// With the record tombstoned the node must not appear.
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn episode_tombstone_fixture() -> EpisodeTombstoneFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("episode_tombstone_fixture: open_in_memory");
    let store = Arc::new(store);

    let scope_a = ScopeTuple {
        tenant: Some("ep-acme".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_json = serde_json::to_string(&scope_a).expect("scope json");

    let id_node_episode = "01HZEP0000000000000000E001";
    let now = FIXTURE_NOW;

    let node = EntityNode {
        id: EntityId::from(id_node_episode),
        name: "ep-episode-node".to_owned(),
        name_norm: normalize_entity_name("ep-episode-node"),
        summary: None,
        created_at: now,
        embedding_id: None,
    };
    store.upsert_entity(&node).await.expect("upsert_entity");

    {
        let conn = Arc::clone(store.raw_conn().expect("raw_conn present"));

        // Tombstoned record — active=1 but tombstoned=1.
        let records_sql = format!(
            "INSERT INTO records \
             (record_id, target_id, version, path, kind, class, visibility, \
              scope, actor_chain, body, body_hash, created_at, updated_at, \
              active, tombstoned) \
             VALUES \
             ('rec-ep-tomb', 'tgt-ep-tomb', 1, 'p', 'fact', 'episodic', 'private', \
              '{scope_json}', '[]', '', 'h', {now}, {now}, 1, 1);"
        );

        // Episode linking the node to the tombstoned record.
        let episodes_sql = format!(
            "INSERT INTO entity_episodes (episode_id, entity_node_id, linked_at) \
             VALUES ('rec-ep-tomb', '{id_node_episode}', {now});"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&episodes_sql)?;
            Ok(())
        })
        .await
        .expect("episode_tombstone_fixture: seed SQL");
    }

    EpisodeTombstoneFixture {
        store,
        now,
        scope_a,
        node_via_episode: id_node_episode.to_owned(),
    }
}

/// Result of [`scope_user_fixture`].
pub struct ScopeUserFixture {
    /// In-memory store.
    pub store: Arc<SqliteMemoryStore>,
    /// `now` timestamp.
    pub now: i64,
    /// Edge whose source record has `scope.user = "bob"`.
    pub edge_with_user_bob: String,
    /// Node A — source of the edge.
    pub node_a: String,
    /// Node B — target of the edge.
    pub node_b: String,
}

/// Fixture: a record with `scope.user = "bob"` owns the edge between A and B.
///
/// A query with `ScopeTuple { tenant: Some("a"), user: None, .. }` must NOT
/// match that record (no wildcard on user — `user=None` binds `''`, which
/// does not equal `'bob'`).
///
/// # Panics
///
/// Panics on any database error — intended for use in tests only.
#[allow(clippy::expect_used)]
pub async fn scope_user_fixture() -> ScopeUserFixture {
    use cairn_core::contract::memory_store::MemoryStore as _;

    let store = cairn_store_sqlite::open_in_memory()
        .await
        .expect("scope_user_fixture: open_in_memory");
    let store = Arc::new(store);

    // The record's scope has user="bob".
    let scope_with_user = ScopeTuple {
        tenant: Some("a".to_owned()),
        user: Some("bob".to_owned()),
        ..ScopeTuple::default()
    };
    let scope_json = serde_json::to_string(&scope_with_user).expect("scope json");

    let id_node_a = "01HZSU0000000000000000A001";
    let id_node_b = "01HZSU0000000000000000B001";
    let now = FIXTURE_NOW;

    for (id, name) in &[(id_node_a, "su-alpha"), (id_node_b, "su-beta")] {
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
             ('rec-su-bob', 'tgt-su-bob', 1, 'p', 'fact', 'episodic', 'private', \
              '{scope_json}', '[]', '', 'h', {now}, {now}, 1, 0);"
        );

        let edges_sql = format!(
            "INSERT INTO entity_edges \
             (id, source_id, target_id, relation, confidence, confidence_score, \
              valid_at, created_at, body_hash, source_record_id) \
             VALUES \
             ('edge-su-ab', '{id_node_a}', '{id_node_b}', 'relates', 'EXTRACTED', 0.9, \
              {now}, {now}, X'D1', 'rec-su-bob');"
        );

        conn.call(move |c| {
            c.execute_batch(&records_sql)?;
            c.execute_batch(&edges_sql)?;
            Ok(())
        })
        .await
        .expect("scope_user_fixture: seed SQL");
    }

    ScopeUserFixture {
        store,
        now,
        edge_with_user_bob: "edge-su-ab".to_owned(),
        node_a: id_node_a.to_owned(),
        node_b: id_node_b.to_owned(),
    }
}
