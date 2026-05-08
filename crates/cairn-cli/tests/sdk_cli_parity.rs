//! Cross-surface parity for the P0 prelude (`status`, `handshake`).
//!
//! Spawn the `cairn` binary, capture its `--json` output, and compare
//! the structural shape against `cairn_sdk::Sdk` output for the same
//! verb. Volatile fields (`incarnation`, `started_at`, `nonce`,
//! `expires_at`) are masked — only the protocol-level shape and the
//! stable fields are checked. Catches drift in field names, value
//! types, capability sets, and `contract`/`build`/`version` strings
//! the moment one surface diverges from the other.

use std::path::Path;
use std::process::Command;

// ── Minimal stub store for wired-store parity tests ──────────────────────────

/// A minimal `MemoryStore` stub used only for capability-advertisement
/// parity tests. Implements `capabilities()` and `name()`; every other
/// trait method is unreachable in this test path because `status_response()`
/// and `Sdk::status()` only consult `capabilities()`.
struct ParityStubStore {
    caps: cairn_core::contract::memory_store::MemoryStoreCapabilities,
}

#[async_trait::async_trait]
impl cairn_core::contract::memory_store::MemoryStore for ParityStubStore {
    fn name(&self) -> &'static str {
        "parity-stub"
    }

    fn capabilities(&self) -> &cairn_core::contract::memory_store::MemoryStoreCapabilities {
        &self.caps
    }

    fn supported_contract_versions(&self) -> cairn_core::contract::version::VersionRange {
        let v = cairn_core::contract::memory_store::CONTRACT_VERSION;
        cairn_core::contract::version::VersionRange::new(
            v,
            cairn_core::contract::version::ContractVersion::new(v.major, v.minor + 1, 0),
        )
    }

    async fn upsert(
        &self,
        _r: &cairn_core::domain::record::MemoryRecord,
    ) -> Result<
        cairn_core::contract::memory_store::UpsertOutcome,
        cairn_core::contract::memory_store::StoreError,
    > {
        panic!("unreachable in parity test: upsert")
    }

    async fn get(
        &self,
        _id: &cairn_core::domain::RecordId,
    ) -> Result<
        Option<cairn_core::domain::record::MemoryRecord>,
        cairn_core::contract::memory_store::StoreError,
    > {
        panic!("unreachable in parity test: get")
    }

    async fn list(
        &self,
        _args: &cairn_core::contract::memory_store::ListArgs,
    ) -> Result<
        cairn_core::contract::memory_store::ListPage,
        cairn_core::contract::memory_store::StoreError,
    > {
        panic!("unreachable in parity test: list")
    }

    async fn tombstone(
        &self,
        _id: &cairn_core::domain::RecordId,
        _reason: cairn_core::contract::memory_store::TombstoneReason,
    ) -> Result<(), cairn_core::contract::memory_store::StoreError> {
        panic!("unreachable in parity test: tombstone")
    }

    async fn versions(
        &self,
        _target: &cairn_core::domain::TargetId,
    ) -> Result<
        Vec<cairn_core::contract::memory_store::RecordVersion>,
        cairn_core::contract::memory_store::StoreError,
    > {
        panic!("unreachable in parity test: versions")
    }

    async fn put_edge(
        &self,
        _edge: &cairn_core::contract::memory_store::Edge,
    ) -> Result<(), cairn_core::contract::memory_store::StoreError> {
        panic!("unreachable in parity test: put_edge")
    }

    async fn remove_edge(
        &self,
        _key: &cairn_core::contract::memory_store::EdgeKey,
    ) -> Result<bool, cairn_core::contract::memory_store::StoreError> {
        panic!("unreachable in parity test: remove_edge")
    }

    async fn neighbours(
        &self,
        _id: &cairn_core::domain::RecordId,
        _dir: cairn_core::contract::memory_store::EdgeDir,
    ) -> Result<
        Vec<cairn_core::contract::memory_store::Edge>,
        cairn_core::contract::memory_store::StoreError,
    > {
        panic!("unreachable in parity test: neighbours")
    }

    async fn search_keyword(
        &self,
        _args: &cairn_core::contract::memory_store::KeywordSearchArgs<'_>,
    ) -> Result<
        cairn_core::contract::memory_store::KeywordSearchPage,
        cairn_core::contract::memory_store::StoreError,
    > {
        panic!("unreachable in parity test: search_keyword")
    }
}

use cairn_sdk::Sdk;
use serde_json::Value;

fn cairn_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    // Force CWD into the OS tempdir to avoid CWD-pollution: the parity
    // test must compare CLI and SDK in equivalent "no vault, no store"
    // states. The workspace root acquires a `.cairn/vault.id` from
    // other test runs (`cli::bootstrap_*`, identity tests), and that
    // sentinel makes `cairn status` emit a populated capability list
    // while `Sdk::new` still emits an empty one. Run from a path that
    // has no `.cairn/` so both surfaces see the no-vault P0 path.
    cmd.current_dir(std::env::temp_dir());
    // Equivalent precaution for any stray env: parity assumes no
    // pre-bound vault.
    cmd.env_remove("CAIRN_VAULT");
    cmd
}

/// Sanity-check that the chosen tempdir is genuinely vault-less. If any
/// outer process has dropped a `.cairn/vault.id` into the OS tempdir,
/// the parity assumption breaks; surface that as a clear test failure
/// rather than a confusing capability-array mismatch.
fn assert_tempdir_unbound() {
    let candidate: &Path = &std::env::temp_dir();
    assert!(
        !candidate.join(".cairn").join("vault.id").exists(),
        "test precondition: {} unexpectedly contains .cairn/vault.id; \
         the parity test assumes a vault-less CWD",
        candidate.display()
    );
}

