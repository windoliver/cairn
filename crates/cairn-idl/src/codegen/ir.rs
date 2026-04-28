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
    /// Error codes legal on `status=rejected` responses, per
    /// `envelope/response.json#x-cairn-error-code-families.rejected`.
    pub rejected_error_codes: Vec<String>,
    /// Error codes legal on `status=aborted` responses, per
    /// `envelope/response.json#x-cairn-error-code-families.aborted`.
    pub aborted_error_codes: Vec<String>,
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
    /// Locally-defined `$defs` types referenced from Args/Data that need to be
    /// emitted as siblings in the per-verb file (e.g. `search.Hit`). Stored as
    /// `PascalCase` `TypeName`s so emitters and ref resolution agree.
    pub local_types: BTreeMap<TypeName, RustType>,
    pub cli: CliShape,
    pub skill: SkillBlock,
    pub capability: Option<String>,
    pub auth: AuthModel,
    /// Field- / mode-level auth overrides lifted from `x-cairn-auth`
    /// annotations attached to Args properties or Args sub-types. Surfaced
    /// in the MCP tool declarations so callers know `lint.write_report=true`
    /// and `summarize.persist=true` need `write_capability` even though the
    /// verb-level auth is `read_only` / `rebac`.
    pub auth_overrides: Vec<AuthOverride>,
    /// Field- / mode-level capability overrides lifted from
    /// `x-cairn-capability` annotations attached to Args properties or Args
    /// sub-types. Surfaced in the MCP tool declarations so callers know which
    /// concrete capability is required when the verb-level capability is
    /// `null` (e.g. `search.mode=keyword` requires `cairn.mcp.v1.search.keyword`).
    pub capability_overrides: Vec<CapabilityOverride>,
    pub args_schema_bytes: Vec<u8>,
    pub data_schema_bytes: Vec<u8>,
}

/// One field-level / mode-level `x-cairn-auth` annotation lifted from the IDL.
/// Surfaced in MCP tool declarations so MCP clients can require the stronger
/// auth before calling a write-producing verb mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthOverride {
    /// Dot-path inside Args identifying which property triggers the override.
    /// Examples: `write_report`, `persist`. For tagged-union sub-types,
    /// `mode=record` style equality is encoded as `<discriminator>=<wire>`.
    pub path: String,
    /// Auth model required when the path is present (or set to true for
    /// boolean flags).
    pub auth: AuthModel,
}

