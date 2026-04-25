// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const EXPECTED_VERB_IDS: [&str; 8] = [
    "ingest",
    "search",
    "retrieve",
    "summarize",
    "assemble_hot",
    "capture_trace",
    "lint",
    "forget",
];

const EXPECTED_CONTRACT: &str = "cairn.mcp.v1";

fn schema_dir() -> &'static Path {
    Path::new(cairn_idl::SCHEMA_DIR)
}

fn read_json(path: &Path) -> Value {
    let bytes =
        fs::read(path).unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("failed to parse {} as JSON: {err}", path.display()))
}

fn manifest() -> Value {
    read_json(&schema_dir().join("index.json"))
}

fn manifest_paths() -> Vec<PathBuf> {
    let manifest = manifest();
    let files = manifest
        .get("x-cairn-files")
        .and_then(Value::as_object)
        .expect("index.json: x-cairn-files must be an object");
    let mut out: Vec<PathBuf> = Vec::new();
    for (category, arr) in files {
        let arr = arr
            .as_array()
            .unwrap_or_else(|| panic!("x-cairn-files.{category} must be an array"));
        for entry in arr {
            let rel = entry
                .as_str()
                .unwrap_or_else(|| panic!("x-cairn-files.{category} entries must be strings"));
            out.push(schema_dir().join(rel));
        }
    }
    out
}

fn require_object<'a>(v: &'a Value, path: &Path) -> &'a serde_json::Map<String, Value> {
    v.as_object()
        .unwrap_or_else(|| panic!("{}: top-level value must be a JSON object", path.display()))
}

#[test]
fn manifest_parses_and_has_required_top_level_keys() {
    let m = manifest();
    let path = schema_dir().join("index.json");
    let obj = require_object(&m, &path);
    for key in [
        "$schema",
        "$id",
        "title",
        "x-cairn-contract",
        "x-cairn-files",
        "x-cairn-verb-ids",
    ] {
        assert!(
            obj.contains_key(key),
            "index.json missing required key {key}"
        );
    }
    assert_eq!(
        obj.get("x-cairn-contract").and_then(Value::as_str),
        Some(EXPECTED_CONTRACT),
        "index.json x-cairn-contract mismatch"
    );
}

#[test]
fn manifest_verb_ids_match_eight_verb_set_in_order() {
    let m = manifest();
    let verb_ids: Vec<String> = m
        .get("x-cairn-verb-ids")
        .and_then(Value::as_array)
        .expect("index.json x-cairn-verb-ids must be an array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("x-cairn-verb-ids entries must be strings")
                .to_string()
        })
        .collect();
    let expected: Vec<String> = EXPECTED_VERB_IDS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        verb_ids, expected,
        "x-cairn-verb-ids must match §8.0 exactly, in order"
    );
}

#[test]
fn every_manifest_file_exists_and_parses_and_has_top_level_fields() {
    for path in manifest_paths() {
        assert!(
            path.is_file(),
            "manifest lists {path:?} but file does not exist"
        );
        let v = read_json(&path);
        let obj = require_object(&v, &path);
        for key in ["$schema", "$id", "title", "x-cairn-contract"] {
            assert!(
                obj.contains_key(key),
                "{path:?} missing required top-level key {key}"
            );
        }
        assert_eq!(
            obj.get("x-cairn-contract").and_then(Value::as_str),
            Some(EXPECTED_CONTRACT),
            "{path:?} x-cairn-contract mismatch"
        );
    }
}

#[test]
fn manifest_and_filesystem_are_bijective() {
    // Every .json file under schema/ (except index.json) must be listed.
    let mut on_disk: BTreeSet<PathBuf> = BTreeSet::new();
    walk_json(schema_dir(), &mut on_disk);
    on_disk.remove(&schema_dir().join("index.json"));

    let in_manifest: BTreeSet<PathBuf> = manifest_paths().into_iter().collect();

    let missing_in_manifest: Vec<_> = on_disk.difference(&in_manifest).collect();
    let missing_on_disk: Vec<_> = in_manifest.difference(&on_disk).collect();
    assert!(
        missing_in_manifest.is_empty(),
        "files on disk but not in manifest: {missing_in_manifest:?}"
    );
    assert!(
        missing_on_disk.is_empty(),
        "files in manifest but missing on disk: {missing_on_disk:?}"
    );
}

fn walk_json(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    for entry in fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read dir {}: {err}", dir.display()))
    {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            walk_json(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            out.insert(path);
        }
    }
}

