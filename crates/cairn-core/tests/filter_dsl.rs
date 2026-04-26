//! Tests for the metadata filter DSL validation and SQL compiler (§8.0.d).
//!
//! Covers:
//! 1. `validate_filter` — field allowlist + per-type op compatibility.
//! 2. `compile_filter` — parameterized SQL output; no string interpolation of values.
//! 3. End-to-end invalid DSL fixture rejection (before `SQLite`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::domain::filter::{FilterError, compile_filter, validate_filter};
use cairn_core::generated::verbs::search::SearchArgsFilters;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse(json: serde_json::Value) -> SearchArgsFilters {
    serde_json::from_value(json).expect("well-formed filter JSON")
}

// ── validate_filter: unknown field rejected ──────────────────────────────────

#[test]
fn validate_rejects_unknown_field() {
    let f = parse(serde_json::json!({"field": "nonexistent_xyz", "op": "eq", "value": "x"}));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnknownField(ref name) if name == "nonexistent_xyz"),
        "expected UnknownField, got: {err:?}"
    );
}

#[test]
fn validate_rejects_unknown_field_inside_and() {
    let f = parse(serde_json::json!({
        "and": [
            {"field": "kind", "op": "eq", "value": "note"},
            {"field": "mystery_column", "op": "eq", "value": "x"},
        ]
    }));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnknownField(ref name) if name == "mystery_column"),
        "expected UnknownField for nested leaf, got: {err:?}"
    );
}

#[test]
fn validate_rejects_unknown_field_inside_not() {
    let f = parse(serde_json::json!({"not": {"field": "foo_bar", "op": "eq", "value": "x"}}));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnknownField(ref name) if name == "foo_bar"),
        "expected UnknownField inside not, got: {err:?}"
    );
}

// ── validate_filter: unsupported op for field type ───────────────────────────

#[test]
fn validate_rejects_array_op_on_string_field() {
    // `kind` is a string field; `array_contains` is an array-only op.
    let f = parse(serde_json::json!({"field": "kind", "op": "array_contains", "value": "note"}));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnsupportedOp { ref field, ref op, .. }
            if field == "kind" && op == "array_contains"),
        "expected UnsupportedOp, got: {err:?}"
    );
}

#[test]
fn validate_rejects_numeric_op_on_boolean_field() {
    // `is_static` is boolean; only `eq` is supported.
    // `lt` with a numeric value passes the serde shape check (valid number op)
    // but must fail the field-type check because `is_static` is boolean.
    let f = parse(serde_json::json!({"field": "is_static", "op": "lt", "value": 1}));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnsupportedOp { ref field, ref op, .. }
            if field == "is_static" && op == "lt"),
        "expected UnsupportedOp for boolean lt, got: {err:?}"
    );
}

#[test]
fn validate_rejects_string_op_on_number_field() {
    // `priority` is a number field; `string_contains` is string-only.
    let f = parse(serde_json::json!({"field": "priority", "op": "string_contains", "value": "7"}));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnsupportedOp { ref field, .. } if field == "priority"),
        "expected UnsupportedOp for string_contains on number field, got: {err:?}"
    );
}

#[test]
fn validate_rejects_scalar_eq_on_array_field() {
    // `tags` is an array field; `eq` is not supported (use `array_contains`).
    let f = parse(serde_json::json!({"field": "tags", "op": "eq", "value": "infra"}));
    let err = validate_filter(&f).unwrap_err();
    assert!(
        matches!(err, FilterError::UnsupportedOp { ref field, ref op, .. }
            if field == "tags" && op == "eq"),
        "expected UnsupportedOp for eq on array field, got: {err:?}"
    );
}

// ── validate_filter: valid field+op combos accepted ──────────────────────────

#[test]
fn validate_accepts_string_eq() {
    let f = parse(serde_json::json!({"field": "kind", "op": "eq", "value": "note"}));
    validate_filter(&f).expect("kind eq is valid");
}

#[test]
fn validate_accepts_string_in() {
    let f = parse(serde_json::json!({"field": "kind", "op": "in", "value": ["note", "rule"]}));
    validate_filter(&f).expect("kind in is valid");
}

#[test]
fn validate_accepts_string_contains() {
    let f = parse(serde_json::json!({"field": "title", "op": "string_contains", "value": "pg"}));
    validate_filter(&f).expect("title string_contains is valid");
}

#[test]
fn validate_accepts_number_gte() {
    let f = parse(serde_json::json!({"field": "priority", "op": "gte", "value": 7}));
    validate_filter(&f).expect("priority gte is valid");
}

#[test]
fn validate_accepts_number_between() {
    let f = parse(serde_json::json!({"field": "confidence", "op": "between", "value": [0.5, 0.9]}));
    validate_filter(&f).expect("confidence between is valid");
}

#[test]
fn validate_accepts_boolean_eq() {
    let f = parse(serde_json::json!({"field": "is_static", "op": "eq", "value": false}));
    validate_filter(&f).expect("is_static eq is valid");
}