/// One field-level / mode-level `x-cairn-capability` annotation lifted
/// from the IDL. Surfaced in MCP tool declarations so MCP clients know
/// which capability string the verb actually needs once a specific Args
/// sub-type or property has been chosen — verb-level capability is null
/// for `search` / `retrieve` / `forget`, but each mode/target/sub-type
/// carries its own concrete capability string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityOverride {
    /// Dot-path inside Args identifying the trigger. Same encoding as
    /// [`AuthOverride::path`]: bare property name for property-level
    /// triggers, `<discriminator>=<wire>` for tagged-union sub-types.
    pub path: String,
    /// Capability string required when the path is present.
    pub capability: String,
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
    /// True when the positional accepts more than one value (clap
    /// `num_args(1..)`). Currently set from the optional `repeatable` field
    /// in `x-cairn-cli.positional`.
    pub repeatable: bool,
    /// Schema field names this positional satisfies in a `oneOf` exclusivity
    /// group. E.g., `cairn ingest`'s `source` positional aliases `body`,
    /// `file`, and `url` — presence of the positional satisfies any of those
    /// branches and conflicts with all of them.
    pub aliases_one_of: Vec<String>,
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
    /// Capability-token auth used by the `forget` verb.
    ForgetCapability,
    /// Generic write-capability auth used by field-level overrides
    /// (`lint.write_report`, `summarize.persist`).
    WriteCapability,
    /// Read-only auth model (no mutation capability required).
    ReadOnly,
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
            "forget_capability" => Some(Self::ForgetCapability),
            "write_capability" => Some(Self::WriteCapability),
            "read_only" => Some(Self::ReadOnly),
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
            Self::ForgetCapability => "forget_capability",
            Self::WriteCapability => "write_capability",
            Self::ReadOnly => "read_only",
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
    /// Field names captured from a sibling top-level `anyOf` whose every
    /// branch is a single-element `required: [<field>]` predicate. The struct
    /// must surface "at least one of these fields present" at deserialise
    /// time. None when no such anyOf was attached.
    pub any_of_required: Option<Vec<String>>,
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
    /// Per-variant capability lifted from `x-cairn-capability` on the
    /// originating `oneOf` const entry (e.g. `search.mode` variants advertise
    /// `cairn.mcp.v1.search.semantic` etc.).
    pub capability: Option<String>,
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
        Self {
            target,
            defs: self.defs.clone(),
        }
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

    // (1b) `allOf: [{ "$ref": "..." }]` — single-entry allOf used to annotate a $ref
    // with description/metadata. Unwrap the inner $ref and pass through.
    // Multi-entry allOf or non-$ref allOf falls through so `type`/`properties` can
    // still produce a struct if present.
    if let Some(all_of) = value.get("allOf").and_then(Value::as_array)
        && all_of.len() == 1
        && let Some(reference) = all_of[0].get("$ref").and_then(Value::as_str)
    {
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
        // (2d) single-entry oneOf that contains a $ref — treat as just the $ref.
        if arr.len() == 1
            && let Some(reference) = arr[0].get("$ref").and_then(Value::as_str)
        {
            return Ok(RustType::Ref(typename_from_ref(reference)));
        }
        // (2e) all entries are $refs without a discriminator — the variants are
        // structurally distinct; we cannot generate typed enum arms without a
        // discriminator field, so we fall back to opaque Json. Consumers that need
        // typed access must add x-cairn-discriminator to the IDL.
        if arr.iter().all(|v| v.get("$ref").is_some()) {
            return Ok(RustType::Json);
        }
        return Err(CodegenError::Ir(
            "oneOf shape not recognised — needs x-cairn-discriminator or all-const variants"
                .to_string(),
        ));
    }

    // (3) explicit type.
    // JSON Schema allows `type` to be an array of strings (e.g. `["object", "array"]`).
    // For IR purposes we take the first listed type; in practice this only appears
    // in the request/response envelope's opaque dispatch fields which lower to Json.
    let ty_value = value.get("type");
    let ty: Option<&str> = ty_value.and_then(Value::as_str).or_else(|| {
        ty_value
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
    });
    let enum_arr = value.get("enum").and_then(Value::as_array);

    match (ty, enum_arr) {
        (Some("string"), Some(values)) => lower_string_enum(values, ctx),
        (Some("string"), None) => Ok(RustType::Primitive(Prim::String)),
        (Some("integer"), _) => Ok(if value.get("minimum").and_then(Value::as_i64) == Some(0) {
            RustType::Primitive(Prim::U64)
        } else {
            RustType::Primitive(Prim::I64)
        }),
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
    let target_name = ctx.target.clone().unwrap_or_else(|| TypeName::new("Anon"));
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
    // Detect a sibling top-level `anyOf` whose every branch is a single-
    // element `required: [<field>]` predicate (no other keys). When present,
    // the struct must enforce "at least one of these fields" at deserialise
    // time — see emit_sdk's struct emitter. JSON Schema `required` is a
    // presence check, so we honour presence semantics here; per-field
    // `minLength: 1`/`minItems: 1` already enforce non-emptiness.
    let any_of_required = value
        .get("anyOf")
        .and_then(Value::as_array)
        .and_then(|arr| extract_required_only_anyof(arr));

    Ok(RustType::Struct(StructDef {
        name: target_name,
        fields,
        deny_unknown_fields: !additional,
        doc: value
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
        any_of_required,
    }))
}

