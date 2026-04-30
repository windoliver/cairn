//! E2E execution tests for the metadata filter SQL compiler (§8.0.d).
//!
//! Creates an in-memory `SQLite` `records` table that mirrors the real
//! `MemoryRecord` storage layout:
//!
//! - Scalar columns for direct `MemoryRecord` fields (`kind`, `class`,
//!   `visibility`, `confidence`).
//! - Store-owned scalar columns set by `cairn-store-sqlite::projection`:
//!   `path`, `version`, `is_static`, `tombstoned`, `active`. These are NOT
//!   inside `extra_frontmatter` — the filter compiler routes them to the
//!   physical columns.
//! - A `provenance TEXT` JSON column — `created_at` lives here as
//!   `json_extract(provenance, '$.created_at')`.
//! - A `tags TEXT` JSON column for the plain-string array.
//! - An `actor_chain TEXT` JSON column storing `Vec<ActorChainEntry>` objects;
//!   array ops filter on `json_extract(value, '$.identity')`.
//! - An `extra_frontmatter TEXT` JSON column for ingest-supplied fields
//!   that have no physical column (`title`, `category`, `priority`,
//!   `backlinks`).
//!
//! This validates SQL syntax AND semantics against the actual expression
//! mapping in `field_col` — not just structure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::domain::filter::{compile_filter, validate_filter};
use cairn_core::generated::verbs::search::SearchArgsFilters;
use rusqlite::{Connection, params_from_iter, types::Value as SqlVal};

// ── Test schema ───────────────────────────────────────────────────────────────

fn open_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE records (
            id                TEXT PRIMARY KEY,
            kind              TEXT,
            class             TEXT,
            visibility        TEXT,
            confidence        REAL,
            path              TEXT,
            version           INTEGER,
            is_static         INTEGER,
            tombstoned        INTEGER,
            active            INTEGER,
            provenance        TEXT,
            tags              TEXT,
            actor_chain       TEXT,
            extra_frontmatter TEXT
        );",
    )
    .unwrap();
    conn
}

fn insert(conn: &Connection, row: &[(&str, &str)]) {
    let cols: Vec<&str> = row.iter().map(|(c, _)| *c).collect();
    let vals: Vec<&str> = row.iter().map(|(_, v)| *v).collect();
    let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT INTO records ({}) VALUES ({placeholders})",
        cols.join(", ")
    );
    let p: Vec<&dyn rusqlite::types::ToSql> = vals
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect();
    conn.execute(&sql, p.as_slice()).unwrap();
}

