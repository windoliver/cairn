//! Capability-matrix gate for v0.1 — issue #98, brief §15.
//!
//! Asserts the *exact* set returned by `cairn_core::status::advertise()`
//! for several v0.1 configurations. The pure decision table is the
//! single source of truth (brief §8.0.a invariant #6), so over- and
//! under-advertising are both regressions; a `HashSet<Capabilities>`
//! equality check catches either direction. The wire-stable closed-set
//! check on the IDL artefacts themselves lives in
//! `crates/cairn-idl/tests/wire_compat_v1.rs`.
//!
//! Per-surface filters: this test pins the *pure* `advertise()` output.
//! Individual surfaces (CLI, SDK, MCP) may post-filter capabilities
//! whose dispatch is not yet wired on that specific surface — for
//! example, `cairn-sdk::transport::advertised_capabilities` strips
//! `retrieve.session/turn/tool_call` until the SDK transport implements
//! them end-to-end. Those per-surface filters are pinned by
//! `crates/cairn-sdk/tests/surface.rs::retrieve_capabilities_filtered_until_sdk_dispatch_is_wired`
//! (and analogous tests on other surfaces). The `contract-drift` CI
//! job runs both this matrix *and* the per-surface filter tests so a
//! drift in either direction (matrix or surface) fails the gate.
//!
//! Scenarios:
//!   A. Full P0 — every gate ON, model + LLM ready, all workflow
//!      runtimes ready. The expected set lists every capability the
//!      runtime advertises today; if a v0.1 capability is added or
//!      removed from `advertise()`, this case fails until the expected
//!      set is updated by hand.
//!   B1. Config-off — `search.local_embeddings: false` style: the
//!       semantic / hybrid bits in `CapabilitySet` are flipped off
//!       while runtime probes stay green. Catches a regression where
//!       the cfg gate is ignored.
//!   B2. Provider-not-ready — `embedding_provider_ready: false`,
//!       cfg bits still on. Catches a regression where the AND with
//!       provider readiness is dropped (over-advertise risk).
//!   B3. Store-not-ready — wired store reports `vector: false`.
//!       Catches a regression where store capabilities are skipped
//!       when advertising semantic/hybrid.
//!   C.  No LLM — `llm_configured: false`. Dream workflow disappears;
//!       consolidation, expiration, evaluation remain.
//!   D.  Unbound vault — `vault_bound: false` short-circuits to an
//!       empty set.
#![allow(missing_docs)]

use std::collections::HashSet;

use cairn_core::config::CapabilitySet;
use cairn_core::generated::common::Capabilities;
use cairn_core::status::{CapabilityGates, Phase, StoreCaps, advertise};

fn gates_full() -> CapabilityGates {
    CapabilityGates {
        config: CapabilitySet {
            keyword_search: true,
            semantic_search: true,
            hybrid_search: true,
            llm_extract: true,
            agent_extract: false,
            screen_capture_enabled: false,
            graph_edges: false,
            policy_trace: true,
            replay_sequence: true,
            replay_challenge: true,
        },
        store: Some(StoreCaps {
            fts: true,
            vector: true,
        }),
        vault_bound: true,
        model_present: true,
        embedding_provider_ready: true,
        llm_configured: true,
        contract_phase: Phase::V0_1,
        consolidation_runtime_ready: true,
        dream_runtime_ready: true,
        expiration_runtime_ready: true,
        evaluation_runtime_ready: true,
    }
}

