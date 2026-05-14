//! Operator-facing remediation hints for `CapabilityUnavailable` rejections.
//!
//! Every site that constructs a `CapabilityUnavailable` error pulls its
//! remediation string from this map so all four surfaces (CLI, MCP, SDK,
//! skill) emit identical hint text. Capabilities not in the map cause the
//! caller to omit `data.remediation` from the wire envelope (the field is
//! optional per IDL — Task 1).

/// Static lookup table — capability string → free-form operator hint.
///
/// Order is decision-table order (search → `policy_trace` → forget → replay)
/// so a future generated exhaustiveness test reads top-to-bottom.
pub const REMEDIATION: &[(&str, &str)] = &[
    (
        "cairn.mcp.v1.search.semantic",
        "set search.local_embeddings: true in .cairn/config.yaml and run \
         cairn embed download to materialize the embedding model",
    ),
    (
        "cairn.mcp.v1.search.hybrid",
        "set search.local_embeddings: true in .cairn/config.yaml and run \
         cairn embed download to materialize the embedding model",
    ),
    (
        "cairn.mcp.v1.summarize.narrative",
        "configure an LLM provider with JSON-mode support to enable narrative summaries",
    ),
    (
        "cairn.mcp.v1.policy_trace",
        "policy_trace is enabled by default; check .cairn/config.yaml for \
         an explicit override that disabled it",
    ),
    (
        "cairn.mcp.v1.forget.record",
        "forget.record is part of the v0.1 core surface; if you see this, \
         the runtime is reporting a wiring fault — file a bug",
    ),
    (
        "cairn.mcp.v1.forget.session",
        "forget.session ships in v0.2; upgrade to a v0.2+ runtime",
    ),
    (
        "cairn.mcp.v1.forget.scope",
        "forget.scope ships in v0.3; upgrade to a v0.3+ runtime",
    ),
    (
        "cairn.mcp.v1.retrieve.session",
        "retrieve.session is part of the v0.1 core surface; if you see this, \
         the runtime is reporting a wiring fault — file a bug",
    ),
    (
        "cairn.mcp.v1.retrieve.turn",
        "retrieve.turn is part of the v0.1 core surface; if you see this, \
         the runtime is reporting a wiring fault — file a bug",
    ),
    (
        "cairn.mcp.v1.retrieve.tool_call",
        "retrieve.tool_call is part of the v0.1 core surface; if you see \
         this, the runtime is reporting a wiring fault — file a bug",
    ),
    (
        "cairn.mcp.v1.replay.sequence",
        "signed-intent replay protection requires a wired challenge dispatch \
         path; not available in this build",
    ),
    (
        "cairn.mcp.v1.replay.challenge",
        "signed-intent replay protection requires a wired challenge dispatch \
         path; not available in this build",
    ),
    (
        "consolidation.unavailable",
        "Rolling-summary ConsolidationWorkflow is disabled or its scheduler is not running. \
         Check that `consolidation.enabled = true` in .cairn/config.yaml and that the \
         long-running `cairn mcp serve` host is up so the scheduler can lease jobs.",
    ),
];

/// Operator-facing remediation hint for `capability`, or `None` when no hint
/// is registered. Callers that get `None` SHOULD omit `data.remediation`
/// rather than emit an empty string — `data.remediation` declares
/// `minLength: 1`.
#[must_use]
pub fn remediation_for(capability: &str) -> Option<&'static str> {
    REMEDIATION
        .iter()
        .find_map(|(k, v)| if *k == capability { Some(*v) } else { None })
}
