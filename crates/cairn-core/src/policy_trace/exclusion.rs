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
///
/// Fields are private to seal the Tier-2-only invariant: the only
/// constructor [`Self::new`] rejects non-`ReadFilter*` gates. External
/// callers cannot bypass it via `RecordExclusion { gate: ScopeCheck, … }`
/// because the field-literal form is unavailable outside the module.
/// Read-only accessors expose the data after construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordExclusion {
    target_id: TargetId,
    gate: PolicyGate,
    detail: PolicyDetail,
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

    /// Borrow the target id of the filtered record.
    #[must_use]
    pub fn target_id(&self) -> &TargetId {
        &self.target_id
    }

    /// The Tier-2 read-filter gate that excluded the record. Always
    /// one of `ReadFilterRelevance` / `ReadFilterStaleness` /
    /// `ReadFilterDedup`; the constructor enforces this.
    #[must_use]
    pub const fn gate(&self) -> PolicyGate {
        self.gate
    }

    /// Body-free metadata describing the exclusion.
    #[must_use]
    pub const fn detail(&self) -> &PolicyDetail {
        &self.detail
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
