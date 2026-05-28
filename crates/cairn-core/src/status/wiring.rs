//! Per-capability "is the runtime wired end-to-end?" flags.
//!
//! Brief §15 / §8.0.a forbid over-advertising: a capability appears in
//! `status.capabilities` only when the runtime can honor every call against
//! it. The flags below start `false`; the issue that lands a verb's dispatch
//! flips the matching flag to `true`. CLI, SDK, and MCP all read these
//! through `cairn_core::status::advertise()` so flipping one constant
//! propagates to every surface.

/// `forget --record` end-to-end dispatch path is wired (issue family #54+).
pub const FORGET_RECORD_WIRED: bool = true;

/// `forget --session` (v0.2+ runtime).
pub const FORGET_SESSION_WIRED: bool = true;

/// `forget --scope` (v0.3+ runtime).
pub const FORGET_SCOPE_WIRED: bool = false;

/// `retrieve --record` dispatch path (issue #61 family).
pub const RETRIEVE_RECORD_WIRED: bool = false;

/// `retrieve --session` dispatch path.
pub const RETRIEVE_SESSION_WIRED: bool = true;

/// `retrieve --turn` dispatch path.
pub const RETRIEVE_TURN_WIRED: bool = true;

/// `retrieve --tool-call` dispatch path.
pub const RETRIEVE_TOOL_CALL_WIRED: bool = true;

/// `retrieve --folder` dispatch path.
pub const RETRIEVE_FOLDER_WIRED: bool = false;

/// `retrieve --scope` dispatch path.
pub const RETRIEVE_SCOPE_WIRED: bool = false;

/// `retrieve --profile` dispatch path.
pub const RETRIEVE_PROFILE_WIRED: bool = false;

/// `cairn.coord.v1` multi-agent coordination extension.
///
/// The registry and `FlushPlan` mutation contract can land before the runtime
/// dispatcher. Keep the capability hidden until CLI/MCP/SDK calls can honor
/// leases, signals, actions, routines, and frontier end to end.
pub const COORD_EXTENSION_WIRED: bool = false;

/// `cairn coord` CLI dispatch is wired end to end.
pub const COORD_CLI_DISPATCH_WIRED: bool = false;

/// Flush apply/requeue can execute coord mutations end to end.
pub const COORD_FLUSH_RUNTIME_WIRED: bool = false;

/// `cairn.coord.v1` MCP tool declarations are wired.
pub const COORD_MCP_TOOLS_WIRED: bool = false;

/// `cairn.coord.v1` MCP tool dispatch is wired end to end.
pub const COORD_MCP_DISPATCH_WIRED: bool = false;

/// Single readiness source for advertising `cairn.coord.v1`.
#[must_use]
pub const fn coord_extension_ready() -> bool {
    COORD_EXTENSION_WIRED
        && COORD_CLI_DISPATCH_WIRED
        && COORD_FLUSH_RUNTIME_WIRED
        && COORD_MCP_TOOLS_WIRED
        && COORD_MCP_DISPATCH_WIRED
}

/// Readiness gate for local flush execution of coord mutations.
#[must_use]
pub const fn coord_flush_runtime_ready() -> bool {
    COORD_EXTENSION_WIRED && COORD_FLUSH_RUNTIME_WIRED
}

/// `PreCompact` sensor capture + status advertisement path (issue #310).
///
/// Held off until a real dispatched runtime caller invokes
/// `pipeline::pre_compact::run_pre_compact` and persists the snapshot.
/// Today the orchestrator and trace projector exist (issue #310 core
/// landing), but no sensor or MCP path dispatches them, so advertising
/// the capability would let clients negotiate support that has no
/// callable hook on the wire — exactly the over-advertise failure mode
/// brief §15 forbids. Flip to `true` in the issue that lands the
/// sensor + MCP dispatch path.
pub const SENSORS_PRE_COMPACT_WIRED: bool = false;

/// Sequence-mode replay rejection routed through every signed-verb path
/// (`prepare_wal_with_replay` integration; held back per
/// `crates/cairn-cli/src/verbs/status.rs` round-2 review #2).
pub const REPLAY_SEQUENCE_WIRED: bool = false;

/// Challenge-mode replay rejection routed through every signed-verb path.
pub const REPLAY_CHALLENGE_WIRED: bool = false;

/// Rolling-summary `ConsolidationWorkflow` dispatch path (issue #90).
///
/// Flipped `true` in Task 17: the scheduler is now booted inside
/// `cairn mcp serve` (the long-lived entry point). `enqueue_if_due` is
/// called from the `capture_trace` CLI path (Task 16). Both criteria are
/// met — the capability is now advertised.
pub const CONSOLIDATION_WORKFLOW_WIRED: bool = true;

/// `DreamWorkflow` (LLM-only minimum) dispatch path (issue #91, brief §10.1).
///
/// Flipped `true` in the issue #91 wiring commit: `cairn mcp serve`
/// registers `DreamHandler` on the scheduler. Runtime advertisement
/// still requires `dream.enabled = true` in config plus a configured
/// `LLMProvider` — the handler returns `Permanent` when no provider is
/// wired, but the capability gate (this constant AND `gates.dream_runtime_ready`)
/// already keeps the capability hidden on default deployments.
pub const DREAM_WORKFLOW_WIRED: bool = true;

/// `ExpirationWorkflow` (soft-retirement via `TombstoneReason::Expire`)
/// dispatch path (issue #91, brief §10.0).
///
/// Flipped `true` in the issue #91 wiring commit. Brief §10 places this
/// workflow on the v0.1 roadmap; the minimum path here is
/// `MemoryStore::tombstone(_, Expire)` — full WAL-`expire` integration
/// is deferred to §5.6.
pub const EXPIRATION_WORKFLOW_WIRED: bool = true;

