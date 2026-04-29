//! Metadata filter DSL validation and SQL compilation (§8.0.d).
//!
//! Two-step pipeline:
//! 1. [`validate_filter`] — walks the parsed [`SearchArgsFilters`] tree,
//!    checks every leaf field against the P0 allowlist, and ensures the op
//!    is valid for that field's type. Rejects with [`FilterError`] before any
//!    store is touched.
//! 2. [`compile_filter`] — converts a validated tree into a parameterized
//!    [`CompiledFilter`] (`sql` fragment + positional `params`). All user
//!    values land in `params`; the `sql` string contains only structure and
//!    `?` placeholders, so SQL injection via field values is impossible.

use thiserror::Error;

use crate::domain::timestamp::Rfc3339Timestamp;
use crate::generated::verbs::search::SearchArgsFilters;

// ── Field allowlist ───────────────────────────────────────────────────────────

/// Type classification of a P0 filter field (§8.0.d table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Single string value (`kind`, `class`, `visibility`, `path`, `title`, `category`, …).
    Str,
    /// Numeric value (`priority`, `version`, `confidence`).
    Number,
    /// Boolean flag (`is_static`, `tombstoned`, `active`).
    Boolean,
    /// JSON-array column (`tags`, `actor_chain`, `backlinks`).
    Array,
    /// RFC3339 timestamp (`created_at`). Only exact/membership ops are valid;
    /// range ordering over raw RFC3339 strings is unreliable due to timezone
    /// offsets, so `lt/lte/gt/gte/between` are rejected until a normalized
    /// epoch column is available in the store.
    Timestamp,
}

impl FieldType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Str => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Array => "array",
            Self::Timestamp => "timestamp",
        }
    }
}

// Ordered list of (field_name, type) pairs.  All P0 allowlisted fields are here;
// any field not in this list is rejected by `validate_filter`.
//
// P0 contract: every field listed here MUST exist as a same-named physical column
// (or addressable expression) on the `records` table in `cairn-store-sqlite`.
// When the store migration is implemented, this list must be reconciled with the
// actual schema; fields stored inside a JSON blob need an explicit expression in
// `field_col` rather than the identity mapping.
const KNOWN_FIELDS: &[(&str, FieldType)] = &[
    // String fields — direct `records` table columns.
    ("kind", FieldType::Str),
    ("class", FieldType::Str),
    ("visibility", FieldType::Str),
    ("path", FieldType::Str),
    ("title", FieldType::Str),
    ("category", FieldType::Str),
    // Numeric fields.
    ("priority", FieldType::Number),
    ("version", FieldType::Number),
    ("confidence", FieldType::Number),
    // Timestamp — stored as RFC3339 text; only exact/membership ops accepted.
    ("created_at", FieldType::Timestamp),
    // Boolean fields.
    ("is_static", FieldType::Boolean),
    ("tombstoned", FieldType::Boolean),
    ("active", FieldType::Boolean),
    // Array fields — stored as JSON text columns.
    ("tags", FieldType::Array),
    ("actor_chain", FieldType::Array),
    ("backlinks", FieldType::Array),
];

