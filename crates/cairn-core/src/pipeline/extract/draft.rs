//! `MemoryDraft` and its constituent newtypes.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::domain::{CaptureEventId, taxonomy::MemoryKind};

/// Confidence score in `[0.0, 1.0]`. Constructed via `try_from`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    /// Confidence of zero — useful as a default sentinel.
    pub const ZERO: Self = Self(0.0);

    /// Inner value as `f32`.
    #[must_use]
    pub fn as_f32(self) -> f32 {
        self.0
    }
}

/// Errors that can occur when constructing a [`Confidence`].
#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ConfidenceError {
    /// Value was finite but outside the closed interval `[0.0, 1.0]`.
    #[error("confidence {0} is outside [0.0, 1.0]")]
    OutOfRange(f32),
    /// Value was `NaN`.
    #[error("confidence is NaN")]
    Nan,
}

impl TryFrom<f32> for Confidence {
    type Error = ConfidenceError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        if value.is_nan() {
            return Err(ConfidenceError::Nan);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(ConfidenceError::OutOfRange(value));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// Byte range within a body: `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSpan {
    /// Inclusive byte offset where the span begins.
    pub start: u32,
    /// Exclusive byte offset where the span ends.
    pub end: u32,
}

impl TextSpan {
    /// Construct a span. Debug-asserts `start <= end`.
    #[must_use]
    pub fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "TextSpan: start must be <= end");
        Self { start, end }
    }

    /// Length of the span in bytes.
    #[must_use]
    pub fn len(self) -> u32 {
        self.end - self.start
    }

    /// Whether the span is empty (`start == end`).
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether `self` and `other` share at least one byte.
    /// Touching ranges (e.g. `[0,10)` and `[10,20)`) do not overlap.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Wrapper around `MemoryKind`. Always serialises as the inner kind
/// string for wire stability with the IDL.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KindHint(pub MemoryKind);

impl From<MemoryKind> for KindHint {
    fn from(kind: MemoryKind) -> Self {
        Self(kind)
    }
}

impl KindHint {
    /// The wrapped [`MemoryKind`].
    #[must_use]
    pub fn kind(&self) -> MemoryKind {
        self.0
    }
}

/// Draft memory record, the standard output of an extractor for an
/// observed-memorise intent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryDraft {
    /// Suggested taxonomy kind for the resulting record.
    pub kind_hint: KindHint,
    /// Extracted body text destined for the record.
    pub body: String,
    /// Extractor confidence in this draft.
    pub confidence: Confidence,
    /// The capture event the draft was extracted from.
    pub source_event: CaptureEventId,
    /// Byte span within the source body that produced this draft, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<TextSpan>,
    /// Identifier of the trigger / rule that produced this draft, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;

    #[test]
    fn confidence_accepts_in_range() {
        assert!(Confidence::try_from(0.0).is_ok());
        assert!(Confidence::try_from(0.5).is_ok());
        assert!(Confidence::try_from(1.0).is_ok());
    }

    #[test]
    fn confidence_rejects_out_of_range() {
        assert_eq!(
            Confidence::try_from(-0.1).unwrap_err(),
            ConfidenceError::OutOfRange(-0.1)
        );
        assert_eq!(
            Confidence::try_from(1.5).unwrap_err(),
            ConfidenceError::OutOfRange(1.5)
        );
    }

    #[test]
    fn confidence_rejects_nan() {
        assert_eq!(
            Confidence::try_from(f32::NAN).unwrap_err(),
            ConfidenceError::Nan
        );
    }

    #[test]
    fn text_span_overlap_detects_intersection() {
        let a = TextSpan::new(0, 10);
        let b = TextSpan::new(5, 15);
        let c = TextSpan::new(20, 30);
        assert!(a.overlaps(b));
        assert!(!a.overlaps(c));
        // Touching but not overlapping
        assert!(!a.overlaps(TextSpan::new(10, 20)));
    }

    #[test]
    fn kind_hint_round_trips_via_serde() {
        let hint = KindHint::from(MemoryKind::User);
        let json = serde_json::to_string(&hint).expect("ser");
        // Inner enum serialises as a snake_case string.
        assert_eq!(json, "\"user\"");
        let back: KindHint = serde_json::from_str(&json).expect("deser");
        assert_eq!(back, hint);
    }

    #[test]
    fn memory_draft_round_trips_via_serde() {
        use crate::domain::CaptureEventId;
        let draft = MemoryDraft {
            kind_hint: KindHint::from(MemoryKind::User),
            body: "I prefer dark mode".to_owned(),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid"),
            source_span: Some(TextSpan::new(0, 18)),
            trigger_id: Some("remember.preference".to_owned()),
        };
        let json = serde_json::to_string(&draft).expect("ser");
        let back: MemoryDraft = serde_json::from_str(&json).expect("deser");
        assert_eq!(back, draft);
    }
}
