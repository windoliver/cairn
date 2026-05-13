//! One graph-leg candidate.

use crate::domain::RecordId;

/// One graph-leg candidate: a record reached via a neighbor entity edge.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphCandidate {
    /// Record reached via a 1-hop neighbor edge.
    pub record_id: RecordId,
    /// Confidence score of the *connecting edge* in `[0.0, 1.0]`.
    pub edge_confidence_score: f32,
    /// 1-based rank in the graph leg's SQL output order. Carried
    /// explicitly so RRF fusion does not infer rank from list position
    /// after hydration shuffles ids. See spec §5.2.1.
    pub graph_rank: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(suffix: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N00000000000000{suffix}")).expect("valid id")
    }

    #[test]
    fn construct_and_clone() {
        let c = GraphCandidate {
            record_id: rid("0A"),
            edge_confidence_score: 0.85,
            graph_rank: 3,
        };
        let c2 = c.clone();
        assert_eq!(c, c2);
    }
}