/// Return the declared [`FieldType`] for a filter field name, or `None` if the
/// field is not in the P0 allowlist.
#[must_use]
pub fn field_type(name: &str) -> Option<FieldType> {
    KNOWN_FIELDS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, t)| *t)
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Failure from filter validation.
///
/// Produced by [`validate_filter`] and surfaced to the caller so it can
/// translate to an `InvalidFilter` wire error before touching `SQLite`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FilterError {
    /// The filter references a field not in the P0 allowlist (§8.0.d).
    #[error("unknown filter field `{0}` — only P0 allowlisted fields are accepted")]
    UnknownField(String),

    /// The operator is not valid for the declared type of the field (§8.0.d table).
    #[error("op `{op}` is not supported for field `{field}` of type {field_type}")]
    UnsupportedOp {
        /// Field that was queried.
        field: String,
        /// Operator that was rejected.
        op: String,
        /// Human-readable field type ("string", "number", "boolean", "array").
        field_type: &'static str,
    },

    /// The value's JSON type is incompatible with the field type and operator.
    #[error("value for field `{field}` op `{op}` has wrong shape: expected {expected}, got {got}")]
    WrongValueShape {
        /// Field that was queried.
        field: String,
        /// Operator that was rejected.
        op: String,
        /// What shape was expected.
        expected: &'static str,
        /// What shape was actually received.
        got: &'static str,
    },

    /// A timestamp operand is a string but not a valid RFC3339 value.
    #[error("field `{field}` op `{op}` has invalid RFC3339 timestamp `{value}`: {reason}")]
    InvalidTimestamp {
        /// Field that was queried.
        field: String,
        /// Operator that was rejected.
        op: String,
        /// The offending value.
        value: String,
        /// Parse failure description.
        reason: String,
    },

    /// An array operand must be non-empty but was empty.
    #[error("field `{field}` op `{op}` requires a non-empty array")]
    EmptyArray {
        /// Field that was queried.
        field: String,
        /// Operator that was rejected.
        op: String,
    },

    /// `array_size_eq` operand must be a non-negative integer.
    #[error("field `{field}` op `{op}` requires a non-negative integer, got `{value}`")]
    InvalidArraySize {
        /// Field that was queried.
        field: String,
        /// Operator that was rejected.
        op: String,
        /// The offending value.
        value: String,
    },

    /// An `and` or `or` node has an empty children array.
    #[error("`{0}` node must contain at least one child filter")]
    EmptyOperator(&'static str),

    /// A leaf node is structurally invalid (missing or wrong-typed `field`/`op`/`value`).
    #[error("malformed leaf filter: {0}")]
    MalformedLeaf(String),
}

// ── validate_filter ───────────────────────────────────────────────────────────

/// Maximum nesting depth for a filter tree (matches the serde-layer cap).
const MAX_FILTER_DEPTH: usize = 8;
/// Maximum fanout for `and` / `or` nodes (matches the serde-layer cap).
const MAX_FILTER_FANOUT: usize = 32;
/// Maximum number of items in a set operand (`in`, `nin`, `array_contains_any`,
/// `array_contains_all`). Per-leaf limit; total params across the whole filter
/// are further capped by `MAX_TOTAL_PARAMS`.
const MAX_SET_OPERAND_SIZE: usize = 64;
/// Maximum total `?` parameters compiled across the entire filter tree.
/// Stays well below the `SQLITE_MAX_VARIABLE_NUMBER` default of `999`.
const MAX_TOTAL_PARAMS: usize = 500;

/// Validate a parsed [`SearchArgsFilters`] tree against the P0 field allowlist
/// and per-type operator rules (§8.0.d).
///
/// Call this before [`compile_filter`] and before dispatching to any store.
/// Returns the first [`FilterError`] found, depth-first left-to-right.
///
/// Enforces the same depth/fanout caps as the serde parser so that
/// programmatically constructed trees cannot cause stack exhaustion or
/// unbounded SQL generation.
pub fn validate_filter(filter: &SearchArgsFilters) -> Result<ValidatedFilter<'_>, FilterError> {
    let mut param_budget = MAX_TOTAL_PARAMS;
    validate_filter_inner(filter, 0, &mut param_budget)?;
    Ok(ValidatedFilter(filter))
}

/// A filter that has passed all [`validate_filter`] checks.
///
/// The only way to produce a `ValidatedFilter` is through [`validate_filter`].
/// [`compile_filter`] requires this type so the budget and semantic checks
/// cannot be bypassed through the public API.
#[derive(Clone, Copy, Debug)]
pub struct ValidatedFilter<'a>(&'a SearchArgsFilters);

fn validate_filter_inner(
    filter: &SearchArgsFilters,
    depth: usize,
    param_budget: &mut usize,
) -> Result<(), FilterError> {
    if depth > MAX_FILTER_DEPTH {
        return Err(FilterError::MalformedLeaf(format!(
            "filter tree exceeds maximum nesting depth of {MAX_FILTER_DEPTH}"
        )));
    }
    match filter {
        SearchArgsFilters::And { and } => {
            if and.is_empty() {
                return Err(FilterError::EmptyOperator("and"));
            }
            if and.len() > MAX_FILTER_FANOUT {
                return Err(FilterError::MalformedLeaf(format!(
                    "`and` node has {} children; maximum is {MAX_FILTER_FANOUT}",
                    and.len()
                )));
            }
            for child in and {
                validate_filter_inner(child, depth + 1, param_budget)?;
            }
            Ok(())
        }
        SearchArgsFilters::Or { or } => {
            if or.is_empty() {
                return Err(FilterError::EmptyOperator("or"));
            }
            if or.len() > MAX_FILTER_FANOUT {
                return Err(FilterError::MalformedLeaf(format!(
                    "`or` node has {} children; maximum is {MAX_FILTER_FANOUT}",
                    or.len()
                )));
            }
            for child in or {
                validate_filter_inner(child, depth + 1, param_budget)?;
            }
            Ok(())
        }
        SearchArgsFilters::Not { not } => validate_filter_inner(not, depth + 1, param_budget),
        SearchArgsFilters::Leaf(v) => validate_leaf_with_budget(v, param_budget),
    }
}

