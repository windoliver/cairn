//! Regex rule shapes. Spec §5.

use ::regex::Regex;
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

/// Pre-compiled form of a `RegexRule`. Built by `RuleSet::from_config`
/// or `RuleSet::builtin`.
#[derive(Clone, Debug)]
pub struct CompiledRule {
    /// Stable id matching the source [`RegexRule`].
    pub id: String,
    /// Whether this rule came from the built-in defaults or a user config.
    pub origin: RuleOrigin,
    /// The compiled, dispatch-ready form of the rule.
    pub kind: CompiledRuleKind,
}

/// Whether a compiled rule originated from the built-in defaults or a user
/// config file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleOrigin {
    /// Rule ships with Cairn and cannot be overridden by user config.
    BuiltIn,
    /// Rule was provided by the user in `.cairn/config.yaml`.
    User,
}

/// The compiled, dispatch-ready payload for a [`CompiledRule`].
#[derive(Clone, Debug)]
pub enum CompiledRuleKind {
    /// A memorise-trigger: fires when the compiled regex matches body text.
    TriggerPhrase {
        /// Compiled regex, case-insensitive, size-limited.
        re: Regex,
        /// Suggested taxonomy kind for extracted drafts.
        kind_hint: KindHint,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
        /// Capture group whose text becomes the draft body (`0` = full match).
        capture_group: u8,
    },
    /// A forget-trigger: fires when the compiled regex matches body text.
    ForgetPhrase {
        /// Compiled regex, case-insensitive, size-limited.
        re: Regex,
        /// Capture group whose text becomes the forget target.
        target_group: u8,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
        /// How the resolver should compare the captured text to record bodies.
        match_strategy: ForgetMatchStrategy,
    },
    /// A hook-event rule: fires when a named hook event is observed.
    HookEvent {
        /// Name of the hook that must fire for this rule to match.
        hook_name: String,
        /// Optional tool name filter; `None` means match any tool.
        tool_name: Option<String>,
        /// Suggested taxonomy kind for extracted drafts.
        kind_hint: KindHint,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
    },
    /// A tool-frame rule: fires when a structured tool frame is observed.
    ToolFrame {
        /// The tool-frame family this rule targets.
        family: ToolFrameFamily,
        /// Suggested taxonomy kind for extracted drafts.
        kind_hint: KindHint,
        /// Confidence score for outputs produced by this rule.
        confidence: Confidence,
    },
}

/// Bucketed compiled rules, ready for dispatch.
#[derive(Clone, Debug, Default)]
pub struct RuleSet {
    pub(crate) builtin_text: Vec<CompiledRule>,
    pub(crate) builtin_hook: Vec<CompiledRule>,
    pub(crate) builtin_tool_frame: Vec<CompiledRule>,
    pub(crate) user_text: Vec<CompiledRule>,
    pub(crate) user_hook: Vec<CompiledRule>,
    pub(crate) user_tool_frame: Vec<CompiledRule>,
}

impl RuleSet {
    /// Empty ruleset — useful in tests.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Built-in rule set populated from `defaults.rs` (Task 8).
    #[must_use]
    pub fn builtin() -> Self {
        super::defaults::builtin_rule_set()
    }

    /// Compile and validate user rules into a fresh `RuleSet`. Use
    /// `with_user_rules` to merge with built-ins.
    ///
    /// # Errors
    ///
    /// Returns [`super::super::ExtractError::InvalidRule`] if any rule fails
    /// compilation or validation.
    pub fn from_config(rules: &[RegexRule]) -> Result<Self, super::super::ExtractError> {
        let mut set = Self::empty();
        for rule in rules {
            compile_user_rule(&mut set, rule)?;
        }
        Ok(set)
    }

    /// Append user rules to an existing ruleset (typically the built-in one).
    ///
    /// # Errors
    ///
    /// Returns [`super::super::ExtractError::InvalidRule`] if any rule id
    /// collides with a built-in id, or if any rule fails compilation.
    pub fn with_user_rules(
        mut self,
        rules: &[RegexRule],
    ) -> Result<Self, super::super::ExtractError> {
        let existing_ids: std::collections::HashSet<&str> = self
            .builtin_text
            .iter()
            .chain(self.builtin_hook.iter())
            .chain(self.builtin_tool_frame.iter())
            .map(|r| r.id.as_str())
            .collect();
        for rule in rules {
            if existing_ids.contains(rule.id()) {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: rule.id().to_owned(),
                    reason: "user rule id collides with built-in".to_owned(),
                });
            }
        }
        for rule in rules {
            compile_user_rule(&mut self, rule)?;
        }
        Ok(self)
    }
}

