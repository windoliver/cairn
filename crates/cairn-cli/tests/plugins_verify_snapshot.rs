//! Snapshot test: `cairn plugins verify` human + JSON outputs are stable.

use cairn_cli::plugins::{host::register_all, verify};

#[test]
fn verify_human_snapshot() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    let text = verify::render_human(&report);
    insta::assert_snapshot!("plugins_verify_human", text);
}

#[test]
fn verify_json_snapshot() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    let json = verify::render_json(&report);
    insta::assert_snapshot!("plugins_verify_json", json);
}

#[test]
fn verify_default_mode_exit_zero() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    assert_eq!(verify::exit_code(&report, false), 0);
}

#[test]
fn verify_strict_mode_exit_69_with_pendings() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    assert_eq!(verify::exit_code(&report, true), 69);
}
