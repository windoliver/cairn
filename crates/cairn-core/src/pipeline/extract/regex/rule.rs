//! Regex rule shapes. Spec §5.

use serde::Deserialize;

use super::super::{Confidence, ForgetMatchStrategy, KindHint};

/// A user-or-built-in rule, before compilation.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegexRule {
    /// A memorise-trigger rule: fires when the pattern matches body text.
    TriggerPhrase {
        /// Stable rule id for audit and dedup.
        id: String,
        /// Regex pattern to match against the body.
        pattern: String,
        /// Suggested taxonomy kind for extracted drafts.
        kind_hint: KindHint,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
        /// Optional capture group index whose text becomes the draft body.
        #[serde(default)]
        capture_group: Option<u8>,
    },
    /// A forget-trigger rule: fires when the pattern matches body text.
    ForgetPhrase {
        /// Stable rule id for audit and dedup.
        id: String,
        /// Regex pattern to match against the body.
        pattern: String,
        /// Capture group index whose text becomes the forget target.
        target_group: u8,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
        /// How the resolver should compare the captured text to record bodies.
        #[serde(default = "ForgetMatchStrategy::default_substring")]
        match_strategy: ForgetMatchStrategy,
        /// Whether the captured text is expected to be double-quoted.
        #[serde(default)]
        quoted_capture: bool,
    },
    /// A hook-event rule: fires when a named hook event is observed.
    HookEvent {
        /// Stable rule id for audit and dedup.
        id: String,
        /// Name of the hook that must fire for this rule to match.
        hook_name: String,
        /// Optional tool name filter; `None` means match any tool.
        #[serde(default)]
        tool_name: Option<String>,
        /// Suggested taxonomy kind for extracted drafts.
        kind_hint: KindHint,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
    },
    /// A tool-frame rule: fires when a structured tool frame is observed.
    ToolFrame {
        /// Stable rule id for audit and dedup.
        id: String,
        /// The tool-frame family this rule targets.
        family: ToolFrameFamily,
        /// Suggested taxonomy kind for extracted drafts.
        kind_hint: KindHint,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
    },
}

impl RegexRule {
    /// Stable id of the rule, used for audit and dedup.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            RegexRule::TriggerPhrase { id, .. }
            | RegexRule::ForgetPhrase { id, .. }
            | RegexRule::HookEvent { id, .. }
            | RegexRule::ToolFrame { id, .. } => id,
        }
    }
}

/// Identifies which category of tool-frame event a [`RegexRule::ToolFrame`]
/// targets.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolFrameFamily {
    /// A terminal execution event.
    Terminal {
        /// Whether this rule fires only on non-zero exit codes.
        exit_code_nonzero: bool,
    },
    /// An IDE editor event.
    Ide {
        /// The specific IDE event kind string (e.g. `"file_saved"`).
        event_kind: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;

    #[test]
    fn trigger_phrase_round_trips() {
        let json = r#"{
            "type": "trigger_phrase",
            "id": "remember.preference",
            "pattern": "^\\s*remember.+",
            "kind_hint": "user",
            "confidence": 0.95
        }"#;
        let rule: RegexRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.id(), "remember.preference");
        match rule {
            RegexRule::TriggerPhrase {
                kind_hint,
                confidence,
                capture_group,
                ..
            } => {
                assert_eq!(kind_hint, KindHint::from(MemoryKind::User));
                assert!((confidence.as_f32() - 0.95).abs() < f32::EPSILON);
                assert!(capture_group.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forget_phrase_defaults_strategy_to_substring() {
        let json = r#"{
            "type": "forget_phrase",
            "id": "forget",
            "pattern": "^forget (.+)$",
            "target_group": 1,
            "confidence": 0.95
        }"#;
        let rule: RegexRule = serde_json::from_str(json).unwrap();
        match rule {
            RegexRule::ForgetPhrase {
                match_strategy,
                quoted_capture,
                ..
            } => {
                assert_eq!(match_strategy, ForgetMatchStrategy::Substring);
                assert!(!quoted_capture);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn unknown_field_is_rejected() {
        let json = r#"{"type":"trigger_phrase","id":"a","pattern":"b","kind_hint":"user","confidence":0.5,"bogus":1}"#;
        assert!(serde_json::from_str::<RegexRule>(json).is_err());
    }

    #[test]
    fn tool_frame_terminal_round_trips() {
        let json = r#"{
            "type": "tool_frame",
            "id": "tool.terminal_failure",
            "family": {"kind": "terminal", "exit_code_nonzero": true},
            "kind_hint": "strategy_failure",
            "confidence": 0.7
        }"#;
        let rule: RegexRule = serde_json::from_str(json).unwrap();
        match rule {
            RegexRule::ToolFrame {
                family: ToolFrameFamily::Terminal { exit_code_nonzero },
                ..
            } => {
                assert!(exit_code_nonzero);
            }
            _ => panic!("wrong variant"),
        }
    }
}
