//! Snapshot test: `cairn plugins list` human + JSON outputs are stable.

use cairn_cli::plugins::{host::register_all, list};

#[test]
fn list_human_snapshot() {
    let reg = register_all().expect("registers");
    let text = list::render_human(&reg);
    insta::assert_snapshot!("plugins_list_human", text);
}

#[test]
fn list_json_snapshot() {
    let reg = register_all().expect("registers");
    let json = list::render_json(&reg);
    insta::assert_snapshot!("plugins_list_json", json);
}
