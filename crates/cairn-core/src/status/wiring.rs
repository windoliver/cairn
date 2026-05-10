//! Per-capability "is the runtime wired end-to-end?" flags.
//!
//! Brief §15 / §8.0.a forbid over-advertising: a capability appears in
//! `status.capabilities` only when the runtime can honor every call against
//! it. The flags below start `false`; the issue that lands a verb's dispatch
//! flips the matching flag to `true`.
//!
//! ## Surface granularity
//!
//! Capabilities can land on different surfaces at different times — the
//! CLI verb often arrives ahead of the SDK transport and the MCP handler,
//! and a global flag would over-advertise on the lagging surfaces. For
//! capabilities that have arrived on at least one but not all surfaces,
//! split the flag per-surface and have `advertise()` consult the gate for
//! the calling [`crate::status::Surface`]. Once every surface honors the
//! call, fold the per-surface flags back into a single global constant.
//!
//! `forget.record` is currently in this split state: the CLI path is wired
//! through `SqliteMemoryStore::forget_record`, but the SDK transport and
//! MCP handler still fall through to `unimplemented!` / `dispatch_stub`.
//! Brief §15 fail-closed therefore forbids advertising
//! `cairn.mcp.v1.forget.record` from the SDK or MCP `status` until those
//! dispatch paths land.

/// `forget --record` end-to-end dispatch path on the CLI surface.
///
/// Flipped to `true` once `cairn forget --record <id>` was wired through
/// `SqliteMemoryStore::forget_record` (issue #58 dispatch extension; the
/// engine landed earlier in #58).
pub const FORGET_RECORD_WIRED_CLI: bool = true;

/// `forget --record` end-to-end dispatch path on the SDK surface.
///
/// `false` until `Sdk::forget` calls into `MemoryStore::forget_record`
/// instead of returning `unimplemented`. Brief §15 fail-closed: SDK
/// callers must not see `cairn.mcp.v1.forget.record` in `status` while
/// `Sdk::forget` rejects with `Unimplemented`. Wiring lands in a
/// follow-up PR; flip this constant and merge with
/// [`FORGET_RECORD_WIRED_MCP`] + [`FORGET_RECORD_WIRED_CLI`] back into a
/// single global flag once every surface honors the call.
pub const FORGET_RECORD_WIRED_SDK: bool = false;

/// `forget --record` end-to-end dispatch path on the MCP surface.
///
/// `false` until the MCP `tools/call` handler dispatches `forget` against
/// the wired store instead of falling through to `dispatch_stub`. Brief
/// §15 fail-closed: same rationale as [`FORGET_RECORD_WIRED_SDK`].
pub const FORGET_RECORD_WIRED_MCP: bool = false;

/// `forget --session` (v0.2+ runtime).
pub const FORGET_SESSION_WIRED: bool = false;

/// `forget --scope` (v0.3+ runtime).
pub const FORGET_SCOPE_WIRED: bool = false;

/// `retrieve --record` dispatch path (issue #61 family).
pub const RETRIEVE_RECORD_WIRED: bool = false;

/// `retrieve --session` dispatch path.
pub const RETRIEVE_SESSION_WIRED: bool = false;

/// `retrieve --turn` dispatch path.
pub const RETRIEVE_TURN_WIRED: bool = false;

/// `retrieve --folder` dispatch path.
pub const RETRIEVE_FOLDER_WIRED: bool = false;

/// `retrieve --scope` dispatch path.
pub const RETRIEVE_SCOPE_WIRED: bool = false;

/// `retrieve --profile` dispatch path.
pub const RETRIEVE_PROFILE_WIRED: bool = false;

/// Sequence-mode replay rejection routed through every signed-verb path
/// (`prepare_wal_with_replay` integration; held back per
/// `crates/cairn-cli/src/verbs/status.rs` round-2 review #2).
pub const REPLAY_SEQUENCE_WIRED: bool = false;

/// Challenge-mode replay rejection routed through every signed-verb path.
pub const REPLAY_CHALLENGE_WIRED: bool = false;
