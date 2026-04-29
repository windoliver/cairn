//! Regression guard: `usr:` prefix must not appear as live (non-test)
//! usage in workspace Rust source files.
//!
//! The `usr:` prefix was the pre-#50 name for human identities; it was
//! replaced wholesale by `hmn:` in commit `a097781`.  This test ensures no
//! future commit silently reintroduces it.
//!
//! Allowed exceptions (all exempted inline below):
//!   - Lines that are pure `//` comments (documentation of the old name).
//!   - Lines inside rejection-test helpers that deliberately pass `"usr:…"`
//!     to `Identity::parse` in order to assert the prefix is invalid.
//!     These are identified by the substring `parse("usr:` or `parse('usr:`.

use std::path::{Path, PathBuf};

/// Recursively collect every `*.rs` file under `dir`, skipping `target/`,
/// `.git/`, and `node_modules/`.
fn walk_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("target" | ".git" | "node_modules")
            );
            if !skip {
                walk_rust_files(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Returns `true` when the line is explicitly allowed to contain `usr:`.
///
/// Two categories are whitelisted:
///   1. Pure comment lines (begin with `//` after leading whitespace) —
///      these may reference `usr:` as historical nomenclature.
///   2. `Identity::parse` call lines inside rejection tests — the substring
///      `parse("usr:` or `parse('usr:` signals an intentional negative test,
///      not live usage of the banned prefix.
fn is_allowed(line: &str, file: &Path) -> bool {
    let trimmed = line.trim_start();
    // Rule 1: pure comment lines.
    if trimmed.starts_with("//") {
        return true;
    }
    // Rule 2: rejection-test parse calls — intentional negative-test literals.
    if trimmed.contains("parse(\"usr:") || trimmed.contains("parse('usr:") {
        return true;
    }
    // Rule 3: this guard file itself — string literals in the guard are
    // necessarily about `usr:` but are not live usage of the prefix.
    if file
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "usr_prefix_banned.rs")
    {
        return true;
    }
    false
}

#[test]
fn no_usr_colon_in_workspace_rust_sources() {
    // Resolve workspace root from this crate's manifest dir.
    //   CARGO_MANIFEST_DIR = …/crates/cairn-core
    //   parent()           = …/crates
    //   parent()           = …/ (workspace root)
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("crate lives inside crates/")
        .parent()
        .expect("crates/ is direct child of workspace root");

    let mut files = Vec::new();
    walk_rust_files(workspace_root, &mut files);

    let mut offenders: Vec<String> = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        for (i, line) in src.lines().enumerate() {
            if line.contains("usr:") && !is_allowed(line, f) {
                offenders.push(format!(
                    "{}:{}: {}",
                    f.display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Found banned `usr:` prefix in workspace sources \
         (use `hmn:` for human identities — brief §4.2):\n{}",
        offenders.join("\n"),
    );
}
