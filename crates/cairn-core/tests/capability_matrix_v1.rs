//! Capability-matrix gate for v0.1 — issue #98, brief §15.
//!
//! Tests the pure `advertise()` decision table against four config
//! flavors that map 1:1 to issue #98's acceptance criteria:
//!   A. Default P0 with embeddings + LLM + all workflow runtimes ready.
//!   B. Embeddings disabled (`search.local_embeddings: false`).
//!   C. No LLM provider configured.
//!   D. Unbound vault (no `vault.id`).
//!
//! Each case asserts the *required* presence and absence of capabilities
//! tied to that scenario; the wire-stable closed-set lives in the
//! `cairn-idl` wire-compat fixtures (`crates/cairn-idl/tests/wire_compat_v1.rs`).
//! A change to `advertise()` either flips an assertion (intentional,
//! reviewed) or breaks (regression).
#![allow(missing_docs)]

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

fn assert_contains(actual: &[Capabilities], expected: &[Capabilities], case: &str) {
    for cap in expected {
        assert!(
            actual.contains(cap),
            "{case}: expected capability {cap:?} not in advertised set {actual:?}"
        );
    }
}

fn assert_excludes(actual: &[Capabilities], excluded: &[Capabilities], case: &str) {
    for cap in excluded {
        assert!(
            !actual.contains(cap),
            "{case}: capability {cap:?} must NOT be advertised, but appeared in {actual:?}"
        );
    }
}

#[test]
fn case_a_default_p0_advertises_full_v01_matrix() {
    let caps = advertise(&gates_full());
    // Issue #98 acceptance: keyword, semantic, hybrid, record forget,
    // local workflows (consolidation, dream, expiration, evaluation).
    assert_contains(
        &caps,
        &[
            Capabilities::CairnMcpV1SearchKeyword,
            Capabilities::CairnMcpV1SearchSemantic,
            Capabilities::CairnMcpV1SearchHybrid,
            Capabilities::CairnMcpV1ForgetRecord,
            Capabilities::CairnMcpV1PolicyTrace,
            Capabilities::CairnWorkflowsV1Consolidation,
            Capabilities::CairnWorkflowsV1Dream,
            Capabilities::CairnWorkflowsV1Expiration,
            Capabilities::CairnWorkflowsV1Evaluation,
        ],
        "case A",
    );
    // v0.2-only capabilities must NOT appear at phase v0.1.
    assert_excludes(
        &caps,
        &[
            Capabilities::CairnMcpV1ForgetSession,
            Capabilities::CairnMcpV1ForgetScope,
        ],
        "case A",
    );
}

#[test]
fn case_b_embeddings_off_excludes_semantic_and_hybrid() {
    let mut g = gates_full();
    g.config.semantic_search = false;
    g.config.hybrid_search = false;
    g.embedding_provider_ready = false;
    let caps = advertise(&g);
    assert_contains(&caps, &[Capabilities::CairnMcpV1SearchKeyword], "case B");
    assert_excludes(
        &caps,
        &[
            Capabilities::CairnMcpV1SearchSemantic,
            Capabilities::CairnMcpV1SearchHybrid,
        ],
        "case B",
    );
}

#[test]
fn case_c_no_llm_excludes_dream_workflow() {
    let mut g = gates_full();
    g.llm_configured = false;
    let caps = advertise(&g);
    // Dream needs an LLM; others remain.
    assert_excludes(&caps, &[Capabilities::CairnWorkflowsV1Dream], "case C");
    assert_contains(
        &caps,
        &[
            Capabilities::CairnWorkflowsV1Consolidation,
            Capabilities::CairnWorkflowsV1Expiration,
            Capabilities::CairnWorkflowsV1Evaluation,
        ],
        "case C",
    );
}

#[test]
fn case_d_unbound_vault_advertises_nothing() {
    let mut g = gates_full();
    g.vault_bound = false;
    let caps = advertise(&g);
    assert!(
        caps.is_empty(),
        "case D: unbound vault must advertise zero capabilities, got {caps:?}"
    );
}
