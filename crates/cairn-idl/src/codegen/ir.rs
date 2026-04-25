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
