//! Asserts every `oneOf` in the IDL either carries `x-cairn-discriminator`
//! or is in a known allow-list (string-const enums; XOR-required patterns;
//! the request envelope's verb-dispatch oneOf).

use serde_json::Value;
use std::path::PathBuf;

const ALLOWED_NO_DISCRIMINATOR: &[&str] = &[
    // Closed string enums:
    "capabilities/capabilities.json#/oneOf",
    "verbs/search.json#/$defs/Args/properties/mode/oneOf",
    "verbs/search.json#/$defs/filter_leaf/oneOf",
    "verbs/search.json#/$defs/filter_leaf_array_contains/properties/value/oneOf",
    "verbs/search.json#/$defs/filter_leaf_array_contains_set/properties/value/items/oneOf",
    "verbs/search.json#/$defs/filter_L1/oneOf",
    "verbs/search.json#/$defs/filter_L2/oneOf",
    "verbs/search.json#/$defs/filter_L3/oneOf",
    "verbs/search.json#/$defs/filter_L4/oneOf",
    "verbs/search.json#/$defs/filter_L5/oneOf",
    "verbs/search.json#/$defs/filter_L6/oneOf",
    "verbs/search.json#/$defs/filter_L7/oneOf",
    "verbs/search.json#/$defs/filter_L8/oneOf",
    // XOR-required pattern:
    "verbs/ingest.json#/$defs/Args/oneOf",
    "envelope/signed_intent.json#/oneOf",
    // Errors enum is a tagged union on `code` — gets its own discriminator below.
    "errors/error.json#/oneOf",
    // Closed const-object quadruples — discriminated by whole-object match, not a single field.
    "extensions/registry.json#/$defs/namespace/oneOf",
    // Response union — variants are structurally distinct (different required fields);
    // a discriminator field will be added in a later increment once the response envelope lands.
    "verbs/retrieve.json#/$defs/Data/oneOf",
];

#[test]
fn every_oneof_has_discriminator_or_is_allowlisted() {
    let root = PathBuf::from(cairn_idl::SCHEMA_DIR);
    let mut violations = Vec::new();
    walk(&root, &root, &mut violations);
    assert!(
        violations.is_empty(),
        "the following oneOf sites need x-cairn-discriminator or to be added to ALLOWED_NO_DISCRIMINATOR:\n{}",
        violations.join("\n")
    );
}

fn walk(root: &std::path::Path, dir: &std::path::Path, violations: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            walk(root, &path, violations);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        check(&value, &rel, &mut String::new(), violations);
    }
}

fn check(value: &Value, file: &str, pointer: &mut String, violations: &mut Vec<String>) {
    if let Value::Object(map) = value {
        if let Some(_one_of) = map.get("oneOf") {
            let site = format!("{file}#{pointer}/oneOf");
            let has_discriminator = map.contains_key("x-cairn-discriminator");
            if !has_discriminator && !ALLOWED_NO_DISCRIMINATOR.contains(&site.as_str()) {
                violations.push(site);
            }
        }
        for (k, v) in map {
            let saved = pointer.len();
            pointer.push('/');
            for c in k.chars() {
                match c {
                    '~' => pointer.push_str("~0"),
                    '/' => pointer.push_str("~1"),
                    other => pointer.push(other),
                }
            }
            check(v, file, pointer, violations);
            pointer.truncate(saved);
        }
    } else if let Value::Array(arr) = value {
        for (i, v) in arr.iter().enumerate() {
            let saved = pointer.len();
            pointer.push('/');
            pointer.push_str(&i.to_string());
            check(v, file, pointer, violations);
            pointer.truncate(saved);
        }
    }
}
