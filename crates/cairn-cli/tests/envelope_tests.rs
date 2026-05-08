//! Verify that every verb returns a valid cairn.mcp.v1 JSON envelope.
//! These tests invoke the compiled binary and will pass after Task 7 wires dispatch.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn assert_aborted_internal(verb_args: &[&str]) {
    let out = {
        let vault = tempfile::tempdir().expect("temp vault");
        let mut cmd = cli();
        cmd.current_dir(vault.path());
        cmd.args(verb_args);
        cmd.output()
            .unwrap_or_else(|e| panic!("failed to run {verb_args:?}: {e}"))
    };
    // Aborted → exit 1 (generic failure)
    assert_eq!(
        out.status.code(),
        Some(1),
        "verb {verb_args:?} should exit 1 (Internal aborted), got {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("verb {verb_args:?} JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["contract"], "cairn.mcp.v1", "verb {verb_args:?}");
    assert_eq!(v["status"], "aborted", "verb {verb_args:?}");
    assert_eq!(v["error"]["code"], "Internal", "verb {verb_args:?}");
    assert!(v["operation_id"].is_string(), "verb {verb_args:?}");
    assert!(v["policy_trace"].is_array(), "verb {verb_args:?}");
}

fn assert_rejected_capability_unavailable(verb_args: &[&str], capability: &str) {
    let out = {
        let mut cmd = cli();
        cmd.args(verb_args);
        cmd.output()
            .unwrap_or_else(|e| panic!("failed to run {verb_args:?}: {e}"))
    };
    assert_eq!(
        out.status.code(),
        Some(69),
        "verb {verb_args:?} should exit 69 (CapabilityUnavailable), got {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("verb {verb_args:?} JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["contract"], "cairn.mcp.v1", "verb {verb_args:?}");
    assert_eq!(v["status"], "rejected", "verb {verb_args:?}");
    assert_eq!(
        v["error"]["code"], "CapabilityUnavailable",
        "verb {verb_args:?}"
    );
    assert_eq!(
        v["error"]["data"]["capability"], capability,
        "verb {verb_args:?}"
    );
    assert!(v["operation_id"].is_string(), "verb {verb_args:?}");
    assert!(v["policy_trace"].is_array(), "verb {verb_args:?}");
}

