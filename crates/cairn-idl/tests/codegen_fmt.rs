//! Tests for `cairn_idl::codegen::fmt` — verifies the canonical JSON writer
//! and the Rust source-code helper render bytes deterministically.

use cairn_idl::codegen::fmt::{write_json_canonical, RustWriter};
use serde_json::json;

#[test]
fn json_keys_sorted_and_two_space_indent() {
    let v = json!({"b": 1, "a": {"y": 2, "x": 3}});
    let s = write_json_canonical(&v);
    assert!(s.ends_with('\n'));
    let lines: Vec<&str> = s.split_inclusive('\n').collect();
    // First key must be "a" (sorted).
    assert!(lines.get(1).is_some_and(|l| l.starts_with("  \"a\"")));
}

#[test]
fn json_array_order_preserved() {
    let v = json!(["second", "first"]);
    let s = write_json_canonical(&v);
    let i_first = s.find("first").unwrap();
    let i_second = s.find("second").unwrap();
    assert!(i_second < i_first, "array order must be preserved");
}

#[test]
fn rust_writer_indent_and_trailing_newline() {
    let mut w = RustWriter::new();
    w.line("pub fn demo() {");
    w.indent();
    w.line("return;");
    w.dedent();
    w.line("}");
    let out = w.finish();
    assert_eq!(out, "pub fn demo() {\n    return;\n}\n");
}

#[test]
fn rust_writer_blank_line_no_trailing_whitespace() {
    let mut w = RustWriter::new();
    w.line("a");
    w.blank();
    w.line("b");
    let out = w.finish();
    assert_eq!(out, "a\n\nb\n");
}