fn capabilities_enum() -> BTreeSet<String> {
    let caps = read_json(&schema_dir().join("capabilities/capabilities.json"));
    let arr = caps
        .get("oneOf")
        .and_then(Value::as_array)
        .expect("capabilities.json: oneOf must be an array");
    arr.iter()
        .map(|entry| {
            entry
                .get("const")
                .and_then(Value::as_str)
                .expect("capabilities.json oneOf entries must have a const string")
                .to_string()
        })
        .collect()
}

fn collect_capability_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(cap) = map.get("x-cairn-capability").and_then(Value::as_str) {
                out.push(cap.to_string());
            }
            for (_, child) in map {
                collect_capability_refs(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_capability_refs(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_x_cairn_capability_is_in_capabilities_enum() {
    let enum_set = capabilities_enum();
    for path in manifest_paths() {
        let v = read_json(&path);
        let mut refs: Vec<String> = Vec::new();
        collect_capability_refs(&v, &mut refs);
        for cap in refs {
            assert!(
                enum_set.contains(&cap),
                "{path:?} references capability {cap:?} that is not in capabilities.json"
            );
        }
    }
}

fn collect_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(r) = map.get("$ref").and_then(Value::as_str) {
                out.push(r.to_string());
            }
            for (_, child) in map {
                collect_refs(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, out);
            }
        }
        _ => {}
    }
}

fn resolve_fragment<'a>(doc: &'a Value, fragment: &str) -> Option<&'a Value> {
    // Fragment per RFC 6901 JSON Pointer. Empty fragment ⇒ root.
    if fragment.is_empty() {
        return Some(doc);
    }
    if !fragment.starts_with('/') {
        return None;
    }
    let mut current = doc;
    for raw in fragment.split('/').skip(1) {
        // Decode JSON Pointer escapes: ~1 → /, ~0 → ~. Order matters (~1 first).
        let seg = raw.replace("~1", "/").replace("~0", "~");
        match current {
            Value::Object(m) => {
                current = m.get(&seg)?;
            }
            Value::Array(a) => {
                let idx: usize = seg.parse().ok()?;
                current = a.get(idx)?;
            }
            _ => return None,
        }
    }
    Some(current)
}

fn ref_resolves(source_path: &Path, reference: &str) -> bool {
    let (file_part, fragment) = reference.split_once('#').unwrap_or((reference, ""));
    let target_doc: Value = if file_part.is_empty() {
        // Local fragment — resolve against the source document itself.
        read_json(source_path)
    } else {
        let source_dir = source_path.parent().expect("schema file has parent");
        let target = source_dir.join(file_part);
        if !target.is_file() {
            return false;
        }
        read_json(&target)
    };
    resolve_fragment(&target_doc, fragment).is_some()
}

fn error_code_enum() -> BTreeSet<String> {
    let err = read_json(&schema_dir().join("errors/error.json"));
    let arr = err
        .get("oneOf")
        .and_then(Value::as_array)
        .expect("errors/error.json: oneOf must be an array");
    arr.iter()
        .map(|branch| {
            branch
                .get("properties")
                .and_then(|p| p.get("code"))
                .and_then(|c| c.get("const"))
                .and_then(Value::as_str)
                .expect("every error.oneOf branch must pin properties.code.const")
                .to_string()
        })
        .collect()
}

