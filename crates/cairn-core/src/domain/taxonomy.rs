//! Memory taxonomy enums (brief §6).
//!
//! Every record carries four orthogonal tags: `kind × class × visibility ×
//! scope`. Three of them — `kind`, `class`, `visibility` — are closed
//! enums; `scope` is a tuple ([`crate::domain::ScopeTuple`]).

use std::fmt;

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

    /// Parse a wire-form kind string. Returns
    /// [`DomainError::UnsupportedKind`] for unknown values so classifiers
    /// cannot invent arbitrary kinds.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "user" => Ok(Self::User),
            "feedback" => Ok(Self::Feedback),
            "project" => Ok(Self::Project),
            "reference" => Ok(Self::Reference),
            "fact" => Ok(Self::Fact),
            "belief" => Ok(Self::Belief),
            "opinion" => Ok(Self::Opinion),
            "event" => Ok(Self::Event),
            "entity" => Ok(Self::Entity),
            "workflow" => Ok(Self::Workflow),
            "rule" => Ok(Self::Rule),
            "strategy_success" => Ok(Self::StrategySuccess),
            "strategy_failure" => Ok(Self::StrategyFailure),
            "trace" => Ok(Self::Trace),
            "reasoning" => Ok(Self::Reasoning),
            "playbook" => Ok(Self::Playbook),
            "sensor_observation" => Ok(Self::SensorObservation),
            "user_signal" => Ok(Self::UserSignal),
            "knowledge_gap" => Ok(Self::KnowledgeGap),
            other => Err(DomainError::UnsupportedKind {
                value: other.to_owned(),
            }),
        }
    }
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

impl MemoryClass {
    /// Wire-format identifier (lower-snake-case). Stable across surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
            Self::Graph => "graph",
        }
    }

    /// Parse a wire-form class string. Returns
    /// [`DomainError::UnsupportedClass`] for unknown values.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "episodic" => Ok(Self::Episodic),
            "semantic" => Ok(Self::Semantic),
            "procedural" => Ok(Self::Procedural),
            "graph" => Ok(Self::Graph),
            other => Err(DomainError::UnsupportedClass {
                value: other.to_owned(),
            }),
        }
    }
}

impl fmt::Display for MemoryClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
    /// Wire-format identifier (lower-snake-case). Stable across surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Session => "session",
            Self::Project => "project",
            Self::Team => "team",
            Self::Org => "org",
            Self::Public => "public",
        }
    }

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

