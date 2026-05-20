//! Inclusion / exclusion trace types for `assemble_hot` (issue #82).
//!
//! Surface the typed reason a record either entered the assembled hot
//! prefix or was filtered out. Adapter callers gate this behind a
//! per-call `include_debug` flag so production callers do not pay for
//! the bookkeeping when they do not need it.

use crate::domain::RecordId;

/// Why a candidate record did not enter the assembled hot prefix.
///
/// Wire form: `snake_case` strings matching `HotExclusion.reason`
/// in `cairn-idl/schema/verbs/assemble_hot.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExclusionReason {
    /// The record's `tombstoned` flag was set.
    ///
    /// Reserved for the store adapter (issue #80): `MemoryRecord` does
    /// not carry a `tombstoned` field, so core never emits this
    /// variant. The store layer filters tombstoned rows at the query
    /// boundary; this reason is the wire shape it will use when it
    /// does report them.
    Tombstoned,
    /// The record sat under a forget-state-scope.
    ///
    /// Reserved for the store adapter (issue #80). Core does not
    /// inspect the forget-state journal; the variant exists so the
    /// adapter can populate the debug trace with a typed reason.
    ForgottenScope,
    /// `record.confidence < 0.3` (matches the profile synthesizer floor).
    BelowConfidenceFloor,
    /// The record's `scope` is not satisfied by the caller's authorized scope.
    OutOfScope,
    /// The record's `visibility` is not in `authorized_visibility`.
    VisibilityDenied,
    /// The record's `updated_at` is older than the source's recency window.
    OutsideRecencyWindow,
    /// The record was admissible but ranked below the source's top-K cap.
    BeyondTopK,
    /// The record did not match the source's pin / kind narrowing.
    NotPinned,
    /// `record.body` was empty.
    EmptyBody,
}

impl ExclusionReason {
    /// Wire-form name as exported by the IDL `HotExclusion.reason` enum.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Tombstoned => "tombstoned",
            Self::ForgottenScope => "forgotten_scope",
            Self::BelowConfidenceFloor => "below_confidence_floor",
            Self::OutOfScope => "out_of_scope",
            Self::VisibilityDenied => "visibility_denied",
            Self::OutsideRecencyWindow => "outside_recency_window",
            Self::BeyondTopK => "beyond_top_k",
            Self::NotPinned => "not_pinned",
            Self::EmptyBody => "empty_body",
        }
    }
}

/// Why a record did enter the assembled hot prefix, with the rank score.
#[derive(Debug, Clone, PartialEq)]
pub struct InclusionTrace {
    /// Record that contributed to the segment body.
    pub record_id: RecordId,
    /// Source-specific score (e.g. `salience × recency` for pinned).
    pub score: f64,
    /// Human-readable note describing why this record won (`"top-1 by updated_at"`).
    pub note: &'static str,
}

/// Why a record was filtered, paired with its `record_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExclusionTrace {
    /// Record that was filtered.
    pub record_id: RecordId,
    /// Typed reason.
    pub reason: ExclusionReason,
}

/// Output of one source's `select` call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadedSegment {
    /// Concatenated markdown body produced by the source.
    pub body: String,
    /// Records that contributed to `body`, in emission order.
    pub included: Vec<InclusionTrace>,
    /// Records that were filtered, in evaluation order.
    pub excluded: Vec<ExclusionTrace>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_reason_wire_strings_match_idl() {
        assert_eq!(ExclusionReason::Tombstoned.as_wire(), "tombstoned");
        assert_eq!(ExclusionReason::ForgottenScope.as_wire(), "forgotten_scope");
        assert_eq!(
            ExclusionReason::BelowConfidenceFloor.as_wire(),
            "below_confidence_floor"
        );
        assert_eq!(ExclusionReason::OutOfScope.as_wire(), "out_of_scope");
        assert_eq!(
            ExclusionReason::VisibilityDenied.as_wire(),
            "visibility_denied"
        );
        assert_eq!(
            ExclusionReason::OutsideRecencyWindow.as_wire(),
            "outside_recency_window"
        );
        assert_eq!(ExclusionReason::BeyondTopK.as_wire(), "beyond_top_k");
        assert_eq!(ExclusionReason::NotPinned.as_wire(), "not_pinned");
        assert_eq!(ExclusionReason::EmptyBody.as_wire(), "empty_body");
    }

    #[test]
    fn loaded_segment_default_is_empty() {
        let s = LoadedSegment::default();
        assert!(s.body.is_empty());
        assert!(s.included.is_empty());
        assert!(s.excluded.is_empty());
    }
}
