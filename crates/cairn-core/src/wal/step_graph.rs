//! Static step-graph definitions for P0 WAL operation kinds (brief §5.6).
//!
//! Step names mirror the brief §5.6 fan-out tables. They are written into
//! `wal_steps.step_kind` and are wire-stable: renaming a step requires a
//! schema migration to map old names forward.

/// P0 mutation kinds whose step graphs this module defines. Marked
/// `#[non_exhaustive]` so `Promote`, `ForgetSession`, `Evolve`, and the
/// graph kinds can be added later without breaking matchers.
///
/// The existing `lint_repair` kind (in
/// `cairn-store-sqlite/src/wal/lint_repair.rs`) is intentionally not part
/// of this enum yet — its migration onto the scaffold is a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WalKind {
    /// `upsert` — create or update a record (brief §5.6 fan-out table row 1).
    Upsert,
    /// `forget_record` — record-level tombstone + purge (brief §5.6 row 2).
    ForgetRecord,
    /// `expire` — soft-expire (brief §5.6 row 5).
    Expire,
}

impl WalKind {
    /// Wire form used by `wal_ops.kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::ForgetRecord => "forget_record",
            Self::Expire => "expire",
        }
    }
}

/// One step in a fan-out graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepDef {
    /// 0-based ordinal; matches `wal_steps.step_ord`.
    pub ord: u32,
    /// Stable name; matches `wal_steps.step_kind`.
    pub name: &'static str,
    /// `true` ⇒ step body may be invoked more than once with the same args
    /// without producing duplicate effects (brief §5.6 `[idem]` marker).
    /// `false` ⇒ step body must run exactly once. Recovery never re-invokes
    /// a non-idempotent step that was already marked `Done`.
    pub idempotent: bool,
}

/// A complete step graph for one [`WalKind`].
#[derive(Debug, Clone, Copy)]
pub struct StepGraph {
    /// The kind this graph applies to.
    pub kind: WalKind,
    /// Steps in execution order. Indexed by `ord` matches the slice index.
    pub steps: &'static [StepDef],
}

impl StepGraph {
    /// Returns the step at `ord`, or `None` if out of range.
    #[must_use]
    pub fn step(&self, ord: u32) -> Option<&StepDef> {
        self.steps.get(ord as usize)
    }