/// Convert `serde_json::Value` params from [`compile_filter`] to `rusqlite` values.
fn to_sql_params(params: &[serde_json::Value]) -> Vec<SqlVal> {
    params
        .iter()
        .map(|v| match v {
            serde_json::Value::String(s) => SqlVal::Text(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    SqlVal::Integer(i)
                } else {
                    SqlVal::Real(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::Bool(b) => SqlVal::Integer(i64::from(*b)),
            _ => SqlVal::Null,
        })
        .collect()
}

/// Run a compiled filter against the `records` table and return matching `id`s.
fn run(conn: &Connection, filter_json: serde_json::Value) -> Vec<String> {
    let f: SearchArgsFilters = serde_json::from_value(filter_json).unwrap();
    let compiled = compile_filter(validate_filter(&f).unwrap());
    let sql = format!("SELECT id FROM records WHERE {}", compiled.sql);
    let sql_params = to_sql_params(&compiled.params);
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map(params_from_iter(sql_params.iter()), |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

fn seed_db() -> Connection {
    let conn = open_db();

    // r1: kind=user, confidence=0.9, tags=["infra","migration"]
    //     actor_chain=[{role:author,identity:agt:claude}]
    //     physical: path=vault/.../r1.md, version=1, is_static=0,
    //               tombstoned=0, active=1
    //     extra_frontmatter: title, category=shipped, priority=8, backlinks=[]
    insert(
        &conn,
        &[
            ("id", "r1"),
            ("kind", "user"),
            ("class", "semantic"),
            ("visibility", "private"),
            ("confidence", "0.9"),
            ("path", "vault/private/r1.md"),
            ("version", "1"),
            ("is_static", "0"),
            ("tombstoned", "0"),
            ("active", "1"),
            (
                "provenance",
                r#"{"created_at":"2026-04-01T10:00:00Z","source_sensor":"snr:local:hook:cc-session:v1"}"#,
            ),
            ("tags", r#"["infra","migration"]"#),
            (
                "actor_chain",
                r#"[{"role":"author","identity":"agt:claude","at":"2026-04-01T10:00:00Z"}]"#,
            ),
            (
                "extra_frontmatter",
                r#"{"title":"pg migration strategy","category":"shipped","priority":8,"backlinks":[]}"#,
            ),
        ],
    );

    // r2: kind=feedback, confidence=0.5, tags=["pref"]
    //     actor_chain=[{role:author,identity:usr:tafeng}]
    //     physical: path=vault/.../r2.md, version=2, is_static=1,
    //               tombstoned=0, active=1
    //     extra_frontmatter: title, category=draft, priority=3, backlinks=[]
    insert(
        &conn,
        &[
            ("id", "r2"),
            ("kind", "feedback"),
            ("class", "semantic"),
            ("visibility", "private"),
            ("confidence", "0.5"),
            ("path", "vault/private/r2.md"),
            ("version", "2"),
            ("is_static", "1"),
            ("tombstoned", "0"),
            ("active", "1"),
            (
                "provenance",
                r#"{"created_at":"2026-04-02T11:00:00Z","source_sensor":"snr:local:hook:cc-session:v1"}"#,
            ),
            ("tags", r#"["pref"]"#),
            (
                "actor_chain",
                r#"[{"role":"author","identity":"usr:tafeng","at":"2026-04-02T11:00:00Z"}]"#,
            ),
            (
                "extra_frontmatter",
                r#"{"title":"user prefers dark mode","category":"draft","priority":3,"backlinks":[]}"#,
            ),
        ],
    );

    // r3: kind=rule, confidence=0.8, tags=["infra","db","backup"]
    //     actor_chain=[{role:author,identity:agt:claude}]
    //     physical: path=vault/.../r3.md, version=1, is_static=0,
    //               tombstoned=1, active=0
    //     extra_frontmatter: title, category=shipped, priority=9, backlinks=["r1"]
    insert(
        &conn,
        &[
            ("id", "r3"),
            ("kind", "rule"),
            ("class", "semantic"),
            ("visibility", "private"),
            ("confidence", "0.8"),
            ("path", "vault/private/r3.md"),
            ("version", "1"),
            ("is_static", "0"),
            ("tombstoned", "1"),
            ("active", "0"),
            (
                "provenance",
                r#"{"created_at":"2026-04-03T09:00:00Z","source_sensor":"snr:local:hook:cc-session:v1"}"#,
            ),
            ("tags", r#"["infra","db","backup"]"#),
            (
                "actor_chain",
                r#"[{"role":"author","identity":"agt:claude","at":"2026-04-03T09:00:00Z"}]"#,
            ),
            (
                "extra_frontmatter",
                r#"{"title":"always use pg for backups","category":"shipped","priority":9,"backlinks":["r1"]}"#,
            ),
        ],
    );

    conn
}

// ── String field ops ──────────────────────────────────────────────────────────

#[test]
fn exec_string_eq_matches_exact() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "kind", "op": "eq", "value": "user"}),
    );
    assert_eq!(ids, vec!["r1"]);
}

#[test]
fn exec_string_neq_excludes_value() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "kind", "op": "neq", "value": "feedback"}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_string_in_matches_set() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "kind", "op": "in", "value": ["user", "rule"]}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_string_nin_excludes_set() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "kind", "op": "nin", "value": ["user", "rule"]}),
    );
    assert_eq!(ids, vec!["r2"]);
}

#[test]
fn exec_string_contains_substring() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "title", "op": "string_contains", "value": "pg"}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_string_starts_with() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "title", "op": "string_starts_with", "value": "pg"}),
    );
    assert_eq!(ids, vec!["r1"]);
}

#[test]
fn exec_string_ends_with() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "title", "op": "string_ends_with", "value": "mode"}),
    );
    assert_eq!(ids, vec!["r2"]);
}

// ── Numeric field ops ─────────────────────────────────────────────────────────

#[test]
fn exec_number_gte_filters_rows() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "priority", "op": "gte", "value": 8}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_number_lt_filters_rows() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "priority", "op": "lt", "value": 5}),
    );
    assert_eq!(ids, vec!["r2"]);
}

#[test]
fn exec_number_between_inclusive() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "confidence", "op": "between", "value": [0.8, 1.0]}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

// ── Boolean field ops ─────────────────────────────────────────────────────────

#[test]
fn exec_boolean_eq_true() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "is_static", "op": "eq", "value": true}),
    );
    assert_eq!(ids, vec!["r2"]);
}

#[test]
fn exec_boolean_eq_false() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "is_static", "op": "eq", "value": false}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_tombstoned_eq_true_returns_only_tombstoned() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({"field": "tombstoned", "op": "eq", "value": true}),
    );
    assert_eq!(ids, vec!["r3"]);
}

// ── Timestamp field ops ───────────────────────────────────────────────────────

#[test]
fn exec_created_at_eq_matches_exact_timestamp() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({
            "field": "created_at",
            "op": "eq",
            "value": "2026-04-01T10:00:00Z"
        }),
    );
    assert_eq!(ids, vec!["r1"]);
}

