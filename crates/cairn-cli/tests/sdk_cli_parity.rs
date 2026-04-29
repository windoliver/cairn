//! Cross-surface parity for the P0 prelude (`status`, `handshake`).
//!
//! Spawn the `cairn` binary, capture its `--json` output, and compare
//! the structural shape against `cairn_sdk::Sdk` output for the same
//! verb. Volatile fields (`incarnation`, `started_at`, `nonce`,
//! `expires_at`) are masked — only the protocol-level shape and the
//! stable fields are checked. Catches drift in field names, value
//! types, capability sets, and `contract`/`build`/`version` strings
//! the moment one surface diverges from the other.

use std::process::Command;

use cairn_sdk::Sdk;
use serde_json::Value;

fn cairn_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn run_json(args: &[&str]) -> Value {
    let out = cairn_bin()
        .args(args)
        .output()
        .expect("spawn cairn binary");
    assert!(
        out.status.success(),
        "cairn {args:?} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cairn --json must emit valid JSON")
}

/// Replace volatile values with a sentinel so deep-equal still works.
fn mask(value: &mut Value, paths: &[&[&str]]) {
    for path in paths {
        mask_one(value, path);
    }
}

fn mask_one(value: &mut Value, path: &[&str]) {
    let Some((head, tail)) = path.split_first() else {
        *value = Value::String("<masked>".to_owned());
        return;
    };
    if let Some(child) = value.as_object_mut().and_then(|o| o.get_mut(*head)) {
        mask_one(child, tail);
    }
}

#[test]
fn status_parity_cli_vs_sdk() {
    let mut cli = run_json(&["status", "--json"]);
    let mut sdk = serde_json::to_value(Sdk::new().status()).expect("sdk status serializes");

    let volatile: &[&[&str]] = &[
        &["server_info", "incarnation"],
        &["server_info", "started_at"],
    ];
    mask(&mut cli, volatile);
    mask(&mut sdk, volatile);

    assert_eq!(
        cli, sdk,
        "CLI and SDK status must agree on every stable field — drift indicates a wire-contract regression"
    );
}

#[test]
fn handshake_parity_cli_vs_sdk() {
    let mut cli = run_json(&["handshake", "--json"]);
    let mut sdk =
        serde_json::to_value(Sdk::new().handshake()).expect("sdk handshake serializes");

    let volatile: &[&[&str]] = &[
        &["challenge", "nonce"],
        &["challenge", "expires_at"],
    ];
    mask(&mut cli, volatile);
    mask(&mut sdk, volatile);

    assert_eq!(cli, sdk, "CLI and SDK handshake envelopes must agree on shape");
}

#[test]
fn status_volatile_fields_have_expected_shape() {
    // Sanity-check the masked fields independently: incarnation must
    // round-trip through the canonical Ulid validator (26 chars,
    // Crockford), and started_at must be RFC-3339 with second precision.
    let cli = run_json(&["status", "--json"]);
    let sdk = serde_json::to_value(Sdk::new().status()).expect("serialize");
    for (label, value) in [("cli", &cli), ("sdk", &sdk)] {
        let inc = value["server_info"]["incarnation"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: incarnation missing"));
        assert_eq!(inc.len(), 26, "{label}: incarnation must be 26 chars");
        assert!(
            inc.bytes().all(|b| matches!(b,
                b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'
            )),
            "{label}: incarnation must be Crockford base32"
        );
        let started = value["server_info"]["started_at"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: started_at missing"));
        assert_eq!(started.len(), 20, "{label}: started_at must be 20 chars");
        assert!(started.ends_with('Z'), "{label}: started_at must end with Z");
        assert!(started.contains('T'), "{label}: started_at must contain T");
    }
}

#[test]
fn handshake_volatile_fields_have_expected_shape() {
    let cli = run_json(&["handshake", "--json"]);
    let sdk = serde_json::to_value(Sdk::new().handshake()).expect("serialize");
    for (label, value) in [("cli", &cli), ("sdk", &sdk)] {
        let nonce = value["challenge"]["nonce"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: nonce missing"));
        assert_eq!(nonce.len(), 24, "{label}: nonce must be 16-byte base64 (24 chars)");
        assert!(nonce.ends_with("=="), "{label}: nonce must end with == padding");
        let expires = value["challenge"]["expires_at"]
            .as_u64()
            .unwrap_or_else(|| panic!("{label}: expires_at must be u64"));
        assert!(expires > 0, "{label}: expires_at must be positive epoch-ms");
    }
}