#[test]
fn ingest_returns_committed_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let out = cli()
        .current_dir(dir.path())
        .args(["ingest", "--kind", "user", "--body", "hello", "--json"])
        .output()
        .expect("cairn ingest --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "ingest should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("ingest JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "ingest");
    assert!(v["error"].is_null());
    assert!(v["data"]["record_id"].is_string());
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn search_keyword_json_exits_zero_with_hits() {
    // Search is now wired to `cairn_core::verbs::search::run`. Keyword mode
    // (the default) works without an embedder — it opens (or creates) the
    // SQLite store and runs FTS5. Against an empty vault the result is an
    // empty `hits` array; the exit code is 0 (success).
    //
    // The vault-binding gate added in round-2 requires `.cairn/vault.id`
    // to exist before the store is opened, so seed a minimal sentinel
    // here. We don't go through `cairn bootstrap` to avoid the BGE
    // model download it would trigger.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cairn_dir = tmp.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");
    let out = {
        let mut cmd = cli();
        cmd.env("CAIRN_VAULT", tmp.path());
        cmd.args(["search", "--mode", "keyword", "test query", "--json"]);
        cmd.output()
            .expect("cairn search --mode keyword test query --json should run")
    };
    assert_eq!(
        out.status.code(),
        Some(0),
        "keyword search against an empty vault must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    // The committed wire shape is the IDL `Response` envelope
    // (round-8 review #1). Round-trip through the generated
    // `Response` deserializer so a future serde-annotation drift
    // breaks the test instead of silently emitting a non-IDL
    // envelope to advertised search clients.
    let resp: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("search envelope parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(resp.contract, "cairn.mcp.v1");
    assert!(
        matches!(
            resp.status,
            cairn_core::generated::envelope::ResponseStatus::Committed
        ),
        "successful search must be status=committed"
    );
    assert!(matches!(
        resp.verb,
        cairn_core::generated::envelope::ResponseVerb::Search
    ));
    let data = resp.data.expect("committed search must have data");
    let cairn_core::generated::envelope::ResponseData::Search(payload) = data else {
        panic!("data must be Search variant");
    };
    assert!(
        payload.hits.is_empty(),
        "empty vault must yield zero hits; got {} hits",
        payload.hits.len()
    );
}

#[test]
fn retrieve_record_returns_capability_unavailable() {
    assert_rejected_capability_unavailable(
        &["retrieve", "01JXXXXXXXXXXXXXXXXXXXXXXX", "--json"],
        "cairn.mcp.v1.retrieve.record",
    );
}

#[test]
fn retrieve_turn_returns_capability_unavailable() {
    assert_rejected_capability_unavailable(
        &[
            "retrieve",
            "--session",
            "session-1",
            "--turn",
            "3",
            "--json",
        ],
        "cairn.mcp.v1.retrieve.turn",
    );
}

#[test]
fn summarize_returns_aborted_internal() {
    assert_aborted_internal(&["summarize", "01JXXXXXXXXXXXXXXXXXXXXXXX", "--json"]);
}

#[test]
fn assemble_hot_returns_committed_envelope() {
    // `assemble_hot` is wired to the stub-body assembler. The verb now exits 0
    // and returns a committed envelope with six zero-length segments (default recipe).
    // Bootstrap a tempdir vault — the verb fails closed on a non-vault cwd.
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--json"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run assemble_hot: {e}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "assemble_hot should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("assemble_hot JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "assemble_hot");
    assert!(
        v["error"].is_null(),
        "committed envelope must not have error"
    );
    assert!(v["data"]["segments"].is_array(), "segments must be present");
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn capture_trace_returns_aborted_internal() {
    assert_aborted_internal(&["capture_trace", "--from", "/dev/null", "--json"]);
}

#[test]
fn forget_record_returns_capability_unavailable() {
    assert_rejected_capability_unavailable(
        &["forget", "--record", "01JXXXXXXXXXXXXXXXXXXXXXXX", "--json"],
        "cairn.mcp.v1.forget.record",
    );
}

#[test]
fn status_in_unbound_dir_advertises_no_capabilities() {
    // When `cairn status` runs without a real vault behind it, every
    // capability gates on `.cairn/vault.id`. A tempdir without bootstrap
    // therefore advertises nothing — same shape as `Sdk::new`. This is
    // the fail-closed P0 contract: do not promise capabilities that no
    // store can back.
    let out = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.env_remove("CAIRN_VAULT");
        let tmp = tempfile::tempdir().expect("tempdir");
        cmd.current_dir(tmp.path());
        cmd.args(["status", "--json"]);
        let res = cmd.output().expect("status --json should run");
        std::mem::forget(tmp);
        res
    };
    assert_eq!(
        out.status.code(),
        Some(0),
        "status should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("status JSON parse failed: {e}\nstdout: {stdout:?}"));
    let caps = v["capabilities"].as_array().expect("capabilities array");
    assert!(
        caps.is_empty(),
        "unbound vault must advertise no capabilities; got {caps:?}"
    );
}

#[test]
fn status_in_bound_vault_advertises_search_and_policy_trace() {
    // After #49 + the round-2 vault-presence gate, `cairn status` only
    // advertises capabilities when the vault is actually bootstrapped
    // (`.cairn/vault.id` exists). Hand-fabricate a minimal vault and
    // assert keyword + policy_trace appear, while semantic/hybrid are
    // absent because no embedding model is on disk. Skipping the real
    // `cairn bootstrap` avoids the ~25 MB BGE model download it would
    // otherwise trigger; we only need the sentinel for status to flip
    // out of the no-vault path.
    use std::collections::BTreeSet;

    let tmp = tempfile::tempdir().expect("tempdir");
    let vault_root = tmp.path().to_owned();
    let cairn_dir = vault_root.join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");
    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env("CAIRN_VAULT", &vault_root)
        .args(["status", "--json"])
        .output()
        .expect("status --json should run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "status should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("status JSON parse failed: {e}\nstdout: {stdout:?}"));
    let caps: BTreeSet<String> = v["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    assert!(
        caps.contains("cairn.mcp.v1.search.keyword"),
        "keyword must be advertised in a bound vault; got {caps:?}"
    );
    assert!(
        caps.contains("cairn.mcp.v1.policy_trace"),
        "policy_trace must be advertised in a bound vault; got {caps:?}"
    );
    assert!(
        !caps.contains("cairn.mcp.v1.search.semantic"),
        "semantic must NOT be advertised when no embedding model is on disk; got {caps:?}"
    );
    assert!(
        !caps.contains("cairn.mcp.v1.search.hybrid"),
        "hybrid must NOT be advertised when no embedding model is on disk \
         (runtime resolves an embedder for hybrid mode); got {caps:?}"
    );
    for stub_cap in [
        "cairn.mcp.v1.retrieve.session",
        "cairn.mcp.v1.retrieve.full",
        "cairn.mcp.v1.forget.record",
        "cairn.mcp.v1.forget.session",
    ] {
        assert!(
            !caps.contains(stub_cap),
            "stub-only capability {stub_cap} must NOT be advertised; got {caps:?}"
        );
    }
}

#[test]
fn status_in_malformed_config_vault_exits_ex_config() {
    // Codex round-1 finding: `unwrap_or_default` on the config load was
    // hiding genuine errors and advertising defaults. A malformed
    // `.cairn/config.yaml` must propagate as exit 78 (EX_CONFIG) so
    // operators see the failure instead of silently negotiating against
    // discarded config.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cairn_dir = tmp.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("config.yaml"), b": :\nnot: [valid yaml")
        .expect("write malformed config");
    // Bind the vault so the vault-presence gate would normally let the
    // status path proceed; failure must still come from the config load.
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");
    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env("CAIRN_VAULT", tmp.path())
        .args(["status", "--json"])
        .output()
        .expect("status should run");
    assert_eq!(
        out.status.code(),
        Some(78),
        "malformed config must exit EX_CONFIG (78); got {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn status_with_unbound_registry_default_exits_ex_config() {
    // Round-7 review #2: a registry default that points at a directory
    // without `.cairn/vault.id` must NOT silently fall through to the
    // empty-capabilities path. Operators who created a vault entry but
    // never bootstrapped it should see EX_CONFIG so they discover the
    // missing bootstrap, not an apparently healthy `capabilities: []`
    // (which the same directory would also reject from `cairn search`
    // and `cairn admin`).
    use cairn_cli::vault::registry::VaultRegistryStore;
    use cairn_core::config::{VaultEntry, VaultRegistry};

    // Vault dir with `.cairn/` but no `vault.id` — i.e. the operator
    // created a directory and registered it but never ran bootstrap.
    let vault_dir = tempfile::tempdir().expect("vault tempdir");
    std::fs::create_dir_all(vault_dir.path().join(".cairn")).expect("create .cairn");

    // Registry on disk pointing at that vault as the default.
    let reg_dir = tempfile::tempdir().expect("reg tempdir");
    let reg_path = reg_dir.path().join("vaults.toml");
    let store = VaultRegistryStore::new(reg_path.clone());
    let mut reg = VaultRegistry::default();
    reg.default = Some("home".into());
    reg.vaults.push(VaultEntry::new(
        "home",
        vault_dir.path().to_str().expect("utf-8 vault path"),
        None,
        None,
    ));
    store.save(&reg).expect("save registry");

    // Run from a *different* tempdir so the cwd-walk leg cannot match
    // anything — the registry default is the only source that can fire.
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env_remove("CAIRN_VAULT")
        .env("CAIRN_REGISTRY", &reg_path)
        .current_dir(cwd.path())
        .args(["status", "--json"])
        .output()
        .expect("status --json should run");
    assert_eq!(
        out.status.code(),
        Some(78),
        "unbound registry default must exit EX_CONFIG (78); got {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn search_capability_unavailable_emits_rejected_envelope() {
    // Round-9 review #1: `CapabilityUnavailable` belongs to the
    // *rejected* error family in the IDL — `Aborted +
    // CapabilityUnavailable` would fail the generated `Response`
    // validator. Round-trip the failure envelope through the same
    // deserializer generated clients use so any future drift in the
    // helper or a stray `Aborted` regression breaks the test instead
    // of silently producing wire-invalid traffic.
    //
    // `--mode semantic` against an unbound, unmodelled vault is the
    // simplest path to this envelope: the CLI capability gate fires
    // before embedder resolution and emits the rejected envelope with
    // `code=CapabilityUnavailable` (round-9 review #2).
    let tmp = tempfile::tempdir().expect("tempdir");
    let cairn_dir = tmp.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");

    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env("CAIRN_VAULT", tmp.path())
        .env_remove("CAIRN_MOCK_EMBEDDER")
        .args(["search", "test", "--mode", "semantic", "--json"])
        .output()
        .expect("cairn search --mode semantic --json should run");
    assert_eq!(
        out.status.code(),
        Some(69),
        "unadvertised mode must exit EX_UNAVAILABLE (69); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let resp: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| {
            panic!("CapabilityUnavailable envelope parse failed: {e}\nstdout: {stdout:?}")
        });
    assert!(
        matches!(
            resp.status,
            cairn_core::generated::envelope::ResponseStatus::Rejected
        ),
        "CapabilityUnavailable must be status=rejected per IDL; got {:?}",
        resp.status
    );
    let err = resp.error.expect("rejected envelope must carry error");
    assert_eq!(err["code"], "CapabilityUnavailable");
    assert_eq!(
        err["data"]["capability"], "cairn.mcp.v1.search.semantic",
        "unadvertised semantic mode must surface the semantic capability id; got {err}"
    );
}

#[test]
fn search_default_response_omits_excluded_field() {
    use cairn_core::generated::verbs::search::SearchData;

    // Construct a SearchData without `excluded` (the explain=false path
    // never populates it). Serialized JSON must not contain the
    // "excluded" key — the IDL marks it Option<...> with
    // skip_serializing_if = "Option::is_none".
    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        !json.contains("\"excluded\""),
        "default SearchData must not emit excluded field; got {json}"
    );
}

#[test]
fn search_with_excluded_emits_field() {
    use cairn_core::generated::common::{RecordExclusion, RecordExclusionGate, Ulid};
    use cairn_core::generated::verbs::search::SearchData;

    // Sanity check the inverse: when explain=true populates excluded,
    // the JSON does carry the field. This guards against a future
    // serde annotation accidentally hiding the field permanently.
    let data = SearchData {
        excluded: Some(vec![RecordExclusion {
            target_id: Ulid("01HQZX9F5N0000000000000000".to_owned()),
            gate: RecordExclusionGate::ReadFilterStaleness,
            detail: String::new(),
        }]),
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        json.contains("\"excluded\""),
        "excluded must appear when set; got {json}"
    );
}

#[test]
fn search_without_degraded_legs_omits_field() {
    use cairn_core::generated::verbs::search::SearchData;

    // `degraded_legs: None` (a healthy hybrid result) must not emit
    // the field. Keeps the wire shape stable for callers that only
    // read it under partial-failure conditions.
    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        !json.contains("\"degraded_legs\""),
        "healthy SearchData must not emit degraded_legs; got {json}"
    );
}

#[test]
fn search_with_degraded_legs_emits_full_entry() {
    use cairn_core::generated::verbs::search::{
        DegradedLegEntry, DegradedLegEntryLeg, DegradedLegEntryReason, DegradedLegEntrySource,
        SearchData,
    };

    // A hybrid response with a per-source graph-leg failure must
    // serialize the full `{leg, reason, source}` shape so callers can
    // attribute the partial-recall to the right seed source.
    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: Some(vec![
            DegradedLegEntry {
                leg: DegradedLegEntryLeg::Semantic,
                reason: DegradedLegEntryReason::SqlError,
                source: None,
            },
            DegradedLegEntry {
                leg: DegradedLegEntryLeg::Graph,
                reason: DegradedLegEntryReason::CapabilityUnavailable,
                source: Some(DegradedLegEntrySource::AuthSemanticSeed),
            },
        ]),
    };
    let json = serde_json::to_string(&data).expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&json).expect("roundtrip");
    let arr = v["degraded_legs"]
        .as_array()
        .expect("degraded_legs is an array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["leg"], "semantic");
    assert_eq!(arr[0]["reason"], "sql_error");
    assert!(
        arr[0].get("source").is_none() || arr[0]["source"].is_null(),
        "semantic leg must omit `source`; got {}",
        arr[0],
    );
    assert_eq!(arr[1]["leg"], "graph");
    assert_eq!(arr[1]["reason"], "capability_unavailable");
    assert_eq!(arr[1]["source"], "auth_semantic_seed");
}
