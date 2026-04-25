//! Deterministic formatting helpers shared by every emitter.

use serde_json::Value;

/// Serialise a `serde_json::Value` deterministically: object keys sorted
/// recursively, two-space indent, trailing newline. Arrays preserve order.
#[must_use]
pub fn write_json_canonical(value: &Value) -> String {
    let mut buf = String::new();
    write_inner(value, 0, &mut buf);
    buf.push('\n');
    buf
}

fn write_inner(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in arr.iter().enumerate() {
                push_indent(depth + 1, out);
                write_inner(item, depth + 1, out);
                if i + 1 < arr.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(depth, out);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push_str("{\n");
            for (i, key) in keys.iter().enumerate() {
                push_indent(depth + 1, out);
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_inner(&map[*key], depth + 1, out);
                if i + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(depth, out);
            out.push('}');
        }
    }
}

fn push_indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Tiny Rust source builder. Tracks indent (4 spaces per level) and ensures
/// the final string ends with exactly one trailing newline.
#[derive(Debug, Default)]
pub struct RustWriter {
    buf: String,
    depth: usize,
}

impl RustWriter {
    /// Create a new, empty `RustWriter` at indent level zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Increase indent by one level (4 spaces).
    pub fn indent(&mut self) {
        self.depth += 1;
    }

    /// Decrease indent by one level. Panics in debug builds if already at zero.
    pub fn dedent(&mut self) {
        debug_assert!(self.depth > 0, "dedent below zero");
        self.depth -= 1;
    }

    /// Append a blank line (no indent, no content).
    pub fn blank(&mut self) {
        self.buf.push('\n');
    }

    /// Append `s` at the current indent level followed by a newline.
    pub fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.buf.push_str("    ");
        }
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    /// Append raw text with no indent or newline manipulation.
    pub fn raw(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    /// Consume the writer, returning the accumulated source with exactly one
    /// trailing newline.
    #[must_use]
    pub fn finish(mut self) -> String {
        // Collapse any accidental trailing whitespace and ensure exactly one '\n'.
        while self.buf.ends_with('\n') {
            self.buf.pop();
        }
        self.buf.push('\n');
        self.buf
    }
}
