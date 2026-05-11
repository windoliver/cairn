//! Conformance fixtures shared by `crates/cairn-mcp/tests/mcp_conformance.rs`.
//!
//! Each fixture is a `(request, response)` envelope pair embedded from
//! `fixtures/v0/mcp/conformance/` at compile time via `include_dir!`. Each
//! verb-group directory carries a `_meta.json` that names per-case kind and
//! config overrides; the loader pairs files and meta entries strictly, panicking
//! on orphans or missing entries (brief §8.0.a fail-closed invariant projected
//! into the test infra layer).

use include_dir::{Dir, include_dir};
use serde::Deserialize;
use serde_json::{Map, Value};

/// One fixture entry, ready for replay.
#[derive(Debug, Clone)]
pub struct ConformanceCase {
    /// `"<verb_dir>/<case_id>"` — e.g., `"search/err_semantic_disabled"`.
    pub id: String,
    /// Cairn verb name as it appears in the envelope, e.g., `"search"`. For
    /// cross-verb directories (`_envelope`, `_extension`) this is the directory
    /// name (callers reading `verb` should treat values starting with `_` as
    /// synthetic groupings, not real verbs).
    pub verb: String,
    /// What the runner should expect when dispatching the request.
    pub kind: CaseKind,
    /// Per-case capability gates fed into `build_handler_for`.
    pub config: ConfigOverrides,
    /// Canonical envelope per brief §8.0.b.
    pub request: serde_json::Value,
    /// Expected canonical envelope after replay.
    pub response: serde_json::Value,
}

/// What outcome a case asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// `status = "committed"` (or `"aborted"` for valid-but-rejected
    /// non-capability cases — none expected at v0.1).
    Ok,
    /// `status = "rejected"`, `error.code = "InvalidArgs"` or
    /// `"InvalidFilter"` or `"UnknownVerb"`.
    InvalidArgs,
    /// `status = "rejected"`, `error.code = "CapabilityUnavailable"`,
    /// `error.data.capability` matches a known capability id.
    CapabilityRejected,
    /// Same as `CapabilityRejected` but for verbs from an extension namespace
    /// that the runtime does not advertise (brief §8.0.a, extensions table).
    ExtensionRejected,
}

/// Per-case capability gates. Mirrors the subset of
/// `cairn_core::status::CapabilityGates` and `wiring::*_WIRED` that conformance
/// cases need to switch on. Typed booleans, not strings — drift between this
/// struct and `cairn-core::status::advertise` is caught by
/// `config_overrides_match_advertised_capabilities` in the runner self-tests.
// Six booleans are intentional here: each maps 1-to-1 to a distinct
// capability gate in `cairn-core::status::advertise`. Refactoring into a
// state machine would obscure that direct correspondence.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ConfigOverrides {
    /// Whether keyword search (`fts5`) is advertised by the test handler.
    pub keyword_search: bool,
    /// Whether semantic (vector) search is advertised; requires an embedding provider.
    pub semantic_search: bool,
    /// Whether hybrid search (keyword + semantic) is advertised.
    pub hybrid_search: bool,
    /// Whether policy-trace capture is advertised.
    pub policy_trace: bool,
    /// Whether the aggregate extension namespace is advertised.
    pub aggregate_extension_enabled: bool,
    /// Whether the admin extension namespace is advertised.
    pub admin_extension_enabled: bool,
}

impl Default for ConfigOverrides {
    /// P0 baseline — keyword on, semantic + hybrid require an embedding
    /// provider to be ready (off by default in tests), extensions disabled.
    fn default() -> Self {
        Self {
            keyword_search: true,
            semantic_search: false,
            hybrid_search: false,
            policy_trace: false,
            aggregate_extension_enabled: false,
            admin_extension_enabled: false,
        }
    }
}

impl ConfigOverrides {
    /// Convenience: every search mode on (requires the test handler to be
    /// constructed with `embedding_provider_ready = true`).
    #[must_use]
    pub fn search_all_on() -> Self {
        Self {
            keyword_search: true,
            semantic_search: true,
            hybrid_search: true,
            ..Self::default()
        }
    }
}

