//! Integration: shell out to the built `cairn` binary and assert the
//! `plugins verify --json` output. This is the CI-protective wrapper:
//! it runs under `cargo nextest` regardless of any workflow-yaml drift.

use std::process::Command;

fn cairn_binary() -> std::path::PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for every binary in the package.
    let raw = env!("CARGO_BIN_EXE_cairn");
    std::path::PathBuf::from(raw)
}

#[test]
fn plugins_verify_json_default_succeeds() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--json"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins verify --json must exit 0 in default mode; got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(v["summary"]["failed"], 0, "no tier-1 failures expected");
    assert_eq!(
        v["plugins"].as_array().expect("plugins array").len(),
        7,
        "all seven bundled plugins must be reported"
    );
}

#[test]
fn plugins_verify_json_exercises_local_sensor_registration_e2e() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--json"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins verify --json must exit 0 in default mode; got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let plugins = v["plugins"].as_array().expect("plugins array");
    let sensors = plugins
        .iter()
        .find(|plugin| plugin["name"] == "cairn-sensors-local")
        .expect("cairn-sensors-local plugin reported");

    assert_eq!(sensors["contract"], "SensorIngress");
    assert_case_status(sensors, "manifest_matches_host", "ok");
    assert_case_status(sensors, "capability_self_consistency_floor", "ok");
    assert_case_status(sensors, "manifest_features_match_capabilities", "ok");
    assert_case_status(sensors, "emits_envelope_when_poked", "pending");
}

#[test]
fn plugins_verify_json_reports_mcp_stdio_runtime_e2e() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--json"])
        .output()
        .expect("spawn cairn binary");

    assert!(output.status.success(), "exit: {:?}", output.status);

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let plugins = v["plugins"].as_array().expect("plugins array");
    let mcp = plugins
        .iter()
        .find(|plugin| plugin["name"] == "cairn-mcp")
        .expect("cairn-mcp plugin present");

    assert_case_status(mcp, "manifest_features_match_capabilities", "ok");
    assert_case_status(mcp, "initialize_and_list_tools", "ok");
}

#[test]
fn plugins_verify_strict_exits_69_with_pendings() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--strict"])
        .output()
        .expect("spawn cairn binary");

    let code = output.status.code().expect("exit code present");
    assert_eq!(
        code, 69,
        "verify --strict must exit 69 while tier-2 cases are pending"
    );
}

#[test]
fn mcp_subcommand_help_is_available() {
    let output = Command::new(cairn_binary())
        .args(["mcp", "--help"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn mcp --help must succeed; got {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("Start an MCP stdio server"));
}

#[test]
fn plugins_list_emits_alphabetical_rows() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "list"])
        .output()
        .expect("spawn cairn binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");

    let mcp_idx = stdout.find("cairn-mcp").expect("mcp present");
    let sensors_idx = stdout.find("cairn-sensors-local").expect("sensors present");
    let store_idx = stdout.find("cairn-store-sqlite").expect("store present");
    let workflows_idx = stdout.find("cairn-workflows").expect("workflows present");

    assert!(mcp_idx < sensors_idx);
    assert!(sensors_idx < store_idx);
    assert!(store_idx < workflows_idx);
}

#[test]
fn plugins_describe_mcp_prints_rendered_tool_descriptions() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "describe", "--mcp"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins describe --mcp must exit 0; got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    for tool in cairn_mcp::generated::TOOLS {
        assert!(
            stdout.contains(tool.description),
            "describe output must include the rendered description for {}",
            tool.name
        );
    }
}

#[test]
fn plugins_describe_mcp_output_is_exactly_generated_and_diff_friendly() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "describe", "--mcp"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins describe --mcp must exit 0; got {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let mut expected = cairn_mcp::generated::TOOLS
        .iter()
        .map(|tool| format!("# {}\n{}", tool.name, tool.description))
        .collect::<Vec<_>>()
        .join("\n\n");
    expected.push('\n');

    assert_eq!(
        stdout, expected,
        "describe --mcp should be a stable, newline-terminated projection of generated TOOLS"
    );
}

#[test]
fn plugins_list_json_reports_mcp_stdio_capability_e2e() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "list", "--json"])
        .output()
        .expect("spawn cairn binary");
    assert!(output.status.success(), "exit: {:?}", output.status);

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let plugins = v["plugins"].as_array().expect("plugins array");
    let mcp = plugins
        .iter()
        .find(|plugin| plugin["name"] == "cairn-mcp")
        .expect("cairn-mcp plugin present");

    assert_eq!(mcp["capabilities"]["stdio"], true);
    assert_eq!(mcp["capabilities"]["sse"], false);
    assert_eq!(mcp["capabilities"]["http_streamable"], false);
}

fn assert_case_status(plugin: &serde_json::Value, case_id: &str, expected: &str) {
    let cases = plugin["cases"].as_array().expect("cases array");
    let case = cases
        .iter()
        .find(|case| case["id"] == case_id)
        .unwrap_or_else(|| panic!("missing conformance case {case_id}: {plugin}"));

    assert_eq!(case["status"], expected, "case {case_id}: {case}");
}