/// If every entry in `arr` is an object whose only key is `required` (a
/// single-string array), return the union of those required field names in
/// declaration order. Returns `None` for any other shape — multi-required
/// branches, branches with extra keys, or empty arrays — so we don't enforce
/// a constraint we can't faithfully reproduce.
fn extract_required_only_anyof(arr: &[Value]) -> Option<Vec<String>> {
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    let mut seen = std::collections::BTreeSet::new();
    for entry in arr {
        let map = entry.as_object()?;
        if map.len() != 1 {
            return None;
        }
        let req = map.get("required")?.as_array()?;
        if req.len() != 1 {
            return None;
        }
        let name = req[0].as_str()?.to_string();
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    Some(out)
}

fn lower_string_enum(values: &[Value], ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let variants = values
        .iter()
        .map(|v| -> Result<EnumVariant, CodegenError> {
            let wire = v
                .as_str()
                .ok_or_else(|| CodegenError::Ir("enum value not a string".to_string()))?;
            Ok(EnumVariant {
                wire: wire.to_string(),
                rust_ident: pascal_case(wire),
                doc: None,
                capability: None,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RustType::Enum(EnumDef {
        name: ctx.target.clone().unwrap_or_else(|| TypeName::new("Enum")),
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
            let capability = v
                .get("x-cairn-capability")
                .and_then(Value::as_str)
                .map(String::from);
            Ok(EnumVariant {
                wire: wire.to_string(),
                rust_ident: pascal_case(wire),
                doc: None,
                capability,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RustType::Enum(EnumDef {
        name: ctx.target.clone().unwrap_or_else(|| TypeName::new("Enum")),
        variants,
        rename_all: Some("snake_case"),
        doc: None,
    }))
}

fn lower_tagged_union(
    value: &Value,
    arr: &[Value],
    ctx: &mut Ctx,
) -> Result<RustType, CodegenError> {
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
        let def_name = reference.strip_prefix("#/$defs/").ok_or_else(|| {
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
            doc: def
                .get("description")
                .and_then(Value::as_str)
                .map(String::from),
        });
    }
    Ok(RustType::TaggedUnion(TaggedUnionDef {
        name: target,
        discriminator,
        variants,
        doc: value
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
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
        name: p
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: p
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        repeatable: p
            .get("repeatable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        aliases_one_of: p
            .get("aliases_one_of")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    });
    Ok(CliCommand {
        command,
        flags,
        positional,
    })
}

fn lower_untagged_union(
    value: &Value,
    arr: &[Value],
    ctx: &mut Ctx,
) -> Result<RustType, CodegenError> {
    let target = ctx.target.clone().unwrap_or_else(|| TypeName::new("Union"));
    // Borrow the object lowering for the outer struct so we get the property fields.
    // When the outer schema has only `oneOf` (no `properties` at the outer level —
    // e.g. extensions/registry's `namespace` quadruple), there is no parent struct
    // to enforce XOR groups against; fall back to an opaque `Json` blob so the
    // type is at least addressable.
    let outer = lower_object(value, ctx)?;
    let RustType::Struct(StructDef { mut fields, .. }) = outer else {
        return Ok(RustType::Json);
    };
    let xor_groups: Vec<Vec<String>> = arr
        .iter()
        .map(|entry| {
            entry
                .get("required")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    // Fields that participate in *any* xor_group must remain Optional regardless
    // of whether the outer schema marked them required — the XOR validator owns
    // their presence. Fields that do NOT appear in any xor_group keep whatever
    // shape `lower_object` produced (required → non-Optional, optional →
    // Option<T>) so outer `required` stays load-bearing (e.g. ingest.kind,
    // signed_intent's 10 auth-critical fields).
    let xor_field_names: std::collections::BTreeSet<&str> = xor_groups
        .iter()
        .flat_map(|g| g.iter().map(String::as_str))
        .collect();
    for field in &mut fields {
        if xor_field_names.contains(field.name.as_str())
            && !matches!(field.ty, RustType::Optional(_))
        {
            field.ty = RustType::Optional(Box::new(field.ty.clone()));
            field.required = false;
        }
    }
    Ok(RustType::UntaggedUnion(UntaggedUnionDef {
        name: target,
        fields,
        xor_groups,
        doc: value
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
    }))
}

/// Special-case lowering for the `filter` family. Collapses `filter_L0..L8`
/// into a single recursive enum:
///
/// ```rust,ignore
/// pub enum Filter {
///     Leaf(FilterLeaf),
///     And(Vec<Filter>),
///     Or(Vec<Filter>),
///     Not(Box<Filter>),
/// }
/// ```
///
/// The depth bound stays in JSON Schema only — runtime depth checks belong
/// to the search verb implementation (#9 / #63).
///
/// # Errors
/// Returns [`CodegenError::Ir`] when `filter_leaf` is absent from `ctx.defs`
/// or the leaf schema cannot be lowered.
pub fn lower_filter_root(value: &Value, ctx: &mut Ctx) -> Result<RustType, CodegenError> {
    let max_depth = value
        .get("x-cairn-max-depth")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(8);
    let max_fanout = value
        .get("x-cairn-max-fanout")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(32);
    let leaf = ctx
        .defs
        .get("filter_leaf")
        .ok_or_else(|| CodegenError::Ir("filter family missing filter_leaf def".to_string()))?
        .clone();
    let mut leaf_ctx = Ctx::with_target("FilterLeaf").with_defs(ctx.defs.clone());
    let leaf_ty = lower_schema(&leaf, &mut leaf_ctx)?;
    Ok(RustType::Recursive(RecursiveEnumDef {
        name: ctx
            .target
            .clone()
            .unwrap_or_else(|| TypeName::new("Filter")),
        leaf: Box::new(leaf_ty),
        max_depth,
        max_fanout,
        doc: value
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
    }))
}

fn typename_from_ref(reference: &str) -> TypeName {
    // Three shapes are accepted:
    //   "../common/primitives.json#/$defs/Ulid" → "Ulid"
    //   "#/$defs/namespace"                     → "Namespace"
    //   "../common/scope_filter.json"           → "ScopeFilter" (file stem, no fragment)
    // The trailing identifier is PascalCase-normalised so wire-form $defs keys
    // (e.g. lowercase `namespace`) become valid Rust type identifiers.
    let (file_part, after_hash) = match reference.split_once('#') {
        Some((file, frag)) => (file, frag),
        None => (reference, ""),
    };
    let raw = after_hash.rsplit('/').next().unwrap_or("");
    let basis = if raw.is_empty() {
        // Whole-file ref — derive from file basename stem.
        std::path::Path::new(file_part)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        raw.to_string()
    };
    TypeName::new(pascal_case(&basis))
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

// ── build: RawDocument → Document ────────────────────────────────────────────

use super::loader::{RawDocument, RawFile};

/// Walk a [`RawDocument`] and lower it to the fully-typed [`Document`] IR.
///
/// This is the entry point for Phase 2. Each verb, prelude, common file, and
/// envelope file is lowered exactly once; the search verb's `filters` field is
/// special-cased to call [`lower_filter_root`].
///
/// # Errors
/// Returns [`CodegenError::Ir`] when any schema cannot be represented in the IR.
pub fn build(raw: &RawDocument) -> Result<Document, CodegenError> {
    let contract = "cairn.mcp.v1".to_string();

    // Capabilities (already validated by loader).
    let capabilities: Vec<String> = raw
        .capabilities
        .get("capabilities")
        .and_then(|f| f.value.get("oneOf"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.get("const").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Errors → flat list of (code, data-typename) pairs.
    let error_codes = build_error_codes(&raw.errors)?;

    // Per-status code families lifted from response.json.
    let (rejected_error_codes, aborted_error_codes) = build_error_code_families(&raw.envelope)?;

    // Common types — lower every entry under common/*.json#/$defs/*, plus the
    // capabilities and extensions registries (their types are addressable from
    // the same `crate::generated::common::*` module).
    let mut common = BTreeMap::new();
    for file in raw.common.values() {
        ingest_defs_into(file, &mut common)?;
    }
    for file in raw.capabilities.values() {
        ingest_defs_into(file, &mut common)?;
    }
    for file in raw.extensions.values() {
        ingest_defs_into(file, &mut common)?;
    }
    // Errors: each variant is dispatched on `code`, not via a single Rust type.
    // For the envelope's `Option<Error>` field we emit an opaque alias here so
    // the generated code compiles end-to-end. Stronger typing lands when the
    // error model is fully lowered (#62).
    common.insert(TypeName::new("Error"), RustType::Json);

    // Envelope types — request, response, signed_intent.
    let mut envelope = BTreeMap::new();
    for file in raw.envelope.values() {
        let stem = file
            .rel_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let name = TypeName::new(pascal_case(stem));
        let mut ctx = Ctx::with_target(&name.0);
        envelope.insert(name, lower_schema(&file.value, &mut ctx)?);
    }

    // Verbs.
    let mut verbs = Vec::with_capacity(raw.verbs.len());
    for file in &raw.verbs {
        verbs.push(build_verb(file)?);
    }

    // Preludes (BTreeMap — iterate in sorted order for stability).
    let mut preludes = Vec::with_capacity(raw.preludes.len());
    for (id, file) in &raw.preludes {
        let mut ctx = Ctx::with_target(format!("{}Response", pascal_case(id)));
        preludes.push(PreludeDef {
            id: id.clone(),
            response: lower_schema(&file.value, &mut ctx)?,
            schema_bytes: file.bytes.clone(),
        });
    }

    Ok(Document {
        contract,
        capabilities,
        error_codes,
        rejected_error_codes,
        aborted_error_codes,
        common,
        envelope,
        verbs,
        preludes,
    })
}

/// Lift `x-cairn-error-code-families.{rejected,aborted}` arrays from
/// `envelope/response.json`. Returns `(rejected, aborted)` in IDL declaration
/// order so codegen output is stable across builds.
fn build_error_code_families(
    envelope: &BTreeMap<String, RawFile>,
) -> Result<(Vec<String>, Vec<String>), CodegenError> {
    let response = envelope.get("response").ok_or_else(|| {
        CodegenError::Ir("envelope/response.json missing — required for error-code families".into())
    })?;
    let families = response
        .value
        .get("x-cairn-error-code-families")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CodegenError::Ir("envelope/response.json missing x-cairn-error-code-families".into())
        })?;
    let collect = |key: &str| -> Result<Vec<String>, CodegenError> {
        families
            .get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CodegenError::Ir(format!(
                    "x-cairn-error-code-families.{key} must be an array"
                ))
            })?
            .iter()
            .map(|v| {
                v.as_str().map(String::from).ok_or_else(|| {
                    CodegenError::Ir(format!(
                        "x-cairn-error-code-families.{key}: items must be strings"
                    ))
                })
            })
            .collect()
    };
    Ok((collect("rejected")?, collect("aborted")?))
}

/// Ingest type definitions from a single IDL file into `out`.
///
/// For files that have `$defs`, every entry is lowered individually under its
/// (`PascalCase`-normalised) `$defs` key. For top-level schema files (e.g.
/// `common/scope_filter.json`) the file's stem is `PascalCase`'d and used as
/// the type name — this matches the convention used by `typename_from_ref`
/// for fragment-less `$ref`s, so the IR map and consumer field types agree.
fn ingest_defs_into(
    file: &RawFile,
    out: &mut BTreeMap<TypeName, RustType>,
) -> Result<(), CodegenError> {
    if let Some(defs) = file.value.get("$defs").and_then(Value::as_object) {
        for (name, def) in defs {
            let pascal = pascal_case(name);
            let mut ctx = Ctx::with_target(&pascal);
            let ty = lower_schema(def, &mut ctx)?;
            out.insert(TypeName::new(pascal), ty);
        }
    } else {
        // Top-level schema (e.g. scope_filter.json) — use the file stem.
        let stem = file
            .rel_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !stem.is_empty() {
            let name = TypeName::new(pascal_case(stem));
            let mut ctx = Ctx::with_target(&name.0);
            let ty = lower_schema(&file.value, &mut ctx)?;
            out.insert(name, ty);
        }
    }
    Ok(())
}

/// Lower the `oneOf` in `errors/error.json` into a flat `Vec<ErrorVariant>`.
fn build_error_codes(
    errors: &BTreeMap<String, RawFile>,
) -> Result<Vec<ErrorVariant>, CodegenError> {
    let file = errors
        .get("error")
        .ok_or_else(|| CodegenError::Ir("errors/error.json missing".to_string()))?;
    let one_of = file
        .value
        .get("oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| CodegenError::Ir("errors.json missing oneOf".to_string()))?;
    let mut out = Vec::with_capacity(one_of.len());
    for entry in one_of {
        let code = entry
            .pointer("/properties/code/const")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CodegenError::Ir("error variant missing properties.code.const".to_string())
            })?
            .to_string();
        let data = entry
            .pointer("/properties/data/$ref")
            .and_then(Value::as_str)
            .map(typename_from_ref);
        out.push(ErrorVariant { code, data });
    }
    Ok(out)
}

/// Lower one verb file into a [`VerbDef`].
#[allow(
    clippy::too_many_lines,
    reason = "single-pass lowering keeps the verb plumbing in one place; splitting it would obscure the data flow"
)]
fn build_verb(file: &RawFile) -> Result<VerbDef, CodegenError> {
    let path_str = file.rel_path.to_string_lossy();

    let id = file
        .value
        .get("x-cairn-verb-id")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir(format!("{path_str}: x-cairn-verb-id missing")))?
        .to_string();

    // Collect all `$defs` entries so tagged-union lowering can resolve local refs.
    let defs: BTreeMap<String, Value> = file
        .value
        .get("$defs")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    let args_schema = file
        .value
        .pointer("/$defs/Args")
        .ok_or_else(|| CodegenError::Ir(format!("{path_str}: $defs.Args missing")))?;
    let data_schema = file
        .value
        .pointer("/$defs/Data")
        .ok_or_else(|| CodegenError::Ir(format!("{path_str}: $defs.Data missing")))?;

    let target_args = format!("{}Args", pascal_case(&id));
    let target_data = format!("{}Data", pascal_case(&id));

    // Lower Args — special-case search's `filters` field to use lower_filter_root.
    let mut args_ctx = Ctx::with_target(&target_args).with_defs(defs.clone());
    let args = if id == "search" {
        let RustType::Struct(mut s) = lower_schema(args_schema, &mut args_ctx)? else {
            return Err(CodegenError::Ir(
                "search.$defs.Args expected to lower to Struct".to_string(),
            ));
        };
        if let Some(field) = s.fields.iter_mut().find(|f| f.name == "filters") {
            let filter_def = file
                .value
                .pointer("/$defs/filter")
                .cloned()
                .ok_or_else(|| CodegenError::Ir("search.json missing /$defs/filter".to_string()))?;
            let mut filter_ctx = Ctx::with_target("Filter").with_defs(defs.clone());
            let filter_ty = lower_filter_root(&filter_def, &mut filter_ctx)?;
            // Preserve the Optional wrapper that lower_schema produced; replace inner.
            field.ty = RustType::Optional(Box::new(filter_ty));
        }
        RustType::Struct(s)
    } else {
        lower_schema(args_schema, &mut args_ctx)?
    };

    let mut data_ctx = Ctx::with_target(&target_data).with_defs(defs.clone());
    let data = lower_schema(data_schema, &mut data_ctx)?;

    // Walk Args/Data and lower any local `$defs` entry referenced by name but
    // not already covered by Args/Data themselves (e.g. `search.$defs.Hit`).
    // The filter family is intentionally excluded — it is collapsed into the
    // recursive `Filter` enum by `lower_filter_root`.
    let mut local_types: BTreeMap<TypeName, RustType> = BTreeMap::new();
    let mut wanted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    collect_ref_names(&args, &mut wanted);
    collect_ref_names(&data, &mut wanted);
    // Retrieve's `Data` is a oneOf without a discriminator (the dispatch lives
    // on the response envelope's `target` field), so the generic walker above
    // falls through to `RustType::Json` and harvests no refs. Force-include the
    // six per-target Data sub-types so the response envelope can dispatch
    // typed payloads against them. See response.json's per-target if/then arms.
    if id == "retrieve" {
        for sub in [
            "DataRecord",
            "DataProfile",
            "DataSession",
            "DataTurn",
            "DataFolder",
            "DataScope",
        ] {
            wanted.insert(sub.to_string());
        }
    }
    // Iterate to a fixed point so transitive refs resolve. Each pass lowers
    // every newly-wanted def and harvests any refs *its* lowered shape needs.
    // Bounded by `defs.len()` (each def can only be added once).
    for _ in 0..=defs.len() {
        let mut added = false;
        for (def_name, def_value) in &defs {
            if def_name == "Args" || def_name == "Data" {
                continue;
            }
            if def_name == "filter" || def_name.starts_with("filter_") {
                continue;
            }
            let pascal = pascal_case(def_name);
            if !wanted.contains(&pascal) || local_types.contains_key(&TypeName::new(&pascal)) {
                continue;
            }
            let mut local_ctx = Ctx::with_target(&pascal).with_defs(defs.clone());
            let lowered = lower_schema(def_value, &mut local_ctx)?;
            collect_ref_names(&lowered, &mut wanted);
            local_types.insert(TypeName::new(pascal), lowered);
            added = true;
        }
        if !added {
            break;
        }
    }

    let cli = build_cli_shape(&file.value, &args)?;
    let skill = parse_skill_block(&file.value);
    let capability = file
        .value
        .get("x-cairn-capability")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let auth_str = file
        .value
        .get("x-cairn-auth")
        .and_then(Value::as_str)
        .ok_or_else(|| CodegenError::Ir(format!("{path_str}: x-cairn-auth missing")))?;
    let auth = AuthModel::from_str(auth_str).ok_or_else(|| {
        CodegenError::Ir(format!(
            "{path_str}: unknown x-cairn-auth value {auth_str:?}"
        ))
    })?;
    let auth_overrides = collect_auth_overrides(args_schema, &defs, &path_str)?;
    let capability_overrides = collect_capability_overrides(args_schema, &defs, &path_str)?;

    Ok(VerbDef {
        id,
        args,
        data,
        local_types,
        cli,
        skill,
        capability,
        auth,
        auth_overrides,
        capability_overrides,
        // Capture the *entire* verb file so any `#/$defs/...` reference inside
        // Args/Data resolves against siblings present in the same on-disk JSON.
        // Cross-file `$ref`s like `../common/scope_filter.json` resolve via the
        // sibling `common/`, `errors/`, `capabilities/`, `extensions/`, and
        // `envelope/` schema bundles emitted by `emit_mcp` alongside the
        // verbs/ directory.
        args_schema_bytes: serde_json::to_vec(&file.value)
            .map_err(|e| CodegenError::Ir(e.to_string()))?,
        data_schema_bytes: serde_json::to_vec(data_schema)
            .map_err(|e| CodegenError::Ir(e.to_string()))?,
    })
}

/// Walk the verb's `$defs/Args` schema and harvest every `x-cairn-auth`
/// annotation found below the root. Two shapes are supported:
///
/// * Property-level — `Args.properties.<name>.x-cairn-auth: "<auth>"`.
///   Yields an [`AuthOverride`] with `path: <name>` (e.g. `write_report`).
///
/// * Sub-type-level — `Args` is a `oneOf` over `#/$defs/Args*` entries
///   (forget, retrieve), each carrying `x-cairn-auth`. Yields an
///   [`AuthOverride`] with `path: <discriminator>=<wire-const>`.
fn collect_auth_overrides(
    args_schema: &Value,
    defs: &BTreeMap<String, Value>,
    where_: &str,
) -> Result<Vec<AuthOverride>, CodegenError> {
    let mut out = Vec::new();

    // Sub-type dispatch (forget, retrieve): each oneOf branch refs an
    // ArgsXxx $defs entry whose discriminator value identifies the mode.
    if let Some(arr) = args_schema.get("oneOf").and_then(Value::as_array) {
        let discriminator = args_schema
            .get("x-cairn-discriminator")
            .and_then(Value::as_str);
        for entry in arr {
            let Some(reference) = entry.get("$ref").and_then(Value::as_str) else {
                continue;
            };
            let Some(def_name) = reference.strip_prefix("#/$defs/") else {
                continue;
            };
            let Some(def) = defs.get(def_name) else {
                continue;
            };
            let Some(auth_str) = def.get("x-cairn-auth").and_then(Value::as_str) else {
                continue;
            };
            let auth = AuthModel::from_str(auth_str).ok_or_else(|| {
                CodegenError::Ir(format!(
                    "{where_}: $defs.{def_name}.x-cairn-auth = {auth_str:?} is not a known AuthModel"
                ))
            })?;
            // Try to extract the discriminator wire value to build a stable path.
            let path = if let Some(disc) = discriminator
                && let Some(wire) = def
                    .pointer(&format!("/properties/{disc}/const"))
                    .and_then(Value::as_str)
            {
                format!("{disc}={wire}")
            } else {
                def_name.to_string()
            };
            out.push(AuthOverride { path, auth });
        }
    }

    // Property-level: walk Args.properties and pick up `x-cairn-auth`.
    if let Some(props) = args_schema.get("properties").and_then(Value::as_object) {
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        for k in keys {
            let prop = &props[k];
            let Some(auth_str) = prop.get("x-cairn-auth").and_then(Value::as_str) else {
                continue;
            };
            let auth = AuthModel::from_str(auth_str).ok_or_else(|| {
                CodegenError::Ir(format!(
                    "{where_}: Args.properties.{k}.x-cairn-auth = {auth_str:?} is not a known AuthModel"
                ))
            })?;
            out.push(AuthOverride {
                path: k.clone(),
                auth,
            });
        }
    }

    Ok(out)
}

/// Walk the verb's `$defs/Args` schema and harvest every `x-cairn-capability`
/// annotation found below the root. Three shapes are supported, mirroring
/// [`collect_auth_overrides`] plus a const-level form:
///
/// * Sub-type-level — `Args` is a `oneOf` over `#/$defs/Args*` entries
///   (forget, retrieve), each carrying `x-cairn-capability`. Yields a
///   [`CapabilityOverride`] with `path: <discriminator>=<wire-const>`.
///
/// * Const-level (closed string enum) — a property whose value is a
///   `oneOf` of `{const: <wire>, x-cairn-capability: <cap>}` entries
///   (search.mode). Yields one override per const with
///   `path: <property>=<wire>`.
///
/// * Property-level — `Args.properties.<name>.x-cairn-capability: "<cap>"`.
///   Yields a [`CapabilityOverride`] with `path: <name>`. (Currently
///   unused by any verb; kept for symmetry so future IDL annotations
///   land without re-plumbing.)
#[allow(
    clippy::unnecessary_wraps,
    reason = "mirrors collect_auth_overrides signature; future IDL constructs may surface errors"
)]
fn collect_capability_overrides(
    args_schema: &Value,
    defs: &BTreeMap<String, Value>,
    where_: &str,
) -> Result<Vec<CapabilityOverride>, CodegenError> {
    let mut out = Vec::new();

    // Sub-type dispatch (forget, retrieve).
    if let Some(arr) = args_schema.get("oneOf").and_then(Value::as_array) {
        let discriminator = args_schema
            .get("x-cairn-discriminator")
            .and_then(Value::as_str);
        for entry in arr {
            let Some(reference) = entry.get("$ref").and_then(Value::as_str) else {
                continue;
            };
            let Some(def_name) = reference.strip_prefix("#/$defs/") else {
                continue;
            };
            let Some(def) = defs.get(def_name) else {
                continue;
            };
            let Some(cap) = def.get("x-cairn-capability").and_then(Value::as_str) else {
                continue;
            };
            let path = if let Some(disc) = discriminator
                && let Some(wire) = def
                    .pointer(&format!("/properties/{disc}/const"))
                    .and_then(Value::as_str)
            {
                format!("{disc}={wire}")
            } else {
                def_name.to_string()
            };
            out.push(CapabilityOverride {
                path,
                capability: cap.to_string(),
            });
        }
    }

    // Property-level + const-level walk over Args.properties.
    if let Some(props) = args_schema.get("properties").and_then(Value::as_object) {
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        for k in keys {
            let prop = &props[k];
            // Property-level.
            if let Some(cap) = prop.get("x-cairn-capability").and_then(Value::as_str) {
                out.push(CapabilityOverride {
                    path: k.clone(),
                    capability: cap.to_string(),
                });
            }
            // Const-level — `oneOf` of `{const: <wire>, x-cairn-capability: <cap>}`
            // entries (search.mode pattern). Each const becomes its own override
            // keyed by `<property>=<wire>` so the MCP layer can reason about the
            // specific reachable mode.
            if let Some(arr) = prop.get("oneOf").and_then(Value::as_array) {
                for entry in arr {
                    let Some(wire) = entry.get("const").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(cap) = entry.get("x-cairn-capability").and_then(Value::as_str) else {
                        continue;
                    };
                    out.push(CapabilityOverride {
                        path: format!("{k}={wire}"),
                        capability: cap.to_string(),
                    });
                }
            }
        }
    }

    // Sanity: we shouldn't emit unparseable content. If the IDL grew an
    // unknown capability shape we'd silently drop it; callers should still
    // be able to surface an error path. Right now the function never fails,
    // so propagate `Result` for symmetry with `collect_auth_overrides`.
    let _ = where_;
    Ok(out)
}

/// Walk a [`RustType`] and accumulate every `Ref` target name into `out`.
/// Names land already `PascalCase`'d by the loader.
fn collect_ref_names(ty: &RustType, out: &mut std::collections::BTreeSet<String>) {
    match ty {
        RustType::Ref(name) => {
            out.insert(name.0.clone());
        }
        RustType::Optional(inner) | RustType::Vec(inner) | RustType::Map(inner) => {
            collect_ref_names(inner, out);
        }
        RustType::Struct(s) => {
            for f in &s.fields {
                collect_ref_names(&f.ty, out);
            }
        }
        RustType::TaggedUnion(t) => {
            for v in &t.variants {
                for f in &v.fields {
                    collect_ref_names(&f.ty, out);
                }
            }
        }
        RustType::UntaggedUnion(u) => {
            for f in &u.fields {
                collect_ref_names(&f.ty, out);
            }
        }
        RustType::Recursive(r) => {
            collect_ref_names(&r.leaf, out);
        }
        RustType::Enum(_) | RustType::Primitive(_) | RustType::Json => {}
    }
}

/// Build the [`CliShape`] for a verb.
///
/// When `args` is a [`RustType::TaggedUnion`] the shape is built from per-variant
/// `x-cairn-cli` blocks (the top-level `x-cairn-cli` is ignored for tagged-union
/// verbs). Otherwise the top-level block is used.
fn build_cli_shape(verb_value: &Value, args: &RustType) -> Result<CliShape, CodegenError> {
    if let RustType::TaggedUnion(t) = args {
        let mut variants = Vec::with_capacity(t.variants.len());
        for v in &t.variants {
            let cli = v.cli.clone().ok_or_else(|| {
                CodegenError::Ir(format!("tagged variant {:?} missing x-cairn-cli", v.wire))
            })?;
            variants.push(cli);
        }
        return Ok(CliShape::Variants(variants));
    }
    let block = verb_value
        .get("x-cairn-cli")
        .ok_or_else(|| CodegenError::Ir("verb missing top-level x-cairn-cli".to_string()))?;
    Ok(CliShape::Single(parse_cli_block(block)?))
}

/// Extract `x-cairn-skill-triggers` from a verb file into a [`SkillBlock`].
fn parse_skill_block(verb_value: &Value) -> SkillBlock {
    let Some(block) = verb_value.get("x-cairn-skill-triggers") else {
        return SkillBlock::default();
    };
    SkillBlock {
        positive: block
            .get("positive")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        negative: block
            .get("negative")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        exclusivity: block
            .get("exclusivity")
            .and_then(Value::as_str)
            .map(String::from),
    }
}
