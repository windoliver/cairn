//! Per-record exclusion for read verbs in `--explain` mode (brief §5.1).
//!
//! Tier-1-invisible records (caller's scope cannot see them) never
//! appear in this list; the store query filters them before
//! rank-and-filter runs. Only Tier-2 read-filter gates
//! (`ReadFilter*`) may construct an exclusion; constructing one with
//! a Tier-1 gate panics as a fail-closed safety check.

use crate::domain::TargetId;
use crate::generated::common::{
    RecordExclusion as WireRecordExclusion, RecordExclusionGate as WireRecordExclusionGate,
    Ulid as WireUlid,
};

use super::{PolicyDetail, PolicyGate};

/// Per-record exclusion entry returned on search/retrieve responses
/// when `args.explain` is true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordExclusion {
    /// Target id of the record that was filtered.
    pub target_id: TargetId,
    /// The Tier-2 read-filter gate that excluded the record.
    pub gate: PolicyGate,
    /// Body-free metadata for the exclusion.
    pub detail: PolicyDetail,
}

impl RecordExclusion {
    /// Construct an exclusion. Panics on a non-`ReadFilter*` gate;
    /// this is a programmer error and a fail-closed safety check
    /// against leaking record ids through Tier-1 gates.
    ///
    /// Returns `Self` (not `Result`) by design: a wrong gate is an
    /// internal bug, not user input, and pushing the panic-or-ignore
    /// decision up to every caller would defeat the fail-closed
    /// guarantee.
    #[must_use]
    pub fn new(target_id: TargetId, gate: PolicyGate, detail: PolicyDetail) -> Self {
        assert!(
            matches!(
                gate,
                PolicyGate::ReadFilterRelevance
                    | PolicyGate::ReadFilterStaleness
                    | PolicyGate::ReadFilterDedup
            ),
            "RecordExclusion only accepts ReadFilter* gates; got {gate:?}"
        );
        Self {
            target_id,
            gate,
            detail,
        }
    }
}

impl From<&RecordExclusion> for WireRecordExclusion {
    fn from(src: &RecordExclusion) -> Self {
        let gate = match src.gate {
            PolicyGate::ReadFilterRelevance => WireRecordExclusionGate::ReadFilterRelevance,
            PolicyGate::ReadFilterStaleness => WireRecordExclusionGate::ReadFilterStaleness,
            PolicyGate::ReadFilterDedup => WireRecordExclusionGate::ReadFilterDedup,
            other => {
                unreachable!("RecordExclusion::new rejects non-ReadFilter* gates; got {other:?}")
            }
        };
        Self {
            target_id: WireUlid(src.target_id.to_string()),
            gate,
            detail: src.detail.to_wire_string(),
        }
    }
}

/// Map a slice of producer-side exclusions into the wire shape used by
/// `SearchData.excluded`. Body-free: each `detail` is the stable code
/// from [`PolicyDetail::to_wire_string`].
#[must_use]
pub fn to_wire_exclusions(items: &[RecordExclusion]) -> Vec<WireRecordExclusion> {
    items.iter().map(WireRecordExclusion::from).collect()
}
