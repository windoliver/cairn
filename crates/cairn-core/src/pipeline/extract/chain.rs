//! `ExtractChain` — sequential runner for `ExtractorWorker`s. See
//! `docs/superpowers/specs/2026-05-01-issue-74-llm-extractor-design.md`
//! §4.3.

use crate::pipeline::extract::{
    DiscardCandidate, ExtractError, ExtractOutput, ExtractorWorker, TruncationReason, WorkerRole,
};

/// Per-worker failure record.
#[derive(Debug)]
pub struct WorkerFailure {
    /// Stable name of the worker that failed.
    pub worker: &'static str,
    /// Role the worker held in the chain.
    pub role: WorkerRole,
    /// The error the worker produced.
    pub error: ExtractError,
}

/// Result of a successful `ExtractChain::run`. A chain run is
/// "successful" iff no `Gating` worker failed; `Augmenting` failures
/// are recorded in `failures` but do not change the `Ok` shape.
#[derive(Debug)]
pub struct ChainResult {
    /// Outputs collected from all workers that completed successfully.
    pub outputs: Vec<ExtractOutput>,
    /// Discards collected from all workers (LLM extractor may produce these).
    pub discards: Vec<DiscardCandidate>,
    /// Failures from `Augmenting` workers (never `Gating` — those go to `ChainRunError`).
    pub failures: Vec<WorkerFailure>,
    /// Whether and why any worker in the chain truncated.
    pub truncated: TruncationReason,
}

impl Default for ChainResult {
    fn default() -> Self {
        Self {
            outputs: Vec::new(),
            discards: Vec::new(),
            failures: Vec::new(),
            truncated: TruncationReason::None,
        }
    }
}

/// Error produced by `ExtractChain::run`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChainRunError {
    /// At least one `Gating` worker failed during this run. Extraction
    /// was suppressed by safety policy. The `partial` result carries
    /// any outputs from workers that completed successfully *before*
    /// the gate failure (always empty in the P0 regex→llm chain since
    /// regex is the only gate and runs first).
    #[error("chain gating worker(s) failed")]
    GatingFailed {
        /// Partial outputs from workers that completed before the gate failure.
        partial: ChainResult,
        /// The gating worker failures.
        failures: Vec<WorkerFailure>,
    },
}

/// Error produced by `ExtractChain::new`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractChainBuildError {
    /// An `Augmenting` worker appears before the chain has any
    /// `Gating` worker, or an `Augmenting` worker appears at all in a
    /// chain whose gating worker has not yet been seen.
    #[error(
        "augmenting worker `{worker}` at position {position} has no preceding gating worker"
    )]
    AugmentingBeforeGating {
        /// Name of the worker that caused the violation.
        worker: &'static str,
        /// Zero-based index of the offending worker in the chain.
        position: usize,
    },
    /// More than one `Gating` worker appears in the chain. P0 chains
    /// must have exactly one gate; multi-gate semantics with
    /// output-buffering rollback are deferred (spec §4.3 + §10).
    #[error(
        "multiple gating workers ({first} at {first_pos}, {second} at {second_pos}); \
         P0 chains must have exactly one gate"
    )]
    MultipleGatingWorkers {
        /// Name of the first gating worker.
        first: &'static str,
        /// Position of the first gating worker.
        first_pos: usize,
        /// Name of the second gating worker.
        second: &'static str,
        /// Position of the second gating worker.
        second_pos: usize,
    },
}

/// Sequential runner for a fixed list of `ExtractorWorker`s.
///
/// Construction validates worker ordering per spec §4.3. The `run`
/// method sequences the workers and implements monotonic eligibility
/// narrowing.
pub struct ExtractChain {
    workers: Vec<Box<dyn ExtractorWorker>>,
}

