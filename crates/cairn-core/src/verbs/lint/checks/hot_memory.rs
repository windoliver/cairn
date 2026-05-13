//! §6.6 — `hot_memory_over_budget` canary check.
//!
//! P0 framing: walk `vault.hot_memory.recipe`, sum the projected
//! markdown bytes for the bounded subset of records each step would
//! load, and compare against `vault.hot_memory.max_bytes`. When the
//! estimate exceeds the budget, emit one `HotMemoryOverBudget`
//! Warning. For every recipe step whose runtime selector cannot be
//! reproduced from the records-side signal alone, emit an `Info`
//! `DeferredCheck` so the coverage gap is loud.
//!
//! Why projected bytes (not raw bodies):
//! the runtime budget covers the assembled markdown prefix, so byte
//! counting must include YAML frontmatter via
//! [`MarkdownProjector::project`] — the same renderer the runtime
//! contract uses.
//!
//! Step-by-step coverage matrix (brief §7):
//!
//! | Recipe step              | Runtime selector                       | Canary action                          |
//! |--------------------------|----------------------------------------|----------------------------------------|
//! | `Purpose`                | reads `purpose.md` from disk           | `DeferredCheck` (filesystem-backed)    |
//! | `Index`                  | reads `index.md` from disk             | `DeferredCheck` (filesystem-backed)    |
//! | `PinnedFeedback`         | top 8 `user`/`feedback` by salience×recency | `DeferredCheck` (recency factor missing — salience-only would not be conservative; old high-salience records can displace newer larger ones) |
//! | `TopSalienceProject`     | top 6 `project` by salience            | top 6 by salience (faithful)           |
//! | `ActivePlaybook`         | the `active` playbook                  | `DeferredCheck` (no `active` predicate; canary cannot pick) |
//! | `RecentUserSignal`       | last 24h of `user_signal`              | `DeferredCheck` (no clock; canary cannot window) |
//!
//! The deferred steps do not contribute to the byte estimate. The
//! canary therefore reports a strict lower bound for the assembled
//! prefix, plus loud coverage findings for everything it could not
//! weigh — silence on a recipe that leans on deferred steps is never
//! valid. The deferred-coverage finding is severity `Warning` (not
//! `Info`) because a default recipe contains four deferred steps and
//! a real over-budget vault could otherwise pass §6.6 with only an
//! `Info` advisory.
//!
//! Followups: a real recipe walker (calls the assembler) or a per-
//! record `hot_scope` predicate closes every gap above. Both are
//! tracked under #259.

use crate::config::HotMemoryRecipeStep;
use crate::domain::projection::MarkdownProjector;
use crate::domain::taxonomy::MemoryKind;
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, LintRecord, finding, target_path};

/// Tracking issue for every coverage gap below. Closing it lands a
/// real recipe walker / `hot_scope` predicate.
const TRACKING_ISSUE: i64 = 259;

/// Top-K bound for the one step the canary can faithfully reproduce.
/// Brief §7: "highest-salience project — top 6".
const TOP_K_SALIENCE_PROJECT: usize = 6;

/// Outcome of weighing a single recipe step.
enum StepWeighing {
    /// The canary contributed `bytes` to the overall estimate.
    Counted { bytes: u64 },
    /// The canary could not faithfully reproduce the runtime
    /// selector for this step. The step is omitted from the byte
    /// estimate; a `DeferredCheck` will surface the gap.
    Deferred { reason: &'static str },
}

/// Canary `hot_memory_over_budget` check (§6.6).
///
/// Returns up to two findings:
///
/// 1. A `Warning` `HotMemoryOverBudget` when the bytes from steps
///    the canary *can* weigh exceed `vault.hot_memory.max_bytes`.
/// 2. A `Warning` `DeferredCheck` listing every step the canary could
///    not weigh — so an overflow contributed exclusively by deferred
///    steps cannot pass §6.6 silently. Severity is `Warning` rather
///    than `Info`: deferred-step coverage on the default recipe is
///    load-bearing for the budget guarantee, not an advisory note.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let recipe = inputs.config.vault.hot_memory.recipe.as_slice();
    let max_bytes = u64::from(inputs.config.vault.hot_memory.max_bytes);
    let projector = MarkdownProjector;

    let mut findings: Vec<Finding> = Vec::new();
    let mut estimated: u64 = 0;
    let mut deferred_steps: Vec<&'static str> = Vec::new();

