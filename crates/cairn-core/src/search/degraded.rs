//! Typed degradation signal for hybrid search responses.
//!
//! When a leg of hybrid search fails (capability missing, deadline exceeded,
//! SQL error, worker panic) the orchestrator returns the surviving legs and
//! flags the failed ones via [`DegradedLeg`]. Callers (CLI, MCP, SDK) decide
//! whether to surface a warning, retry, or fail-closed.

/// Why a hybrid-search leg did not contribute results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DegradationReason {
    /// Store does not advertise the capability needed by this leg.
    CapabilityUnavailable,
    /// External semantic embedding provider had a transient outage.
    TransientProviderOutage,
    /// Per-leg deadline elapsed before results landed.
    DeadlineExceeded,
    /// `SQLite` returned an error.
    SqlError,
    /// The leg's worker task panicked.
    WorkerPanic,
}

/// Which seed source the graph leg was using when it degraded.
///
/// Only meaningful for [`DegradedLeg::Graph`]. Lets callers attribute a
/// graph-leg failure to the auth-only keyword seed query, the auth-only
/// semantic seed query, or the union ("all").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphSource {
    /// Both seed paths failed (or no seeds were attempted).
    All,
    /// Auth-only keyword-seed retrieval was the in-flight source when the
    /// leg degraded.
    AuthKeywordSeed,
    /// Auth-only semantic-seed retrieval was the in-flight source.
    AuthSemanticSeed,
}

/// One leg of hybrid search that did not contribute results.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DegradedLeg {
    /// Semantic (vector-search) leg degraded.
    Semantic {
        /// Why the leg degraded.
        reason: DegradationReason,
    },
    /// 1-hop entity-graph expansion leg degraded.
    Graph {
        /// Why the leg degraded.
        reason: DegradationReason,
        /// Which seed path was in flight when the leg degraded.
        source: GraphSource,
    },
}

impl DegradedLeg {
    /// Convenience constructor for the most common graph-leg degradation:
    /// the store does not advertise `graph_search` capability at all.
    #[must_use]
    pub fn graph_capability_unavailable() -> Self {
        Self::Graph {
            reason: DegradationReason::CapabilityUnavailable,
            source: GraphSource::All,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_constructor_helper() {
        let d = DegradedLeg::graph_capability_unavailable();
        assert!(matches!(
            d,
            DegradedLeg::Graph {
                reason: DegradationReason::CapabilityUnavailable,
                source: GraphSource::All
            }
        ));
    }

    #[test]
    fn semantic_variant_carries_reason() {
        let d = DegradedLeg::Semantic {
            reason: DegradationReason::DeadlineExceeded,
        };
        assert!(matches!(
            d,
            DegradedLeg::Semantic {
                reason: DegradationReason::DeadlineExceeded
            }
        ));
    }
}