/// Exact set of capabilities `advertise()` must return for the full-P0
/// gates above. Derived from `crates/cairn-core/src/status/mod.rs::advertise`
/// at phase `V0_1`: every gate whose flags evaluate true, *and only those*.
/// A new variant must either appear here (intentional addition for v0.1)
/// or be excluded by `advertise()`'s phase/wiring gates — there is no
/// third option.
fn expected_full_p0() -> HashSet<Capabilities> {
    HashSet::from([
        Capabilities::CairnMcpV1SearchKeyword,
        Capabilities::CairnMcpV1SearchSemantic,
        Capabilities::CairnMcpV1SearchHybrid,
        Capabilities::CairnMcpV1PolicyTrace,
        Capabilities::CairnMcpV1ForgetRecord,
        Capabilities::CairnMcpV1RetrieveSession,
        Capabilities::CairnMcpV1RetrieveTurn,
        Capabilities::CairnMcpV1RetrieveToolCall,
        Capabilities::CairnWorkflowsV1Consolidation,
        Capabilities::CairnWorkflowsV1Dream,
        Capabilities::CairnWorkflowsV1Expiration,
        Capabilities::CairnWorkflowsV1Evaluation,
    ])
}

fn advertised_set(gates: &CapabilityGates) -> HashSet<Capabilities> {
    advertise(gates).into_iter().collect()
}

#[test]
fn case_a_default_p0_advertises_exact_v01_matrix() {
    let caps = advertised_set(&gates_full());
    let expected = expected_full_p0();
    assert_eq!(
        caps, expected,
        "case A: advertised v0.1 capability set drifted from the expected matrix \
         (over- or under-advertise). If this is intentional, update \
         `expected_full_p0()` in this test and call it out in the PR description \
         and `docs/design/traceability.md`."
    );
}

#[test]
fn case_b1_config_off_drops_semantic_and_hybrid() {
    let mut g = gates_full();
    g.config.semantic_search = false;
    g.config.hybrid_search = false;
    let caps = advertised_set(&g);
    let mut expected = expected_full_p0();
    expected.remove(&Capabilities::CairnMcpV1SearchSemantic);
    expected.remove(&Capabilities::CairnMcpV1SearchHybrid);
    assert_eq!(
        caps, expected,
        "case B1 (config off): cfg.semantic_search / cfg.hybrid_search = false \
         must drop the two search modes and leave every other capability intact."
    );
}

#[test]
fn case_b2_provider_not_ready_drops_semantic_and_hybrid() {
    let mut g = gates_full();
    g.embedding_provider_ready = false;
    let caps = advertised_set(&g);
    let mut expected = expected_full_p0();
    expected.remove(&Capabilities::CairnMcpV1SearchSemantic);
    expected.remove(&Capabilities::CairnMcpV1SearchHybrid);
    assert_eq!(
        caps, expected,
        "case B2 (provider not ready): embedding_provider_ready = false must \
         drop semantic + hybrid even when cfg bits stay on — proves the AND \
         with provider readiness is honored."
    );
}

#[test]
fn case_b3_store_no_vector_drops_semantic_and_hybrid() {
    let mut g = gates_full();
    g.store = Some(StoreCaps {
        fts: true,
        vector: false,
    });
    let caps = advertised_set(&g);
    let mut expected = expected_full_p0();
    expected.remove(&Capabilities::CairnMcpV1SearchSemantic);
    expected.remove(&Capabilities::CairnMcpV1SearchHybrid);
    assert_eq!(
        caps, expected,
        "case B3 (store no vector): a wired store reporting vector=false must \
         drop semantic + hybrid — proves the store-capability AND is honored \
         even when cfg + provider stay green."
    );
}

#[test]
fn case_b4_store_no_fts_drops_keyword_and_hybrid() {
    let mut g = gates_full();
    g.store = Some(StoreCaps {
        fts: false,
        vector: true,
    });
    let caps = advertised_set(&g);
    let mut expected = expected_full_p0();
    expected.remove(&Capabilities::CairnMcpV1SearchKeyword);
    expected.remove(&Capabilities::CairnMcpV1SearchHybrid);
    assert_eq!(
        caps, expected,
        "case B4 (store no fts): a wired store reporting fts=false must drop \
         keyword + hybrid — proves the store-fts AND is honored. Semantic \
         remains since it depends only on vector + provider readiness."
    );
}

