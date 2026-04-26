//! Memory taxonomy enums (brief §6).
//!
//! Every record carries four orthogonal tags: `kind × class × visibility ×
//! scope`. Three of them — `kind`, `class`, `visibility` — are closed
//! enums; `scope` is a tuple ([`crate::domain::ScopeTuple`]).

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// 19 memory kinds (§6.1). Wire form is the lower-snake-case variant name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryKind {
    /// Stable preference or trait of the user.
    User,
    /// Explicit corrective signal from the user.
    Feedback,
    /// Active project or initiative the user is working on.
    Project,
    /// External document, ticket, or URL referenced by the agent.
    Reference,
    /// Verifiable factual claim with a citation.
    Fact,
    /// Belief held with less than fact-grade evidence.
    Belief,
    /// Subjective opinion of the user or agent.
    Opinion,
    /// Discrete event with a timestamp (meeting, ship, incident).
    Event,
    /// Named entity (person, org, system).
    Entity,
    /// Reusable workflow or process artifact.
    Workflow,
    /// Hard rule or constraint that gates behavior.
    Rule,
    /// Successful agent strategy worth replicating.
    StrategySuccess,
    /// Failed agent strategy worth avoiding.
    StrategyFailure,
    /// Tool calls / tool results / timeline of what happened.
    Trace,
    /// Decision rationale, alternatives considered, heuristics applied.
    Reasoning,
    /// Curated playbook ("how to do X end-to-end").
    Playbook,
    /// Raw observation from a sensor.
    SensorObservation,
    /// Implicit user behavior signal (clicks, dwell, undo).
    UserSignal,
    /// Question the agent could not answer — drives eval generation.
    KnowledgeGap,
}

impl MemoryKind {
    /// Wire-format identifier (lower-snake-case). Stable across surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
            Self::Fact => "fact",
            Self::Belief => "belief",
            Self::Opinion => "opinion",
            Self::Event => "event",
            Self::Entity => "entity",
            Self::Workflow => "workflow",
            Self::Rule => "rule",
            Self::StrategySuccess => "strategy_success",
            Self::StrategyFailure => "strategy_failure",
            Self::Trace => "trace",
            Self::Reasoning => "reasoning",
            Self::Playbook => "playbook",
            Self::SensorObservation => "sensor_observation",
            Self::UserSignal => "user_signal",
            Self::KnowledgeGap => "knowledge_gap",
        }
    }
}

/// 4 memory classes (§6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryClass {
    /// Timed events and traces.
    Episodic,
    /// Stable facts and entities.
    Semantic,
    /// How-to / playbook / strategy memories.
    Procedural,
    /// Relationships, edges, backlinks.
    Graph,
}

/// 6 visibility tiers (§6.3). Order matters: lower index = more private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MemoryVisibility {
    /// Default; never leaves the vault without explicit promotion.
    Private,
    /// Reachable by any turn in the current session.
    Session,
    /// Within one project tree; agents can propose, human signs off.
    Project,
    /// Small-group knowledge; one human approval to enter this tier.
    Team,
    /// Cross-team; two human approvals required.
    Org,
    /// Opt-in only; three human approvals required.
    Public,
}

impl MemoryVisibility {
    /// Parse a wire-form tier string. Returns
    /// [`DomainError::UnsupportedVisibility`] for unknown values.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "private" => Ok(Self::Private),
            "session" => Ok(Self::Session),
            "project" => Ok(Self::Project),
            "team" => Ok(Self::Team),
            "org" => Ok(Self::Org),
            "public" => Ok(Self::Public),
            other => Err(DomainError::UnsupportedVisibility {
                value: other.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_through_snake_case() {
        let json = serde_json::to_string(&MemoryKind::StrategyFailure).expect("serialize");
        assert_eq!(json, "\"strategy_failure\"");
        let back: MemoryKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, MemoryKind::StrategyFailure);
    }

    #[test]
    fn visibility_parse_known() {
        assert_eq!(
            MemoryVisibility::parse("project").expect("known"),
            MemoryVisibility::Project
        );
    }

    #[test]
    fn visibility_parse_rejects_unknown() {
        let err = MemoryVisibility::parse("internal").unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedVisibility { .. }));
    }

    #[test]
    fn visibility_orders_low_to_high() {
        assert!(MemoryVisibility::Private < MemoryVisibility::Public);
        assert!(MemoryVisibility::Project < MemoryVisibility::Team);
    }

    #[test]
    fn class_round_trips() {
        let json = serde_json::to_string(&MemoryClass::Procedural).expect("serialize");
        assert_eq!(json, "\"procedural\"");
    }
}