/// Embedded fixture tree — `fixtures/v0/mcp/conformance/` at compile time.
static CONFORMANCE_DIR: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../fixtures/v0/mcp/conformance");

#[derive(Debug, Deserialize)]
struct MetaFile {
    cases: std::collections::BTreeMap<String, MetaEntry>,
}

#[derive(Debug, Deserialize)]
struct MetaEntry {
    kind: CaseKind,
    #[serde(default)]
    config: ConfigOverrides,
}

/// Load every fixture under `fixtures/v0/mcp/conformance/`, sorted by id.
///
/// # Panics
///
/// Panics if:
/// - A `_meta.json` is malformed.
/// - A case named in `_meta.json` is missing its `.request.json` or
///   `.response.json`.
/// - A `.request.json` / `.response.json` on disk is not named in `_meta.json`.
#[must_use]
pub fn load_all() -> Vec<ConformanceCase> {
    let mut out = Vec::new();
    for verb_dir in CONFORMANCE_DIR.dirs() {
        let verb = verb_dir
            .path()
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| {
                panic!("verb dir has non-UTF-8 name: {}", verb_dir.path().display())
            });
        let meta_file = verb_dir
            .get_file(format!("{}/_meta.json", verb_dir.path().display()))
            .unwrap_or_else(|| panic!("missing _meta.json in {}", verb_dir.path().display()));
        let meta: MetaFile = serde_json::from_slice(meta_file.contents())
            .unwrap_or_else(|e| panic!("malformed _meta.json in {verb}: {e}"));

        // Build the set of on-disk case ids from filenames.
        let mut on_disk = std::collections::BTreeSet::<String>::new();
        for f in verb_dir.files() {
            let name = f
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_else(|| {
                    panic!("fixture file has non-UTF-8 name: {}", f.path().display())
                });
            if name == "_meta.json" {
                continue;
            }
            if let Some(case_id) = name.strip_suffix(".request.json") {
                on_disk.insert(case_id.to_string());
            } else if let Some(case_id) = name.strip_suffix(".response.json") {
                on_disk.insert(case_id.to_string());
            } else {
                panic!("unexpected file {} in {}", name, verb_dir.path().display());
            }
        }

        // Orphan check: every on-disk case id must be in _meta.json.
        for case_id in &on_disk {
            assert!(
                meta.cases.contains_key(case_id),
                "fixture {verb}/{case_id} has no _meta.json entry",
            );
        }
        // Reverse orphan check: every _meta.json entry must have files on disk.
        for case_id in meta.cases.keys() {
            assert!(
                on_disk.contains(case_id),
                "_meta.json entry {verb}/{case_id} has no .request/.response files",
            );
            let req = verb_dir
                .get_file(format!(
                    "{}/{}.request.json",
                    verb_dir.path().display(),
                    case_id
                ))
                .unwrap_or_else(|| panic!("missing {verb}/{case_id}.request.json"));
            let resp = verb_dir
                .get_file(format!(
                    "{}/{}.response.json",
                    verb_dir.path().display(),
                    case_id
                ))
                .unwrap_or_else(|| panic!("missing {verb}/{case_id}.response.json"));
            let entry = &meta.cases[case_id];
            out.push(ConformanceCase {
                id: format!("{verb}/{case_id}"),
                verb: verb.to_string(),
                kind: entry.kind,
                config: entry.config,
                request: serde_json::from_slice(req.contents())
                    .unwrap_or_else(|e| panic!("{verb}/{case_id}.request.json: {e}")),
                response: serde_json::from_slice(resp.contents())
                    .unwrap_or_else(|e| panic!("{verb}/{case_id}.response.json: {e}")),
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Load one case by id (e.g., `"search/ok_keyword"`).
///
/// # Panics
///
/// If the case does not exist.
#[must_use]
pub fn load_case(id: &str) -> ConformanceCase {
    load_all()
        .into_iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no conformance case with id {id}"))
}

/// Replace non-deterministic fields with stable placeholders and sort object
/// keys recursively. Idempotent.
///
/// Volatile fields (replaced):
/// - `operation_id` (top-level or nested) → `"<OPERATION_ID>"`
/// - `data.server_info.started_at` → `"<STARTED_AT>"`
/// - `data.server_info.incarnation` → `"<INCARNATION>"`
/// - `data.server_info.build` → `"<BUILD>"`
/// - `data.challenge.nonce` → `"<NONCE>"`
/// - `data.challenge.expires_at` → `"<EXPIRES_AT>"`
/// - any `policy_trace[*].timestamp` → `"<TIMESTAMP>"`
///
/// Every other field is preserved as-is. Object keys are sorted lexicographically.
#[must_use]
pub fn canonicalize(value: &Value) -> Value {
    let mut v = value.clone();
    replace_volatile_in_place(&mut v, &[]);
    sort_keys_in_place(&mut v);
    v
}

const VOLATILE_LEAF_KEYS: &[&str] = &["operation_id"];

/// Maps a path (sequence of keys from the envelope root) to the placeholder
/// used when the leaf at that path is replaced.
const VOLATILE_PATHS: &[(&[&str], &str)] = &[
    (&["data", "server_info", "started_at"], "<STARTED_AT>"),
    (&["data", "server_info", "incarnation"], "<INCARNATION>"),
    (&["data", "server_info", "build"], "<BUILD>"),
    (&["data", "challenge", "nonce"], "<NONCE>"),
    (&["data", "challenge", "expires_at"], "<EXPIRES_AT>"),
];

fn replace_volatile_in_place(v: &mut Value, path: &[String]) {
    match v {
        Value::Object(map) => {
            // Path-based replacements (specific to known structural paths).
            for (target, placeholder) in VOLATILE_PATHS {
                if path.len() + 1 == target.len() {
                    let prefix_matches = path
                        .iter()
                        .zip(target.iter())
                        .all(|(p, t)| p.as_str() == *t);
                    if prefix_matches {
                        let leaf = target[target.len() - 1];
                        if map.contains_key(leaf) {
                            map.insert(leaf.to_string(), Value::String((*placeholder).into()));
                        }
                    }
                }
            }
            // Leaf-key replacements (apply anywhere in the tree).
            for key in VOLATILE_LEAF_KEYS {
                if map.contains_key(*key) {
                    let placeholder = if *key == "operation_id" {
                        "<OPERATION_ID>"
                    } else {
                        continue;
                    };
                    map.insert((*key).into(), Value::String(placeholder.into()));
                }
            }
            // Recurse with appended path.
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                let mut next_path = path.to_vec();
                next_path.push(k.clone());
                if let Some(child) = map.get_mut(&k) {
                    replace_volatile_in_place(child, &next_path);
                }
            }
        }
        Value::Array(arr) => {
            // Policy-trace timestamp scrub: when the parent key is "policy_trace",
            // each element's `timestamp` (if present) becomes "<TIMESTAMP>".
            let in_policy_trace = matches!(path.last(), Some(k) if k == "policy_trace");
            for item in arr.iter_mut() {
                if in_policy_trace
                    && let Value::Object(map) = &mut *item
                    && map.contains_key("timestamp")
                {
                    map.insert("timestamp".into(), Value::String("<TIMESTAMP>".into()));
                }
                replace_volatile_in_place(item, path);
            }
        }
        _ => {}
    }
}

fn sort_keys_in_place(v: &mut Value) {
    match v {
        Value::Object(map) => {
            let mut sorted: Map<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            sorted.sort_keys();
            for child in sorted.values_mut() {
                sort_keys_in_place(child);
            }
            *map = sorted;
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                sort_keys_in_place(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_all_returns_at_least_one_case() {
        let cases = load_all();
        assert!(
            !cases.is_empty(),
            "expected at least one fixture under fixtures/v0/mcp/conformance/"
        );
    }

    #[test]
    fn load_all_pairs_request_with_response() {
        let cases = load_all();
        for c in &cases {
            assert!(
                c.request.is_object(),
                "case {}: request must be a JSON object",
                c.id
            );
            assert!(
                c.response.is_object(),
                "case {}: response must be a JSON object",
                c.id
            );
        }
    }

    #[test]
    fn load_case_returns_by_id() {
        let c = load_case("search/ok_keyword");
        assert_eq!(c.id, "search/ok_keyword");
        assert_eq!(c.verb, "search");
        assert_eq!(c.kind, CaseKind::Ok);
    }

    #[test]
    fn case_kind_deserializes_snake_case() {
        let v: CaseKind = serde_json::from_str("\"ok\"").unwrap();
        assert_eq!(v, CaseKind::Ok);
        let v: CaseKind = serde_json::from_str("\"invalid_args\"").unwrap();
        assert_eq!(v, CaseKind::InvalidArgs);
        let v: CaseKind = serde_json::from_str("\"capability_rejected\"").unwrap();
        assert_eq!(v, CaseKind::CapabilityRejected);
        let v: CaseKind = serde_json::from_str("\"extension_rejected\"").unwrap();
        assert_eq!(v, CaseKind::ExtensionRejected);
    }

    #[test]
    fn config_overrides_default_is_p0_baseline() {
        let c = ConfigOverrides::default();
        assert!(c.keyword_search);
        assert!(!c.semantic_search);
        assert!(!c.hybrid_search);
        assert!(!c.aggregate_extension_enabled);
    }

    #[test]
    fn config_overrides_deserialize_partial() {
        let v: ConfigOverrides = serde_json::from_str(r#"{"semantic_search": true}"#).unwrap();
        assert!(v.semantic_search);
        assert!(v.keyword_search); // serde(default) applied → default = true
    }

    #[test]
    fn canonicalize_sorts_keys_recursively() {
        let unsorted = serde_json::json!({
            "z": 1,
            "a": { "z": 2, "a": 3 }
        });
        let canon = canonicalize(&unsorted);
        let s = serde_json::to_string(&canon).unwrap();
        // Inner object keys are sorted: "a" before "z".
        assert_eq!(s, r#"{"a":{"a":3,"z":2},"z":1}"#);
    }

    #[test]
    fn canonicalize_replaces_operation_id() {
        let v = serde_json::json!({
            "operation_id": "01HQZX9F5N0000000000000000",
            "status": "committed"
        });
        let canon = canonicalize(&v);
        assert_eq!(canon["operation_id"], serde_json::json!("<OPERATION_ID>"));
    }

    #[test]
    fn canonicalize_replaces_server_info_volatile_fields() {
        let v = serde_json::json!({
            "data": {
                "server_info": {
                    "build": "abc123",
                    "incarnation": "01HQZ...",
                    "started_at": "2026-05-11T10:00:00Z",
                    "version": "0.1.0"
                }
            }
        });
        let canon = canonicalize(&v);
        let s = &canon["data"]["server_info"];
        assert_eq!(s["build"], serde_json::json!("<BUILD>"));
        assert_eq!(s["incarnation"], serde_json::json!("<INCARNATION>"));
        assert_eq!(s["started_at"], serde_json::json!("<STARTED_AT>"));
        assert_eq!(s["version"], serde_json::json!("0.1.0"));
    }

    #[test]
    fn canonicalize_replaces_handshake_challenge_fields() {
        let v = serde_json::json!({
            "data": {
                "challenge": {
                    "expires_at": 1_735_000_000_000_u64,
                    "nonce": "base64stuff=="
                }
            }
        });
        let canon = canonicalize(&v);
        let c = &canon["data"]["challenge"];
        assert_eq!(c["nonce"], serde_json::json!("<NONCE>"));
        assert_eq!(c["expires_at"], serde_json::json!("<EXPIRES_AT>"));
    }

    #[test]
    fn canonicalize_is_idempotent_on_every_fixture() {
        for case in load_all() {
            let a = canonicalize(&case.response);
            let b = canonicalize(&a);
            assert_eq!(a, b, "canonicalize is not idempotent on {}", case.id);
        }
    }
}
