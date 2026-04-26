//! `ConfidenceBand` + Evidence vector (brief §6.4).
//!
//! Confidence is a single scalar in `[0.0, 1.0]`; Evidence is a four-part
//! vector that drives promotion, expiration, and dream scheduling.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Discrete confidence band derived from a scalar confidence value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfidenceBand {
    /// `> 0.9` — eligible for promotion if evidence also clears.
    High,
    /// `[0.3, 0.9]` — normal recall.
    Normal,
    /// `< 0.3` — uncertain; suppressed unless explicitly requested.
    Uncertain,
}

impl ConfidenceBand {
    /// Wire-format identifier. Stable across surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Uncertain => "uncertain",
        }
    }

    /// Parse a wire-form band string. Returns
    /// [`DomainError::UnsupportedConfidenceBand`] for unknown values.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "high" => Ok(Self::High),
            "normal" => Ok(Self::Normal),
            "uncertain" => Ok(Self::Uncertain),
            other => Err(DomainError::UnsupportedConfidenceBand {
                value: other.to_owned(),
            }),
        }
    }

    /// Map a confidence scalar to its band.
    #[must_use]
    pub fn from_scalar(confidence: f32) -> Self {
        if confidence > 0.9 {
            Self::High
        } else if confidence < 0.3 {
            Self::Uncertain
        } else {
            Self::Normal
        }
    }
}

impl fmt::Display for ConfidenceBand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Four-component evidence vector. Each component is threshold-configurable
/// per [`crate::domain::MemoryKind`] in `.cairn/config.yaml` — defaults below
/// match brief §6.4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceVector {
    /// Times this record has been returned by a Read path. Default gate ≥ 3.
    pub recall_count: u32,
    /// Best retrieval score across recalls, `[0.0, 1.0]`. Default gate ≥ 0.7.
    pub score: f32,
    /// Number of distinct queries that surfaced this record. Default gate ≥ 2.
    pub unique_queries: u32,
    /// Exponential decay horizon in days. Default 14.
    pub recency_half_life_days: u32,
}

impl EvidenceVector {
    /// Validate scalar component ranges. Counter fields are unbounded; only
    /// `score` is range-checked.
    pub fn validate(&self) -> Result<(), DomainError> {
        if !(0.0..=1.0).contains(&self.score) || self.score.is_nan() {
            return Err(DomainError::OutOfRange {
                field: "evidence.score",
                message: format!("must be in [0.0, 1.0], was {}", self.score),
            });
        }
        Ok(())
    }
}

impl Default for EvidenceVector {
    fn default() -> Self {
        Self {
            recall_count: 0,
            score: 0.0,
            unique_queries: 0,
            recency_half_life_days: 14,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_high() {
        assert_eq!(ConfidenceBand::from_scalar(0.95), ConfidenceBand::High);
    }

    #[test]
    fn band_normal_at_boundary() {
        assert_eq!(ConfidenceBand::from_scalar(0.3), ConfidenceBand::Normal);
        assert_eq!(ConfidenceBand::from_scalar(0.9), ConfidenceBand::Normal);
    }

    #[test]
    fn band_uncertain() {
        assert_eq!(ConfidenceBand::from_scalar(0.1), ConfidenceBand::Uncertain);
    }

    #[test]
    fn band_as_str_all_3_values() {
        assert_eq!(ConfidenceBand::High.as_str(), "high");
        assert_eq!(ConfidenceBand::Normal.as_str(), "normal");
        assert_eq!(ConfidenceBand::Uncertain.as_str(), "uncertain");
    }

    #[test]
    fn band_parse_all_3_valid() {
        assert_eq!(
            ConfidenceBand::parse("high").expect("high"),
            ConfidenceBand::High
        );
        assert_eq!(
            ConfidenceBand::parse("normal").expect("normal"),
            ConfidenceBand::Normal
        );
        assert_eq!(
            ConfidenceBand::parse("uncertain").expect("uncertain"),
            ConfidenceBand::Uncertain
        );
    }

    #[test]
    fn band_parse_rejects_invented() {
        let err = ConfidenceBand::parse("very_high").unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedConfidenceBand { .. }));
    }

    #[test]
    fn band_as_str_parse_round_trip() {
        for band in [
            ConfidenceBand::High,
            ConfidenceBand::Normal,
            ConfidenceBand::Uncertain,
        ] {
            assert_eq!(
                ConfidenceBand::parse(band.as_str()).expect(band.as_str()),
                band
            );
        }
    }

    #[test]
    fn band_display_matches_wire_form() {
        assert_eq!(format!("{}", ConfidenceBand::High), "high");
        assert_eq!(format!("{}", ConfidenceBand::Uncertain), "uncertain");
    }

    #[test]
    fn evidence_validates_score_range() {
        let e = EvidenceVector {
            score: 1.5,
            ..EvidenceVector::default()
        };
        let err = e.validate().unwrap_err();
        assert!(matches!(err, DomainError::OutOfRange { .. }));
    }

    #[test]
    fn evidence_round_trips_json() {
        let e = EvidenceVector {
            recall_count: 7,
            score: 0.82,
            unique_queries: 4,
            recency_half_life_days: 14,
        };
        let s = serde_json::to_string(&e).expect("ser");
        let back: EvidenceVector = serde_json::from_str(&s).expect("de");
        assert_eq!(e, back);
    }
}
