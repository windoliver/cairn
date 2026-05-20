//! `ReBAC` allow/deny and policy-trace fixtures.

use cairn_core::domain::{Identity, MemoryVisibility, ScopeTuple};
use cairn_core::generated::envelope::ResponsePolicyTraceResult;
use cairn_core::policy_trace::to_wire;
use cairn_core::rebac::{RebacAction, RebacContext, RebacDecisionKind, RebacRelation};

fn principal() -> Identity {
    Identity::parse("agt:cairn-cli:default:writer:v1").expect("valid identity")
}

fn project_scope() -> ScopeTuple {
    ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("vault-a".to_owned()),
        entity: Some("ingest".to_owned()),
        ..ScopeTuple::default()
    }
}

#[test]
fn private_tier_bypasses_rebac_relations() {
    let decision = RebacContext::default().evaluate(
        RebacAction::Read,
        &project_scope(),
        MemoryVisibility::Private,
    );

    assert_eq!(decision.kind, RebacDecisionKind::LocalTier);
    assert!(decision.allowed());
}

#[test]
fn shared_tier_denies_without_relation() {
    let decision = RebacContext::for_principal(principal()).evaluate(
        RebacAction::Read,
        &project_scope(),
        MemoryVisibility::Project,
    );

    assert_eq!(decision.kind, RebacDecisionKind::DeniedNoRelation);
    assert!(!decision.allowed());
}

#[test]
fn shared_tier_allows_matching_relation() {
    let principal = principal();
    let scope = project_scope();
    let ctx = RebacContext::new(
        principal.clone(),
        vec![RebacRelation::new(
            principal,
            RebacAction::Read,
            scope.clone(),
            MemoryVisibility::Project,
        )],
    );

    let decision = ctx.evaluate(RebacAction::Read, &scope, MemoryVisibility::Project);

    assert_eq!(decision.kind, RebacDecisionKind::AllowedRelation);
    assert!(decision.allowed());
}

#[test]
fn signed_scope_context_allows_shared_tiers_up_to_authorized_tier() {
    let principal = principal();
    let scope = project_scope();
    let ctx = RebacContext::for_scope(principal, &scope, RebacAction::Read, MemoryVisibility::Team);

    assert!(
        ctx.evaluate(RebacAction::Read, &scope, MemoryVisibility::Project)
            .allowed()
    );
    assert!(
        ctx.evaluate(RebacAction::Read, &scope, MemoryVisibility::Team)
            .allowed()
    );
    assert!(
        !ctx.evaluate(RebacAction::Read, &scope, MemoryVisibility::Org)
            .allowed()
    );
}

#[test]
fn shared_tier_does_not_reuse_write_relation_for_read() {
    let principal = principal();
    let scope = project_scope();
    let ctx = RebacContext::new(
        principal.clone(),
        vec![RebacRelation::new(
            principal,
            RebacAction::Write,
            scope.clone(),
            MemoryVisibility::Project,
        )],
    );

    let decision = ctx.evaluate(RebacAction::Read, &scope, MemoryVisibility::Project);

    assert_eq!(decision.kind, RebacDecisionKind::DeniedNoRelation);
    assert!(!decision.allowed());
}

#[test]
fn rebac_policy_trace_explains_denied_shared_tier() {
    let decision = RebacContext::for_principal(principal()).evaluate(
        RebacAction::Read,
        &project_scope(),
        MemoryVisibility::Project,
    );
    let wire = to_wire(&[decision.to_policy_trace_entry()]);

    assert_eq!(wire[0].gate, "rebac");
    assert_eq!(wire[0].result, ResponsePolicyTraceResult::Deny);
    assert_eq!(
        wire[0].detail.as_deref(),
        Some("rebac:read:project:no_relation")
    );
}
