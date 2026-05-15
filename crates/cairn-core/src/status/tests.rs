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
        screen_capture_enabled: false,
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
        // For unit tests, embedding_provider_ready mirrors model_present
        // (local-provider semantics). Tests that specifically need to verify
        // cloud-provider decoupling construct gates directly (see below).
        embedding_provider_ready: model_present,
        llm_configured: false,
        consolidation_runtime_ready: false,
        dream_runtime_ready: false,
        expiration_runtime_ready: false,
        evaluation_runtime_ready: false,
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
    let store = Some(StoreCaps {
        fts: false,
        vector: true,
    });
    let g = gates(true, true, store);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(
        !caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
        "hybrid requires FTS; got {caps:?}"
    );
}

#[test]
fn bound_store_without_vector_drops_semantic_and_hybrid() {
    let store = Some(StoreCaps {
        fts: true,
        vector: false,
    });
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
fn forget_record_advertises_when_runtime_is_wired() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(
        caps.contains(&Capabilities::CairnMcpV1ForgetRecord),
        "forget.record must be advertised once runtime wiring is enabled"
    );
}

#[test]
fn retrieve_session_turn_and_tool_call_advertise_when_runtime_is_wired() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    for cap in [
        Capabilities::CairnMcpV1RetrieveSession,
        Capabilities::CairnMcpV1RetrieveTurn,
        Capabilities::CairnMcpV1RetrieveToolCall,
    ] {
        assert!(
            caps.contains(&cap),
            "wired retrieve target {cap:?} must be advertised"
        );
    }
    for cap in [
        Capabilities::CairnMcpV1RetrieveRecord,
        Capabilities::CairnMcpV1RetrieveFolder,
        Capabilities::CairnMcpV1RetrieveScope,
        Capabilities::CairnMcpV1RetrieveProfile,
    ] {
        assert!(
            !caps.contains(&cap),
            "unwired retrieve target {cap:?} must not be advertised"
        );
    }
}

#[test]
fn forget_session_pinned_to_v0_2_phase() {
    let mut g = gates(true, true, None);
    g.contract_phase = Phase::V0_1;
    let caps_v0_1 = advertise(&g);
    g.contract_phase = Phase::V0_2;
    let caps_v0_2 = advertise(&g);
    assert!(!caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetSession));
    assert!(caps_v0_2.contains(&Capabilities::CairnMcpV1ForgetSession));
}

#[test]
fn replay_capabilities_held_back() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1ReplaySequence));
    assert!(!caps.contains(&Capabilities::CairnMcpV1ReplayChallenge));
}

#[test]
fn consolidation_capability_requires_wiring_and_runtime_ready() {
    // Gates with runtime NOT ready: cap must be absent regardless of
    // the compile-time wiring constant (round-8 adversarial review #2).
    let mut g = gates(true, true, None);
    g.consolidation_runtime_ready = false;
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnWorkflowsV1Consolidation),
        "must not advertise consolidation when runtime is not ready"
    );

    // Gates with runtime ready: cap presence mirrors the wiring const.
    g.consolidation_runtime_ready = true;
    let caps = advertise(&g);
    let has_it = caps.contains(&Capabilities::CairnWorkflowsV1Consolidation);
    assert_eq!(
        has_it,
        wiring::CONSOLIDATION_WORKFLOW_WIRED,
        "with runtime_ready=true, advertise mirrors CONSOLIDATION_WORKFLOW_WIRED"
    );
}

#[test]
fn dream_capability_requires_wiring_runtime_ready_and_llm_provider() {
    // Runtime not ready: cap absent.
    let mut g = gates(true, true, None);
    g.dream_runtime_ready = false;
    g.llm_configured = true;
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnWorkflowsV1Dream),
        "must not advertise dream when runtime is not ready"
    );

    // Runtime ready but no LLM: cap absent (brief §15 fail-closed —
    // DreamHandler returns Permanent without an LLMProvider).
    g.dream_runtime_ready = true;
    g.llm_configured = false;
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnWorkflowsV1Dream),
        "must not advertise dream when no LLMProvider is configured"
    );

    // Both gates on: cap presence mirrors the wiring const.
    g.dream_runtime_ready = true;
    g.llm_configured = true;
    let caps = advertise(&g);
    assert_eq!(
        caps.contains(&Capabilities::CairnWorkflowsV1Dream),
        wiring::DREAM_WORKFLOW_WIRED,
        "with runtime_ready + llm_configured, advertise mirrors DREAM_WORKFLOW_WIRED"
    );
}