#[test]
fn validate_accepts_array_contains() {
    let f = parse(serde_json::json!({"field": "tags", "op": "array_contains", "value": "infra"}));
    validate_filter(&f).expect("tags array_contains is valid");
}

#[test]
fn validate_accepts_array_contains_all() {
    let f = parse(serde_json::json!({
        "field": "tags", "op": "array_contains_all", "value": ["infra", "migration"]
    }));
    validate_filter(&f).expect("tags array_contains_all is valid");
}

#[test]
fn validate_accepts_array_size_eq() {
    let f = parse(serde_json::json!({"field": "tags", "op": "array_size_eq", "value": 3}));
    validate_filter(&f).expect("tags array_size_eq is valid");
}

#[test]
fn validate_accepts_deep_nested_valid_filter() {
    let f = parse(serde_json::json!({
        "and": [
            {"field": "kind", "op": "in", "value": ["strategy_success", "playbook"]},
            {"field": "is_static", "op": "eq", "value": false},
            {"or": [
                {"field": "category", "op": "eq", "value": "shipped"},
                {"not": {"field": "category", "op": "eq", "value": "draft"}}
            ]}
        ]
    }));
    validate_filter(&f).expect("deep nested filter with valid fields is valid");
}

// ── compile_filter: SQL output and parameter binding ─────────────────────────

#[test]
fn compile_string_eq_produces_parameterized_sql() {
    let f = parse(serde_json::json!({"field": "kind", "op": "eq", "value": "note"}));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "kind = ?");
    assert_eq!(compiled.params, vec![serde_json::json!("note")]);
}

#[test]
fn compile_string_neq() {
    let f = parse(serde_json::json!({"field": "kind", "op": "neq", "value": "draft"}));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "kind != ?");
    assert_eq!(compiled.params.len(), 1);
}

#[test]
fn compile_string_in() {
    let f = parse(serde_json::json!({
        "field": "kind", "op": "in", "value": ["strategy_success", "playbook"]
    }));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "kind IN (?, ?)");
    assert_eq!(compiled.params.len(), 2);
    assert_eq!(compiled.params[0], serde_json::json!("strategy_success"));
    assert_eq!(compiled.params[1], serde_json::json!("playbook"));
}

#[test]
fn compile_string_nin() {
    let f = parse(serde_json::json!({
        "field": "category", "op": "nin", "value": ["draft", "archived"]
    }));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "category NOT IN (?, ?)");
    assert_eq!(compiled.params.len(), 2);
}

#[test]
fn compile_string_contains_uses_instr() {
    let f = parse(serde_json::json!({"field": "title", "op": "string_contains", "value": "pg"}));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "instr(title, ?) > 0");
    assert_eq!(compiled.params, vec![serde_json::json!("pg")]);
}

#[test]
fn compile_string_starts_with() {
    let f = parse(serde_json::json!({
        "field": "title", "op": "string_starts_with", "value": "migration"
    }));
    let compiled = compile_filter(&f);
    // LIKE with trailing %; value is the pattern param
    assert!(compiled.sql.contains("title") && compiled.sql.contains("LIKE"));
    assert_eq!(compiled.params.len(), 1);
    let param = compiled.params[0].as_str().expect("param is a string");
    assert!(param.ends_with('%'), "starts_with param must end with %");
    assert!(param.starts_with("migration"));
}

#[test]
fn compile_string_ends_with() {
    let f = parse(serde_json::json!({
        "field": "title", "op": "string_ends_with", "value": "config"
    }));
    let compiled = compile_filter(&f);
    assert!(compiled.sql.contains("title") && compiled.sql.contains("LIKE"));
    assert_eq!(compiled.params.len(), 1);
    let param = compiled.params[0].as_str().expect("param is a string");
    assert!(param.starts_with('%'), "ends_with param must start with %");
    assert!(param.ends_with("config"));
}

#[test]
fn compile_number_lt() {
    let f = parse(serde_json::json!({"field": "priority", "op": "lt", "value": 5}));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "priority < ?");
    assert_eq!(compiled.params, vec![serde_json::json!(5)]);
}

#[test]
fn compile_number_between() {
    let f = parse(serde_json::json!({
        "field": "confidence", "op": "between", "value": [0.5, 0.9]
    }));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "confidence BETWEEN ? AND ?");
    assert_eq!(compiled.params.len(), 2);
    assert_eq!(compiled.params[0], serde_json::json!(0.5));
    assert_eq!(compiled.params[1], serde_json::json!(0.9));
}

#[test]
fn compile_boolean_eq() {
    let f = parse(serde_json::json!({"field": "is_static", "op": "eq", "value": false}));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "is_static = ?");
    assert_eq!(compiled.params, vec![serde_json::json!(false)]);
}

#[test]
fn compile_array_contains() {
    let f = parse(serde_json::json!({"field": "tags", "op": "array_contains", "value": "infra"}));
    let compiled = compile_filter(&f);
    assert!(
        compiled.sql.contains("json_each(tags)") && compiled.sql.contains("value = ?"),
        "array_contains must use json_each, got: {}",
        compiled.sql
    );
    assert_eq!(compiled.params, vec![serde_json::json!("infra")]);
}

