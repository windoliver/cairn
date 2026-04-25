//! Typed intermediate representation produced by [`super::loader`] and
//! consumed by every `emit_*` module.

#![allow(clippy::module_name_repetitions)]
#![allow(clippy::struct_field_names)]
#![allow(missing_docs)]

use std::collections::BTreeMap;

/// Top-level IR built from a validated [`super::loader::RawDocument`].
#[derive(Debug, Clone)]
pub struct Document {
    pub contract: String,
    pub capabilities: Vec<String>,
    pub error_codes: Vec<ErrorVariant>,
    pub common: BTreeMap<TypeName, RustType>,
    pub envelope: BTreeMap<TypeName, RustType>,
    pub verbs: Vec<VerbDef>,
    pub preludes: Vec<PreludeDef>,
}

/// One verb in IDL order.
#[derive(Debug, Clone)]
pub struct VerbDef {
    pub id: String,
    pub args: RustType,
    pub data: RustType,
    pub cli: CliShape,
    pub skill: SkillBlock,
    pub capability: Option<String>,
    pub auth: AuthModel,
    pub args_schema_bytes: Vec<u8>,
    pub data_schema_bytes: Vec<u8>,
}

/// One protocol prelude (status, handshake).
#[derive(Debug, Clone)]
pub struct PreludeDef {
    pub id: String,
    pub response: RustType,
    pub schema_bytes: Vec<u8>,
}

/// One error code variant, lowered from the closed `oneOf` in `errors/error.json`.
#[derive(Debug, Clone)]
pub struct ErrorVariant {
    pub code: String,
    pub data: Option<TypeName>,
}

/// CLI shape extracted from `x-cairn-cli`. Verbs whose Args are a tagged union
/// (`RetrieveArgs`) carry one `CliShape` per variant.
#[derive(Debug, Clone)]
pub enum CliShape {
    Single(CliCommand),
    Variants(Vec<CliCommand>),
}

#[derive(Debug, Clone)]
pub struct CliCommand {
    pub command: String,
    pub flags: Vec<CliFlag>,
    pub positional: Option<CliPositional>,
}

#[derive(Debug, Clone)]
pub struct CliFlag {
    pub name: String,
    pub long: String,
    pub value_source: String,
}

#[derive(Debug, Clone)]
pub struct CliPositional {
    pub name: String,
    pub description: String,
}

/// Skill triggers extracted from `x-cairn-skill-triggers`.
#[derive(Debug, Clone, Default)]
pub struct SkillBlock {
    pub positive: Vec<String>,
    pub negative: Vec<String>,
    pub exclusivity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthModel {
    SignedChain,
    Rebac,
    SignedPrincipal,
    HardwareKey,
}

impl AuthModel {
    /// Parse an `AuthModel` from its IDL wire string. Returns `None` for unknown values.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "signed_chain" => Some(Self::SignedChain),
            "rebac" => Some(Self::Rebac),
            "signed_principal" => Some(Self::SignedPrincipal),
            "hardware_key" => Some(Self::HardwareKey),
            _ => None,
        }
    }

    /// Return the IDL wire string for this `AuthModel`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SignedChain => "signed_chain",
            Self::Rebac => "rebac",
            Self::SignedPrincipal => "signed_principal",
            Self::HardwareKey => "hardware_key",
        }
    }
}

/// Stable identifier used for Rust types and module paths.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeName(pub String);