fn run_json(args: &[&str]) -> Value {
    let out = cairn_bin().args(args).output().expect("spawn cairn binary");
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
    assert_tempdir_unbound();
    let mut cli = run_json(&["status", "--json"]);
    let mut sdk = serde_json::to_value(Sdk::new().status()).expect("sdk status serializes");

    // Both surfaces run from a vault-less, config-less context here
    // (CLI is forced into the OS tempdir; `Sdk::new()` has no store).
    // Under those conditions both surfaces must emit
    // `mcp_graph_tools = None` — the CLI has no config to validate
    // and the SDK has no MCP server to probe. Enforcing equality on
    // the field catches regressions where one adapter starts
    // synthesizing a value the other cannot.
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
    let mut sdk = serde_json::to_value(Sdk::new().handshake()).expect("sdk handshake serializes");

    let volatile: &[&[&str]] = &[&["challenge", "nonce"], &["challenge", "expires_at"]];
    mask(&mut cli, volatile);
    mask(&mut sdk, volatile);

    assert_eq!(
        cli, sdk,
        "CLI and SDK handshake envelopes must agree on shape"
    );
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
        assert!(
            started.ends_with('Z'),
            "{label}: started_at must end with Z"
        );
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
        assert_eq!(
            nonce.len(),
            24,
            "{label}: nonce must be 16-byte base64 (24 chars)"
        );
        assert!(
            nonce.ends_with("=="),
            "{label}: nonce must end with == padding"
        );
        let expires = value["challenge"]["expires_at"]
            .as_u64()
            .unwrap_or_else(|| panic!("{label}: expires_at must be u64"));
        assert!(expires > 0, "{label}: expires_at must be positive epoch-ms");
    }
}

#[test]
fn status_parity_cli_vs_sdk_vs_mcp() {
    use cairn_mcp::CairnMcpHandler;

    assert_tempdir_unbound();

    let mut cli = run_json(&["status", "--json"]);
    let mut sdk = serde_json::to_value(Sdk::new().status()).expect("sdk status serializes");
    let mut mcp = serde_json::to_value(CairnMcpHandler::new().status_response())
        .expect("mcp status serializes");

    let volatile: &[&[&str]] = &[
        &["server_info", "incarnation"],
        &["server_info", "started_at"],
    ];
    mask(&mut cli, volatile);
    mask(&mut sdk, volatile);
    mask(&mut mcp, volatile);

    assert_eq!(cli, sdk, "CLI and SDK status diverge");
    assert_eq!(sdk, mcp, "SDK and MCP status diverge");
    // Transitive: cli == mcp follows.
}

/// Three-way parity for the non-trivial case: both SDK and MCP wired to a
/// `fts=true, vector=false` store.
///
/// This test catches the class of bug where MCP's capability derivation
/// diverges from SDK's (e.g. masking `semantic` against the wrong config
/// field instead of `store.vector`). CLI is excluded from this case because
/// `cairn status` does not open the store — the existing empty-case test
/// (`status_parity_cli_vs_sdk_vs_mcp`) covers CLI<->SDK byte equality.
///
/// Expected capability set:
/// - `keyword`: both surfaces emit it (store.fts=true).
/// - `policy_trace`: both emit it (config-only gate).
/// - `semantic` / `hybrid`: both drop (store.vector=false).
#[test]
fn status_parity_cli_vs_sdk_vs_mcp_with_fts_only_store() {
    use cairn_core::contract::memory_store::MemoryStoreCapabilities;
    use cairn_mcp::CairnMcpHandler;
    use std::sync::Arc;

    assert_tempdir_unbound();

    let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
        Arc::new(ParityStubStore {
            caps: MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: false,
                per_record_consent_model: true,
            },
        });
    let config = cairn_core::config::CairnConfig::default();

    let mut sdk =
        serde_json::to_value(cairn_sdk::Sdk::with_store(store.clone(), config.clone()).status())
            .expect("sdk serialize");
    let mut mcp = serde_json::to_value(
        CairnMcpHandler::with_store(store.clone(), config.clone()).status_response(),
    )
    .expect("mcp serialize");

    let volatile: &[&[&str]] = &[
        &["server_info", "incarnation"],
        &["server_info", "started_at"],
    ];
    mask(&mut sdk, volatile);
    mask(&mut mcp, volatile);

    // SDK and MCP both wired to the same fts-only store: they MUST agree.
    assert_eq!(sdk, mcp, "SDK and MCP status diverge for fts-only store");

    // The capabilities array must contain keyword + policy_trace; semantic/
    // hybrid must be absent (vector=false drops them on both surfaces).
    let caps = sdk["capabilities"].as_array().expect("array");
    let cap_strings: Vec<&str> = caps.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        cap_strings.contains(&"cairn.mcp.v1.search.keyword"),
        "keyword must advertise; got {cap_strings:?}"
    );
    assert!(
        cap_strings.contains(&"cairn.mcp.v1.policy_trace"),
        "policy_trace must advertise; got {cap_strings:?}"
    );
    assert!(
        !cap_strings.contains(&"cairn.mcp.v1.search.semantic"),
        "semantic must drop with vector=false; got {cap_strings:?}"
    );
    assert!(
        !cap_strings.contains(&"cairn.mcp.v1.search.hybrid"),
        "hybrid must drop with vector=false; got {cap_strings:?}"
    );
}
