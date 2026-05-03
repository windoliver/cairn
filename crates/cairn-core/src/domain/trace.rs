//! Trace-record domain types (issue #77, brief §5.0, §9.3).

use serde::{Deserialize, Serialize};

/// Classifies the kind of event captured in a trace record.
///
/// Each variant maps to a distinct moment in an agent turn:
/// the human message, agent reply, tool lifecycle steps, and
/// session bookends. `#[non_exhaustive]` allows new variants
/// to be added without a semver break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraceEvent {
    /// A message sent by the human / user turn.
    UserMessage,
    /// A message produced by the agent.
    AgentMessage,
    /// Captured immediately before a tool call is dispatched.
    PreTool,
    /// Captured immediately after a tool call returns.
    PostTool,
    /// The raw output returned by a tool invocation.
    ToolOutput,
    /// The agent stop / end-of-turn event.
    Stop,
    /// A summarising record written at the end of a turn.
    TurnSummary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TraceEvent::UserMessage).unwrap(),
            "\"user_message\""
        );
        assert_eq!(
            serde_json::from_str::<TraceEvent>("\"turn_summary\"").unwrap(),
            TraceEvent::TurnSummary
        );
    }
}