/// Validate a single leaf, also deducting its expected `?` parameter count
/// from `param_budget`. Returns `FilterError::MalformedLeaf` when the budget
/// would go negative.
fn validate_leaf_with_budget(
    v: &serde_json::Value,
    param_budget: &mut usize,
) -> Result<(), FilterError> {
    let Some(obj) = v.as_object() else {
        return Err(FilterError::MalformedLeaf(
            "leaf value is not a JSON object".into(),
        ));
    };
    let Some(field) = obj.get("field").and_then(|f| f.as_str()) else {
        return Err(FilterError::MalformedLeaf(
            "leaf missing string `field` key".into(),
        ));
    };
    let Some(op) = obj.get("op").and_then(|o| o.as_str()) else {
        return Err(FilterError::MalformedLeaf(
            "leaf missing string `op` key".into(),
        ));
    };
    let Some(value) = obj.get("value") else {
        return Err(FilterError::MalformedLeaf(
            "leaf missing `value` key".into(),
        ));
    };

    let ft = field_type(field).ok_or_else(|| FilterError::UnknownField(field.to_owned()))?;
    validate_op_for_type(field, op, ft)?;
    validate_value_shape(field, op, ft, value)?;

    let cost = leaf_param_count(op, value);
    if cost > *param_budget {
        return Err(FilterError::MalformedLeaf(format!(
            "filter would require more than {MAX_TOTAL_PARAMS} total SQL parameters"
        )));
    }
    *param_budget -= cost;
    Ok(())
}

/// Estimate the number of `?` placeholders `compile_leaf` will emit for `op`.
fn leaf_param_count(op: &str, value: &serde_json::Value) -> usize {
    match op {
        "in" | "nin" | "array_contains_any" => value.as_array().map_or(1, Vec::len),
        "array_contains_all" => {
            // N items + 1 count param
            value.as_array().map_or(2, |a| a.len() + 1)
        }
        // between → 2 params; string_starts_with → substr(col, 1, length(?)) = ? → 2 params
        "between" | "string_starts_with" => 2,
        // substr(col, -length(?), length(?)) = ? → 3 params
        "string_ends_with" => 3,
        _ => 1, // eq, neq, lt, lte, gt, gte, string_contains, array_contains, array_size_eq
    }
}

fn validate_op_for_type(field: &str, op: &str, ft: FieldType) -> Result<(), FilterError> {
    let valid = match ft {
        FieldType::Str => matches!(
            op,
            "eq" | "neq"
                | "in"
                | "nin"
                | "string_contains"
                | "string_starts_with"
                | "string_ends_with"
        ),
        FieldType::Number => {
            matches!(
                op,
                "eq" | "neq" | "lt" | "lte" | "gt" | "gte" | "between" | "in" | "nin"
            )
        }
        FieldType::Boolean => op == "eq",
        FieldType::Array => matches!(
            op,
            "array_contains" | "array_contains_any" | "array_contains_all" | "array_size_eq"
        ),
        // Range ops are rejected for Timestamp until a normalized epoch column
        // exists in the store; raw RFC3339 strings are not reliably ordered.
        FieldType::Timestamp => matches!(op, "eq" | "neq" | "in" | "nin"),
    };
    if valid {
        Ok(())
    } else {
        Err(FilterError::UnsupportedOp {
            field: field.to_owned(),
            op: op.to_owned(),
            field_type: ft.as_str(),
        })
    }
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::String(_) => "string",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Null => "null",
        serde_json::Value::Object(_) => "object",
    }
}

fn require_set_size(field: &str, op: &str, len: usize) -> Result<(), FilterError> {
    if len > MAX_SET_OPERAND_SIZE {
        return Err(FilterError::MalformedLeaf(format!(
            "field `{field}` op `{op}` has {len} items; maximum is {MAX_SET_OPERAND_SIZE}"
        )));
    }
    Ok(())
}