impl ExtractChain {
    /// Validate worker ordering per spec §4.3:
    /// - empty chain is allowed
    /// - non-empty chain must contain exactly one `Gating` worker
    /// - the `Gating` worker must precede any `Augmenting` workers
    ///
    /// # Errors
    ///
    /// Returns [`ExtractChainBuildError::AugmentingBeforeGating`] if an
    /// `Augmenting` worker appears before a `Gating` worker.
    ///
    /// Returns [`ExtractChainBuildError::MultipleGatingWorkers`] if more
    /// than one `Gating` worker appears.
    pub fn new(
        workers: Vec<Box<dyn ExtractorWorker>>,
    ) -> Result<Self, ExtractChainBuildError> {
        let mut seen_gating: Option<(&'static str, usize)> = None;
        for (i, w) in workers.iter().enumerate() {
            match w.role() {
                WorkerRole::Gating => {
                    if let Some((first, first_pos)) = seen_gating {
                        return Err(ExtractChainBuildError::MultipleGatingWorkers {
                            first,
                            first_pos,
                            second: w.name(),
                            second_pos: i,
                        });
                    }
                    seen_gating = Some((w.name(), i));
                }
                WorkerRole::Augmenting => {
                    if seen_gating.is_none() {
                        return Err(ExtractChainBuildError::AugmentingBeforeGating {
                            worker: w.name(),
                            position: i,
                        });
                    }
                }
            }
        }
        Ok(Self { workers })
    }

    /// Access the underlying worker list (used by `run` in Task 5 and chain inspectors).
    // `run` is added in Task 5 (Commit C); allow dead_code until then.
    #[allow(dead_code)]
    pub(crate) fn workers(&self) -> &[Box<dyn ExtractorWorker>] {
        &self.workers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::extract::{ExtractBudget, ExtractInput, ExtractResult, TextSpan};

    struct StubGate;
    struct StubAug;

    #[async_trait::async_trait]
    impl ExtractorWorker for StubGate {
        fn name(&self) -> &'static str {
            "stub_gate"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Gating
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::regex_default()
        }

        async fn extract(
            &self,
            _: &ExtractInput<'_>,
        ) -> Result<ExtractResult, ExtractError> {
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                llm_eligible_spans: vec![],
            })
        }
    }

    #[async_trait::async_trait]
    impl ExtractorWorker for StubAug {
        fn name(&self) -> &'static str {
            "stub_aug"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Augmenting
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::llm_default()
        }

        async fn extract(
            &self,
            _: &ExtractInput<'_>,
        ) -> Result<ExtractResult, ExtractError> {
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                llm_eligible_spans: vec![],
            })
        }
    }

    fn gate() -> Box<dyn ExtractorWorker> {
        Box::new(StubGate)
    }

    fn aug() -> Box<dyn ExtractorWorker> {
        Box::new(StubAug)
    }

    #[test]
    fn empty_chain_constructs() {
        assert!(ExtractChain::new(vec![]).is_ok());
    }

    #[test]
    fn gate_only_constructs() {
        assert!(ExtractChain::new(vec![gate()]).is_ok());
    }

    #[test]
    fn gate_then_aug_constructs() {
        assert!(ExtractChain::new(vec![gate(), aug()]).is_ok());
    }

    #[test]
    fn gate_then_two_augs_constructs() {
        assert!(ExtractChain::new(vec![gate(), aug(), aug()]).is_ok());
    }

    #[test]
    fn aug_only_rejected() {
        assert!(matches!(
            ExtractChain::new(vec![aug()]),
            Err(ExtractChainBuildError::AugmentingBeforeGating { .. })
        ));
    }

    #[test]
    fn aug_then_gate_rejected() {
        assert!(matches!(
            ExtractChain::new(vec![aug(), gate()]),
            Err(ExtractChainBuildError::AugmentingBeforeGating { .. })
        ));
    }

    #[test]
    fn two_gates_rejected() {
        assert!(matches!(
            ExtractChain::new(vec![gate(), gate()]),
            Err(ExtractChainBuildError::MultipleGatingWorkers { .. })
        ));
    }

    #[test]
    fn gate_aug_gate_rejected() {
        assert!(matches!(
            ExtractChain::new(vec![gate(), aug(), gate()]),
            Err(ExtractChainBuildError::MultipleGatingWorkers { .. })
        ));
    }

    #[test]
    fn two_augs_rejected() {
        assert!(matches!(
            ExtractChain::new(vec![aug(), aug()]),
            Err(ExtractChainBuildError::AugmentingBeforeGating { .. })
        ));
    }

    // Verify the workers() accessor returns the right count (used by chain inspectors).
    #[test]
    fn workers_accessor_returns_all_workers() {
        let chain = ExtractChain::new(vec![gate(), aug()]).expect("valid chain");
        assert_eq!(chain.workers().len(), 2);
    }

    // Ensure TextSpan is usable in the test scope (compile-time guard for imports).
    #[test]
    fn text_span_is_accessible() {
        let _s = TextSpan::new(0, 10);
    }
}