    /// Number of steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// `true` if there are no steps.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Step graph for `upsert` — brief §5.6 fan-out table row 1.
///
/// Step 7 (`consent_log_materializer`) is async and not part of the WAL
/// step graph per the brief; it lives in a separate background tail.
pub const UPSERT_STEPS: &[StepDef] = &[
    StepDef {
        ord: 0,
        name: "snapshot.stage",
        idempotent: false,
    },
    StepDef {
        ord: 1,
        name: "primary.upsert_cow",
        idempotent: true,
    },
    StepDef {
        ord: 2,
        name: "vector.upsert",
        idempotent: true,
    },
    StepDef {
        ord: 3,
        name: "fts.upsert",
        idempotent: true,
    },
    StepDef {
        ord: 4,
        name: "edges.upsert",
        idempotent: true,
    },
    StepDef {
        ord: 5,
        name: "primary.activate",
        idempotent: true,
    },
];

const UPSERT_GRAPH: StepGraph = StepGraph {
    kind: WalKind::Upsert,
    steps: UPSERT_STEPS,
};

/// Step graph for `forget_record` — brief §5.6 fan-out table row 2.
pub const FORGET_RECORD_STEPS: &[StepDef] = &[
    StepDef {
        ord: 0,
        name: "primary.mark_tombstone",
        idempotent: true,
    },
    StepDef {
        ord: 1,
        name: "vector.drain",
        idempotent: true,
    },
    StepDef {
        ord: 2,
        name: "fts.drain",
        idempotent: true,
    },
    StepDef {
        ord: 3,
        name: "edges.drain",
        idempotent: true,
    },
    StepDef {
        ord: 4,
        name: "primary.purge",
        idempotent: false,
    },
    StepDef {
        ord: 5,
        name: "wal.purge_pre_images",
        idempotent: true,
    },
    StepDef {
        ord: 6,
        name: "snapshot.purge",
        idempotent: true,
    },
];

const FORGET_RECORD_GRAPH: StepGraph = StepGraph {
    kind: WalKind::ForgetRecord,
    steps: FORGET_RECORD_STEPS,
};

/// Step graph for `expire` — brief §5.6 fan-out table row 5.
///
/// Brief §5.6 lists `consent_journal.append(expire)` as step 6, but the
/// brief also specifies it commits "atomic with step 2 in one `SQLite`
/// transaction" — i.e. fused into the `primary.mark_expired` step body.
/// It is therefore not a separate WAL step; the same fusion treatment is
/// applied to `upsert`'s consent journal append (see `UPSERT_STEPS` doc).
pub const EXPIRE_STEPS: &[StepDef] = &[
    StepDef {
        ord: 0,
        name: "snapshot.stage",
        idempotent: false,
    },
    StepDef {
        ord: 1,
        name: "primary.mark_expired",
        idempotent: true,
    },
    StepDef {
        ord: 2,
        name: "vector.drain",
        idempotent: true,
    },
    StepDef {
        ord: 3,
        name: "fts.drain",
        idempotent: true,
    },
    StepDef {
        ord: 4,
        name: "edges.drain",
        idempotent: true,
    },
];

const EXPIRE_GRAPH: StepGraph = StepGraph {
    kind: WalKind::Expire,
    steps: EXPIRE_STEPS,
};

/// Resolves a [`WalKind`] to its static step graph.
#[must_use]
pub fn graph_for(kind: WalKind) -> &'static StepGraph {
    match kind {
        WalKind::Upsert => &UPSERT_GRAPH,
        WalKind::ForgetRecord => &FORGET_RECORD_GRAPH,
        WalKind::Expire => &EXPIRE_GRAPH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_ords_match_slice_indices() {
        for graph in [&UPSERT_GRAPH, &FORGET_RECORD_GRAPH, &EXPIRE_GRAPH] {
            for (idx, step) in graph.steps.iter().enumerate() {
                assert_eq!(
                    step.ord as usize, idx,
                    "{:?} step {idx} has ord {}",
                    graph.kind, step.ord
                );
            }
        }
    }

    #[test]
    fn step_names_unique_within_graph() {
        for graph in [&UPSERT_GRAPH, &FORGET_RECORD_GRAPH, &EXPIRE_GRAPH] {
            let mut seen: Vec<&str> = Vec::with_capacity(graph.steps.len());
            for step in graph.steps {
                assert!(
                    !seen.contains(&step.name),
                    "{:?} has duplicate step name {}",
                    graph.kind,
                    step.name
                );
                seen.push(step.name);
            }
        }
    }

    #[test]
    fn graph_for_returns_kind_self() {
        assert_eq!(graph_for(WalKind::Upsert).kind, WalKind::Upsert);
        assert_eq!(graph_for(WalKind::ForgetRecord).kind, WalKind::ForgetRecord);
        assert_eq!(graph_for(WalKind::Expire).kind, WalKind::Expire);
    }

    #[test]
    fn graph_step_lookup() {
        let g = graph_for(WalKind::Upsert);
        assert_eq!(g.step(0).map(|s| s.name), Some("snapshot.stage"));
        assert_eq!(g.step(5).map(|s| s.name), Some("primary.activate"));
        assert_eq!(g.step(99), None);
    }

    #[test]
    fn graph_lengths() {
        assert_eq!(graph_for(WalKind::Upsert).len(), 6);
        assert_eq!(graph_for(WalKind::ForgetRecord).len(), 7);
        assert_eq!(graph_for(WalKind::Expire).len(), 5);
    }

    #[test]
    fn wire_form_matches_schema_check() {
        // wal_ops.kind CHECK in 0002_wal.sql widened by 0041_wal_kind_widening.sql
        // includes 'upsert', 'forget_record', 'expire'.
        assert_eq!(WalKind::Upsert.as_str(), "upsert");
        assert_eq!(WalKind::ForgetRecord.as_str(), "forget_record");
        assert_eq!(WalKind::Expire.as_str(), "expire");
    }
}
