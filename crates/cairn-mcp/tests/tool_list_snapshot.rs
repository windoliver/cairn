//! Snapshot: tool names and descriptions must not drift accidentally.
#![allow(missing_docs)]

use cairn_mcp::generated::TOOLS;

#[test]
fn tool_names_snapshot() {
    let names: Vec<&str> = TOOLS.iter().map(|d| d.name).collect();
    insta::assert_snapshot!(names.join("\n"));
}

#[test]
fn tool_description_first_lines_snapshot() {
    let summary: Vec<String> = TOOLS
        .iter()
        .map(|d| {
            let first_line = d
                .description
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            format!("{}: {first_line}", d.name)
        })
        .collect();

    insta::assert_snapshot!(summary.join("\n"));
}