fn compile_user_rule(set: &mut RuleSet, rule: &RegexRule) -> Result<(), super::super::ExtractError> {
    let compiled = compile_rule(rule, RuleOrigin::User)?;
    match &compiled.kind {
        CompiledRuleKind::TriggerPhrase { .. } | CompiledRuleKind::ForgetPhrase { .. } => {
            set.user_text.push(compiled);
        }
        CompiledRuleKind::HookEvent { .. } => set.user_hook.push(compiled),
        CompiledRuleKind::ToolFrame { .. } => set.user_tool_frame.push(compiled),
    }
    Ok(())
}

/// Compile a single rule. Public so `defaults.rs` can build built-ins.
pub(crate) fn compile_rule(
    rule: &RegexRule,
    origin: RuleOrigin,
) -> Result<CompiledRule, super::super::ExtractError> {
    match rule {
        RegexRule::TriggerPhrase {
            id,
            pattern,
            kind_hint,
            confidence,
            capture_group,
        } => {
            let re = compile_pattern(id, pattern)?;
            Ok(CompiledRule {
                id: id.clone(),
                origin,
                kind: CompiledRuleKind::TriggerPhrase {
                    re,
                    kind_hint: kind_hint.clone(),
                    confidence: *confidence,
                    capture_group: capture_group.unwrap_or(0),
                },
            })
        }
        RegexRule::ForgetPhrase {
            id,
            pattern,
            target_group,
            confidence,
            match_strategy,
            quoted_capture,
        } => {
            let strategy = *match_strategy;
            if matches!(strategy, ForgetMatchStrategy::Fuzzy) {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: "Fuzzy match strategy is reserved for #75".to_owned(),
                });
            }
            if matches!(strategy, ForgetMatchStrategy::Exact) && !*quoted_capture {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: "Exact match strategy requires quoted_capture: true".to_owned(),
                });
            }
            let re = compile_pattern(id, pattern)?;
            Ok(CompiledRule {
                id: id.clone(),
                origin,
                kind: CompiledRuleKind::ForgetPhrase {
                    re,
                    target_group: *target_group,
                    confidence: *confidence,
                    match_strategy: strategy,
                },
            })
        }
        RegexRule::HookEvent {
            id,
            hook_name,
            tool_name,
            kind_hint,
            confidence,
        } => Ok(CompiledRule {
            id: id.clone(),
            origin,
            kind: CompiledRuleKind::HookEvent {
                hook_name: hook_name.clone(),
                tool_name: tool_name.clone(),
                kind_hint: kind_hint.clone(),
                confidence: *confidence,
            },
        }),
        RegexRule::ToolFrame {
            id,
            family,
            kind_hint,
            confidence,
        } => Ok(CompiledRule {
            id: id.clone(),
            origin,
            kind: CompiledRuleKind::ToolFrame {
                family: family.clone(),
                kind_hint: kind_hint.clone(),
                confidence: *confidence,
            },
        }),
    }
}

fn compile_pattern(id: &str, pattern: &str) -> Result<Regex, super::super::ExtractError> {
    ::regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| super::super::ExtractError::InvalidRule {
            rule_id: id.to_owned(),
            reason: e.to_string(),
        })
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

    #[test]
    fn from_config_compiles_valid_rules() {
        let json = r#"[{
            "type":"trigger_phrase","id":"u.x","pattern":"^x .+",
            "kind_hint":"user","confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let set = RuleSet::from_config(&rules).expect("compile ok");
        assert_eq!(set.user_text.len(), 1);
    }

    #[test]
    fn from_config_rejects_invalid_pattern() {
        let json = r#"[{
            "type":"trigger_phrase","id":"u.bad","pattern":"[unclosed",
            "kind_hint":"user","confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, .. } => {
                assert_eq!(rule_id, "u.bad");
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_fuzzy_strategy() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.f","pattern":"^forget (.+)$",
            "target_group":1,"confidence":0.5,"match_strategy":"fuzzy"
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("Fuzzy"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_exact_without_quoted_capture() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.e","pattern":"^forget (.+)$",
            "target_group":1,"confidence":0.5,"match_strategy":"exact"
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("quoted_capture"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    #[ignore = "enabled in Task 8"]
    fn with_user_rules_rejects_duplicate_id_against_builtin() {
        let builtin = RuleSet::builtin();
        let json = r#"[{
            "type":"trigger_phrase","id":"remember.preference",
            "pattern":"^x .+","kind_hint":"user","confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = builtin.with_user_rules(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, reason } => {
                assert_eq!(rule_id, "remember.preference");
                assert!(reason.contains("collides"));
            }
            _ => panic!("wrong error"),
        }
    }
}
