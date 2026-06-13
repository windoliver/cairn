//! Host-injected dispatch for the mutating core verbs (`ingest`,
//! `capture_trace`, `forget`).
//!
//! The MCP adapter must not grow a parallel implementation of the signed
//! write path (CLAUDE.md §4 invariant 3; brief §5.6 WAL + two-phase apply).
//! The full path — identity registry, keystore, server challenge, signed
//! intent verification, WAL admission — lives in the embedding process
//! (`cairn mcp` in `cairn-cli`), which already drives it for the CLI verbs.
//! Instead of duplicating that flow here, the embedder injects a
//! [`MutatingVerbHost`] via
//! [`CairnMcpHandler::with_mutation_host`](crate::CairnMcpHandler::with_mutation_host)
//! and the handler routes the three mutating tools through it after the
//! generated arg parsers have validated the payload.
//!
//! Without a wired host the handler keeps the pre-existing fail-closed
//! behaviour: a typed aborted stub envelope, no memory operation performed.

use cairn_core::generated::envelope::Response;
use cairn_core::generated::verbs::capture_trace::CaptureTraceArgs;
use cairn_core::generated::verbs::forget::ForgetArgs;
use cairn_core::generated::verbs::ingest::IngestArgs;

/// Dispatch already-validated mutating verb args through the host's signed
/// write path and return the generated response envelope.
///
/// Contract for implementors:
///
/// - The returned [`Response`] must carry the matching `verb`
///   (`Ingest` / `CaptureTrace` / `Forget`) and one of the three terminal
///   statuses (`Committed`, `Rejected`, `Aborted`) — the handler serializes
///   it verbatim into the MCP `CallToolResult` text content.
/// - Every mutation must go through the WAL state machine (brief §5.6);
///   implementors route to the same domain logic the CLI verbs use, never a
///   parallel write path.
/// - `async_trait` is used (not RPITIT) because the handler stores the host
///   as a trait object.
#[async_trait::async_trait]
pub trait MutatingVerbHost: Send + Sync {
    /// Run the `ingest` verb (brief §8) against the host's vault.
    async fn ingest(&self, args: IngestArgs) -> Response;

    /// Run the `capture_trace` verb (brief §8) against the host's vault.
    async fn capture_trace(&self, args: CaptureTraceArgs) -> Response;

    /// Run the `forget` verb (brief §8) against the host's vault.
    async fn forget(&self, args: ForgetArgs) -> Response;
}