#[test]
fn case_c_no_llm_drops_dream_only() {
    let mut g = gates_full();
    g.llm_configured = false;
    let caps = advertised_set(&g);
    let mut expected = expected_full_p0();
    expected.remove(&Capabilities::CairnWorkflowsV1Dream);
    assert_eq!(
        caps, expected,
        "case C (no LLM): llm_configured = false must drop the dream workflow \
         capability and leave every other v0.1 capability intact."
    );
}

#[test]
fn case_d_unbound_vault_advertises_empty_set() {
    let mut g = gates_full();
    g.vault_bound = false;
    let caps = advertised_set(&g);
    assert!(
        caps.is_empty(),
        "case D (unbound vault): vault_bound = false must short-circuit to an \
         empty advertised set; got {caps:?}"
    );
}

/// Capabilities advertised by `default_sensor_capabilities()` from the
/// host surface (CLI, SDK, MCP) on top of `advertise()`'s output, when
/// the runtime is on the default-platform (`xcap` + `ocr.tesseract`)
/// build. Other platforms swap in alternative backends — see
/// `BUCKET_NON_DEFAULT` below.
const BUCKET_SURFACE_SENSORS_DEFAULT: &[&str] = &[
    "cairn.sensor.v1.screen.xcap",
    "cairn.sensor.v1.screen.ocr.tesseract",
];

/// Capabilities held back at v0.1 because the runtime path that would
/// advertise them is not yet wired. Each entry is a deliberate
/// fail-closed decision: the contract reserves the capability string
/// but `advertise()` must not publish it until the matching dispatch
/// path lands. Two flavors are bucketed together:
///   - Verb/sensor capabilities gated by a `*_WIRED = false` constant
///     in `crates/cairn-core/src/status/wiring.rs`.
///   - Extension-namespace capabilities (`cairn.mcp.v1.extension.*`)
///     whose `advertise()` row + bidirectional binding (brief §8.0.a:
///     `cairn.admin.v1` in `status.extensions` ↔ `cairn.mcp.v1.extension.admin`
///     in `status.capabilities`, encoded in `schema/prelude/status.json`)
///     are not yet wired into the runtime. The capability string is
///     reserved; when the extension is enabled, the surface advertising
///     it must publish both halves together.
///
/// Flipping the gate (wiring constant, or wiring up the extension
/// `advertise()` row) moves the entry into `expected_full_p0()` (or a
/// future bound/extension-enabled scenario) in a single, reviewed PR.
const BUCKET_DEFERRED_WIRING: &[&str] = &[
    "cairn.mcp.v1.retrieve.record",
    "cairn.mcp.v1.retrieve.folder",
    "cairn.mcp.v1.retrieve.scope",
    "cairn.mcp.v1.retrieve.profile",
    "cairn.mcp.v1.sensors.pre_compact",
    "cairn.mcp.v1.replay.sequence",
    "cairn.mcp.v1.replay.challenge",
    "cairn.mcp.v1.extension.admin",
];

/// v0.1 capability strings the default-platform runtime never publishes:
/// alternative-platform sensor backends (advertised only when the host
/// is on that specific platform/config combination).
const BUCKET_NON_DEFAULT: &[&str] = &[
    "cairn.sensor.v1.screen.screenpipe",
    "cairn.sensor.v1.screen.ocr.vision",
    "cairn.sensor.v1.screen.ocr.winrt",
];

