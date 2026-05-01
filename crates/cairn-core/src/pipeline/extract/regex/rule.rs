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
        reject_duplicate_user_ids(&std::collections::HashSet::new(), rules)?;
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
    /// collides with a built-in id, an already-merged user id, or another
    /// rule in the same incoming batch, or if any rule fails compilation.
    pub fn with_user_rules(
        mut self,
        rules: &[RegexRule],
    ) -> Result<Self, super::super::ExtractError> {
        let existing_ids: std::collections::HashSet<&str> = self
            .builtin_text
            .iter()
            .chain(self.builtin_hook.iter())
            .chain(self.builtin_tool_frame.iter())
            .chain(self.user_text.iter())
            .chain(self.user_hook.iter())
            .chain(self.user_tool_frame.iter())
            .map(|r| r.id.as_str())
            .collect();
        reject_duplicate_user_ids(&existing_ids, rules)?;
        for rule in rules {
            compile_user_rule(&mut self, rule)?;
        }
        Ok(self)
    }
}

fn reject_duplicate_user_ids(
    existing: &std::collections::HashSet<&str>,
    rules: &[RegexRule],
) -> Result<(), super::super::ExtractError> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for rule in rules {
        let id = rule.id();
        if existing.contains(id) {
            return Err(super::super::ExtractError::InvalidRule {
                rule_id: id.to_owned(),
                reason: "user rule id collides with an existing rule".to_owned(),
            });
        }
        if !seen.insert(id) {
            return Err(super::super::ExtractError::InvalidRule {
                rule_id: id.to_owned(),
                reason: "duplicate user rule id within the same config".to_owned(),
            });
        }
    }
    Ok(())
}

fn compile_user_rule(
    set: &mut RuleSet,
    rule: &RegexRule,
) -> Result<(), super::super::ExtractError> {
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
#[allow(clippy::too_many_lines)] // straight-line per-variant arms; splitting hurts readability
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
            let group = capture_group.unwrap_or(0);
            // captures_len() includes group 0 (the whole match), so
            // valid indices are 0..captures_len().
            if usize::from(group) >= re.captures_len() {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: format!(
                        "capture_group {group} out of range; pattern has {} capture group(s)",
                        re.captures_len().saturating_sub(1),
                    ),
                });
            }
            Ok(CompiledRule {
                id: id.clone(),
                origin,
                kind: CompiledRuleKind::TriggerPhrase {
                    re,
                    kind_hint: kind_hint.clone(),
                    confidence: *confidence,
                    capture_group: group,
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
            if matches!(strategy, ForgetMatchStrategy::Exact) {
                if !*quoted_capture {
                    return Err(super::super::ExtractError::InvalidRule {
                        rule_id: id.clone(),
                        reason: "Exact match strategy requires quoted_capture: true".to_owned(),
                    });
                }
                // Structurally verify the target capture group is wrapped
                // in matching quote characters in the pattern source. This
                // makes `quoted_capture: true` a load-bearing claim about
                // the regex itself, not just an unchecked boolean.
                if !pattern_target_group_is_quote_wrapped(pattern, *target_group) {
                    return Err(super::super::ExtractError::InvalidRule {
                        rule_id: id.clone(),
                        reason: format!(
                            "Exact match strategy with quoted_capture: true requires \
                             target_group {target_group} to be immediately wrapped \
                             by matching quote characters (\", ', or `) in the pattern",
                        ),
                    });
                }
            }
            let re = compile_pattern(id, pattern)?;
            // ForgetPhrase target_group must reference an actual capture
            // group (not group 0, which is the whole match — using that
            // would yield a too-broad selector).
            if *target_group == 0 {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: "target_group 0 is the whole match; specify a real capture group"
                        .to_owned(),
                });
            }
            if usize::from(*target_group) >= re.captures_len() {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: format!(
                        "target_group {} out of range; pattern has {} capture group(s)",
                        target_group,
                        re.captures_len().saturating_sub(1),
                    ),
                });
            }
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