fn require_nonempty_string(field: &str, op: &str, s: &str) -> Result<(), FilterError> {
    if s.is_empty() {
        Err(FilterError::MalformedLeaf(format!(
            "field `{field}` op `{op}` value must not be an empty string"
        )))
    } else {
        Ok(())
    }
}

/// Check that every element in `arr` satisfies `pred`; return `WrongValueShape`
/// pointing at the first offending element if not.
fn require_array_of(
    field: &str,
    op: &str,
    arr: &[serde_json::Value],
    expected: &'static str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> Result<(), FilterError> {
    for item in arr {
        if !pred(item) {
            return Err(FilterError::WrongValueShape {
                field: field.to_owned(),
                op: op.to_owned(),
                expected,
                got: json_type_name(item),
            });
        }
    }
    Ok(())
}

fn validate_value_shape(
    field: &str,
    op: &str,
    ft: FieldType,
    value: &serde_json::Value,
) -> Result<(), FilterError> {
    let scalar_err = |expected: &'static str| FilterError::WrongValueShape {
        field: field.to_owned(),
        op: op.to_owned(),
        expected,
        got: json_type_name(value),
    };

    match ft {
        FieldType::Str => match op {
            "in" | "nin" => {
                let arr = value
                    .as_array()
                    .ok_or_else(|| scalar_err("array of strings"))?;
                if arr.is_empty() {
                    return Err(FilterError::EmptyArray {
                        field: field.to_owned(),
                        op: op.to_owned(),
                    });
                }
                require_set_size(field, op, arr.len())?;
                for item in arr {
                    let s = item.as_str().ok_or_else(|| FilterError::WrongValueShape {
                        field: field.to_owned(),
                        op: op.to_owned(),
                        expected: "array of strings",
                        got: json_type_name(item),
                    })?;
                    require_nonempty_string(field, op, s)?;
                }
                Ok(())
            }
            _ => {
                let s = value
                    .as_str()
                    .ok_or_else(|| scalar_err("non-empty string"))?;
                require_nonempty_string(field, op, s)
            }
        },
        FieldType::Number => validate_number_value(field, op, value, scalar_err),
        FieldType::Boolean => value
            .is_boolean()
            .then_some(())
            .ok_or_else(|| scalar_err("boolean")),
        FieldType::Array => validate_array_field_value(field, op, value, scalar_err),
        // Timestamp: eq/neq require a valid RFC3339 string; in/nin require
        // a non-empty array of valid RFC3339 strings.
        FieldType::Timestamp => validate_timestamp_value(field, op, value, scalar_err),
    }
}

