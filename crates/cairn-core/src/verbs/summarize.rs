//! Deterministic P0 summarize helpers for issue #61.

use crate::domain::MemoryRecord;

/// Render a deterministic, citation-friendly rollup over source records.
#[must_use]
pub fn render_summary(records: &[MemoryRecord], citations: bool) -> String {
    let mut rows: Vec<_> = records.iter().collect();
    rows.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    let mut out = String::from("# Summary\n\n");
    for record in rows {
        let snippet = snippet(&record.body, 240);
        if citations {
            out.push_str(&format!("- [{}] {snippet}\n", record.id.as_str()));
        } else {
            out.push_str(&format!("- {snippet}\n"));
        }
    }
    out
}

fn snippet(body: &str, max: usize) -> String {
    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= max {
        return one_line;
    }

    let mut end = max;
    while !one_line.is_char_boundary(end) {
        end -= 1;
    }
    one_line[..end].to_owned()
}
