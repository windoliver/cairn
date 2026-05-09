//! Built-in rule set. Spec §7.
//!
//! Text rules are listed in dispatch order (most specific first); the
//! per-window first-match-wins policy in §6.2 makes ordering
//! load-bearing — `remember.rule` MUST precede `remember.preference`.

use super::super::{Confidence, ForgetMatchStrategy, KindHint};
use super::rule::{
    CompiledRuleKind, RegexRule, RuleOrigin, RuleSet, ToolFrameFamily, compile_rule,
};
use crate::domain::taxonomy::MemoryKind;

/// Compile and bucket the built-in rule set.
#[must_use]
pub fn builtin_rule_set() -> RuleSet {
    let mut set = RuleSet::empty();

    let rules = builtin_rules();
    for rule in rules {
        #[allow(clippy::expect_used)] // invariant: built-in patterns are const and valid
        let compiled = compile_rule(&rule, RuleOrigin::BuiltIn)
            .expect("invariant: built-in rules are compile-checked at build time");
        match &compiled.kind {
            CompiledRuleKind::TriggerPhrase { .. } | CompiledRuleKind::ForgetPhrase { .. } => {
                set.builtin_text.push(compiled);
            }
            CompiledRuleKind::HookEvent { .. } => set.builtin_hook.push(compiled),
            CompiledRuleKind::ToolFrame { .. } => set.builtin_tool_frame.push(compiled),
        }
    }
    set
}

#[allow(clippy::expect_used)] // invariant: literal constant is always in [0,1]
fn conf(v: f32) -> Confidence {
    Confidence::try_from(v).expect("invariant: static built-in confidence in [0,1]")
}

fn kind(k: MemoryKind) -> KindHint {
    KindHint::from(k)
}

fn builtin_rules() -> Vec<RegexRule> {
    vec![
        RegexRule::TriggerPhrase {
            id: "remember.rule".into(),
            pattern: r"^\s*remember(?::|,)?\s+never\s+(.+?)\s*$".into(),
            kind_hint: kind(MemoryKind::Rule),
            confidence: conf(0.95),
            capture_group: Some(1),
        },
        RegexRule::TriggerPhrase {
            id: "remember.preference".into(),
            pattern: r"^\s*remember(?:\s+that)?\s+(.+?)\s*$".into(),
            kind_hint: kind(MemoryKind::User),
            confidence: conf(0.95),
            capture_group: Some(1),
        },
        RegexRule::TriggerPhrase {
            id: "correction".into(),
            pattern: r"^\s*correction:?\s+(.+?)\s*$".into(),
            kind_hint: kind(MemoryKind::Feedback),
            confidence: conf(0.95),
            capture_group: Some(1),
        },
        RegexRule::TriggerPhrase {
            id: "success.recipe".into(),
            pattern: r"^\s*this is how we did it\s*[—-]\s*it worked\s*$".into(),
            kind_hint: kind(MemoryKind::StrategySuccess),
            confidence: conf(0.85),
            capture_group: Some(0),
        },
        RegexRule::TriggerPhrase {
            id: "skillify".into(),
            pattern: r"^\s*skillify\s+(?:this|it)\s*$".into(),
            kind_hint: kind(MemoryKind::Playbook),
            confidence: conf(0.95),
            capture_group: Some(0),
        },
        RegexRule::ForgetPhrase {
            id: "forget".into(),
            pattern: r"^\s*forget\s+(?:that\s+|what\s+)?(.+?)\s*$".into(),
            target_group: 1,
            confidence: conf(0.95),
            match_strategy: ForgetMatchStrategy::Substring,
            quoted_capture: false,
        },
        RegexRule::HookEvent {
            id: "hook.post_tool_use".into(),
            hook_name: "PostToolUse".into(),
            tool_name: None,
            kind_hint: kind(MemoryKind::Trace),
            confidence: conf(0.8),
        },
        RegexRule::HookEvent {
            id: "hook.stop".into(),
            hook_name: "Stop".into(),
            tool_name: None,
            kind_hint: kind(MemoryKind::Trace),
            confidence: conf(0.7),
        },
        RegexRule::HookEvent {
            id: "hook.pre_compact".into(),
            hook_name: "PreCompact".into(),
            tool_name: None,
            kind_hint: kind(MemoryKind::Trace),
            confidence: conf(0.7),
        },
        RegexRule::ToolFrame {
            id: "tool.terminal_failure".into(),
            family: ToolFrameFamily::Terminal {
                exit_code_nonzero: true,
            },
            kind_hint: kind(MemoryKind::StrategyFailure),
            confidence: conf(0.7),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::super::rule::CompiledRuleKind;
    use super::*;

    #[test]
    fn builtin_set_has_expected_buckets() {
        let set = builtin_rule_set();
        assert_eq!(set.builtin_text.len(), 6);
        assert_eq!(set.builtin_hook.len(), 3);
        assert_eq!(set.builtin_tool_frame.len(), 1);
        assert!(set.user_text.is_empty());
    }

    #[test]
    fn text_rule_order_is_specific_first() {
        let set = builtin_rule_set();
        let ids: Vec<&str> = set.builtin_text.iter().map(|r| r.id.as_str()).collect();
        let rule_pos = ids.iter().position(|s| *s == "remember.rule").unwrap();
        let pref_pos = ids
            .iter()
            .position(|s| *s == "remember.preference")
            .unwrap();
        assert!(
            rule_pos < pref_pos,
            "remember.rule must precede remember.preference"
        );
    }

    #[test]
    fn remember_preference_matches_basic_phrase() {
        let set = builtin_rule_set();
        let rule = set
            .builtin_text
            .iter()
            .find(|r| r.id == "remember.preference")
            .unwrap();
        let CompiledRuleKind::TriggerPhrase { re, .. } = &rule.kind else {
            panic!("wrong kind");
        };
        assert!(re.is_match("remember that I prefer dark mode"));
        assert!(re.is_match("Remember I prefer cash"));
    }

    #[test]
    fn remember_rule_matches_never_form() {
        let set = builtin_rule_set();
        let rule = set
            .builtin_text
            .iter()
            .find(|r| r.id == "remember.rule")
            .unwrap();
        let CompiledRuleKind::TriggerPhrase { re, .. } = &rule.kind else {
            panic!("wrong kind");
        };
        assert!(re.is_match("remember never share API keys"));
        assert!(re.is_match("Remember: never push to main"));
        assert!(!re.is_match("remember that I prefer dark mode"));
    }

    #[test]
    fn forget_rule_captures_target() {
        let set = builtin_rule_set();
        let rule = set.builtin_text.iter().find(|r| r.id == "forget").unwrap();
        let CompiledRuleKind::ForgetPhrase {
            re, target_group, ..
        } = &rule.kind
        else {
            panic!("wrong kind");
        };
        let caps = re.captures("forget that I mentioned my address").unwrap();
        assert_eq!(&caps[*target_group as usize], "I mentioned my address");
    }

    #[test]
    fn skillify_matches_only_anchored_form() {
        let set = builtin_rule_set();
        let rule = set
            .builtin_text
            .iter()
            .find(|r| r.id == "skillify")
            .unwrap();
        let CompiledRuleKind::TriggerPhrase { re, .. } = &rule.kind else {
            panic!("wrong kind");
        };
        assert!(re.is_match("skillify this"));
        assert!(re.is_match("Skillify it"));
        assert!(!re.is_match("we should skillify the procedure later"));
    }
}