fn capability_to_string(cap: Capabilities) -> String {
    serde_json::to_value(cap)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("Capabilities::{cap:?} must serialize to a string"))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "5-bucket reconciliation: each bucket is one named HashSet plus three \
              sanity passes (membership, pairwise disjoint, full cover). Splitting \
              would hide the cross-bucket invariants in helpers and add no clarity."
)]
fn every_v01_capability_is_classified_exactly_once() {
    // Reconciliation gate (issue #98, round-3 Codex review): every v0.1
    // capability declared by `crates/cairn-idl/schema/capabilities/capabilities.json`
    // must land in EXACTLY ONE bucket:
    //   - `expected_full_p0()` — advertised in the default-P0 status response.
    //   - `BUCKET_SURFACE_SENSORS_DEFAULT` — appended by `default_sensor_capabilities()`.
    //   - `BUCKET_DEFERRED_WIRING` — held back behind a `*_WIRED = false`
    //     flag or an unwired extension namespace.
    //   - `BUCKET_NON_DEFAULT` — platform-alternative sensor backends.
    //
    // A future PR that adds a new v0.1 capability string MUST also add
    // a row here (and either flip a wiring flag or leave it deferred,
    // documented in the PR description). Otherwise this test fails —
    // the contract version cannot grow silently.

    let schema_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("cairn-idl")
        .join("schema")
        .join("capabilities")
        .join("capabilities.json");
    let bytes = std::fs::read(&schema_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", schema_path.display()));
    let schema: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("parse capabilities.json: {err}"));

    let entries = schema
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("capabilities.json: oneOf must be an array");

    let v01_strings: HashSet<String> = entries
        .iter()
        .filter(|entry| {
            entry
                .get("x-cairn-since")
                .and_then(serde_json::Value::as_str)
                == Some("v0.1")
        })
        .map(|entry| {
            entry
                .get("const")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("oneOf entry without `const`: {entry}"))
                .to_owned()
        })
        .collect();

    let advertised: HashSet<String> = expected_full_p0()
        .into_iter()
        .map(capability_to_string)
        .collect();
    let surface_sensors: HashSet<String> = BUCKET_SURFACE_SENSORS_DEFAULT
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let deferred: HashSet<String> = BUCKET_DEFERRED_WIRING
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let non_default: HashSet<String> = BUCKET_NON_DEFAULT
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

    // Sanity 1: every bucket entry must actually be a v0.1 entry in the
    // schema. Catches typos and stale entries left after a deprecation.
    for bucket in [&advertised, &surface_sensors, &deferred, &non_default] {
        for cap in bucket {
            assert!(
                v01_strings.contains(cap),
                "bucket entry `{cap}` is not a v0.1 capability in capabilities.json — \
                 either the entry is stale or `x-cairn-since` was bumped without updating \
                 this test"
            );
        }
    }

    // Sanity 2: buckets must be pairwise disjoint. A capability cannot
    // be both advertised and deferred at the same time.
    let buckets: [(&str, &HashSet<String>); 4] = [
        ("advertised_full_p0", &advertised),
        ("surface_sensors_default", &surface_sensors),
        ("deferred_wiring", &deferred),
        ("non_default", &non_default),
    ];
    for (i, (name_i, bucket_i)) in buckets.iter().enumerate() {
        for (name_j, bucket_j) in buckets.iter().skip(i + 1) {
            let overlap: Vec<&String> = bucket_i.intersection(bucket_j).collect();
            assert!(
                overlap.is_empty(),
                "buckets `{name_i}` and `{name_j}` overlap on {overlap:?} — \
                 every capability must belong to exactly one bucket"
            );
        }
    }

    // Sanity 3: every v0.1 capability in the schema must appear in
    // exactly one bucket — the union covers the schema.
    let union: HashSet<String> = advertised
        .iter()
        .chain(surface_sensors.iter())
        .chain(deferred.iter())
        .chain(non_default.iter())
        .cloned()
        .collect();
    let missing: HashSet<&String> = v01_strings.difference(&union).collect();
    assert!(
        missing.is_empty(),
        "v0.1 capabilities declared in capabilities.json have no classification: {missing:?}. \
         Add each one to exactly one of expected_full_p0() / BUCKET_SURFACE_SENSORS_DEFAULT / \
         BUCKET_DEFERRED_WIRING / BUCKET_NON_DEFAULT. The contract version cannot grow \
         silently — issue #98."
    );
    let extra: HashSet<&String> = union.difference(&v01_strings).collect();
    assert!(
        extra.is_empty(),
        "buckets reference capability strings that are not v0.1 entries in capabilities.json: \
         {extra:?}. Remove them or re-bucket."
    );
}