/// Walk `pattern` and locate the `target`-th capture group's opening
/// `(` and matching `)`. Returns `true` iff that group is immediately
/// preceded and followed by a matching quote character (`"`, `'`, or `` ` ``).
///
/// Counts only capturing groups (`(...)`), skipping non-capturing
/// `(?...)` constructs. Honors backslash escapes.
fn pattern_target_group_is_quote_wrapped(pattern: &str, target: u8) -> bool {
    let bytes = pattern.as_bytes();
    let mut i = 0;
    let mut group_no: u8 = 0;
    let mut depth: u32 = 0;
    let mut group_open: Option<usize> = None;
    let mut want_target_close_at_depth: Option<u32> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            i += 2;
            continue;
        }
        if b == b'[' {
            // Skip character class.
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b']' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b == b'(' {
            depth += 1;
            let is_non_capturing = i + 1 < bytes.len() && bytes[i + 1] == b'?';
            if !is_non_capturing {
                group_no = group_no.saturating_add(1);
                if group_no == target {
                    group_open = Some(i);
                    want_target_close_at_depth = Some(depth);
                }
            }
            i += 1;
            continue;
        }
        if b == b')' {
            if Some(depth) == want_target_close_at_depth {
                let Some(open_at) = group_open else {
                    return false;
                };
                let close_at = i;
                if open_at == 0 || close_at + 1 >= bytes.len() {
                    return false;
                }
                let before = bytes[open_at - 1];
                let after = bytes[close_at + 1];
                let is_quote = matches!(before, b'"' | b'\'' | b'`');
                return is_quote && before == after;
            }
            depth = depth.saturating_sub(1);
            i += 1;
            continue;
        }
        i += 1;
    }
    false
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
    fn from_config_rejects_exact_with_quoted_capture_but_unquoted_pattern() {
        // quoted_capture: true is structurally false here — the target
        // group is `(.+)`, not `"([^"]*)"`. Compile must reject so the
        // unchecked-boolean trust hole is closed.
        let json = r#"[{
            "type":"forget_phrase","id":"u.exact-bad","pattern":"^forget (.+)$",
            "target_group":1,"confidence":0.9,
            "match_strategy":"exact","quoted_capture":true
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("immediately wrapped"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_accepts_exact_with_truly_quoted_capture() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.exact-ok",
            "pattern":"^\\s*forget\\s+\"([^\"]+)\"\\s*$",
            "target_group":1,"confidence":0.9,
            "match_strategy":"exact","quoted_capture":true
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        RuleSet::from_config(&rules).expect("structurally quoted capture is accepted");
    }

    #[test]
    fn from_config_rejects_trigger_capture_group_out_of_range() {
        // Pattern has 1 capture group (group 1); capture_group=2 is invalid.
        let json = r#"[{
            "type":"trigger_phrase","id":"u.cg","pattern":"^remember (.+)$",
            "kind_hint":"user","confidence":0.5,"capture_group":2
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, reason } => {
                assert_eq!(rule_id, "u.cg");
                assert!(reason.contains("capture_group 2 out of range"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_forget_target_group_zero() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.tg0","pattern":"^forget (.+)$",
            "target_group":0,"confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("whole match"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_forget_target_group_out_of_range() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.tgoor","pattern":"^forget (.+)$",
            "target_group":2,"confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("target_group 2 out of range"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_duplicate_user_ids() {
        let json = r#"[
            {"type":"trigger_phrase","id":"u.dup","pattern":"^x .+",
             "kind_hint":"user","confidence":0.5},
            {"type":"trigger_phrase","id":"u.dup","pattern":"^y .+",
             "kind_hint":"user","confidence":0.5}
        ]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(&rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, reason } => {
                assert_eq!(rule_id, "u.dup");
                assert!(reason.contains("duplicate"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn with_user_rules_rejects_duplicate_against_already_merged_user_rule() {
        let first_json = r#"[{
            "type":"trigger_phrase","id":"u.x","pattern":"^x .+",
            "kind_hint":"user","confidence":0.5
        }]"#;
        let first: Vec<RegexRule> = serde_json::from_str(first_json).unwrap();
        let set = RuleSet::builtin()
            .with_user_rules(&first)
            .expect("first ok");

        let second_json = r#"[{
            "type":"trigger_phrase","id":"u.x","pattern":"^y .+",
            "kind_hint":"user","confidence":0.5
        }]"#;
        let second: Vec<RegexRule> = serde_json::from_str(second_json).unwrap();
        let err = set.with_user_rules(&second).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, .. } => {
                assert_eq!(rule_id, "u.x");
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
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
