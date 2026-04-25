use serde_json::json;
use cairn_idl::codegen::ir::{lower_schema, Ctx, Prim, RustType};

#[test]
fn primitive_string() {
    let v = json!({"type": "string"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Primitive(Prim::String)));
}

#[test]
fn primitive_integer_unsigned_minimum_zero() {
    let v = json!({"type": "integer", "minimum": 0});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Primitive(Prim::U64)));
}

#[test]
fn primitive_integer_signed_default() {
    let v = json!({"type": "integer"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Primitive(Prim::I64)));
}

#[test]
fn array_of_strings() {
    let v = json!({"type": "array", "items": {"type": "string"}});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Vec(inner) = ty else { panic!("expected Vec, got {ty:?}") };
    assert!(matches!(*inner, RustType::Primitive(Prim::String)));
}

#[test]
fn ref_resolves_to_typename() {
    let v = json!({"$ref": "../common/primitives.json#/$defs/Ulid"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Ref(name) = ty else { panic!("expected Ref, got {ty:?}") };
    assert_eq!(name.0, "Ulid");
}

#[test]
fn struct_with_required_and_optional() {
    let v = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["a"],
        "properties": {
            "a": {"type": "string"},
            "b": {"type": "integer", "minimum": 0}
        }
    });
    let mut ctx = Ctx::with_target("Demo");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Struct(s) = ty else { panic!("expected Struct, got {ty:?}") };
    assert_eq!(s.fields.len(), 2);
    assert!(s.deny_unknown_fields);
    let a = s.fields.iter().find(|f| f.name == "a").unwrap();
    assert!(a.required);
    let b = s.fields.iter().find(|f| f.name == "b").unwrap();
    assert!(!b.required);
}

#[test]
fn string_enum_lowers_to_enum() {
    let v = json!({
        "type": "string",
        "enum": ["asc", "desc"]
    });
    let mut ctx = Ctx::with_target("Order");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Enum(e) = ty else { panic!("expected Enum, got {ty:?}") };
    assert_eq!(e.variants.len(), 2);
    assert_eq!(e.variants[0].wire, "asc");
    assert_eq!(e.variants[0].rust_ident, "Asc");
}

#[test]
fn oneof_of_const_lowers_to_enum() {
    let v = json!({
        "oneOf": [
            { "const": "keyword" },
            { "const": "semantic" },
            { "const": "hybrid" }
        ]
    });
    let mut ctx = Ctx::with_target("Mode");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::Enum(e) = ty else { panic!("expected Enum, got {ty:?}") };
    assert_eq!(e.variants.len(), 3);
}

#[test]
fn additional_properties_true_lowers_to_json() {
    let v = json!({"type": "object"});
    let mut ctx = Ctx::default();
    let ty = lower_schema(&v, &mut ctx).unwrap();
    assert!(matches!(ty, RustType::Json));
}

#[test]
fn tagged_union_with_discriminator() {
    let mut defs = std::collections::BTreeMap::new();
    defs.insert("ArgsRecord".to_string(), json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["target", "id"],
        "properties": {
            "target": { "const": "record" },
            "id": { "type": "string" }
        }
    }));
    defs.insert("ArgsSession".to_string(), json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["target", "session_id"],
        "x-cairn-capability": "cairn.mcp.v1.retrieve.session",
        "properties": {
            "target": { "const": "session" },
            "session_id": { "type": "string" }
        }
    }));
    let v = json!({
        "x-cairn-discriminator": "target",
        "oneOf": [
            { "$ref": "#/$defs/ArgsRecord" },
            { "$ref": "#/$defs/ArgsSession" }
        ]
    });
    let mut ctx = Ctx::with_target("RetrieveArgs").with_defs(defs);
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::TaggedUnion(t) = ty else { panic!("expected TaggedUnion, got {ty:?}") };
    assert_eq!(t.discriminator, "target");
    assert_eq!(t.variants.len(), 2);
    assert_eq!(t.variants[0].wire, "record");
    assert_eq!(t.variants[0].rust_ident, "Record");
    assert_eq!(t.variants[1].wire, "session");
    assert_eq!(t.variants[1].capability.as_deref(), Some("cairn.mcp.v1.retrieve.session"));
}

#[test]
fn untagged_union_xor_groups() {
    let v = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind"],
        "properties": {
            "kind": { "type": "string" },
            "body": { "type": "string" },
            "file": { "type": "string" },
            "url":  { "type": "string" }
        },
        "oneOf": [
            { "required": ["body"] },
            { "required": ["file"] },
            { "required": ["url"] }
        ]
    });
    let mut ctx = Ctx::with_target("IngestArgs");
    let ty = lower_schema(&v, &mut ctx).unwrap();
    let RustType::UntaggedUnion(u) = ty else { panic!("expected UntaggedUnion, got {ty:?}") };
    assert_eq!(u.fields.len(), 4);
    // `kind` is the outer-required field.
    assert!(u.fields.iter().find(|f| f.name == "kind").unwrap().required);
    // body/file/url stay Optional in the type itself, XOR is in xor_groups.
    assert_eq!(u.xor_groups.len(), 3);
}

#[test]
fn filter_family_collapses_to_recursive() {
    // Minimal stand-in for the filter root schema.
    let v = json!({
        "x-cairn-max-depth": 8,
        "x-cairn-max-fanout": 32,
        "$ref": "#/$defs/filter_L8"
    });
    let mut defs = std::collections::BTreeMap::new();
    defs.insert("filter_leaf".to_string(), json!({
        "oneOf": [
            { "$ref": "#/$defs/filter_leaf_string" }
        ]
    }));
    defs.insert("filter_leaf_string".to_string(), json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["field", "op", "value"],
        "properties": {
            "field": {"type": "string"},
            "op": {"type": "string", "enum": ["eq"]},
            "value": {"type": "string"}
        }
    }));
    let mut ctx = Ctx::with_target("Filter").with_defs(defs);
    let ty = cairn_idl::codegen::ir::lower_filter_root(&v, &mut ctx).unwrap();
    let RustType::Recursive(r) = ty else { panic!("expected Recursive, got {ty:?}") };
    assert_eq!(r.max_depth, 8);
    assert_eq!(r.max_fanout, 32);
}