fn validate_number_value(
    field: &str,
    op: &str,
    value: &serde_json::Value,
    scalar_err: impl Fn(&'static str) -> FilterError,
) -> Result<(), FilterError> {
    match op {
        "in" | "nin" => {
            let arr = value
                .as_array()
                .ok_or_else(|| scalar_err("array of numbers"))?;
            if arr.is_empty() {
                return Err(FilterError::EmptyArray {
                    field: field.to_owned(),
                    op: op.to_owned(),
                });
            }
            require_set_size(field, op, arr.len())?;
            require_array_of(
                field,
                op,
                arr,
                "array of numbers",
                serde_json::Value::is_number,
            )
        }
        "between" => {
            let ok = value
                .as_array()
                .is_some_and(|a| a.len() == 2 && a[0].is_number() && a[1].is_number());
            ok.then_some(())
                .ok_or_else(|| scalar_err("[number, number]"))
        }
        _ => value
            .is_number()
            .then_some(())
            .ok_or_else(|| scalar_err("number")),
    }
}

fn validate_array_field_value(
    field: &str,
    op: &str,
    value: &serde_json::Value,
    scalar_err: impl Fn(&'static str) -> FilterError,
) -> Result<(), FilterError> {
    match op {
        "array_contains" => {
            let s = value
                .as_str()
                .ok_or_else(|| scalar_err("non-empty string"))?;
            require_nonempty_string(field, op, s)
        }
        "array_contains_any" | "array_contains_all" => {
            let arr = value
                .as_array()
                .ok_or_else(|| scalar_err("array of strings"))?;
            if arr.is_empty() {
                return Err(FilterError::EmptyArray {
                    field: field.to_owned(),
                    op: op.to_owned(),
                });
            }
            require_set_size(field, op, arr.len())?;
            for item in arr {
                let s = item.as_str().ok_or_else(|| FilterError::WrongValueShape {
                    field: field.to_owned(),
                    op: op.to_owned(),
                    expected: "array of strings",
                    got: json_type_name(item),
                })?;
                require_nonempty_string(field, op, s)?;
            }
            Ok(())
        }
        "array_size_eq" => {
            let n = value.as_u64();
            if n.is_none() {
                return Err(FilterError::InvalidArraySize {
                    field: field.to_owned(),
                    op: op.to_owned(),
                    value: value.to_string(),
                });
            }
            Ok(())
        }
        _ => unreachable!("op validated by validate_op_for_type"),
    }
}

fn validate_ts(field: &str, op: &str, s: &str) -> Result<(), FilterError> {
    Rfc3339Timestamp::parse(s)
        .map(|_| ())
        .map_err(|e| FilterError::InvalidTimestamp {
            field: field.to_owned(),
            op: op.to_owned(),
            value: s.to_owned(),
            reason: e.to_string(),
        })
}

fn validate_timestamp_value(
    field: &str,
    op: &str,
    value: &serde_json::Value,
    scalar_err: impl Fn(&'static str) -> FilterError,
) -> Result<(), FilterError> {
    match op {
        "in" | "nin" => {
            let arr = value
                .as_array()
                .ok_or_else(|| scalar_err("array of RFC3339 strings"))?;
            if arr.is_empty() {
                return Err(FilterError::EmptyArray {
                    field: field.to_owned(),
                    op: op.to_owned(),
                });
            }
            require_set_size(field, op, arr.len())?;
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err(FilterError::WrongValueShape {
                        field: field.to_owned(),
                        op: op.to_owned(),
                        expected: "array of RFC3339 strings",
                        got: json_type_name(item),
                    });
                };
                validate_ts(field, op, s)?;
            }
            Ok(())
        }
        _ => {
            let s = value.as_str().ok_or_else(|| scalar_err("RFC3339 string"))?;
            validate_ts(field, op, s)
        }
    }
}

// ── compile_filter ────────────────────────────────────────────────────────────

/// A compiled filter: a SQL fragment using `?` placeholders and the matching
/// positional parameters.
///
/// Embed `sql` directly inside a `WHERE` clause and bind `params` in order.
/// No user values are ever interpolated into `sql`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledFilter {
    /// SQL fragment for use inside a `WHERE` clause. Uses `?` placeholders.
    pub sql: String,
    /// Values to bind at the `?` positions, in order.
    pub params: Vec<serde_json::Value>,
}

/// Compile a [`ValidatedFilter`] to a parameterized `SQLite` `WHERE` fragment.
///
/// Accepts only a [`ValidatedFilter`] produced by [`validate_filter`],
/// guaranteeing that all semantic checks and the parameter budget have
/// already passed.
#[must_use]
pub fn compile_filter(filter: ValidatedFilter<'_>) -> CompiledFilter {
    let mut params = Vec::new();
    let sql = compile_node(filter.0, &mut params);
    CompiledFilter { sql, params }
}

fn compile_node(filter: &SearchArgsFilters, params: &mut Vec<serde_json::Value>) -> String {
    match filter {
        SearchArgsFilters::And { and } => {
            let parts: Vec<String> = and.iter().map(|f| compile_node(f, params)).collect();
            format!("({})", parts.join(" AND "))
        }
        SearchArgsFilters::Or { or } => {
            let parts: Vec<String> = or.iter().map(|f| compile_node(f, params)).collect();
            format!("({})", parts.join(" OR "))
        }
        SearchArgsFilters::Not { not } => {
            format!("(NOT {})", compile_node(not, params))
        }
        SearchArgsFilters::Leaf(v) => compile_leaf(v, params),
    }
}