fn response_inline_family_enums() -> std::collections::BTreeMap<String, BTreeSet<String>> {
    // Extract the rejected/aborted code enums from the if/then dispatch
    // arms in envelope/response.json. These are what JSON Schema validators
    // actually enforce and must agree with the x-cairn-error-code-families
    // vendor key codegen will consume.
    let resp = read_json(&schema_dir().join("envelope/response.json"));
    let all_of = resp
        .get("allOf")
        .and_then(Value::as_array)
        .expect("response.json allOf must be an array");

    let mut map: std::collections::BTreeMap<String, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for arm in all_of {
        let Some(if_obj) = arm.get("if").and_then(Value::as_object) else {
            continue;
        };
        let Some(status_const) = if_obj
            .get("properties")
            .and_then(|p| p.get("status"))
            .and_then(|s| s.get("const"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if status_const != "rejected" && status_const != "aborted" {
            continue;
        }
        let Some(code_enum) = arm
            .get("then")
            .and_then(|t| t.get("properties"))
            .and_then(|p| p.get("error"))
            .and_then(|e| e.get("properties"))
            .and_then(|p| p.get("code"))
            .and_then(|c| c.get("enum"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let set: BTreeSet<String> = code_enum
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        map.insert(status_const.to_string(), set);
    }
    map
}

#[test]
fn response_inline_enums_match_x_cairn_error_code_families_vendor_key() {
    let resp = read_json(&schema_dir().join("envelope/response.json"));
    let vendor = resp
        .get("x-cairn-error-code-families")
        .and_then(Value::as_object)
        .expect("response.json must carry x-cairn-error-code-families");
    let vendor_map: std::collections::BTreeMap<String, BTreeSet<String>> = vendor
        .iter()
        .map(|(family, codes)| {
            let set: BTreeSet<String> = codes
                .as_array()
                .unwrap_or_else(|| panic!("family {family} must be an array"))
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            (family.clone(), set)
        })
        .collect();

    let inline = response_inline_family_enums();
    assert_eq!(
        vendor_map, inline,
        "x-cairn-error-code-families must exactly match the rejected/aborted if/then enum arms in response.json — otherwise codegen (reading the vendor key) and validators (enforcing the inline arms) will disagree"
    );
}

#[test]
fn every_error_code_is_in_exactly_one_response_status_family() {
    let resp = read_json(&schema_dir().join("envelope/response.json"));
    let families = resp
        .get("x-cairn-error-code-families")
        .and_then(Value::as_object)
        .expect("response.json: x-cairn-error-code-families must be an object");

    let mut seen: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (family, codes) in families {
        let codes = codes
            .as_array()
            .unwrap_or_else(|| panic!("family {family} must be an array"));
        for code in codes {
            let code = code
                .as_str()
                .unwrap_or_else(|| panic!("family {family} entries must be strings"))
                .to_string();
            seen.entry(code).or_default().push(family.clone());
        }
    }

    let defined = error_code_enum();
    // Every defined error code appears in exactly one family.
    for code in &defined {
        let families_for = seen.get(code);
        match families_for {
            None => panic!("error code {code:?} has no response status family"),
            Some(fs) if fs.len() != 1 => {
                panic!("error code {code:?} appears in multiple families: {fs:?}")
            }
            _ => {}
        }
    }
    // Every family entry is a real error code.
    for code in seen.keys() {
        assert!(
            defined.contains(code),
            "family lists {code:?} but errors/error.json does not define it"
        );
    }
}

fn mandatory_surface_set() -> BTreeSet<String> {
    let caps = read_json(&schema_dir().join("capabilities/capabilities.json"));
    let arr = caps
        .get("x-cairn-mandatory-surfaces")
        .and_then(Value::as_array)
        .expect("capabilities.json: x-cairn-mandatory-surfaces must be an array");
    arr.iter()
        .map(|entry| {
            entry
                .get("surface")
                .and_then(Value::as_str)
                .expect("x-cairn-mandatory-surfaces entries must have a 'surface' string")
                .to_string()
        })
        .collect()
}

#[test]
fn every_verb_and_prelude_surface_is_in_capabilities_or_mandatory_list() {
    let enum_set = capabilities_enum();
    let mandatory = mandatory_surface_set();
    let mut missing: Vec<String> = Vec::new();

    // Verb roots: surface id is "verb.<id>". Must appear in mandatory OR
    // have an x-cairn-capability already covered elsewhere.
    for verb in EXPECTED_VERB_IDS {
        let surface = format!("verb.{verb}");
        let path = schema_dir().join(format!("verbs/{verb}.json"));
        let v = read_json(&path);
        let root_cap = v.get("x-cairn-capability");
        let is_mandatory = mandatory.contains(&surface);
        let has_root_cap = root_cap
            .and_then(Value::as_str)
            .is_some_and(|c| enum_set.contains(c));
        if !is_mandatory && !has_root_cap {
            missing.push(surface);
        }
    }

    // Preludes: "prelude.<name>".
    for prelude in ["status", "handshake"] {
        let surface = format!("prelude.{prelude}");
        if !mandatory.contains(&surface) {
            missing.push(surface);
        }
    }

    // Extension namespaces: each registered triple must carry an
    // x-cairn-capability that exists in the capability enum. This ties
    // status.extensions advertisement to the capability registry so the
    // two cannot drift.
    let reg = read_json(&schema_dir().join("extensions/registry.json"));
    let branches = reg
        .get("$defs")
        .and_then(|d| d.get("namespace"))
        .and_then(|n| n.get("oneOf"))
        .and_then(Value::as_array)
        .expect("extensions/registry.json $defs.namespace.oneOf must exist");
    for branch in branches {
        let props = branch
            .get("properties")
            .and_then(Value::as_object)
            .expect("namespace branch must have properties");
        let name = props
            .get("name")
            .and_then(|n| n.get("const"))
            .and_then(Value::as_str)
            .expect("namespace branch must pin name.const")
            .to_string();
        let cap = props
            .get("x-cairn-capability")
            .and_then(|c| c.get("const"))
            .and_then(Value::as_str)
            .map(str::to_string);
        match cap {
            None => missing.push(format!("extension.{name} (no x-cairn-capability)")),
            Some(c) if !enum_set.contains(&c) => {
                missing.push(format!("extension.{name} -> {c} (not in capability enum)"));
            }
            _ => {}
        }
    }

    assert!(
        missing.is_empty(),
        "surfaces not covered by capability enum or mandatory allowlist: {missing:?}"
    );
}

fn is_string_constrained(node: &serde_json::Map<String, Value>) -> bool {
    for key in [
        "minLength",
        "pattern",
        "enum",
        "format",
        "const",
        "contentEncoding",
        "oneOf",
        "anyOf",
    ] {
        if node.contains_key(key) {
            return true;
        }
    }
    false
}

fn is_array_constrained(node: &serde_json::Map<String, Value>) -> bool {
    if node.contains_key("minItems") || node.contains_key("const") || node.contains_key("enum") {
        return true;
    }
    if let Some(items) = node.get("items").and_then(Value::as_object) {
        if items.contains_key("$ref")
            || items.contains_key("const")
            || items.contains_key("enum")
            || items.contains_key("oneOf")
            || items.contains_key("anyOf")
        {
            return true;
        }
        if let Some(ty) = items.get("type").and_then(Value::as_str) {
            if ty == "object" {
                return true;
            }
            if ty == "string" && is_string_constrained(items) {
                return true;
            }
        }
    }
    false
}

fn is_integer_constrained(node: &serde_json::Map<String, Value>) -> bool {
    for key in [
        "minimum",
        "maximum",
        "enum",
        "const",
        "exclusiveMinimum",
        "exclusiveMaximum",
    ] {
        if node.contains_key(key) {
            return true;
        }
    }
    false
}

fn walk_unguarded(
    doc: &Value,
    pointer: &str,
    file: &str,
    allowlist: &BTreeSet<(String, String)>,
    out: &mut Vec<String>,
) {
    match doc {
        Value::Object(map) => {
            if let Some(ty) = map.get("type").and_then(Value::as_str) {
                let ok = match ty {
                    "string" => is_string_constrained(map),
                    "array" => is_array_constrained(map),
                    "integer" => is_integer_constrained(map),
                    _ => true,
                };
                if !ok {
                    let key = (file.to_string(), pointer.to_string());
                    if !allowlist.contains(&key) {
                        out.push(format!("{file}#{pointer} (unguarded {ty})"));
                    }
                }
            }
            for (k, v) in map {
                let escaped = k.replace('~', "~0").replace('/', "~1");
                walk_unguarded(v, &format!("{pointer}/{escaped}"), file, allowlist, out);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk_unguarded(item, &format!("{pointer}/{i}"), file, allowlist, out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_typed_field_asserts_bounds_or_is_allowlisted() {
    // Open fields with no assertion must be explicitly allowlisted with a
    // reason; a bare `"type": "string"` anywhere else is a policy failure.
    let allow: BTreeSet<(String, String)> = [
        ("verbs/search.json", "/$defs/Hit/properties/snippet"),
        ("verbs/search.json", "/$defs/Hit/properties/citation"),
        ("verbs/assemble_hot.json", "/$defs/Data/properties/prefix"),
        ("verbs/retrieve.json", "/$defs/DataRecord/properties/body"),
        ("verbs/retrieve.json", "/$defs/RecordRef/properties/snippet"),
        ("verbs/retrieve.json", "/$defs/TurnItem/properties/content"),
        (
            "verbs/retrieve.json",
            "/$defs/TurnItem/properties/reasoning",
        ),
        ("verbs/ingest.json", "/$defs/Args/properties/kind"),
        // kind has minLength:1 — guarded by parent fallback; skip
    ]
    .iter()
    .map(|(f, p)| (f.to_string(), p.to_string()))
    .collect();

    let mut findings: Vec<String> = Vec::new();
    for path in manifest_paths() {
        let rel = path
            .strip_prefix(schema_dir())
            .expect("path under schema dir")
            .to_string_lossy()
            .into_owned();
        let v = read_json(&path);
        walk_unguarded(&v, "", &rel, &allow, &mut findings);
    }
    assert!(
        findings.is_empty(),
        "unguarded typed fields found (add a minLength/minItems/minimum or allowlist the pointer):\n  {}",
        findings.join("\n  ")
    );
}

#[test]
fn every_ref_resolves_to_a_real_file_or_local_fragment() {
    for path in manifest_paths() {
        let v = read_json(&path);
        let mut refs: Vec<String> = Vec::new();
        collect_refs(&v, &mut refs);
        for r in refs {
            assert!(
                ref_resolves(&path, &r),
                "{path:?} references {r:?} which does not resolve to a real target (file and/or JSON Pointer fragment missing)"
            );
        }
    }
}