#[test]
fn exec_created_at_in_set_matches_two() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({
            "field": "created_at",
            "op": "in",
            "value": ["2026-04-01T10:00:00Z", "2026-04-03T09:00:00Z"]
        }),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

// ── Array field ops — tags (plain string array) ───────────────────────────────

#[test]
fn exec_array_contains_single() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "tags", "op": "array_contains", "value": "infra"}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_array_contains_any_matches_either() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({
            "field": "tags", "op": "array_contains_any",
            "value": ["pref", "backup"]
        }),
    );
    ids.sort();
    assert_eq!(ids, vec!["r2", "r3"]);
}

#[test]
fn exec_array_contains_all_requires_both() {
    let conn = seed_db();
    // r1 has ["infra","migration"], r3 has ["infra","db","backup"] — only r1 has both
    let ids = run(
        &conn,
        serde_json::json!({
            "field": "tags", "op": "array_contains_all",
            "value": ["infra", "migration"]
        }),
    );
    assert_eq!(ids, vec!["r1"]);
}

#[test]
fn exec_array_size_eq_returns_matching_rows() {
    let conn = seed_db();
    // r1 has 2 tags, r2 has 1 tag, r3 has 3 tags
    let ids = run(
        &conn,
        serde_json::json!({"field": "tags", "op": "array_size_eq", "value": 1}),
    );
    assert_eq!(ids, vec!["r2"]);
}

// ── Array field ops — actor_chain (struct array, filter on $.identity) ────────

#[test]
fn exec_actor_chain_contains_agent_identity() {
    let conn = seed_db();
    // r1 and r3 both have identity "agt:claude" in their actor_chain entries.
    let mut ids = run(
        &conn,
        serde_json::json!({
            "field": "actor_chain",
            "op": "array_contains",
            "value": "agt:claude"
        }),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_actor_chain_contains_user_identity() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({
            "field": "actor_chain",
            "op": "array_contains",
            "value": "usr:tafeng"
        }),
    );
    assert_eq!(ids, vec!["r2"]);
}

#[test]
fn exec_actor_chain_contains_any_matches() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({
            "field": "actor_chain",
            "op": "array_contains_any",
            "value": ["usr:tafeng", "agt:other"]
        }),
    );
    ids.sort();
    assert_eq!(ids, vec!["r2"]);
}

// ── Array field ops — backlinks (JSON array in extra_frontmatter) ─────────────

#[test]
fn exec_backlinks_contains_ref() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({
            "field": "backlinks",
            "op": "array_contains",
            "value": "r1"
        }),
    );
    assert_eq!(ids, vec!["r3"]);
}

#[test]
fn exec_backlinks_size_eq_zero_matches_empty() {
    let conn = seed_db();
    // r1 and r2 have empty backlinks arrays.
    let mut ids = run(
        &conn,
        serde_json::json!({"field": "backlinks", "op": "array_size_eq", "value": 0}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r2"]);
}

// ── Boolean combinators ───────────────────────────────────────────────────────

#[test]
fn exec_and_narrows_results() {
    let conn = seed_db();
    // kind=user AND priority>=8 → only r1
    let ids = run(
        &conn,
        serde_json::json!({
            "and": [
                {"field": "kind", "op": "eq", "value": "user"},
                {"field": "priority", "op": "gte", "value": 8}
            ]
        }),
    );
    assert_eq!(ids, vec!["r1"]);
}

#[test]
fn exec_or_broadens_results() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({
            "or": [
                {"field": "kind", "op": "eq", "value": "user"},
                {"field": "kind", "op": "eq", "value": "rule"}
            ]
        }),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

#[test]
fn exec_not_inverts_match() {
    let conn = seed_db();
    let mut ids = run(
        &conn,
        serde_json::json!({"not": {"field": "category", "op": "eq", "value": "draft"}}),
    );
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}

// ── §8.0.d brief example ──────────────────────────────────────────────────────

#[test]
fn exec_brief_example_filter_returns_correct_rows() {
    let conn = seed_db();
    let ids = run(
        &conn,
        serde_json::json!({
            "and": [
                {"field": "kind", "op": "in", "value": ["user", "rule"]},
                {"field": "is_static", "op": "eq", "value": false},
                {"field": "tags", "op": "array_contains", "value": "infra"},
                {"field": "priority", "op": "gte", "value": 7},
                {"or": [
                    {"field": "category", "op": "eq", "value": "shipped"},
                    {"not": {"field": "category", "op": "eq", "value": "draft"}}
                ]},
                {"field": "title", "op": "string_contains", "value": "pg"}
            ]
        }),
    );
    // r1: user, not static, has infra tag, priority 8, shipped, title has "pg" → matches
    // r3: rule, not static, has infra tag, priority 9, shipped, title has "pg" → matches
    // r2: feedback, static, no infra tag → excluded at multiple predicates
    let mut ids = ids;
    ids.sort();
    assert_eq!(ids, vec!["r1", "r3"]);
}
