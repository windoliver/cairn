//! Real JSON-Schema-vs-TOML-fixture validation for `plugin/manifest.json`.
//!
//! Loads the schema, loads the example fixture, converts TOML to a JSON Value,
//! and runs the fixture through `jsonschema`. Catches schema/fixture drift
//! that the parity tests can't see.

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use jsonschema::Validator;
use serde_json::Value;

const SCHEMA_SRC: &str = include_str!("../schema/plugin/manifest.json");
const FIXTURE_SRC: &str = include_str!("../schema/plugin/example.toml");

fn schema() -> Validator {
    let schema_value: Value = serde_json::from_str(SCHEMA_SRC).expect("schema is valid JSON");
    Validator::new(&schema_value).expect("schema compiles")
}

fn fixture_as_json() -> Value {
    let toml_value: toml::Value = toml::from_str(FIXTURE_SRC).expect("fixture is valid TOML");
    serde_json::to_value(&toml_value).expect("toml -> json round-trip")
}

#[test]
fn example_toml_validates_against_schema() {
    let v = schema();
    let fixture = fixture_as_json();
    v.validate(&fixture).unwrap_or_else(|err| {
        panic!("fixture failed schema validation: {err}");
    });
}

#[test]
fn rejects_extra_top_level_field() {
    let v = schema();
    let mut fixture = fixture_as_json();
    fixture
        .as_object_mut()
        .expect("object")
        .insert("rogue".to_string(), Value::String("nope".into()));
    assert!(
        v.validate(&fixture).is_err(),
        "additionalProperties must reject"
    );
}

#[test]
fn rejects_invalid_name() {
    let v = schema();
    let mut fixture = fixture_as_json();
    fixture
        .as_object_mut()
        .expect("object")
        .insert("name".to_string(), Value::String("BAD_NAME".into()));
    assert!(v.validate(&fixture).is_err(), "name pattern must reject");
}

#[test]
fn rejects_unknown_contract() {
    let v = schema();
    let mut fixture = fixture_as_json();
    fixture.as_object_mut().expect("object").insert(
        "contract".to_string(),
        Value::String("UnknownContract".into()),
    );
    assert!(v.validate(&fixture).is_err(), "contract enum must reject");
}

#[test]
fn rejects_invalid_feature_key() {
    let v = schema();
    let mut fixture = fixture_as_json();
    let features = fixture
        .as_object_mut()
        .expect("object")
        .entry("features".to_string())
        .or_insert(Value::Object(serde_json::Map::default()))
        .as_object_mut()
        .expect("features object");
    features.insert("bad.key".to_string(), Value::Bool(true));
    assert!(
        v.validate(&fixture).is_err(),
        "feature key propertyNames must reject"
    );
}

#[test]
fn rejects_out_of_range_version() {
    let v = schema();
    let mut fixture = fixture_as_json();
    let major = fixture
        .pointer_mut("/contract_version_range/min/major")
        .expect("min.major");
    *major = Value::Number(70_000.into()); // > 65535
    assert!(
        v.validate(&fixture).is_err(),
        "version major maximum must reject"
    );
}
