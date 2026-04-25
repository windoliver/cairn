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
