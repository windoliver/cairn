//! Pure lint domain types and edge winner selection.

/// Lint severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Informational finding.
    Info,
    /// Potential correctness issue.
    Warning,
    /// Hard error.
    Error,
}

/// Lint finding category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LintKind {
    /// Multiple live edges share the same bitemporal identity triple.
    ContradictoryEdge,
    /// A live edge is marked ambiguous and needs human review.
    AmbiguousEdge,
}

/// Structured lint finding used internally before mapping to generated wire types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFinding {
    /// Finding kind.
    pub kind: LintKind,
    /// Severity level.
    pub severity: Severity,
    /// Affected entity or edge ids.
    pub entities: Vec<String>,
    /// Human-readable message.
    pub message: String,
    /// Optional remediation hint.
    pub suggestion: Option<String>,
}

/// Confidence source attached to an entity edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeConfidence {
    /// Human review required.
    Ambiguous,
    /// Inferred by an extractor.
    Inferred,
    /// Directly extracted from source evidence.
    Extracted,
}

impl EdgeConfidence {
    /// Parse an edge confidence wire value.
    ///
    /// # Errors
    ///
    /// Returns the unsupported value when it is not one of the known confidence
    /// literals.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "AMBIGUOUS" => Ok(Self::Ambiguous),
            "INFERRED" => Ok(Self::Inferred),
            "EXTRACTED" => Ok(Self::Extracted),
            other => Err(other.to_owned()),
        }
    }

    /// Edge confidence wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ambiguous => "AMBIGUOUS",
            Self::Inferred => "INFERRED",
            Self::Extracted => "EXTRACTED",
        }
    }
}

/// Candidate live edge in a contradiction group.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeCandidate {
    /// Edge id.
    pub id: String,
    /// Discrete confidence source.
    pub confidence: EdgeConfidence,
    /// Numeric confidence score.
    pub confidence_score: f64,
}

/// Choose the single edge to keep from a live contradiction group.
#[must_use]
pub fn choose_edge_keeper(edges: &[EdgeCandidate]) -> Option<&EdgeCandidate> {
    edges.iter().max_by(|left, right| {
        left.confidence_score
            .total_cmp(&right.confidence_score)
            .then_with(|| left.confidence.cmp(&right.confidence))
            .then_with(|| right.id.cmp(&left.id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, confidence: EdgeConfidence, confidence_score: f64) -> EdgeCandidate {
        EdgeCandidate {
            id: id.to_owned(),
            confidence,
            confidence_score,
        }
    }

    #[test]
    fn keeper_prefers_higher_confidence_score() {
        let edges = [
            candidate("edge-a", EdgeConfidence::Extracted, 0.7),
            candidate("edge-b", EdgeConfidence::Inferred, 0.9),
        ];

        assert_eq!(choose_edge_keeper(&edges).expect("keeper").id, "edge-b");
    }

    #[test]
    fn score_tie_uses_confidence_ordering() {
        let edges = [
            candidate("edge-a", EdgeConfidence::Inferred, 0.8),
            candidate("edge-b", EdgeConfidence::Extracted, 0.8),
            candidate("edge-c", EdgeConfidence::Ambiguous, 0.8),
        ];

        assert_eq!(choose_edge_keeper(&edges).expect("keeper").id, "edge-b");
    }

    #[test]
    fn final_tie_keeps_lexicographically_smaller_id() {
        let edges = [
            candidate("edge-b", EdgeConfidence::Extracted, 1.0),
            candidate("edge-a", EdgeConfidence::Extracted, 1.0),
        ];

        assert_eq!(choose_edge_keeper(&edges).expect("keeper").id, "edge-a");
    }

    #[test]
    fn parses_confidence_values_and_rejects_unsupported_values() {
        assert_eq!(
            EdgeConfidence::parse("EXTRACTED").expect("extracted"),
            EdgeConfidence::Extracted
        );
        assert_eq!(
            EdgeConfidence::parse("INFERRED").expect("inferred"),
            EdgeConfidence::Inferred
        );
        assert_eq!(
            EdgeConfidence::parse("AMBIGUOUS").expect("ambiguous"),
            EdgeConfidence::Ambiguous
        );
        assert_eq!(EdgeConfidence::Extracted.as_str(), "EXTRACTED");
        assert!(EdgeConfidence::parse("LOW").is_err());
    }
}
