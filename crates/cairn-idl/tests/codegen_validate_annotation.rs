#![allow(missing_docs)]

use cairn_idl::codegen::emit_sdk;
use cairn_idl::codegen::{ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

fn assemble_hot_body() -> String {
    let files = emit_sdk::emit(&doc()).unwrap();
    let file = files
        .iter()
        .find(|f| {
            f.path
                .ends_with("crates/cairn-core/src/generated/verbs/assemble_hot.rs")
        })
        .expect("assemble_hot.rs must be generated");
    std::str::from_utf8(&file.bytes)
        .expect("generated file must be valid UTF-8")
        .to_owned()
}

/// The `Data` struct in `assemble_hot.json` carries `x-cairn-validate: true`.
/// Codegen must emit `#[serde(try_from = "AssembleHotDataRaw", into = "AssembleHotDataRaw")]`
/// on the main struct; serde uses this to route deserialization through `TryFrom`.
#[test]
fn validate_struct_has_try_from_serde_attr() {
    let body = assemble_hot_body();
    assert!(
        body.contains(r#"#[serde(try_from = "AssembleHotDataRaw", into = "AssembleHotDataRaw")]"#),
        "AssembleHotData must carry #[serde(try_from = \"AssembleHotDataRaw\", into = \"AssembleHotDataRaw\")]\n\nGenerated body:\n{body}"
    );
}

/// The `Data` struct must have `Deserialize` in its derive AND carry the
/// `#[serde(try_from = "...")]` attribute. Together these cause serde to
/// generate a `Deserialize` impl that routes through `TryFrom<AssembleHotDataRaw>`,
/// so every deserialization path (JSON, MCP, SDK) runs the invariant checks.
///
/// Without `Deserialize` in the derive there would be no deserialization at
/// all; without `try_from` the raw bytes would be accepted unchecked.
#[test]
fn validate_struct_derives_deserialize_via_try_from() {
    let body = assemble_hot_body();
    // Find the derive line immediately before `pub struct AssembleHotData {`.
    let struct_pos = body
        .find("pub struct AssembleHotData {")
        .expect("pub struct AssembleHotData must exist in generated output");
    let before = &body[..struct_pos];
    let derive_line_start = before
        .rfind("#[derive(")
        .expect("derive must precede struct");
    let derive_line_end = before[derive_line_start..]
        .find('\n')
        .map_or(before.len(), |n| derive_line_start + n);
    let derive_line = &before[derive_line_start..derive_line_end];
    // Deserialize MUST be present so serde generates the routed impl.
    assert!(
        derive_line.contains("Deserialize"),
        "AssembleHotData derive line must contain Deserialize (for serde try_from routing), got: {derive_line}"
    );
    // The try_from attribute is separately tested in validate_struct_has_try_from_serde_attr,
    // but assert it here too so this test is self-contained.
    assert!(
        body.contains(r#"#[serde(try_from = "AssembleHotDataRaw", into = "AssembleHotDataRaw")]"#),
        "AssembleHotData must also carry #[serde(try_from = ...)] to route through TryFrom"
    );
}

/// The `<Name>Raw` mirror struct must be emitted after the main struct.
#[test]
fn validate_raw_mirror_struct_is_emitted() {
    let body = assemble_hot_body();
    assert!(
        body.contains("pub struct AssembleHotDataRaw"),
        "pub struct AssembleHotDataRaw must appear in generated assemble_hot.rs\n\nGenerated body:\n{body}"
    );
}

/// The Raw mirror must derive both `Serialize` and `Deserialize`.
#[test]
fn validate_raw_mirror_derives_serialize_and_deserialize() {
    let body = assemble_hot_body();
    let raw_struct_pos = body
        .find("pub struct AssembleHotDataRaw")
        .expect("pub struct AssembleHotDataRaw must exist");
    let before = &body[..raw_struct_pos];
    let derive_start = before
        .rfind("#[derive(")
        .expect("derive must precede AssembleHotDataRaw");
    let derive_end = before[derive_start..]
        .find('\n')
        .map_or(before.len(), |n| derive_start + n);
    let derive_line = &before[derive_start..derive_end];
    assert!(
        derive_line.contains("Serialize"),
        "AssembleHotDataRaw derive must contain Serialize, got: {derive_line}"
    );
    assert!(
        derive_line.contains("Deserialize"),
        "AssembleHotDataRaw derive must contain Deserialize, got: {derive_line}"
    );
}

/// The Raw mirror must carry `#[serde(deny_unknown_fields)]`.
#[test]
fn validate_raw_mirror_has_deny_unknown_fields() {
    let body = assemble_hot_body();
    let raw_struct_pos = body
        .find("pub struct AssembleHotDataRaw")
        .expect("pub struct AssembleHotDataRaw must exist");
    let before = &body[..raw_struct_pos];
    // Within the last 200 chars before the struct declaration there should be
    // deny_unknown_fields.
    let window_start = before.len().saturating_sub(200);
    let window = &before[window_start..];
    assert!(
        window.contains(r"#[serde(deny_unknown_fields)]"),
        "AssembleHotDataRaw must carry #[serde(deny_unknown_fields)]\n\nWindow before struct:\n{window}"
    );
}
