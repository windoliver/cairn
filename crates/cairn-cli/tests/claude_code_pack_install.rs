//! Per-emitted-file snapshot test for the cairn-claude-code pack install.
//!
//! Asserts that the bundled pack installs into an empty tempdir
//! deterministically. Snapshot files land under
//! `crates/cairn-cli/tests/snapshots/claude_code_pack_install__*.snap`.

use std::path::Path;

use cairn_cli::packs::{
    install::{PackInstallOpts, install_pack},
    manifest::Harness,
};
use tempfile::tempdir;

const EXPECTED_AGENT_IDS: &[&str] = &[
    "context-loader",
    "vault-librarian",
    "forget-planner",
    "consolidator",
    "replay-checker",
    "trace-summarizer",
];

const EXPECTED_COMMAND_IDS: &[&str] = &[
    "cairn-ingest",
    "cairn-search",
    "cairn-retrieve",
    "cairn-summarize",
    "cairn-assemble",
    "cairn-capture-trace",
    "cairn-lint",
    "cairn-forget",
    "cairn-status",
    "cairn-standup",
    "cairn-wrap-up",
    "cairn-audit",
    "cairn-recall",
];

#[test]
fn install_bundled_pack_into_tempdir() {
    let tmp = tempdir().expect("tempdir");
    let opts = PackInstallOpts {
        harness: Harness::ClaudeCode,
        project_dir: tmp.path().to_path_buf(),
        force: false,
    };
    let receipt = install_pack(&opts).expect("install");

    // 1. Receipt envelope snapshot.
    let mut receipt_for_snapshot = receipt.clone();
    receipt_for_snapshot.files_created = strip_tempdir(&receipt.files_created, tmp.path());
    receipt_for_snapshot.files_merged = strip_tempdir(&receipt.files_merged, tmp.path());
    receipt_for_snapshot.files_skipped = strip_tempdir(&receipt.files_skipped, tmp.path());
    insta::assert_json_snapshot!("receipt", receipt_for_snapshot);

    // 2. Subagents.
    for id in EXPECTED_AGENT_IDS {
        let body = std::fs::read_to_string(tmp.path().join(format!(".claude/agents/{id}.md")))
            .unwrap_or_else(|_| panic!("agent {id} present"));
        insta::assert_snapshot!(format!("agent-{id}"), body);
    }

    // 3. Commands.
    for id in EXPECTED_COMMAND_IDS {
        let body = std::fs::read_to_string(tmp.path().join(format!(".claude/commands/{id}.md")))
            .unwrap_or_else(|_| panic!("command {id} present"));
        insta::assert_snapshot!(format!("command-{id}"), body);
    }

    // The canonical project_dir is rendered into the install-time
    // hook/MCP command strings. Tempdir paths are non-deterministic
    // so we redact them to `<PROJECT_DIR>` before snapshotting.
    let canonical = std::fs::canonicalize(tmp.path()).unwrap();
    let canonical_display = canonical.display().to_string();
    let redact = |s: String| -> String { s.replace(&canonical_display, "<PROJECT_DIR>") };

    // 4. settings.json (post-merge).
    let settings =
        redact(std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap());
    insta::assert_snapshot!("settings", settings);

    // 5. .mcp.json (post-merge).
    let mcp = redact(std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap());
    insta::assert_snapshot!("mcp-json", mcp);

    // 6. CLAUDE.md (block-injected).
    let claude_md = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
    insta::assert_snapshot!("claude-md", claude_md);
}

fn strip_tempdir(paths: &[std::path::PathBuf], base: &Path) -> Vec<std::path::PathBuf> {
    paths
        .iter()
        .map(|p| {
            p.strip_prefix(base).map_or_else(
                |_| p.clone(),
                |sub| std::path::PathBuf::from("./").join(sub),
            )
        })
        .collect()
}