    for step in recipe {
        for outcome in weigh_step(*step, inputs.records, projector) {
            match outcome {
                StepWeighing::Counted { bytes } => {
                    estimated = estimated.saturating_add(bytes);
                }
                StepWeighing::Deferred { reason } => {
                    if !deferred_steps.contains(&reason) {
                        deferred_steps.push(reason);
                    }
                }
            }
        }
    }

    if estimated > max_bytes {
        let mut f = finding(
            Kind::HotMemoryOverBudget,
            Severity::Warning,
            format!(
                "hot-memory canary: assembler-bound records total ~{estimated} bytes, above \
                 vault.hot_memory.max_bytes = {max_bytes} (lower bound — deferred recipe steps \
                 may push the assembled prefix higher still)"
            ),
        );
        f.target = Some(target_path(".cairn/config.yaml"));
        f.suggested_fix = Some(
            "raise vault.hot_memory.max_bytes, drop recipe steps, prune low-salience records, \
             or wait for the per-record hot_scope predicate"
                .to_owned(),
        );
        findings.push(f);
    }

    if !deferred_steps.is_empty() {
        let joined = deferred_steps.join("; ");
        let mut f = finding(
            Kind::DeferredCheck,
            Severity::Warning,
            format!(
                "§6.6 canary cannot weigh some recipe steps without a real recipe walker / \
                 hot_scope predicate (tracked in #{TRACKING_ISSUE}): {joined}"
            ),
        );
        f.target = Some(target_path(".cairn/config.yaml"));
        f.tracking_issue = Some(TRACKING_ISSUE);
        f.suggested_fix = Some(format!(
            "ship #{TRACKING_ISSUE} to enable byte estimation for the deferred steps"
        ));
        findings.push(f);
    }

    findings
}

/// Weigh one recipe step. Returns one or more `StepWeighing`s — most
/// steps yield exactly one outcome, but `PinnedFeedback` yields both
/// a counted byte estimate (top-K by salience) *and* a deferred
/// coverage reason (the recency factor in "salience × recency" is
/// not derivable from records alone).
fn weigh_step(
    step: HotMemoryRecipeStep,
    records: &[LintRecord],
    projector: MarkdownProjector,
) -> Vec<StepWeighing> {
    match step {
        HotMemoryRecipeStep::Purpose => vec![StepWeighing::Deferred {
            reason: "purpose: reads purpose.md from disk; canary is pure / no I/O",
        }],
        HotMemoryRecipeStep::Index => vec![StepWeighing::Deferred {
            reason: "index: reads index.md from disk; canary is pure / no I/O",
        }],
        HotMemoryRecipeStep::ActivePlaybook => vec![StepWeighing::Deferred {
            reason: "active_playbook: needs the `active` predicate (not a record-side signal)",
        }],
        HotMemoryRecipeStep::RecentUserSignal => vec![StepWeighing::Deferred {
            reason: "recent_user_signal: needs a 24h clock window (not available in pure check)",
        }],
        HotMemoryRecipeStep::TopSalienceProject => {
            let bytes = sum_top_k_by_salience_where(
                records,
                |r| r.stored.record.kind == MemoryKind::Project,
                TOP_K_SALIENCE_PROJECT,
                projector,
            );
            vec![StepWeighing::Counted { bytes }]
        }
        HotMemoryRecipeStep::PinnedFeedback => {
            // Brief specifies "salience × recency"; salience-only is
            // not a conservative bound (old high-salience records can
            // displace newer larger ones the runtime would actually
            // pin), so the canary defers entirely instead of issuing
            // an unsafe estimate.
            vec![StepWeighing::Deferred {
                reason: "pinned_feedback: needs `salience × recency`; salience-only \
                         approximation is not conservative (old small records can displace \
                         newer larger ones)",
            }]
        }
    }
}

