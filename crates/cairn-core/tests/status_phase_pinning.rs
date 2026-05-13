//! Brief §8.0.a / AC: forget.session/scope cannot appear in `capabilities[]`
//! at v0.1 regardless of any wiring flag flip. retrieve targets only appear
//! once their runtime paths are wired, and replay.* hold back behind their
//! own wiring flags. This integration test guards the
//! version-pinning rules — a refactor that loses them must turn this red.

use cairn_core::config::CapabilitySet;
use cairn_core::generated::common::Capabilities;
use cairn_core::status::{CapabilityGates, Phase, StoreCaps, advertise};

fn full_caps_set() -> CapabilitySet {
    CapabilitySet {
        keyword_search: true,
        semantic_search: true,
        hybrid_search: true,
        llm_extract: false,
        agent_extract: false,
        screen_capture_enabled: false,
        graph_edges: false,
        policy_trace: true,
        replay_sequence: true,
        replay_challenge: true,
    }
}

fn full_gates(phase: Phase) -> CapabilityGates {
    CapabilityGates {
        config: full_caps_set(),
        store: Some(StoreCaps {
            fts: true,
            vector: true,
        }),
        vault_bound: true,
        model_present: true,
        embedding_provider_ready: true,
        llm_configured: false,
        contract_phase: phase,
    }
}

#[test]
fn forget_session_pinned_to_v0_2_phase() {
    let caps_v0_1 = advertise(&full_gates(Phase::V0_1));
    assert!(
        !caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetSession),
        "forget.session must NOT appear at v0.1 even with every gate on; got {caps_v0_1:?}"
    );
}

#[test]
fn forget_scope_pinned_to_v0_3_phase() {
    let caps_v0_1 = advertise(&full_gates(Phase::V0_1));
    let caps_v0_2 = advertise(&full_gates(Phase::V0_2));
    assert!(!caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetScope));
    assert!(!caps_v0_2.contains(&Capabilities::CairnMcpV1ForgetScope));
}

#[test]
fn replay_capabilities_held_back_at_every_phase() {
    for phase in [Phase::V0_1, Phase::V0_2, Phase::V0_3] {
        let caps = advertise(&full_gates(phase));
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1ReplaySequence),
            "replay.sequence must stay un-advertised; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1ReplayChallenge),
            "replay.challenge must stay un-advertised; got {caps:?}"
        );
    }
}

#[test]
fn retrieve_capabilities_follow_wiring_flags_at_every_phase() {
    for phase in [Phase::V0_1, Phase::V0_2, Phase::V0_3] {
        let caps = advertise(&full_gates(phase));
        for needle in [
            Capabilities::CairnMcpV1RetrieveSession,
            Capabilities::CairnMcpV1RetrieveTurn,
            Capabilities::CairnMcpV1RetrieveToolCall,
        ] {
            assert!(
                caps.contains(&needle),
                "wired retrieve target {needle:?} must be advertised; got {caps:?}"
            );
        }
        for needle in [
            Capabilities::CairnMcpV1RetrieveRecord,
            Capabilities::CairnMcpV1RetrieveFolder,
            Capabilities::CairnMcpV1RetrieveScope,
            Capabilities::CairnMcpV1RetrieveProfile,
        ] {
            assert!(
                !caps.contains(&needle),
                "unwired retrieve target {needle:?} must stay hidden; got {caps:?}"
            );
        }
    }
}
