//! Hot-prefix source classification — see issue #83 / brief §7.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watermarks_match_is_reflexive() {
        let w = SourceWatermarks::default();
        assert!(w.matches(&w));
    }

    #[test]
    fn watermarks_match_breaks_when_any_field_diverges() {
        let base = SourceWatermarks::default();
        for class in [
            SourceClass::ProfileEvidence,
            SourceClass::Pinned,
            SourceClass::PurposeIndex,
            SourceClass::Summaries,
            SourceClass::Playbooks,
            SourceClass::Policy,
        ] {
            let mut other = base;
            other.bump(class);
            assert!(!base.matches(&other), "class {class:?} did not invalidate match");
        }
    }

    #[test]
    fn source_class_all_returns_six_classes() {
        assert_eq!(SourceClass::ALL.len(), 6);
    }
}