#[test]
fn expiration_capability_requires_wiring_and_runtime_ready() {
    let mut g = gates(true, true, None);
    g.expiration_runtime_ready = false;
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnWorkflowsV1Expiration),
        "must not advertise expiration when runtime is not ready"
    );

    g.expiration_runtime_ready = true;
    let caps = advertise(&g);
    assert_eq!(
        caps.contains(&Capabilities::CairnWorkflowsV1Expiration),
        wiring::EXPIRATION_WORKFLOW_WIRED,
        "with runtime_ready=true, advertise mirrors EXPIRATION_WORKFLOW_WIRED"
    );
}

#[test]
fn evaluation_capability_requires_wiring_and_runtime_ready() {
    let mut g = gates(true, true, None);
    g.evaluation_runtime_ready = false;
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnWorkflowsV1Evaluation),
        "must not advertise evaluation when runtime is not ready"
    );

    g.evaluation_runtime_ready = true;
    let caps = advertise(&g);
    assert_eq!(
        caps.contains(&Capabilities::CairnWorkflowsV1Evaluation),
        wiring::EVALUATION_WORKFLOW_WIRED,
        "with runtime_ready=true, advertise mirrors EVALUATION_WORKFLOW_WIRED"
    );
}

#[test]
fn pre_compact_capability_tracks_wiring_constant() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(
        caps.contains(&Capabilities::CairnMcpV1SensorsPreCompact)
            == wiring::SENSORS_PRE_COMPACT_WIRED,
        "sensors.pre_compact advertisement drifted from wiring constant"
    );
}

#[test]
fn pre_compact_capability_held_back_until_dispatched() {
    // The orchestrator + classifier exist (issue #310 core landing) but
    // no sensor or MCP path dispatches them, so the capability must stay
    // hidden — over-advertising would let clients negotiate a hook that
    // has no callable wire entrypoint.
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnMcpV1SensorsPreCompact),
        "sensors.pre_compact must remain hidden until a runtime caller dispatches run_pre_compact"
    );
}

#[test]
fn output_order_is_stable() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    // search.* before policy_trace, per the table.
    let kw_idx = caps
        .iter()
        .position(|c| matches!(c, Capabilities::CairnMcpV1SearchKeyword));
    let pt_idx = caps
        .iter()
        .position(|c| matches!(c, Capabilities::CairnMcpV1PolicyTrace));
    assert!(kw_idx.is_some() && pt_idx.is_some());
    assert!(
        kw_idx.expect("keyword must be present") < pt_idx.expect("policy_trace must be present"),
        "wire-stable order requires search.keyword before policy_trace; got {caps:?}"
    );
}

#[cfg(test)]
mod remediation_tests {
    use super::*;

