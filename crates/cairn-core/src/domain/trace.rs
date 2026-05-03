//! Trace-record domain types (issue #77, brief §5.0, §9.3).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{CaptureEventId, SessionId};

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

/// Linkage metadata attached to every trace record. Stored under
/// `extra_frontmatter.trace` on the `MemoryRecord`. The single canonical
/// location for all trace fields (spec §6.1, §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceLink {
    /// Owning session.
    pub session_id: SessionId,
    /// Opaque harness-supplied turn id (matches `CaptureRefs.turn_id`).
    pub turn_id: String,
    /// Monotonic position within `(session_id, turn_id)`.
    pub sequence: u64,
    /// Stable identity of the originating `CaptureEvent`.
    pub capture_event_id: CaptureEventId,
    /// Set on `PostTool` and `ToolOutput`; points at the `PreTool` that
    /// initiated the tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<CaptureEventId>,
    /// Set on `PreTool`, `PostTool`, and `ToolOutput`. Groups events
    /// belonging to the same tool invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Set on `TurnSummary` only. The exact set of capture-event ids the
    /// summary covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_event_ids: Vec<CaptureEventId>,
}

/// Field-level validation failures for [`TraceLink::validate`].
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceLinkError {
    /// `turn_id` was empty or whitespace-only.
    #[error("turn_id must not be empty or whitespace")]
    EmptyTurnId,
    /// A tool-bearing event lacked `tool_call_id`.
    #[error("tool_call_id required for this event type")]
    MissingToolCallId,
    /// A `PostTool` / `ToolOutput` lacked `parent_event_id`.
    #[error("parent_event_id required for this event type")]
    MissingParent,
    /// `parent_event_id` was set on an event type that does not allow it.
    #[error("parent_event_id only valid on PostTool/ToolOutput")]
    UnexpectedParent,
    /// `tool_call_id` was set on an event type that does not allow it.
    #[error("tool_call_id only valid on PreTool/PostTool/ToolOutput")]
    UnexpectedToolCallId,
    /// `member_event_ids` was non-empty on a non-`TurnSummary` event.
    #[error("member_event_ids only valid on TurnSummary")]
    UnexpectedMemberIds,
    /// A `TurnSummary` had an empty `member_event_ids`.
    #[error("turn_summary requires non-empty member_event_ids")]
    EmptyMemberIds,
}

impl TraceLink {
    /// Validate field-level invariants given the event type the link is
    /// attached to. Referential checks (parent existence, summary membership)
    /// are enforced separately in the store transaction.
    ///
    /// # Errors
    ///
    /// Returns the first violated invariant.
    pub fn validate(&self, event: TraceEvent) -> Result<(), TraceLinkError> {
        if self.turn_id.trim().is_empty() {
            return Err(TraceLinkError::EmptyTurnId);
        }
        let needs_tool_call = matches!(
            event,
            TraceEvent::PreTool | TraceEvent::PostTool | TraceEvent::ToolOutput
        );
        match (needs_tool_call, self.tool_call_id.as_ref()) {
            (true, None) => return Err(TraceLinkError::MissingToolCallId),
            (false, Some(_)) => return Err(TraceLinkError::UnexpectedToolCallId),
            _ => {}
        }
        let needs_parent = matches!(event, TraceEvent::PostTool | TraceEvent::ToolOutput);
        match (needs_parent, self.parent_event_id.as_ref()) {
            (true, None) => return Err(TraceLinkError::MissingParent),
            (false, Some(_)) => return Err(TraceLinkError::UnexpectedParent),
            _ => {}
        }
        let is_summary = event == TraceEvent::TurnSummary;
        match (is_summary, self.member_event_ids.is_empty()) {
            (true, true) => return Err(TraceLinkError::EmptyMemberIds),
            (false, false) => return Err(TraceLinkError::UnexpectedMemberIds),
            _ => {}
        }
        Ok(())
    }
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

    fn cap(id: &str) -> CaptureEventId {
        CaptureEventId::parse(id).expect("valid ulid")
    }

    fn sess() -> SessionId {
        SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid")
    }

    fn link_template() -> TraceLink {
        TraceLink {
            session_id: sess(),
            turn_id: "turn-4".into(),
            sequence: 0,
            capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            parent_event_id: None,
            tool_call_id: None,
            member_event_ids: Vec::new(),
        }
    }

    #[test]
    fn user_message_link_is_valid() {
        link_template().validate(TraceEvent::UserMessage).expect("valid");
    }

    #[test]
    fn pre_tool_requires_tool_call_id() {
        let err = link_template().validate(TraceEvent::PreTool).unwrap_err();
        assert!(matches!(err, TraceLinkError::MissingToolCallId));
    }

    #[test]
    fn pre_tool_with_tool_call_id_is_valid() {
        let mut l = link_template();
        l.tool_call_id = Some("call_abc".into());
        l.validate(TraceEvent::PreTool).expect("valid");
    }

    #[test]
    fn post_tool_requires_parent() {
        let mut l = link_template();
        l.tool_call_id = Some("call".into());
        let err = l.validate(TraceEvent::PostTool).unwrap_err();
        assert!(matches!(err, TraceLinkError::MissingParent));
        l.parent_event_id = Some(cap("01ARZ3NDEKTSV4RRFFQ69G5FBW"));
        l.validate(TraceEvent::PostTool).expect("valid");
    }

    #[test]
    fn user_message_rejects_parent() {
        let mut l = link_template();
        l.parent_event_id = Some(cap("01ARZ3NDEKTSV4RRFFQ69G5FBW"));
        assert!(matches!(
            l.validate(TraceEvent::UserMessage).unwrap_err(),
            TraceLinkError::UnexpectedParent
        ));
    }

    #[test]
    fn user_message_rejects_tool_call_id() {
        let mut l = link_template();
        l.tool_call_id = Some("call".into());
        assert!(matches!(
            l.validate(TraceEvent::UserMessage).unwrap_err(),
            TraceLinkError::UnexpectedToolCallId
        ));
    }

    #[test]
    fn member_ids_only_on_summary() {
        let mut l = link_template();
        l.member_event_ids = vec![cap("01ARZ3NDEKTSV4RRFFQ69G5FBW")];
        assert!(matches!(
            l.validate(TraceEvent::UserMessage).unwrap_err(),
            TraceLinkError::UnexpectedMemberIds
        ));
    }

    #[test]
    fn summary_requires_member_ids() {
        let l = link_template();
        assert!(matches!(
            l.validate(TraceEvent::TurnSummary).unwrap_err(),
            TraceLinkError::EmptyMemberIds
        ));
    }

    #[test]
    fn empty_turn_id_rejected() {
        let mut l = link_template();
        l.turn_id = "  ".into();
        assert!(matches!(
            l.validate(TraceEvent::UserMessage).unwrap_err(),
            TraceLinkError::EmptyTurnId
        ));
    }
}
