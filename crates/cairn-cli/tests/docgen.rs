//! Tests for maintainer-time generated docs.

use std::path::Path;

use cairn_cli::docgen::{RunMode, RunOpts, run};

fn write_coverage(root: &Path, body: &str) {
    let site = root.join("docs/site");
    std::fs::create_dir_all(&site).expect("create docs/site");
    std::fs::write(site.join("docs-coverage.toml"), body).expect("write coverage");
}

fn all_package_names() -> Vec<String> {
    [
        "cairn-cli",
        "cairn-core",
        "cairn-idl",
        "cairn-mcp",
        "cairn-sensors-local",
        "cairn-store-sqlite",
        "cairn-test-fixtures",
        "cairn-workflows",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn complete_coverage() -> &'static str {
    r#"
[packages.cairn-cli]
audience = "user"
page = "usage/cli.md"
generated_reference = "reference/generated/cli.md"

[packages.cairn-core]
audience = "sdk-author"
page = "reference/rust-api.md"

[packages.cairn-idl]
audience = "maintainer"
page = "reference/idl.md"

[packages.cairn-mcp]
audience = "integration-author"
page = "usage/mcp.md"
generated_reference = "reference/generated/mcp-tools.md"

[packages.cairn-sensors-local]
audience = "operator"
page = "usage/plugins.md"
generated_reference = "reference/generated/plugins.md"

[packages.cairn-store-sqlite]
audience = "operator"
page = "usage/plugins.md"
generated_reference = "reference/generated/plugins.md"

[packages.cairn-test-fixtures]
audience = "internal-test"
page = ""

[packages.cairn-workflows]
audience = "operator"
page = "usage/plugins.md"
generated_reference = "reference/generated/plugins.md"
"#
}

#[test]
fn docgen_command_tree_matches_runtime() {
    let command = cairn_cli::command::build_command();
    let names: Vec<_> = command
        .get_subcommands()
        .map(clap::Command::get_name)
        .collect();
    for expected in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
        "handshake",
        "status",
        "plugins",
        "bootstrap",
    ] {
        assert!(
            names.contains(&expected),
            "runtime command tree missing {expected}; got {names:?}",
        );
    }
}

#[test]
fn docgen_binary_help_is_generated_from_command_tree() {
    let command = cairn_cli::docgen::docgen_command();
    let flags: Vec<_> = command
        .get_arguments()
        .filter_map(clap::Arg::get_long)
        .collect();
    for expected in ["check", "write", "out"] {
        assert!(
            flags.contains(&expected),
            "docgen command missing --{expected}; got {flags:?}",
        );
    }
}

#[test]
fn codegen_binary_help_is_generated_from_command_tree() {
    let command = cairn_idl::codegen::codegen_command();
    let flags: Vec<_> = command
        .get_arguments()
        .filter_map(clap::Arg::get_long)
        .collect();
    for expected in ["check", "out"] {
        assert!(
            flags.contains(&expected),
            "codegen command missing --{expected}; got {flags:?}",
        );
    }
}

#[test]
fn docgen_write_then_check_is_clean() {
    let root = tempfile::tempdir().expect("tempdir");
    write_coverage(root.path(), complete_coverage());

    let write = RunOpts {
        workspace_root: root.path().to_path_buf(),
        mode: RunMode::Write,
        package_names: Some(all_package_names()),
    };
    let report = run(&write).expect("write docs");
    assert!(report.files_emitted >= 6, "report: {report:?}");

    let check = RunOpts {
        mode: RunMode::Check,
        ..write
    };
    let report = run(&check).expect("check docs");
    assert!(
        report.drift.is_empty(),
        "unexpected drift: {:?}",
        report.drift
    );
}

#[test]
fn docgen_detects_generated_file_drift() {
    let root = tempfile::tempdir().expect("tempdir");
    write_coverage(root.path(), complete_coverage());

    let opts = RunOpts {
        workspace_root: root.path().to_path_buf(),
        mode: RunMode::Write,
        package_names: Some(all_package_names()),
    };
    run(&opts).expect("write docs");

    let cli = root.path().join("docs/site/src/reference/generated/cli.md");
    std::fs::write(&cli, "stale generated docs\n").expect("mutate cli docs");

    let check = RunOpts {
        mode: RunMode::Check,
        ..opts
    };
    let report = run(&check).expect("check docs");
    assert!(
        report
            .drift
            .contains(&Path::new("docs/site/src/reference/generated/cli.md").to_path_buf()),
        "drift should include cli.md, got {:?}",
        report.drift,
    );
}

#[test]
fn docgen_requires_package_coverage() {
    let root = tempfile::tempdir().expect("tempdir");
    write_coverage(
        root.path(),
        r#"
[packages.cairn-core]
audience = "sdk-author"
page = "reference/rust-api.md"
"#,
    );

    let opts = RunOpts {
        workspace_root: root.path().to_path_buf(),
        mode: RunMode::Check,
        package_names: Some(vec!["cairn-cli".to_owned(), "cairn-core".to_owned()]),
    };
    let err = run(&opts).expect_err("missing package coverage must fail");
    assert!(
        format!("{err}").contains("cairn-cli"),
        "error should name the missing package: {err}",
    );
}
