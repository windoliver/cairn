//! Unit tests for `cairn_core::status::advertise()`.

use super::*;
use crate::config::CapabilitySet;

fn cap_set_default(model: bool, embed_on: bool) -> CapabilitySet {
    CapabilitySet {
        keyword_search: true,
        semantic_search: model && embed_on,
        hybrid_search: model && embed_on,
        llm_extract: false,
        agent_extract: false,
        graph_edges: false,
        policy_trace: true,
        replay_sequence: true,
        replay_challenge: true,
    }
}

fn gates(bound: bool, model_present: bool, store: Option<StoreCaps>) -> CapabilityGates {
    CapabilityGates {
        config: cap_set_default(model_present, true),
        store,
        vault_bound: bound,
        model_present,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    }
}

#[test]
fn unbound_returns_empty() {
    let g = gates(false, true, None);
    assert!(advertise(&g).is_empty());
}

#[test]
fn bound_no_store_advertises_keyword_and_policy_trace() {
    // CLI status path: vault bound, no store opened, no model on disk.
    let g = gates(true, false, None);
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1PolicyTrace));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
}

#[test]
fn bound_no_store_with_model_advertises_all_search_modes() {
    // CLI status path with the embedding model materialized on disk.
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
    assert!(caps.contains(&Capabilities::CairnMcpV1PolicyTrace));
}

#[test]
fn bound_store_without_fts_does_not_advertise_keyword() {
    let store = Some(StoreCaps { fts: false, vector: true });
    let g = gates(true, true, store);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
        "hybrid requires FTS; got {caps:?}");
}

#[test]
fn bound_store_without_vector_drops_semantic_and_hybrid() {
    let store = Some(StoreCaps { fts: true, vector: false });
    let g = gates(true, true, store);
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
}

#[test]
fn local_embeddings_off_drops_semantic_and_hybrid() {
    let mut g = gates(true, true, None);
    g.config = cap_set_default(true, false); // local_embeddings_off
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
}

#[test]
fn forget_record_held_back_until_wiring_flips() {
    // wiring::FORGET_RECORD_WIRED = false today.
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1ForgetRecord),
        "forget.record advertised before runtime wired (brief §15)");
}

#[test]
fn forget_session_pinned_to_v0_2_phase() {
    let mut g = gates(true, true, None);
    g.contract_phase = Phase::V0_1;
    let caps_v0_1 = advertise(&g);
    g.contract_phase = Phase::V0_2;
    let caps_v0_2 = advertise(&g);
    // Wiring flag is false so neither phase advertises today; structural
    // assertion: V0_1 cannot ever advertise forget.session even if wired.
    assert!(!caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetSession));
    assert!(!caps_v0_2.contains(&Capabilities::CairnMcpV1ForgetSession));
}

#[test]
fn replay_capabilities_held_back() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1ReplaySequence));
    assert!(!caps.contains(&Capabilities::CairnMcpV1ReplayChallenge));
}

#[test]
fn output_order_is_stable() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    // search.* before policy_trace, per the table.
    let kw_idx = caps.iter().position(|c| matches!(c, Capabilities::CairnMcpV1SearchKeyword));
    let pt_idx = caps.iter().position(|c| matches!(c, Capabilities::CairnMcpV1PolicyTrace));
    assert!(kw_idx.is_some() && pt_idx.is_some());
    assert!(kw_idx.expect("keyword must be present") < pt_idx.expect("policy_trace must be present"),
        "wire-stable order requires search.keyword before policy_trace; got {caps:?}");
}