/// Map a validated field name to its SQL column/expression in the `records` table.
///
/// Three categories:
/// 1. Physical scalar columns on `records` (`kind`, `class`, `visibility`,
///    `confidence`, `path`, `version`, `is_static`, `tombstoned`, `active`)
///    map 1-to-1. These are written by
///    `cairn-store-sqlite::store::projection::ProjectedRow` and indexed for
///    cheap predicates.
/// 2. JSON-array columns on `records` (`tags`, `actor_chain`) emit the bare
///    column name; the array ops in `compile_array_op` open them via
///    `json_each`.
/// 3. Sub-objects of the canonical `record_json` blob — `provenance.created_at`,
///    and the free-form `extra_frontmatter` map (`title`, `category`,
///    `priority`, `backlinks`) — emit `json_extract(...)` expressions. The
///    store side surfaces these via VIRTUAL generated columns in
///    `crates/cairn-store-sqlite/src/migrations/sql/0011_filter_alignment.sql`
///    so the same SQL evaluates correctly against the live schema.
///
/// Routing store-owned fields (`path`, `is_static`, `tombstoned`, `active`,
/// `version`) through `extra_frontmatter` would silently miss every record
/// the projection wrote — those values live on the physical columns, not
/// in the user-supplied frontmatter map.
//
// `clippy::doc_markdown` triggers on bare identifiers in prose; the field
// names above are wrapped in backticks already, so the lint is satisfied.
fn field_col(name: &str) -> &'static str {
    match name {
        // ── Physical scalar columns on records ────────────────────────────
        "kind" => "kind",
        "class" => "class",
        "visibility" => "visibility",
        "confidence" => "confidence",
        "path" => "path",
        "version" => "version",
        "is_static" => "is_static",
        "tombstoned" => "tombstoned",
        "active" => "active",

        // ── Sub-object inside record_json (surfaced via the `provenance`
        //    VIRTUAL generated column in migration 0011) ───────────────────
        "created_at" => "json_extract(provenance, '$.created_at')",

        // ── JSON-array columns on records ─────────────────────────────────
        // `tags` is Vec<String> — flat string array; standard json_each works.
        // `actor_chain` is Vec<ActorChainEntry> — struct array; see compile_array_op.
        "tags" => "tags",
        "actor_chain" => "actor_chain",

        // ── Fields the ingest caller put into `extra_frontmatter` ─────────
        // These never have a physical column — they are user-supplied
        // frontmatter that round-trips through `record_json`. The store
        // exposes the wrapping object via the `extra_frontmatter` VIRTUAL
        // column in migration 0011.
        "title" => "json_extract(extra_frontmatter, '$.title')",
        "category" => "json_extract(extra_frontmatter, '$.category')",
        "priority" => "json_extract(extra_frontmatter, '$.priority')",
        // `backlinks` is a JSON array inside extra_frontmatter.
        "backlinks" => "json_extract(extra_frontmatter, '$.backlinks')",

        // Unreachable: validate_filter rejects any name not in KNOWN_FIELDS.
        _ => unreachable!("field not in KNOWN_FIELDS; validate_filter must be called first"),
    }
}

fn compile_leaf(v: &serde_json::Value, params: &mut Vec<serde_json::Value>) -> String {
    let Some(obj) = v.as_object() else {
        unreachable!("leaf is always a JSON object; validate_filter must be called first");
    };
    let Some(field) = obj["field"].as_str() else {
        unreachable!("leaf field is always a string; validate_filter must be called first");
    };
    let Some(op) = obj["op"].as_str() else {
        unreachable!("leaf op is always a string; validate_filter must be called first");
    };
    let value = &obj["value"];
    let col = field_col(field);

    let ft = field_type(field).unwrap_or_else(|| {
        unreachable!("field is in allowlist; validate_filter must be called before compile_filter")
    });

    match ft {
        FieldType::Array => compile_array_op(col, op, value, params),
        _ => compile_scalar_op(col, op, value, params),
    }
}