impl TypeName {
    /// Construct a new `TypeName` from any string-like value.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Lowered Rust type. Mirrors the lowering rules table in
/// `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md` §4.2.
#[derive(Debug, Clone)]
pub enum RustType {
    Primitive(Prim),
    Optional(Box<RustType>),
    Vec(Box<RustType>),
    /// Map<String, T> — used for `additionalProperties: <schema>` blobs.
    Map(Box<RustType>),
    /// Resolved `$ref` — the `TypeName` is one of `common` / `errors` / a
    /// per-verb local def.
    Ref(TypeName),
    Struct(StructDef),
    Enum(EnumDef),
    TaggedUnion(TaggedUnionDef),
    UntaggedUnion(UntaggedUnionDef),
    Recursive(RecursiveEnumDef),
    /// Opaque `serde_json::Value` blob — used when the schema is
    /// `additionalProperties: true` (frontmatter, etc.).
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prim {
    String,
    I64,
    U64,
    F64,
    Bool,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: TypeName,
    pub fields: Vec<StructField>,
    pub deny_unknown_fields: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: RustType,
    pub required: bool,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: TypeName,
    pub variants: Vec<EnumVariant>,
    pub rename_all: Option<&'static str>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    /// Wire string (the `const` from JSON Schema).
    pub wire: String,
    /// Rust identifier — `PascalCase`-cased form of the wire string.
    pub rust_ident: String,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaggedUnionDef {
    pub name: TypeName,
    pub discriminator: String,
    pub variants: Vec<TaggedVariant>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaggedVariant {
    /// Discriminator value (e.g. `"record"`).
    pub wire: String,
    pub rust_ident: String,
    pub fields: Vec<StructField>,
    pub capability: Option<String>,
    pub cli: Option<CliCommand>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UntaggedUnionDef {
    pub name: TypeName,
    /// All-Optional fields; `validate` enforces exactly-one-of these required-sets.
    pub fields: Vec<StructField>,
    pub xor_groups: Vec<Vec<String>>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecursiveEnumDef {
    pub name: TypeName,
    pub leaf: Box<RustType>,
    pub max_depth: u32,
    pub max_fanout: u32,
    pub doc: Option<String>,
}

use serde_json::Value;

use super::CodegenError;

/// Lowering context — carries the target type name (for struct/enum naming)
/// and any additional resolution state.
#[derive(Debug, Default, Clone)]
pub struct Ctx {
    pub target: Option<TypeName>,
    /// Local `$defs` from the containing IDL file. Populated by the per-verb
    /// caller so `lower_tagged_union` can resolve `#/$defs/<Name>` refs.
    pub defs: std::collections::BTreeMap<String, Value>,
}

impl Ctx {
    /// Construct a `Ctx` with a named target type.
    pub fn with_target(name: impl Into<String>) -> Self {
        Self {
            target: Some(TypeName::new(name)),
            defs: std::collections::BTreeMap::new(),
        }
    }

    /// Attach local `$defs` for tagged-union variant resolution.
    #[must_use]
    pub fn with_defs(mut self, defs: std::collections::BTreeMap<String, Value>) -> Self {
        self.defs = defs;
        self
    }

    /// Derive a child context by appending `suffix` to the current target name.
    /// The child inherits the parent's `defs` for nested lookups.
    #[must_use]
    pub fn child(&self, suffix: &str) -> Self {
        let target = self
            .target
            .as_ref()
            .map(|t| TypeName::new(format!("{}{suffix}", t.0)));
        Self { target, defs: self.defs.clone() }
    }
}

/// Lower a JSON Schema `value` to a [`RustType`].
///
/// # Errors
/// Returns [`CodegenError::Ir`] when the schema shape cannot be represented.
pub fn lower_schema(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    // (1) `$ref` short-circuits.
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        return Ok(RustType::Ref(typename_from_ref(reference)));
    }

    // (2) `oneOf` cases.
    if let Some(arr) = value.get("oneOf").and_then(Value::as_array) {
        // (2a) tagged union (handled in Task 9 — fall through if discriminator absent).
        if value.get("x-cairn-discriminator").is_some() {
            return lower_tagged_union(value, arr, ctx);
        }
        // (2b) all-const → string enum.
        if arr
            .iter()
            .all(|v| v.get("const").and_then(Value::as_str).is_some())
        {
            return lower_const_oneof(arr, ctx);
        }
        // (2c) untagged union via XOR (ingest, signed_intent) — Task 10.
        if arr.iter().all(|v| v.get("required").is_some()) {
            return lower_untagged_union(value, arr, ctx);
        }
        return Err(CodegenError::Ir(
            "oneOf shape not recognised — needs x-cairn-discriminator or all-const variants"
                .to_string(),
        ));
    }

    // (3) explicit type.
    let ty = value.get("type").and_then(Value::as_str);
    let enum_arr = value.get("enum").and_then(Value::as_array);

    match (ty, enum_arr) {
        (Some("string"), Some(values)) => lower_string_enum(values, ctx),
        (Some("string"), None) => Ok(RustType::Primitive(Prim::String)),
        (Some("integer"), _) => {
            Ok(if value.get("minimum").and_then(Value::as_i64) == Some(0) {
                RustType::Primitive(Prim::U64)
            } else {
                RustType::Primitive(Prim::I64)
            })
        }
        (Some("number"), _) => Ok(RustType::Primitive(Prim::F64)),
        (Some("boolean"), _) => Ok(RustType::Primitive(Prim::Bool)),
        (Some("array"), _) => {
            let items = value
                .get("items")
                .ok_or_else(|| CodegenError::Ir("array missing items".to_string()))?;
            let mut child = ctx.child("Item");
            let inner = lower_schema(items, &mut child)?;
            Ok(RustType::Vec(Box::new(inner)))
        }
        (Some("object"), _) => lower_object(value, ctx),
        (None, _) if value.get("const").is_some() => {
            // Standalone const (e.g. retrieve target marker). Rare outside oneOf.
            Ok(RustType::Primitive(Prim::String))
        }
        (None, _) => Err(CodegenError::Ir(format!(
            "schema has no `type`, no `$ref`, no `oneOf`: {value}"
        ))),
        (Some(other), _) => Err(CodegenError::Ir(format!("unhandled type: {other}"))),
    }
}

fn lower_object(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let additional = value
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let Some(props) = value.get("properties").and_then(Value::as_object) else {
        // No properties → treat as opaque Json blob.
        return Ok(RustType::Json);
    };
    let target_name = ctx
        .target
        .clone()
        .unwrap_or_else(|| TypeName::new("Anon"));
    let required: std::collections::BTreeSet<String> = value
        .get("required")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut fields = Vec::with_capacity(props.len());
    let mut keys: Vec<&String> = props.keys().collect();
    keys.sort();
    for key in keys {
        let prop = &props[key];
        let mut child = ctx.child(&pascal_case(key));
        let ty = lower_schema(prop, &mut child)?;
        let doc = prop
            .get("description")
            .and_then(Value::as_str)
            .map(String::from);
        let is_required = required.contains(key);
        fields.push(StructField {
            name: key.clone(),
            ty: if is_required {
                ty
            } else {
                RustType::Optional(Box::new(ty))
            },
            required: is_required,
            doc,
        });
    }
    Ok(RustType::Struct(StructDef {
        name: target_name,
        fields,
        deny_unknown_fields: !additional,
        doc: value
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
    }))
}

fn lower_string_enum(values: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let variants = values
        .iter()
        .map(|v| -> Result<EnumVariant, CodegenError> {
            let wire = v.as_str().ok_or_else(|| {
                CodegenError::Ir("enum value not a string".to_string())
            })?;
            Ok(EnumVariant {
                wire: wire.to_string(),
                rust_ident: pascal_case(wire),
                doc: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RustType::Enum(EnumDef {
        name: ctx
            .target
            .clone()
            .unwrap_or_else(|| TypeName::new("Enum")),
        variants,
        rename_all: Some("snake_case"),
        doc: None,
    }))
}

fn lower_const_oneof(arr: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let variants = arr
        .iter()
        .map(|v| -> Result<EnumVariant, CodegenError> {
            let wire = v
                .get("const")
                .and_then(Value::as_str)
                .ok_or_else(|| CodegenError::Ir("oneOf entry missing const".to_string()))?;
            Ok(EnumVariant {
                wire: wire.to_string(),
                rust_ident: pascal_case(wire),
                doc: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RustType::Enum(EnumDef {
        name: ctx
            .target
            .clone()
            .unwrap_or_else(|| TypeName::new("Enum")),
        variants,
        rename_all: Some("snake_case"),
        doc: None,
    }))
}

fn lower_tagged_union(value: &Value, arr: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let discriminator = value
        .get("x-cairn-discriminator")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir("x-cairn-discriminator must be a string".to_string()))?
        .to_string();
    let target = ctx.target.clone().unwrap_or_else(|| TypeName::new("Union"));
    let mut variants = Vec::with_capacity(arr.len());
    for entry in arr {
        let reference = entry
            .get("$ref")
            .and_then(Value::as_str)
            .ok_or_else(|| CodegenError::Ir("tagged-union variant must be a $ref".to_string()))?;
        // Local def lookup ("#/$defs/ArgsRecord" → "ArgsRecord").
        let def_name = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| {
                CodegenError::Ir(format!("non-local $ref in tagged union: {reference}"))
            })?;
        let def = ctx
            .defs
            .get(def_name)
            .ok_or_else(|| CodegenError::Ir(format!("unknown $defs entry: {def_name}")))?
            .clone();

        let wire = def
            .pointer(&format!("/properties/{discriminator}/const"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodegenError::Ir(format!(
                    "{def_name}: properties.{discriminator}.const required for tagged-union variant"
                ))
            })?
            .to_string();
        let rust_ident = pascal_case(&wire);

        // Lower variant body as a struct so we keep its fields.
        let mut child = ctx.child(&rust_ident);
        let body_ty = lower_schema(&def, &mut child)?;
        let RustType::Struct(StructDef { mut fields, .. }) = body_ty else {
            return Err(CodegenError::Ir(format!(
                "tagged variant {def_name} did not lower to a struct"
            )));
        };
        // Drop the discriminator field — serde tag covers it.
        fields.retain(|f| f.name != discriminator);

        let capability = def
            .get("x-cairn-capability")
            .and_then(Value::as_str)
            .map(String::from);
        let cli = def.get("x-cairn-cli").map(parse_cli_block).transpose()?;
        variants.push(TaggedVariant {
            wire,
            rust_ident,
            fields,
            capability,
            cli,
            doc: def.get("description").and_then(Value::as_str).map(String::from),
        });
    }
    Ok(RustType::TaggedUnion(TaggedUnionDef {
        name: target,
        discriminator,
        variants,
        doc: value.get("description").and_then(Value::as_str).map(String::from),
    }))
}

/// Parse a `x-cairn-cli` JSON block into a [`CliCommand`].
/// Shared with later tasks (Task 12+) that need to extract CLI shapes from IDL.
pub(crate) fn parse_cli_block(value: &Value) -> Result<CliCommand, CodegenError> {
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir("x-cairn-cli.command required".to_string()))?
        .to_string();
    let flags = value
        .get("flags")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|f| {
                    Ok(CliFlag {
                        name: f
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        long: f
                            .get("long")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        value_source: f
                            .get("value_source")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect::<Result<Vec<_>, CodegenError>>()
        })
        .transpose()?
        .unwrap_or_default();
    let positional = value.get("positional").map(|p| CliPositional {
        name: p.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        description: p
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    });
    Ok(CliCommand { command, flags, positional })
}

// Untagged union lowering lands in Task 10. Stub:

fn lower_untagged_union(
    _value: &Value,
    _arr: &[Value],
    _ctx: &mut Ctx,
) -> Result<RustType, CodegenError> {
    Err(CodegenError::Ir(
        "untagged union lowering arrives in Task 10".to_string(),
    ))
}

fn typename_from_ref(reference: &str) -> TypeName {
    // "../common/primitives.json#/$defs/Ulid" → "Ulid"
    let after_hash = reference.split('#').nth(1).unwrap_or("");
    let last = after_hash.rsplit('/').next().unwrap_or("");
    TypeName::new(last)
}

/// Convert a `snake_case`, `kebab-case`, or `dot.separated` string to `PascalCase`.
#[must_use]
pub fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}
