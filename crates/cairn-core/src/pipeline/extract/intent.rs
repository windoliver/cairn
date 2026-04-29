//! `ForgetIntent` — a structured forget selector. See spec §4.3.

use serde::{Deserialize, Serialize};

use super::{Confidence, KindHint, TextSpan};
use crate::domain::CaptureEventId;

/// How the resolver in #75 should compare `target_text_normalized`
/// against candidate record bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ForgetMatchStrategy {
    /// Body matches `target_text_normalized` after the same
    /// normalisation. Auto-proceed is permitted iff the candidate set
    /// is exactly one record.
    Exact,
    /// `target_text_normalized` is a substring of the (normalised)
    /// body. **Never** auto-proceeds — always routed through
    /// interactive `forget_ambiguous`. Default for the built-in `forget`
    /// rule.
    Substring,
    /// Reserved for #75; rejected at construction time in this PR.
    Fuzzy,
}

impl ForgetMatchStrategy {
    /// Default selector strategy used by `serde(default)` on
    /// user-supplied rules.
    #[must_use]
    pub fn default_substring() -> Self {
        Self::Substring
    }
}

/// A structured forget candidate. Never an instruction — see spec §4.3
/// for the resolver contract #75 must obey.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgetIntent {
    /// Normalized free-text selector lifted from the trigger
    /// (lowercased, whitespace-collapsed). Hint, never sole basis for
    /// deletion.
    pub target_text_normalized: String,
    /// How the resolver should compare `target_text_normalized` against
    /// candidate record bodies.
    pub match_strategy: ForgetMatchStrategy,
    /// Optional kind hint to narrow the candidate set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_filter: Option<KindHint>,
    /// Byte span within the resolved body where the trigger fired.
    /// Audit + debug only; deletion scope must not be derived from it.
    pub source_span: TextSpan,
    /// Confidence the rule had that this clause is a forget intent.
    pub confidence: Confidence,
    /// `CaptureEvent` that produced this intent.
    pub source_event: CaptureEventId,
    /// Stable id of the rule that fired.
    pub trigger_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;
    use crate::domain::CaptureEventId;

    fn fixture_event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid")
    }

    #[test]
    fn match_strategy_round_trips_substring() {
        let s = ForgetMatchStrategy::Substring;
        let json = serde_json::to_string(&s).expect("ser");
        assert_eq!(json, "\"substring\"");
        let back: ForgetMatchStrategy = serde_json::from_str(&json).expect("deser");
        assert_eq!(back, s);
    }

    #[test]
    fn match_strategy_round_trips_exact_and_fuzzy() {
        for s in [ForgetMatchStrategy::Exact, ForgetMatchStrategy::Fuzzy] {
            let json = serde_json::to_string(&s).unwrap();
            let back: ForgetMatchStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn forget_intent_round_trips_with_kind_filter() {
        let intent = ForgetIntent {
            target_text_normalized: "my old address".to_owned(),
            match_strategy: ForgetMatchStrategy::Substring,
            kind_filter: Some(KindHint::from(MemoryKind::User)),
            source_span: TextSpan::new(0, 21),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: fixture_event_id(),
            trigger_id: "forget".to_owned(),
        };
        let json = serde_json::to_string(&intent).unwrap();
        let back: ForgetIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, intent);
    }

    #[test]
    fn forget_intent_round_trips_without_kind_filter() {
        let intent = ForgetIntent {
            target_text_normalized: "old thing".to_owned(),
            match_strategy: ForgetMatchStrategy::Substring,
            kind_filter: None,
            source_span: TextSpan::new(0, 9),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: fixture_event_id(),
            trigger_id: "forget".to_owned(),
        };
        let json = serde_json::to_string(&intent).unwrap();
        assert!(!json.contains("kind_filter"));
        let back: ForgetIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, intent);
    }
}
