//! E2E execution tests for the metadata filter SQL compiler (§8.0.d).
//!
//! Creates an in-memory `SQLite` `records` table covering every P0 filter
//! field, runs [`compile_filter`] output against it with real data, and
//! asserts the correct rows are returned.  This validates SQL syntax AND
//! semantics — not just structure.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::domain::filter::{compile_filter, validate_filter};
use cairn_core::generated::verbs::search::SearchArgsFilters;
use rusqlite::{Connection, params_from_iter, types::Value as SqlVal};

// ── Test schema ───────────────────────────────────────────────────────────────

fn open_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE records (
            id          TEXT    PRIMARY KEY,
            kind        TEXT,
            class       TEXT,
            visibility  TEXT,
            path        TEXT,
            title       TEXT,
            category    TEXT,
            priority    INTEGER,
            version     INTEGER,
            created_at  TEXT,
            confidence  REAL,
            is_static   INTEGER,
            tombstoned  INTEGER,
            active      INTEGER,
            tags        TEXT,
            actor_chain TEXT,
            backlinks   TEXT
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
    validate_filter(&f).unwrap();
    let compiled = compile_filter(&f);
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

    insert(
        &conn,
        &[
            ("id", "r1"),
            ("kind", "user"),
            ("title", "pg migration strategy"),
            ("category", "shipped"),
            ("priority", "8"),
            ("confidence", "0.9"),
            ("is_static", "0"),
            ("tombstoned", "0"),
            ("active", "1"),
            ("tags", r#"["infra","migration"]"#),
            ("actor_chain", r#"["agt:claude"]"#),
            ("backlinks", r"[]"),
            ("version", "1"),
        ],
    );
    insert(
        &conn,
        &[
            ("id", "r2"),
            ("kind", "feedback"),
            ("title", "user prefers dark mode"),
            ("category", "draft"),
            ("priority", "3"),
            ("confidence", "0.5"),
            ("is_static", "1"),
            ("tombstoned", "0"),
            ("active", "1"),
            ("tags", r#"["pref"]"#),
            ("actor_chain", r#"["usr:tafeng"]"#),
            ("backlinks", r"[]"),
            ("version", "2"),
        ],
    );
    insert(
        &conn,
        &[
            ("id", "r3"),
            ("kind", "rule"),
            ("title", "always use pg for backups"),
            ("category", "shipped"),
            ("priority", "9"),
            ("confidence", "0.8"),
            ("is_static", "0"),
            ("tombstoned", "1"),
            ("active", "0"),
            ("tags", r#"["infra","db","backup"]"#),
            ("actor_chain", r#"["agt:claude"]"#),
            ("backlinks", r#"["r1"]"#),
            ("version", "1"),
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

// ── Array field ops ───────────────────────────────────────────────────────────

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
    // kind=user OR kind=rule → r1 and r3
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
    // NOT category=draft → r1 and r3
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
    // Adapted from §8.0.d: kind in [user, rule], is_static=false, tags contains infra,
    // priority >= 7, category=shipped OR NOT category=draft, title contains pg.
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