fn compile_scalar_op(
    col: &str,
    op: &str,
    value: &serde_json::Value,
    params: &mut Vec<serde_json::Value>,
) -> String {
    match op {
        "eq" => {
            params.push(value.clone());
            format!("{col} = ?")
        }
        "neq" => {
            params.push(value.clone());
            format!("{col} != ?")
        }
        "lt" => {
            params.push(value.clone());
            format!("{col} < ?")
        }
        "lte" => {
            params.push(value.clone());
            format!("{col} <= ?")
        }
        "gt" => {
            params.push(value.clone());
            format!("{col} > ?")
        }
        "gte" => {
            params.push(value.clone());
            format!("{col} >= ?")
        }
        "in" => {
            let Some(arr) = value.as_array() else {
                unreachable!("in value is array after validation");
            };
            let placeholders = "?, ".repeat(arr.len());
            let placeholders = placeholders.trim_end_matches(", ");
            for v in arr {
                params.push(v.clone());
            }
            format!("{col} IN ({placeholders})")
        }
        "nin" => {
            let Some(arr) = value.as_array() else {
                unreachable!("nin value is array after validation");
            };
            let placeholders = "?, ".repeat(arr.len());
            let placeholders = placeholders.trim_end_matches(", ");
            for v in arr {
                params.push(v.clone());
            }
            format!("{col} NOT IN ({placeholders})")
        }
        "between" => {
            let Some(arr) = value.as_array() else {
                unreachable!("between value is 2-element array after validation");
            };
            params.push(arr[0].clone());
            params.push(arr[1].clone());
            format!("{col} BETWEEN ? AND ?")
        }
        "string_contains" => {
            // Use `instr()` for substring matching — avoids `LIKE` special-char escaping.
            params.push(value.clone());
            format!("instr({col}, ?) > 0")
        }
        "string_starts_with" => {
            // Use substr/length for case-sensitive, metachar-safe prefix matching
            // consistent with instr() used by string_contains.
            params.push(value.clone());
            params.push(value.clone());
            format!("substr({col}, 1, length(?)) = ?")
        }
        "string_ends_with" => {
            // Use substr/length for case-sensitive, metachar-safe suffix matching.
            // Three ?'s: length(?) twice, then the equality value.
            params.push(value.clone());
            params.push(value.clone());
            params.push(value.clone());
            format!("substr({col}, -length(?), length(?)) = ?")
        }
        _ => unreachable!("op was validated by validate_filter before compile_filter"),
    }
}

fn compile_array_op(
    col: &str,
    op: &str,
    value: &serde_json::Value,
    params: &mut Vec<serde_json::Value>,
) -> String {
    // `actor_chain` stores Vec<ActorChainEntry> JSON objects. Callers filter by
    // identity string (e.g. "agt:claude"), so extract the `identity` field from
    // each element. All other array columns store plain strings.
    let elem_expr = if col == "actor_chain" {
        "json_extract(value, '$.identity')"
    } else {
        "value"
    };

    match op {
        "array_contains" => {
            params.push(value.clone());
            format!("EXISTS (SELECT 1 FROM json_each({col}) WHERE {elem_expr} = ?)")
        }
        "array_contains_any" => {
            let Some(arr) = value.as_array() else {
                unreachable!("array_contains_any value is array after validation");
            };
            let placeholders = "?, ".repeat(arr.len());
            let placeholders = placeholders.trim_end_matches(", ");
            for v in arr {
                params.push(v.clone());
            }
            format!("EXISTS (SELECT 1 FROM json_each({col}) WHERE {elem_expr} IN ({placeholders}))")
        }
        "array_contains_all" => {
            let Some(arr) = value.as_array() else {
                unreachable!("array_contains_all value is array after validation");
            };
            // Deduplicate to avoid COUNT(DISTINCT) mismatch when the caller
            // provides repeated values (e.g. ["infra","infra"] must not require
            // two distinct matches — the row already satisfies the intent).
            let mut seen = std::collections::HashSet::new();
            let unique: Vec<&serde_json::Value> =
                arr.iter().filter(|v| seen.insert(v.as_str())).collect();
            let n = unique.len();
            let placeholders = "?, ".repeat(n);
            let placeholders = placeholders.trim_end_matches(", ");
            for v in &unique {
                params.push((*v).clone());
            }
            params.push(serde_json::Value::Number(serde_json::Number::from(
                n as u64,
            )));
            format!(
                "(SELECT COUNT(DISTINCT {elem_expr}) FROM json_each({col}) WHERE {elem_expr} IN ({placeholders})) = ?"
            )
        }
        "array_size_eq" => {
            params.push(value.clone());
            format!("json_array_length({col}) = ?")
        }
        _ => unreachable!("op was validated by validate_filter before compile_filter"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_type_lookup_known() {
        assert_eq!(field_type("kind"), Some(FieldType::Str));
        assert_eq!(field_type("priority"), Some(FieldType::Number));
        assert_eq!(field_type("is_static"), Some(FieldType::Boolean));
        assert_eq!(field_type("tags"), Some(FieldType::Array));
    }

    #[test]
    fn field_type_lookup_unknown() {
        assert_eq!(field_type("not_a_real_field"), None);
        assert_eq!(field_type(""), None);
    }
}