impl fmt::Display for MemoryVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    fn kind_parse_all_19_valid() {
        let all = [
            ("user", MemoryKind::User),
            ("feedback", MemoryKind::Feedback),
            ("project", MemoryKind::Project),
            ("reference", MemoryKind::Reference),
            ("fact", MemoryKind::Fact),
            ("belief", MemoryKind::Belief),
            ("opinion", MemoryKind::Opinion),
            ("event", MemoryKind::Event),
            ("entity", MemoryKind::Entity),
            ("workflow", MemoryKind::Workflow),
            ("rule", MemoryKind::Rule),
            ("strategy_success", MemoryKind::StrategySuccess),
            ("strategy_failure", MemoryKind::StrategyFailure),
            ("trace", MemoryKind::Trace),
            ("reasoning", MemoryKind::Reasoning),
            ("playbook", MemoryKind::Playbook),
            ("sensor_observation", MemoryKind::SensorObservation),
            ("user_signal", MemoryKind::UserSignal),
            ("knowledge_gap", MemoryKind::KnowledgeGap),
        ];
        for (wire, expected) in all {
            assert_eq!(MemoryKind::parse(wire).expect(wire), expected);
        }
    }

    #[test]
    fn kind_as_str_parse_round_trip() {
        let all = [
            MemoryKind::User,
            MemoryKind::Feedback,
            MemoryKind::Project,
            MemoryKind::Reference,
            MemoryKind::Fact,
            MemoryKind::Belief,
            MemoryKind::Opinion,
            MemoryKind::Event,
            MemoryKind::Entity,
            MemoryKind::Workflow,
            MemoryKind::Rule,
            MemoryKind::StrategySuccess,
            MemoryKind::StrategyFailure,
            MemoryKind::Trace,
            MemoryKind::Reasoning,
            MemoryKind::Playbook,
            MemoryKind::SensorObservation,
            MemoryKind::UserSignal,
            MemoryKind::KnowledgeGap,
        ];
        for kind in all {
            assert_eq!(
                MemoryKind::parse(kind.as_str()).unwrap_or_else(|_| panic!("{}", kind.as_str())),
                kind
            );
        }
    }

    #[test]
    fn kind_parse_rejects_invented() {
        let err = MemoryKind::parse("invented_kind").unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedKind { .. }));
    }

    #[test]
    fn kind_display_matches_wire_form() {
        assert_eq!(format!("{}", MemoryKind::KnowledgeGap), "knowledge_gap");
        assert_eq!(
            format!("{}", MemoryKind::StrategySuccess),
            "strategy_success"
        );
        assert_eq!(format!("{}", MemoryKind::User), "user");
    }

    #[test]
    fn class_as_str_all_4_values() {
        assert_eq!(MemoryClass::Episodic.as_str(), "episodic");
        assert_eq!(MemoryClass::Semantic.as_str(), "semantic");
        assert_eq!(MemoryClass::Procedural.as_str(), "procedural");
        assert_eq!(MemoryClass::Graph.as_str(), "graph");
    }

    #[test]
    fn class_parse_all_4_valid() {
        assert_eq!(
            MemoryClass::parse("episodic").expect("episodic"),
            MemoryClass::Episodic
        );
        assert_eq!(
            MemoryClass::parse("semantic").expect("semantic"),
            MemoryClass::Semantic
        );
        assert_eq!(
            MemoryClass::parse("procedural").expect("procedural"),
            MemoryClass::Procedural
        );
        assert_eq!(
            MemoryClass::parse("graph").expect("graph"),
            MemoryClass::Graph
        );
    }

    #[test]
    fn class_parse_rejects_invented() {
        let err = MemoryClass::parse("invented_class").unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedClass { .. }));
    }

    #[test]
    fn class_as_str_parse_round_trip() {
        for class in [
            MemoryClass::Episodic,
            MemoryClass::Semantic,
            MemoryClass::Procedural,
            MemoryClass::Graph,
        ] {
            assert_eq!(
                MemoryClass::parse(class.as_str()).unwrap_or_else(|_| panic!("{}", class.as_str())),
                class
            );
        }
    }

    #[test]
    fn class_display_matches_wire_form() {
        assert_eq!(format!("{}", MemoryClass::Procedural), "procedural");
        assert_eq!(format!("{}", MemoryClass::Graph), "graph");
    }

    #[test]
    fn class_round_trips() {
        let json = serde_json::to_string(&MemoryClass::Procedural).expect("serialize");
        assert_eq!(json, "\"procedural\"");
    }

    #[test]
    fn visibility_parse_known() {
        assert_eq!(
            MemoryVisibility::parse("project").expect("known"),
            MemoryVisibility::Project
        );
    }

    #[test]
    fn visibility_parse_all_6_tiers() {
        let all = [
            ("private", MemoryVisibility::Private),
            ("session", MemoryVisibility::Session),
            ("project", MemoryVisibility::Project),
            ("team", MemoryVisibility::Team),
            ("org", MemoryVisibility::Org),
            ("public", MemoryVisibility::Public),
        ];
        for (wire, expected) in all {
            assert_eq!(MemoryVisibility::parse(wire).expect(wire), expected);
        }
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
    fn visibility_as_str_all_6_tiers() {
        assert_eq!(MemoryVisibility::Private.as_str(), "private");
        assert_eq!(MemoryVisibility::Session.as_str(), "session");
        assert_eq!(MemoryVisibility::Project.as_str(), "project");
        assert_eq!(MemoryVisibility::Team.as_str(), "team");
        assert_eq!(MemoryVisibility::Org.as_str(), "org");
        assert_eq!(MemoryVisibility::Public.as_str(), "public");
    }

    #[test]
    fn visibility_as_str_parse_round_trip() {
        for vis in [
            MemoryVisibility::Private,
            MemoryVisibility::Session,
            MemoryVisibility::Project,
            MemoryVisibility::Team,
            MemoryVisibility::Org,
            MemoryVisibility::Public,
        ] {
            assert_eq!(
                MemoryVisibility::parse(vis.as_str())
                    .unwrap_or_else(|_| panic!("{}", vis.as_str())),
                vis
            );
        }
    }

    #[test]
    fn visibility_display_matches_wire_form() {
        assert_eq!(format!("{}", MemoryVisibility::Org), "org");
        assert_eq!(format!("{}", MemoryVisibility::Public), "public");
        assert_eq!(format!("{}", MemoryVisibility::Private), "private");
    }
}
