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
        "cairn.mcp.v1.extension.coord",
        "cairn.coord.v1 ships in v0.3 and requires coordination runtime \
         wiring; upgrade to a build that enables the coord extension",
    ),
    (
        "cairn.mcp.v1.extension.federation",
        "enable federation: set federation.enabled = true and configure a peer endpoint",
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
    (
        "dream.unavailable",
        "DreamWorkflow is disabled or its scheduler is not running. \
         Check that `dream.enabled = true` in .cairn/config.yaml, that an \
         `LLMProvider` plugin is configured, and that `cairn mcp serve` is \
         hosting the scheduler.",
    ),
    (
        "expiration.unavailable",
        "ExpirationWorkflow is disabled or its scheduler is not running. \
         Check that `expiration.enabled = true` in .cairn/config.yaml and \
         that `cairn mcp serve` is hosting the scheduler so the sweep can \
         tombstone TTL/salience-expired records.",
    ),
    (
        "evaluation.unavailable",
        "EvaluationWorkflow is disabled or its scheduler is not running. \
         Check that `evaluation.enabled = true` in .cairn/config.yaml and \
         that `cairn mcp serve` is hosting the scheduler so the golden \
         checks can run and emit report records.",
    ),
    (
        "cairn.mcp.v1.extension.admin.snapshot",
        "enable admin extension: set `admin.enabled: true` in .cairn/config.yaml \
         and grant operator role with `cairn admin grant <identity>`",
    ),
    (
        "cairn.mcp.v1.extension.admin.restore",
        "enable admin extension: set `admin.enabled: true` in .cairn/config.yaml \
         and grant operator role with `cairn admin grant <identity>`",
    ),
    (
        "cairn.mcp.v1.extension.admin.replay_wal",
        "enable admin extension: set `admin.enabled: true` in .cairn/config.yaml. \
         Dry-run requires no role; --apply requires operator",
    ),
    (
        "cairn.mcp.v1.extension.admin.connector.enable",
        "enable admin extension and grant operator role; see `cairn admin --help`",
    ),
    (
        "cairn.mcp.v1.extension.admin.connector.disable",
        "enable admin extension and grant operator role; see `cairn admin --help`",
    ),
    (
        "cairn.mcp.v1.extension.admin.connector.backfill",
        "enable admin extension and grant operator role; \
         ensure the connector is registered and currently enabled",
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
