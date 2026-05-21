//! Local `ReBAC` decision substrate.
//!
//! The production `ReBAC` graph can be backed by Nexus or another resolver, but
//! core needs a pure, deterministic decision shape so verb paths can fail
//! closed before store access. Empty relation sets authorize only local tiers.

use serde::{Deserialize, Serialize};

use crate::domain::{Identity, MemoryVisibility, ScopeTuple};
use crate::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};

/// Read or write operation checked against the `ReBAC` relation set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RebacAction {
    /// Read a record or search result.
    Read,
    /// Write or promote a record.
    Write,
}

impl RebacAction {
    /// Stable policy-trace token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// Body-free decision reason used by traces and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RebacDecisionKind {
    /// Tier is local (`private` or `session`) and never requires `ReBAC`.
    LocalTier,
    /// A matching relation allowed the shared-tier operation.
    AllowedRelation,
    /// A shared-tier operation was requested without an authenticated principal.
    DeniedMissingPrincipal,
    /// No relation matched this `(principal, action, scope, tier)` tuple.
    DeniedNoRelation,
}

impl RebacDecisionKind {
    /// Stable policy-trace token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalTier => "local",
            Self::AllowedRelation => "allowed",
            Self::DeniedMissingPrincipal => "missing_principal",
            Self::DeniedNoRelation => "no_relation",
        }
    }
}

/// One `ReBAC` grant. The grant applies to exactly one action and visibility
/// tier for a principal. The relation scope must cover the requested scope:
/// every dimension present on the relation must match the request, while the
/// request may include narrower dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RebacRelation {
    /// Signed or otherwise authenticated caller identity.
    pub principal: Identity,
    /// Operation allowed by this relation.
    pub action: RebacAction,
    /// Scope tuple this relation covers.
    pub scope: ScopeTuple,
    /// Shared tier this relation covers.
    pub tier: MemoryVisibility,
}

impl RebacRelation {
    /// Construct a relation.
    #[must_use]
    pub fn new(
        principal: Identity,
        action: RebacAction,
        scope: ScopeTuple,
        tier: MemoryVisibility,
    ) -> Self {
        Self {
            principal,
            action,
            scope,
            tier,
        }
    }

    fn matches(
        &self,
        principal: &Identity,
        action: RebacAction,
        requested_scope: &ScopeTuple,
        requested_tier: MemoryVisibility,
    ) -> bool {
        self.principal == *principal
            && self.action == action
            && self.tier == requested_tier
            && scope_has_any_dimension(&self.scope)
            && scope_covers(&self.scope, requested_scope)
    }
}

/// Per-request `ReBAC` evaluation context.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RebacContext {
    principal: Option<Identity>,
    relations: Vec<RebacRelation>,
}

impl RebacContext {
    /// Construct a context for `principal` and the relation set visible to this
    /// request.
    #[must_use]
    pub fn new(principal: Identity, relations: Vec<RebacRelation>) -> Self {
        Self {
            principal: Some(principal),
            relations,
        }
    }

    /// Construct a context with a principal but no matching relations.
    #[must_use]
    pub fn for_principal(principal: Identity) -> Self {
        Self::new(principal, Vec::new())
    }

    /// Construct a request context from a verified signed scope. The signed
    /// scope authorizes `action` for every shared tier up to `max_tier`.
    #[must_use]
    pub fn for_scope(
        principal: Identity,
        scope: &ScopeTuple,
        action: RebacAction,
        max_tier: MemoryVisibility,
    ) -> Self {
        let relations = all_visibilities()
            .into_iter()
            .filter(|tier| is_shared_tier(*tier) && *tier <= max_tier)
            .map(|tier| RebacRelation::new(principal.clone(), action, scope.clone(), tier))
            .collect();
        Self::new(principal, relations)
    }

    /// Borrow the authenticated principal, if one is available.
    #[must_use]
    pub fn principal(&self) -> Option<&Identity> {
        self.principal.as_ref()
    }

    /// Borrow all relations in this request context.
    #[must_use]
    pub fn relations(&self) -> &[RebacRelation] {
        &self.relations
    }

    /// Evaluate one `(action, scope, tier)` operation.
    #[must_use]
    pub fn evaluate(
        &self,
        action: RebacAction,
        scope: &ScopeTuple,
        tier: MemoryVisibility,
    ) -> RebacDecision {
        if !is_shared_tier(tier) {
            return RebacDecision {
                action,
                tier,
                kind: RebacDecisionKind::LocalTier,
            };
        }

        let Some(principal) = self.principal.as_ref() else {
            return RebacDecision {
                action,
                tier,
                kind: RebacDecisionKind::DeniedMissingPrincipal,
            };
        };

        let kind = if self
            .relations
            .iter()
            .any(|relation| relation.matches(principal, action, scope, tier))
        {
            RebacDecisionKind::AllowedRelation
        } else {
            RebacDecisionKind::DeniedNoRelation
        };

        RebacDecision { action, tier, kind }
    }