/// Top-K records matching `pred` by descending salience, projected
/// to markdown and summed.
fn sum_top_k_by_salience_where<F>(
    records: &[LintRecord],
    pred: F,
    top_k: usize,
    projector: MarkdownProjector,
) -> u64
where
    F: Fn(&&LintRecord) -> bool,
{
    // Pre-project so the conservative tie-breaker can compare byte
    // sizes without re-projecting per comparison. Pair (salience,
    // projected_bytes) so ties on salience break in favor of the
    // larger record — never an underestimate.
    let mut candidates: Vec<(&LintRecord, u64)> = records
        .iter()
        .filter(pred)
        .map(|r| {
            let bytes = projector.project(&r.stored).content.len() as u64;
            (r, bytes)
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.0.stored
            .record
            .salience
            .total_cmp(&a.0.stored.record.salience)
            // Conservative tie-breaker: descending byte size on equal
            // salience so the truncation cannot drop a larger peer.
            .then(b.1.cmp(&a.1))
    });
    candidates.truncate(top_k);
    candidates
        .iter()
        .map(|(_, bytes)| *bytes)
        .fold(0u64, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::contract::version::SchemaVersion;
    use crate::domain::record::tests_export::sample_record;
    use crate::verbs::lint::LintRecord;

    fn lint_record_with(kind: MemoryKind, body: String) -> LintRecord {
        let mut r = sample_record();
        r.kind = kind;
        r.body = body;
        LintRecord {
            stored: StoredRecord {
                record: r,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: crate::domain::consent_timeline::ConsentModel::LegacyEvent,
        }
    }

    fn inputs_with<'a>(records: &'a [LintRecord], cfg: &'a CairnConfig) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
        }
    }

    #[test]
    fn default_recipe_with_no_records_emits_warning_coverage_finding() {
        // Default recipe leans heavily on deferred steps (Purpose,
        // Index, PinnedFeedback, ActivePlaybook, RecentUserSignal),
        // so the canary surfaces one `Warning` `DeferredCheck`.
        // Severity is `Warning`, not `Info`: a real over-budget
        // vault could otherwise pass §6.6 with no actionable signal.
        let cfg = CairnConfig::default();
        let inputs = inputs_with(&[], &cfg);
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].tracking_issue, Some(259));
    }

    #[test]
    fn fully_estimable_recipe_with_no_records_emits_nothing() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        let inputs = inputs_with(&[], &cfg);
        assert!(run(&inputs).is_empty());
    }

    #[test]
    fn recipe_eligible_records_within_budget_emit_no_finding() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        let r = lint_record_with(MemoryKind::Project, "x".repeat(64));
        let inputs = inputs_with(std::slice::from_ref(&r), &cfg);
        assert!(run(&inputs).is_empty());
    }

    #[test]
    fn over_budget_emits_warning_targeting_config() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        cfg.vault.hot_memory.max_bytes = 256;
        let big = lint_record_with(MemoryKind::Project, "x".repeat(1024));
        let inputs = inputs_with(std::slice::from_ref(&big), &cfg);
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::HotMemoryOverBudget);
        assert_eq!(f[0].severity, Severity::Warning);
        let target = f[0].target.as_ref().expect("target set");
        assert_eq!(target.path.as_deref(), Some(".cairn/config.yaml"));
        assert!(f[0].suggested_fix.is_some());
        assert!(f[0].tracking_issue.is_none());
    }

    #[test]
    fn records_outside_recipe_kinds_do_not_count() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        cfg.vault.hot_memory.max_bytes = 256;
        let signal = lint_record_with(MemoryKind::UserSignal, "x".repeat(4096));
        let inputs = inputs_with(std::slice::from_ref(&signal), &cfg);
        assert!(run(&inputs).is_empty());
    }

    #[test]
    fn pinned_feedback_step_is_fully_deferred_not_estimated() {
        // `PinnedFeedback` defers entirely — the salience-only
        // approximation is not a conservative bound for the runtime's
        // `salience × recency` selector, so the canary refuses to
        // emit a budget warning that could be a false negative on a
        // mature vault. Even huge feedback bodies must produce only
        // a `DeferredCheck` Warning, never a `HotMemoryOverBudget`.
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::PinnedFeedback];
        cfg.vault.hot_memory.max_bytes = 16;
        let user = lint_record_with(MemoryKind::User, "u".repeat(8192));
        let feedback = lint_record_with(MemoryKind::Feedback, "f".repeat(8192));
        let records = vec![user, feedback];
        let inputs = inputs_with(&records, &cfg);
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(
            f[0].message.contains("pinned_feedback"),
            "message must enumerate pinned_feedback as deferred: {}",
            f[0].message
        );
    }

    #[test]
    fn filesystem_only_recipe_emits_coverage_deferred_even_with_no_records() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe =
            vec![HotMemoryRecipeStep::Purpose, HotMemoryRecipeStep::Index];
        cfg.vault.hot_memory.max_bytes = 1;
        let inputs = inputs_with(&[], &cfg);
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].tracking_issue, Some(259));
        let target = f[0].target.as_ref().expect("target set");
        assert_eq!(target.path.as_deref(), Some(".cairn/config.yaml"));
    }

    #[test]
    fn active_playbook_and_recent_user_signal_are_deferred_not_estimated() {
        // Regression against round-3 high finding: these two steps
        // require runtime predicates the canary cannot reproduce, so
        // they must not contribute to the byte estimate. Even with a
        // huge body, no `HotMemoryOverBudget` is emitted; only the
        // deferred-coverage info finding fires.
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![
            HotMemoryRecipeStep::ActivePlaybook,
            HotMemoryRecipeStep::RecentUserSignal,
        ];
        cfg.vault.hot_memory.max_bytes = 16;
        let huge_playbook = lint_record_with(MemoryKind::Playbook, "p".repeat(8192));
        let huge_signal = lint_record_with(MemoryKind::UserSignal, "s".repeat(8192));
        let records = vec![huge_playbook, huge_signal];
        let inputs = inputs_with(&records, &cfg);
        let f = run(&inputs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::DeferredCheck);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(
            f[0].message.contains("active_playbook") && f[0].message.contains("recent_user_signal"),
            "deferred message must enumerate every uncovered step: {}",
            f[0].message
        );
    }

    #[test]
    fn frontmatter_bytes_are_counted_not_just_body() {
        // Use TopSalienceProject — the only step the canary still
        // counts — to prove the projection helper picks up frontmatter
        // bytes, not just `body.len()`.
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        cfg.vault.hot_memory.max_bytes = 10;
        let r = lint_record_with(MemoryKind::Project, "x".to_owned());
        let inputs = inputs_with(std::slice::from_ref(&r), &cfg);
        let f = run(&inputs);
        assert!(f.iter().any(|x| x.kind == Kind::HotMemoryOverBudget));
    }

    #[test]
    fn tied_salience_top_k_picks_largest_records_conservatively() {
        // Regression: ten projects with identical salience but
        // varying body sizes. Top-K=6 must pick the six *largest* on
        // ties, not the first six in input order — otherwise the
        // canary could undercount on tied salience and miss a real
        // overflow.
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        // Pick a budget that fits 4 small projects but not 6 large.
        cfg.vault.hot_memory.max_bytes = 2_000;
        let small: Vec<LintRecord> = (0..4)
            .map(|_| lint_record_with(MemoryKind::Project, "x".to_owned()))
            .collect();
        let large: Vec<LintRecord> = (0..6)
            .map(|_| lint_record_with(MemoryKind::Project, "L".repeat(800)))
            .collect();
        // Insert smalls first so input order alone would pick them.
        let mut records = small;
        records.extend(large);
        let inputs = inputs_with(&records, &cfg);
        let f = run(&inputs);
        assert!(
            f.iter().any(|x| x.kind == Kind::HotMemoryOverBudget),
            "tied salience must break toward larger records — chosen subset must overflow 2000-byte budget"
        );
    }

    #[test]
    fn historical_projects_above_top_k_do_not_trigger_warning() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![HotMemoryRecipeStep::TopSalienceProject];
        let records: Vec<LintRecord> = (0..100)
            .map(|_| lint_record_with(MemoryKind::Project, "summary".to_owned()))
            .collect();
        let inputs = inputs_with(&records, &cfg);
        assert!(
            run(&inputs).is_empty(),
            "100 historical projects must not trip the canary — top-K bound is {TOP_K_SALIENCE_PROJECT}"
        );
    }

    #[test]
    fn over_budget_with_filesystem_step_emits_both_warning_and_coverage_info() {
        let mut cfg = CairnConfig::default();
        cfg.vault.hot_memory.recipe = vec![
            HotMemoryRecipeStep::Index,
            HotMemoryRecipeStep::TopSalienceProject,
        ];
        cfg.vault.hot_memory.max_bytes = 256;
        let big = lint_record_with(MemoryKind::Project, "x".repeat(1024));
        let inputs = inputs_with(std::slice::from_ref(&big), &cfg);
        let f = run(&inputs);
        assert_eq!(f.len(), 2);
        assert!(f.iter().any(|x| x.kind == Kind::HotMemoryOverBudget));
        assert!(
            f.iter()
                .any(|x| x.kind == Kind::DeferredCheck && x.tracking_issue == Some(259))
        );
    }
}
