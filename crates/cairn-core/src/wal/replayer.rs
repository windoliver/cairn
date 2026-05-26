//! Pure WAL step-graph iterator. Given a [`StepGraph`] and a starting
//! ordinal, produces the ordered sequence of [`StepEvent`]s a replayer
//! would visit.
//!
//! The verb-layer driver (`verbs::admin::replay_wal`) wraps this with
//! the operator-role check, optional apply-mode escalation, and the
//! `AdminError` mapping. This module is the I/O-free primitive.

use crate::wal::{StepDef, StepGraph};

/// One event emitted by [`iter_from`]. Mirrors [`StepDef`] plus an explicit
/// `ord` field so callers don't have to re-derive it from slice position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepEvent {
    /// 0-based ordinal matching `wal_steps.step_ord`.
    pub ord: u32,
    /// Stable name matching `wal_steps.step_kind`.
    pub name: &'static str,
    /// Whether the step body may be invoked more than once safely.
    pub idempotent: bool,
}

impl From<&StepDef> for StepEvent {
    fn from(s: &StepDef) -> Self {
        Self {
            ord: s.ord,
            name: s.name,
            idempotent: s.idempotent,
        }
    }
}

/// Errors specific to the replayer primitive. The verb layer maps these
/// to `AdminError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The requested starting ordinal is beyond the last step in the graph.
    #[error("step ord {requested} is out of range; graph has {len} steps")]
    OrdOutOfRange {
        /// The ordinal that was requested.
        requested: u32,
        /// The number of steps in the graph.
        len: u32,
    },
}

/// Walk `graph` from `from_ord` (inclusive) to the end. Pure — no I/O.
///
/// # Errors
///
/// Returns [`ReplayError::OrdOutOfRange`] if `from_ord >= graph.len()`.
/// `from_ord == graph.len()` is considered out-of-range (no steps remain);
/// use `from_ord == graph.len() - 1` to replay only the last step.
pub fn iter_from(
    graph: &StepGraph,
    from_ord: u32,
) -> Result<impl Iterator<Item = StepEvent> + '_, ReplayError> {
    let len = u32::try_from(graph.len()).unwrap_or(u32::MAX);
    if from_ord >= len {
        return Err(ReplayError::OrdOutOfRange {
            requested: from_ord,
            len,
        });
    }
    let start = from_ord as usize;
    Ok(graph.steps[start..].iter().map(StepEvent::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::{WalKind, graph_for};

    #[test]
    fn iter_from_zero_walks_every_step() {
        let g = graph_for(WalKind::Upsert);
        let events: Vec<_> = iter_from(g, 0).expect("iter").collect();
        assert_eq!(events.len(), g.len());
        for (i, ev) in events.iter().enumerate() {
            assert_eq!(ev.ord as usize, i);
        }
    }

    #[test]
    fn iter_from_middle_skips_earlier_steps() {
        let g = graph_for(WalKind::Upsert);
        let events: Vec<_> = iter_from(g, 2).expect("iter").collect();
        assert_eq!(events.len(), g.len() - 2);
        assert_eq!(events[0].ord, 2);
    }

    #[test]
    fn iter_from_last_step_yields_one() {
        let g = graph_for(WalKind::Upsert);
        let last = u32::try_from(g.len()).unwrap() - 1;
        let events: Vec<_> = iter_from(g, last).expect("iter").collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].ord, last);
    }

    #[test]
    fn iter_from_past_end_errors() {
        let g = graph_for(WalKind::Upsert);
        let past = u32::try_from(g.len()).unwrap();
        let result = iter_from(g, past);
        assert!(
            matches!(result, Err(ReplayError::OrdOutOfRange { .. })),
            "expected OrdOutOfRange error"
        );
    }

    #[test]
    fn idempotent_flag_propagates() {
        // Assert every event mirrors the underlying StepDef exactly.
        let g = graph_for(WalKind::ForgetRecord);
        let events: Vec<_> = iter_from(g, 0).expect("iter").collect();
        for (ev, def) in events.iter().zip(g.steps.iter()) {
            assert_eq!(ev.idempotent, def.idempotent);
            assert_eq!(ev.name, def.name);
        }
    }
}