    /// Filter a requested visibility set through `ReBAC`. The returned decisions
    /// include only shared-tier evaluations so traces stay focused on `ReBAC`
    /// rather than local bypasses.
    #[must_use]
    pub fn filter_visibility_allowlist(
        &self,
        action: RebacAction,
        scope: &ScopeTuple,
        requested: &[MemoryVisibility],
    ) -> (Vec<MemoryVisibility>, Vec<RebacDecision>) {
        let mut allowed = Vec::with_capacity(requested.len());
        let mut decisions = Vec::new();
        for tier in requested {
            let decision = self.evaluate(action, scope, *tier);
            if decision.allowed() {
                allowed.push(*tier);
            }
            if is_shared_tier(*tier) {
                decisions.push(decision);
            }
        }
        (allowed, decisions)
    }

    /// All visibility tiers up to `max`, filtered through `ReBAC`.
    #[must_use]
    pub fn allowed_visibilities_up_to(
        &self,
        action: RebacAction,
        scope: &ScopeTuple,
        max: MemoryVisibility,
    ) -> (Vec<MemoryVisibility>, Vec<RebacDecision>) {
        let requested = all_visibilities()
            .into_iter()
            .filter(|tier| *tier <= max)
            .collect::<Vec<_>>();
        self.filter_visibility_allowlist(action, scope, &requested)
    }
}

/// Result of one `ReBAC` evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RebacDecision {
    /// Operation that was evaluated.
    pub action: RebacAction,
    /// Visibility tier that was evaluated.
    pub tier: MemoryVisibility,
    /// Decision reason.
    pub kind: RebacDecisionKind,
}

impl RebacDecision {
    /// `true` when the request may proceed.
    #[must_use]
    pub const fn allowed(self) -> bool {
        matches!(
            self.kind,
            RebacDecisionKind::LocalTier | RebacDecisionKind::AllowedRelation
        )
    }

    /// Convert the decision into a body-free policy trace entry.
    #[must_use]
    pub const fn to_policy_trace_entry(self) -> PolicyTraceEntry {
        let outcome = if self.allowed() {
            PolicyOutcome::Pass
        } else {
            PolicyOutcome::Deny
        };
        PolicyTraceEntry::new(
            PolicyGate::Rebac,
            outcome,
            PolicyDetail::Rebac {
                action: self.action,
                tier: self.tier,
                reason: self.kind,
            },
        )
    }
}

/// `true` for tiers that leave local/private context and therefore require a
/// `ReBAC` relation.
#[must_use]
pub const fn is_shared_tier(tier: MemoryVisibility) -> bool {
    matches!(
        tier,
        MemoryVisibility::Project
            | MemoryVisibility::Team
            | MemoryVisibility::Org
            | MemoryVisibility::Public
    )
}

/// Stable ordered visibility list.
#[must_use]
pub const fn all_visibilities() -> [MemoryVisibility; 6] {
    [
        MemoryVisibility::Private,
        MemoryVisibility::Session,
        MemoryVisibility::Project,
        MemoryVisibility::Team,
        MemoryVisibility::Org,
        MemoryVisibility::Public,
    ]
}

fn scope_has_any_dimension(scope: &ScopeTuple) -> bool {
    scope.tenant.is_some()
        || scope.workspace.is_some()
        || scope.project.is_some()
        || scope.session_id.is_some()
        || scope.entity.is_some()
        || scope.user.is_some()
        || scope.agent.is_some()
}

fn scope_covers(relation: &ScopeTuple, requested: &ScopeTuple) -> bool {
    scope_dim_covers(relation.tenant.as_deref(), requested.tenant.as_deref())
        && scope_dim_covers(
            relation.workspace.as_deref(),
            requested.workspace.as_deref(),
        )
        && scope_dim_covers(relation.project.as_deref(), requested.project.as_deref())
        && scope_dim_covers(
            relation.session_id.as_deref(),
            requested.session_id.as_deref(),
        )
        && scope_dim_covers(relation.entity.as_deref(), requested.entity.as_deref())
        && scope_dim_covers(relation.user.as_deref(), requested.user.as_deref())
        && scope_dim_covers(relation.agent.as_deref(), requested.agent.as_deref())
}

fn scope_dim_covers(relation: Option<&str>, requested: Option<&str>) -> bool {
    match relation {
        Some(value) => requested == Some(value),
        None => true,
    }
}