#[test]
fn compile_array_contains_any() {
    let f = parse(serde_json::json!({
        "field": "tags", "op": "array_contains_any", "value": ["infra", "db"]
    }));
    let compiled = compile_filter(&f);
    assert!(
        compiled.sql.contains("json_each(tags)") && compiled.sql.contains("IN"),
        "array_contains_any must use json_each + IN, got: {}",
        compiled.sql
    );
    assert_eq!(compiled.params.len(), 2);
}

#[test]
fn compile_array_contains_all() {
    let f = parse(serde_json::json!({
        "field": "tags", "op": "array_contains_all", "value": ["infra", "migration"]
    }));
    let compiled = compile_filter(&f);
    assert!(
        compiled.sql.contains("json_each(tags)"),
        "array_contains_all must use json_each, got: {}",
        compiled.sql
    );
    // params: the set members + the count
    assert_eq!(
        compiled.params.len(),
        3,
        "array_contains_all [a,b] → 2 value params + 1 count param"
    );
    assert_eq!(compiled.params[2], serde_json::json!(2u64));
}

#[test]
fn compile_array_size_eq() {
    let f = parse(serde_json::json!({"field": "tags", "op": "array_size_eq", "value": 3}));
    let compiled = compile_filter(&f);
    assert_eq!(compiled.sql, "json_array_length(tags) = ?");
    assert_eq!(compiled.params, vec![serde_json::json!(3)]);
}

// ── compile_filter: boolean combinators ──────────────────────────────────────

#[test]
fn compile_and_wraps_children_with_and() {
    let f = parse(serde_json::json!({
        "and": [
            {"field": "kind", "op": "eq", "value": "note"},
            {"field": "is_static", "op": "eq", "value": false},
        ]
    }));
    let compiled = compile_filter(&f);
    assert!(
        compiled.sql.starts_with('(') && compiled.sql.ends_with(')'),
        "and must be wrapped in parens, got: {}",
        compiled.sql
    );
    assert!(
        compiled.sql.contains(" AND "),
        "and must use AND keyword, got: {}",
        compiled.sql
    );
    assert_eq!(compiled.params.len(), 2);
}

#[test]
fn compile_or_wraps_children_with_or() {
    let f = parse(serde_json::json!({
        "or": [
            {"field": "category", "op": "eq", "value": "shipped"},
            {"field": "category", "op": "eq", "value": "released"},
        ]
    }));
    let compiled = compile_filter(&f);
    assert!(
        compiled.sql.contains(" OR "),
        "or must use OR keyword, got: {}",
        compiled.sql
    );
    assert_eq!(compiled.params.len(), 2);
}

#[test]
fn compile_not_wraps_child_with_not() {
    let f = parse(serde_json::json!({"not": {"field": "kind", "op": "eq", "value": "draft"}}));
    let compiled = compile_filter(&f);
    assert!(
        compiled.sql.contains("NOT"),
        "not must use NOT keyword, got: {}",
        compiled.sql
    );
    assert_eq!(compiled.params.len(), 1);
}

#[test]
fn compile_params_never_contain_sql_in_string_value() {
    // Injection guard: a malicious value that looks like SQL must appear as a
    // bound parameter — never spliced into the SQL fragment itself.
    let malicious = "'; DROP TABLE records; --";
    let f = parse(serde_json::json!({"field": "title", "op": "eq", "value": malicious}));
    let compiled = compile_filter(&f);
    assert!(
        !compiled.sql.contains("DROP"),
        "SQL injection payload must not appear in the sql fragment, got: {}",
        compiled.sql
    );
    assert_eq!(compiled.params, vec![serde_json::json!(malicious)]);
}

// ── compile_filter: complex example from §8.0.d ──────────────────────────────

#[test]
fn compile_brief_example_filter() {
    // Full example from design brief §8.0.d.
    let f = parse(serde_json::json!({
        "and": [
            {"field": "kind",      "op": "in",             "value": ["strategy_success", "playbook"]},
            {"field": "is_static", "op": "eq",             "value": false},
            {"field": "tags",      "op": "array_contains", "value": "infra"},
            {"field": "priority",  "op": "gte",            "value": 7},
            {"or": [
                {"field": "category", "op": "eq",          "value": "shipped"},
                {"not": {"field": "category", "op": "eq",  "value": "draft"}}
            ]},
            {"field": "title",     "op": "string_contains","value": "pg"}
        ]
    }));
    validate_filter(&f).expect("brief example filter must validate");
    let compiled = compile_filter(&f);
    // Should have produced a non-empty SQL fragment.
    assert!(!compiled.sql.is_empty());
    // Parameters must be bound (not interpolated).
    assert!(
        compiled.params.len() >= 6,
        "expected at least 6 params, got {}",
        compiled.params.len()
    );
    // The literal value "strategy_success" must appear only in params, not in sql.
    assert!(
        !compiled.sql.contains("strategy_success"),
        "field value must not appear in SQL fragment"
    );
}
