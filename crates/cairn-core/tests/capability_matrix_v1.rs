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