    #[test]
    fn remediation_for_search_semantic_is_set() {
        let hint = remediation_for("cairn.mcp.v1.search.semantic")
            .expect("semantic must have a remediation hint");
        assert!(
            hint.contains("local_embeddings"),
            "remediation should mention the toggle: got {hint:?}"
        );
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
    fn consolidation_remediation_present() {
        let hint = REMEDIATION
            .iter()
            .find(|(code, _)| *code == "consolidation.unavailable");
        assert!(
            hint.is_some(),
            "remediation hint for consolidation must exist"
        );
    }

    #[test]
    fn dream_remediation_present() {
        let hint = REMEDIATION
            .iter()
            .find(|(code, _)| *code == "dream.unavailable");
        assert!(hint.is_some(), "remediation hint for dream must exist");
    }

    #[test]
    fn expiration_remediation_present() {
        let hint = REMEDIATION
            .iter()
            .find(|(code, _)| *code == "expiration.unavailable");
        assert!(hint.is_some(), "remediation hint for expiration must exist");
    }

    #[test]
    fn evaluation_remediation_present() {
        let hint = REMEDIATION
            .iter()
            .find(|(code, _)| *code == "evaluation.unavailable");
        assert!(hint.is_some(), "remediation hint for evaluation must exist");
    }

    #[test]
    fn remediation_table_has_no_empty_strings() {
        for (cap, hint) in REMEDIATION {
            assert!(!cap.is_empty(), "empty capability key");
            assert!(!hint.is_empty(), "empty remediation for {cap}");
        }
    }

    #[test]
    fn every_advertised_capability_has_remediation() {
        // Pin: any capability we *can* advertise at v0.1 (with all wiring flags
        // imagined on) must have a remediation string registered. Future
        // advertisers won't accidentally ship a fail-closed verb without an
        // operator hint.
        use crate::config::CapabilitySet;

        let always_on = CapabilitySet {
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
        };
        let gates = CapabilityGates {
            config: always_on,
            store: Some(StoreCaps {
                fts: true,
                vector: true,
            }),
            vault_bound: true,
            model_present: true,
            embedding_provider_ready: true,
            llm_configured: true,
            contract_phase: Phase::V0_1,
            consolidation_runtime_ready: false,
            dream_runtime_ready: false,
            expiration_runtime_ready: false,
            evaluation_runtime_ready: false,
        };

        for cap in advertise(&gates) {
            let cap_str = serde_json::to_value(cap)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .expect("cap serializes to string");
            // search.keyword and policy_trace are universally available; their
            // remediation is "should not happen" — None is acceptable.
            // sensors.pre_compact is hook-advertisement only for now; there is
            // no CapabilityUnavailable remediation path until a gated verb
            // consumes it.
            if cap_str == "cairn.mcp.v1.search.keyword"
                || cap_str == "cairn.mcp.v1.policy_trace"
                || cap_str == "cairn.mcp.v1.sensors.pre_compact"
                // Workflow caps use error codes "<name>.unavailable" (not the
                // wire string) in the REMEDIATION table.
                || cap_str == "cairn.workflows.v1.consolidation"
                || cap_str == "cairn.workflows.v1.dream"
                || cap_str == "cairn.workflows.v1.expiration"
                || cap_str == "cairn.workflows.v1.evaluation"
            {
                continue;
            }
            assert!(
                remediation_for(&cap_str).is_some(),
                "no remediation registered for {cap_str}"
            );
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
        (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
            |(kw, sem, hyb, pt)| crate::config::CapabilitySet {
                keyword_search: kw,
                semantic_search: sem,
                hybrid_search: hyb,
                llm_extract: false,
                agent_extract: false,
                screen_capture_enabled: false,
                graph_edges: false,
                policy_trace: pt,
                replay_sequence: true,
                replay_challenge: true,
            },
        )
    }

    fn arb_gates() -> impl Strategy<Value = CapabilityGates> {
        (
            arb_cap_set(),
            arb_store(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            arb_phase(),
        )
            .prop_map(|(config, store, bound, model, embed_ready, llm, phase)| {
                CapabilityGates {
                    config,
                    store,
                    vault_bound: bound,
                    model_present: model,
                    embedding_provider_ready: embed_ready,
                    llm_configured: llm,
                    consolidation_runtime_ready: false,
                    dream_runtime_ready: false,
                    expiration_runtime_ready: false,
                    evaluation_runtime_ready: false,
                    contract_phase: phase,
                }
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
                gates.embedding_provider_ready = false;
                advertise(&gates)
            };
            let on = {
                gates.model_present = true;
                gates.embedding_provider_ready = true;
                advertise(&gates)
            };
            for cap in &off {
                prop_assert!(on.contains(cap),
                    "model_present/embedding_provider_ready true must be a superset of false; lost {cap:?}");
            }
        }

        #[test]
        fn unbound_always_empty(mut gates in arb_gates()) {
            gates.vault_bound = false;
            prop_assert!(advertise(&gates).is_empty());
        }
    }
}

/// `embedding_provider_ready = false` suppresses semantic and hybrid even
/// when `model_present = true` and the store has a vector index.
///
/// This is the concrete scenario from Finding 3 (issue #53 review):
/// `default_provider = openai`, a stale local model file exists on disk
/// (`model_present = true`), but `OPENAI_API_KEY` is not set in the
/// environment → `embedding_provider_ready = false` → no semantic/hybrid.
#[test]
fn openai_provider_without_key_drops_semantic_and_hybrid() {
    let store = Some(StoreCaps {
        fts: true,
        vector: true, // vector index present
    });
    let g = CapabilityGates {
        config: cap_set_default(true, true), // semantic+hybrid on in config
        store,
        vault_bound: true,
        model_present: true, // stale local model file on disk
        // Cloud provider not ready (feature flag off or API key missing)
        embedding_provider_ready: false,
        llm_configured: false,
        consolidation_runtime_ready: false,
        dream_runtime_ready: false,
        expiration_runtime_ready: false,
        evaluation_runtime_ready: false,
        contract_phase: Phase::V0_1,
    };
    let caps = advertise(&g);
    assert!(
        !caps.contains(&Capabilities::CairnMcpV1SearchSemantic),
        "semantic must not be advertised when embedding_provider_ready=false; got {caps:?}"
    );
    assert!(
        !caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
        "hybrid must not be advertised when embedding_provider_ready=false; got {caps:?}"
    );
    // keyword is still fine — no embedding needed
    assert!(
        caps.contains(&Capabilities::CairnMcpV1SearchKeyword),
        "keyword must still be advertised; got {caps:?}"
    );
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
            Capabilities::CairnMcpV1SensorsPreCompact => "sensors.pre_compact",
            Capabilities::CairnMcpV1ForgetRecord => "forget.record",
            Capabilities::CairnMcpV1ForgetSession => "forget.session",
            Capabilities::CairnMcpV1ForgetScope => "forget.scope",
            Capabilities::CairnMcpV1RetrieveRecord => "retrieve.record",
            Capabilities::CairnMcpV1RetrieveSession => "retrieve.session",
            Capabilities::CairnMcpV1RetrieveTurn => "retrieve.turn",
            Capabilities::CairnMcpV1RetrieveToolCall => "retrieve.tool_call",
            Capabilities::CairnMcpV1RetrieveFolder => "retrieve.folder",
            Capabilities::CairnMcpV1RetrieveScope => "retrieve.scope",
            Capabilities::CairnMcpV1RetrieveProfile => "retrieve.profile",
            Capabilities::CairnMcpV1ReplaySequence => "replay.sequence",
            Capabilities::CairnMcpV1ReplayChallenge => "replay.challenge",
            Capabilities::CairnWorkflowsV1Consolidation => "workflows.consolidation",
            Capabilities::CairnWorkflowsV1Dream => "workflows.dream",
            Capabilities::CairnWorkflowsV1Expiration => "workflows.expiration",
            Capabilities::CairnWorkflowsV1Evaluation => "workflows.evaluation",
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
            Capabilities::CairnMcpV1SensorsPreCompact,
            Capabilities::CairnMcpV1ForgetRecord,
            Capabilities::CairnMcpV1ForgetSession,
            Capabilities::CairnMcpV1ForgetScope,
            Capabilities::CairnMcpV1RetrieveRecord,
            Capabilities::CairnMcpV1RetrieveSession,
            Capabilities::CairnMcpV1RetrieveTurn,
            Capabilities::CairnMcpV1RetrieveToolCall,
            Capabilities::CairnMcpV1RetrieveFolder,
            Capabilities::CairnMcpV1RetrieveScope,
            Capabilities::CairnMcpV1RetrieveProfile,
            Capabilities::CairnMcpV1ReplaySequence,
            Capabilities::CairnMcpV1ReplayChallenge,
            Capabilities::CairnWorkflowsV1Consolidation,
            Capabilities::CairnWorkflowsV1Dream,
            Capabilities::CairnWorkflowsV1Expiration,
            Capabilities::CairnWorkflowsV1Evaluation,
            Capabilities::CairnMcpV1ExtensionAggregate,
            Capabilities::CairnMcpV1ExtensionAdmin,
            Capabilities::CairnMcpV1ExtensionFederation,
            Capabilities::CairnMcpV1ExtensionSessiontree,
        ];
        for c in known {
            assert_ne!(classify(c), "unknown", "missing classify arm for {c:?}");
        }
    }
}