/// `EvaluationWorkflow` (golden-check report + metrics) dispatch path
/// (issue #91, brief §15).
///
/// Flipped `true` in the issue #91 wiring commit. Runtime advertisement
/// still requires `evaluation.enabled = true` in config plus a
/// single-tenant `cairn mcp serve` host.
pub const EVALUATION_WORKFLOW_WIRED: bool = true;

/// `cairn.federation.v1` extension capability registration (issue #123).
///
/// Flipped `true`: every dispatch path is wired end-to-end.
/// `CairnMcpHandler` carries an `Option<FederationState>` that holds
/// the signing key, outbox, and transport deps. When `None`, the
/// dispatch path returns `CapabilityUnavailable` — the wiring
/// constant means "the code path exists", not "every deployment has
/// federation configured". Runtime advertisement is further gated by
/// `Phase::V0_3` in `advertise()`.
pub const FEDERATION_EXTENSION_WIRED: bool = true;

/// `propose_share` + `revoke_share` verb dispatch wired through CLI/SDK/MCP.
///
/// Flipped `true`: verb-layer implementations (T7, T9) are in place
/// and the MCP / CLI surfaces inject the keystore + outbox + transport
/// deps through `FederationState`.
pub const FEDERATION_PROPOSE_DISPATCH_WIRED: bool = true;

/// `accept_share` verb dispatch wired (inbound receive path).
///
/// Flipped `true`: verb-layer landing (T8) is in place and the MCP
/// surface injects the rebac context + consent lookup + outbox deps
/// through `FederationState`.
pub const FEDERATION_ACCEPT_DISPATCH_WIRED: bool = true;

/// `PropagationWorkflow` handler registered on the scheduler.
///
/// Flipped `true`: `cairn mcp serve` registers `PropagationHandler`
/// on the live `Scheduler` when federation state is configured, so
/// propagation jobs drain end to end.
pub const FEDERATION_WORKFLOW_WIRED: bool = true;

/// `cairn.federation.v1` MCP tool declarations wired (rmcp registration).
///
/// Flipped `true`: `crates/cairn-mcp/src/federation_tools.rs::dispatch`
/// is replaced with real verb dispatch through `FederationState`.
pub const FEDERATION_MCP_TOOLS_WIRED: bool = true;

/// Single readiness source for advertising `cairn.federation.v1`.
///
/// Reads as `true` only when all five federation wiring constants
/// flip. `cairn-core::status::advertise()` AND-gates this against
/// `Phase::V0_3` before pushing
/// `Capabilities::CairnMcpV1ExtensionFederation`, so flipping any
/// single constant alone does not change the wire shape.
#[must_use]
pub const fn federation_extension_ready() -> bool {
    FEDERATION_EXTENSION_WIRED
        && FEDERATION_PROPOSE_DISPATCH_WIRED
        && FEDERATION_ACCEPT_DISPATCH_WIRED
        && FEDERATION_WORKFLOW_WIRED
        && FEDERATION_MCP_TOOLS_WIRED
}

/// `cairn.admin.v1` extension capability registration (issue #161).
///
/// Live as of issue #161 Gap 8. Capability advertisement still gates on
/// runtime preconditions (`admin.enabled` config + at-least-one-operator)
/// through [`admin_extension_ready`].
pub const ADMIN_EXTENSION_WIRED: bool = true;

/// `true` — `cairn admin {grant, replay-wal, connector {enable,disable,
/// backfill}}` route through `cairn-core::verbs::admin::*` (issue #161
/// Gap 2). Note: `cairn admin snapshot` and `cairn admin restore` still
/// take the v0.1 directory-tree dispatch path for backward compatibility;
/// the v0.2 IDL verbs `admin_snapshot` / `admin_restore` ship via MCP and
/// SDK only.
pub const ADMIN_CLI_DISPATCH_WIRED: bool = true;

/// `true` — `cairn-mcp` registers tool decls + handler dispatch for all
/// six admin verbs (issue #161 Gap 5). `tools/list` advertises them
/// when the negotiated capability set includes
/// `cairn.mcp.v1.extension.admin`.
pub const ADMIN_MCP_DISPATCH_WIRED: bool = true;

/// Truth-table gate for advertising the `cairn.admin.v1` extension.
///
/// All preconditions must hold: build-time wiring, both per-surface
/// dispatch constants, runtime config opt-in, and at least one operator
/// identity present in `admin_roles`. If ANY is false, the capability is
/// not advertised — brief §15 "fail closed" / CLAUDE.md §4 invariant 6.
///
/// Not a `const fn` because it depends on runtime state (config + DB).
#[must_use]
pub fn admin_extension_ready(config_enabled: bool, has_operator: bool) -> bool {
    ADMIN_EXTENSION_WIRED
        && ADMIN_CLI_DISPATCH_WIRED
        && ADMIN_MCP_DISPATCH_WIRED
        && config_enabled
        && has_operator
}

#[cfg(test)]
mod admin_extension_ready_tests {
    use super::*;

    #[test]
    fn truth_table() {
        // The helper is true iff ALL of {WIRED, CLI_DISPATCH_WIRED,
        // MCP_DISPATCH_WIRED, config_enabled, has_operator} hold. With any
        // of the build-time constants `false`, the last row collapses to
        // `false` — exactly what we want during phased rollout.
        let all_wired =
            ADMIN_EXTENSION_WIRED && ADMIN_CLI_DISPATCH_WIRED && ADMIN_MCP_DISPATCH_WIRED;
        let cases = [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, all_wired),
        ];
        for (config, has_op, expected) in cases {
            assert_eq!(
                admin_extension_ready(config, has_op),
                expected,
                "config={config} has_op={has_op}"
            );
        }
    }
}
