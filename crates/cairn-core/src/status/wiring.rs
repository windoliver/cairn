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
