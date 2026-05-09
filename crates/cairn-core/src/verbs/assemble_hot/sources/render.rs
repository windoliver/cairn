//! Per-record body framing shared by every record-backed source.

use crate::domain::record::MemoryRecord;

/// Render one record into the canonical hot-prefix block:
///
/// ```text
/// ## <kind>: <first-body-line>
/// <body>
///
/// ```
///
/// Trailing blank line separates blocks. Identical for every source so
/// downstream consumers never have to special-case ranking origin.
#[must_use]
pub fn render_record_block(record: &MemoryRecord) -> String {
    let first_line = record.body.lines().next().unwrap_or_default();
    let mut out = String::with_capacity(record.body.len() + 64);
    out.push_str("## ");
    out.push_str(record.kind.as_str());
    out.push_str(": ");
    out.push_str(first_line);
    out.push('\n');
    out.push_str(&record.body);
    if !record.body.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;

    #[test]
    fn renders_canonical_block() {
        let r = sample_record();
        let out = render_record_block(&r);
        assert!(out.starts_with("## user: user prefers dark mode\n"));
        assert!(out.ends_with("\n\n"));
        assert!(out.contains("user prefers dark mode"));
    }

    #[test]
    fn handles_record_body_without_trailing_newline() {
        let mut r = sample_record();
        r.body = "single line".to_owned();
        let out = render_record_block(&r);
        assert!(out.ends_with("\n\n"), "must always end with blank line");
    }
}
