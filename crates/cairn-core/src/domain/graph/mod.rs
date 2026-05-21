//! Bitemporal knowledge-graph domain types (brief §3, §4).

pub mod normalize;

pub use normalize::normalize_entity_name;

use std::fmt;

use crate::domain::record::RecordId;

/// Stable ULID for an entity node. Distinct from [`RecordId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityId(String);

impl EntityId {
    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for EntityId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for EntityId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable ULID for an entity edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityEdgeId(String);

impl EntityEdgeId {
    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for EntityEdgeId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for EntityEdgeId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for EntityEdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Confidence tier on an extracted edge (Graphify model, brief §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EdgeConfidence {
    /// Directly present in the source. Score = 1.0.
    Extracted,
    /// Reasonable LLM inference. Score 0.6–0.9.
    Inferred,
    /// Uncertain; flagged for `lint` review. Score 0.1–0.3.
    Ambiguous,
}

impl EdgeConfidence {
    /// SQL-friendly representation written to the `entity_edges.confidence` column.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Extracted => "EXTRACTED",
            Self::Inferred => "INFERRED",
            Self::Ambiguous => "AMBIGUOUS",
        }
    }

    /// Inverse of [`EdgeConfidence::as_db_str`]. Returns `None` for unrecognized
    /// strings — caller decides whether to treat as a corrupt-row error.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "EXTRACTED" => Some(Self::Extracted),
            "INFERRED" => Some(Self::Inferred),
            "AMBIGUOUS" => Some(Self::Ambiguous),
            _ => None,
        }
    }
}

/// An entity node in the bitemporal graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityNode {
    /// Stable ULID identity.
    pub id: EntityId,
    /// Display name as extracted.
    pub name: String,
    /// Lowercase, punctuation-stripped name used for dedup. UNIQUE in storage.
    pub name_norm: String,
    /// Optional one-line summary.
    pub summary: Option<String>,
    /// Ingestion-time start (unix ms).
    pub created_at: i64,
    /// Optional vector-table FK (nullable; embedder lands in a follow-up).
    pub embedding_id: Option<String>,
}

/// A bitemporal directed edge between two entities.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityEdge {
    /// Stable ULID identity.
    pub id: EntityEdgeId,
    /// Source entity.
    pub source_id: EntityId,
    /// Target entity.
    pub target_id: EntityId,
    /// Relation label, e.g. `works_at`, `lives_in`.
    pub relation: String,
    /// Provenance tier.
    pub confidence: EdgeConfidence,
    /// Score in [0.0, 1.0]. Tier-specific bands enforced by callers; the
    /// store only enforces the [0.0, 1.0] CHECK constraint.
    pub confidence_score: f32,
    /// Event-time start (unix ms).
    pub valid_at: i64,
    /// Event-time end; `None` = currently valid.
    pub invalid_at: Option<i64>,
    /// Ingestion-time start (unix ms).
    pub created_at: i64,
    /// Optional record this fact was extracted from.
    /// FK is `ON DELETE SET NULL`: orphaned edges survive record purge.
    pub source_record_id: Option<RecordId>,
}

/// Args for [`crate::contract::memory_store::MemoryStore::graph_edges`].
#[derive(Debug, Clone)]
pub struct GraphEdgesArgs<'a> {
    /// The pivot node.
    pub node_id: &'a EntityId,
    /// Edge-direction selector (reuses the existing record-edge enum).
    pub direction: crate::contract::memory_store::EdgeDir,
    /// Optional relation filter.
    pub relation_filter: Option<&'a str>,
    /// Bitemporal slice on event-time (unix ms). When `Some(t)`, returns
    /// edges with `valid_at <= t AND (invalid_at IS NULL OR invalid_at > t)`.
    pub as_of_event_time: Option<i64>,
    /// Bitemporal slice on ingestion-time (unix ms). When `Some(t)`, returns
    /// edges with `created_at <= t AND (expired_at IS NULL OR expired_at > t)`.
    pub as_of_ingest_time: Option<i64>,
    /// When true, return invalidated rows too (history view).
    pub include_invalidated: bool,
}

/// Outcome of an edge upsert / contradiction resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityEdgeOutcome {
    /// Id of the edge now considered live for the (source, target, relation).
    /// On idempotent re-upsert this is the existing row's id.
    pub new_edge_id: EntityEdgeId,
    /// Some(id) when an old edge was invalidated by this op; None otherwise.
    pub invalidated_edge_id: Option<EntityEdgeId>,
    /// True when the upsert was a no-op because the body was identical.
    pub body_was_unchanged: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_round_trips_via_string() {
        let raw = "01HZE7JV5N0000000000000000";
        let id = EntityId::from(raw);
        assert_eq!(id.as_str(), raw);
        assert_eq!(format!("{id}"), raw);
    }

    #[test]
    fn edge_confidence_db_string_round_trip() {
        for c in [
            EdgeConfidence::Extracted,
            EdgeConfidence::Inferred,
            EdgeConfidence::Ambiguous,
        ] {
            let s = c.as_db_str();
            assert_eq!(EdgeConfidence::from_db_str(s), Some(c));
        }
        assert_eq!(EdgeConfidence::from_db_str("unknown"), None);
    }

    #[test]
    fn graph_edges_args_defaults_are_sensible() {
        let id = EntityId::from("01HZE7JV5N0000000000000001");
        let args = GraphEdgesArgs {
            node_id: &id,
            direction: crate::contract::memory_store::EdgeDir::Both,
            relation_filter: None,
            as_of_event_time: None,
            as_of_ingest_time: None,
            include_invalidated: false,
        };
        assert!(args.relation_filter.is_none());
        assert!(!args.include_invalidated);
    }
}
