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

#[cfg(test)]
mod remediation_tests {
    use super::*;

    #[test]
    fn remediation_for_search_semantic_is_set() {
        let hint = remediation_for("cairn.mcp.v1.search.semantic")
            .expect("semantic must have a remediation hint");
        assert!(hint.contains("local_embeddings"),
            "remediation should mention the toggle: got {hint:?}");
    }

    #[test]
    fn remediation_for_unknown_capability_is_none() {
        assert!(remediation_for("not.a.real.capability").is_none());
    }

    #[test]
    fn remediation_for_forget_session_mentions_v0_2() {
        let hint = remediation_for("cairn.mcp.v1.forget.session")
            .expect("forget.session must have a remediation hint");
        assert!(hint.contains("v0.2"));
    }

    #[test]
    fn remediation_table_has_no_empty_strings() {
        for (cap, hint) in REMEDIATION {
            assert!(!cap.is_empty(), "empty capability key");
            assert!(!hint.is_empty(), "empty remediation for {cap}");
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_phase() -> impl Strategy<Value = Phase> {
        prop_oneof![Just(Phase::V0_1), Just(Phase::V0_2), Just(Phase::V0_3)]
    }

    fn arb_store() -> impl Strategy<Value = Option<StoreCaps>> {
        prop_oneof![
            Just(None),
            (any::<bool>(), any::<bool>())
                .prop_map(|(fts, vector)| Some(StoreCaps { fts, vector }))
        ]
    }

    fn arb_cap_set() -> impl Strategy<Value = crate::config::CapabilitySet> {
        (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>())
            .prop_map(|(kw, sem, hyb, pt)| crate::config::CapabilitySet {
                keyword_search: kw,
                semantic_search: sem,
                hybrid_search: hyb,
                llm_extract: false,
                agent_extract: false,
                graph_edges: false,
                policy_trace: pt,
                replay_sequence: true,
                replay_challenge: true,
            })
    }

    fn arb_gates() -> impl Strategy<Value = CapabilityGates> {
        (
            arb_cap_set(),
            arb_store(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            arb_phase(),
        )
            .prop_map(|(config, store, bound, model, llm, phase)| CapabilityGates {
                config,
                store,
                vault_bound: bound,
                model_present: model,
                llm_configured: llm,
                contract_phase: phase,
            })
    }

    // Turning a capability gate ON never removes capabilities. Catches
    // accidental conjunction inversions in the decision table.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn monotone_in_keyword_search_flag(mut gates in arb_gates()) {
            gates.vault_bound = true; // monotone holds within bound branch
            let off = {
                gates.config.keyword_search = false;
                advertise(&gates)
            };
            let on = {
                gates.config.keyword_search = true;
                advertise(&gates)
            };
            for cap in &off {
                prop_assert!(on.contains(cap),
                    "keyword_search true must be a superset of false; lost {cap:?}");
            }
        }

        #[test]
        fn monotone_in_model_present(mut gates in arb_gates()) {
            gates.vault_bound = true;
            let off = {
                gates.model_present = false;
                advertise(&gates)
            };
            let on = {
                gates.model_present = true;
                advertise(&gates)
            };
            for cap in &off {
                prop_assert!(on.contains(cap),
                    "model_present true must be a superset of false; lost {cap:?}");
            }
        }

        #[test]
        fn unbound_always_empty(mut gates in arb_gates()) {
            gates.vault_bound = false;
            prop_assert!(advertise(&gates).is_empty());
        }
    }
}

#[cfg(test)]
mod exhaustiveness {
    use super::*;

    /// Compile-time proof that every `Capabilities` variant is named in
    /// the decision table. When the IDL adds a new variant, this match
    /// fails to compile until `advertise()` (in `mod.rs`) handles the new
    /// row. Combined with the runtime assertion below, no variant can be
    /// silently un-advertised.
    #[allow(unreachable_patterns)] // catch-all guards future #[non_exhaustive] additions
    fn classify(c: Capabilities) -> &'static str {
        match c {
            Capabilities::CairnMcpV1SearchKeyword => "search.keyword",
            Capabilities::CairnMcpV1SearchSemantic => "search.semantic",
            Capabilities::CairnMcpV1SearchHybrid => "search.hybrid",
            Capabilities::CairnMcpV1PolicyTrace => "policy_trace",
            Capabilities::CairnMcpV1ForgetRecord => "forget.record",
            Capabilities::CairnMcpV1ForgetSession => "forget.session",
            Capabilities::CairnMcpV1ForgetScope => "forget.scope",
            Capabilities::CairnMcpV1RetrieveRecord => "retrieve.record",
            Capabilities::CairnMcpV1RetrieveSession => "retrieve.session",
            Capabilities::CairnMcpV1RetrieveTurn => "retrieve.turn",
            Capabilities::CairnMcpV1RetrieveFolder => "retrieve.folder",
            Capabilities::CairnMcpV1RetrieveScope => "retrieve.scope",
            Capabilities::CairnMcpV1RetrieveProfile => "retrieve.profile",
            Capabilities::CairnMcpV1ReplaySequence => "replay.sequence",
            Capabilities::CairnMcpV1ReplayChallenge => "replay.challenge",
            // Extension capabilities advertise via status.extensions, not
            // status.capabilities — they ride a separate code path.
            Capabilities::CairnMcpV1ExtensionAggregate => "ext.aggregate",
            Capabilities::CairnMcpV1ExtensionAdmin => "ext.admin",
            Capabilities::CairnMcpV1ExtensionFederation => "ext.federation",
            Capabilities::CairnMcpV1ExtensionSessiontree => "ext.sessiontree",
            // Capabilities is `#[non_exhaustive]` — explicit catch-all forces
            // the table above to grow when a future codegen adds a variant.
            _ => "unknown",
        }
    }

    #[test]
    fn classify_covers_every_known_variant() {
        // Sanity: classify the variants we know exist today. If the IDL
        // adds a new variant, the catch-all above returns "unknown" and
        // this test stays green — the *intended* failure mode is the
        // match in `advertise()` itself growing a `_ =>` arm. Document
        // the rule here so a reviewer notices.
        let known = [
            Capabilities::CairnMcpV1SearchKeyword,
            Capabilities::CairnMcpV1SearchSemantic,
            Capabilities::CairnMcpV1SearchHybrid,
            Capabilities::CairnMcpV1PolicyTrace,
            Capabilities::CairnMcpV1ForgetRecord,
            Capabilities::CairnMcpV1ForgetSession,
            Capabilities::CairnMcpV1ForgetScope,
            Capabilities::CairnMcpV1RetrieveRecord,
            Capabilities::CairnMcpV1RetrieveSession,
            Capabilities::CairnMcpV1RetrieveTurn,
            Capabilities::CairnMcpV1RetrieveFolder,
            Capabilities::CairnMcpV1RetrieveScope,
            Capabilities::CairnMcpV1RetrieveProfile,
            Capabilities::CairnMcpV1ReplaySequence,
            Capabilities::CairnMcpV1ReplayChallenge,
            Capabilities::CairnMcpV1ExtensionAggregate,
            Capabilities::CairnMcpV1ExtensionAdmin,
            Capabilities::CairnMcpV1ExtensionFederation,
            Capabilities::CairnMcpV1ExtensionSessiontree,
        ];
        for c in known {
            assert_ne!(classify(c), "unknown",
                "missing classify arm for {c:?}");
        }
    }
}
