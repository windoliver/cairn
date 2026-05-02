//! `ExtractChain` — sequential runner for `ExtractorWorker`s. See
//! `docs/superpowers/specs/2026-05-01-issue-74-llm-extractor-design.md`
//! §4.3.

use crate::pipeline::extract::body::BodyResolution;
use crate::pipeline::extract::{
    DiscardCandidate, ExtractError, ExtractInput, ExtractOutput, ExtractorWorker, TextSpan,
    TruncationReason, WorkerRole,
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
    #[error("augmenting worker `{worker}` at position {position} has no preceding gating worker")]
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
    pub fn new(workers: Vec<Box<dyn ExtractorWorker>>) -> Result<Self, ExtractChainBuildError> {
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

    /// Access the underlying worker list (used by chain inspectors and tests).
    #[allow(dead_code)] // used in tests; future callers expected when chain is wired up
    pub(crate) fn workers(&self) -> &[Box<dyn ExtractorWorker>] {
        &self.workers
    }

    /// Run the chain sequentially, implementing monotonic eligibility
    /// narrowing per spec §4.3.
    ///
    /// Returns `Ok(ChainResult)` on the happy path — legitimately empty
    /// or with extraction outputs. Augmenting-worker failures are
    /// recorded in `ChainResult.failures` but do not affect the `Ok`
    /// shape.
    ///
    /// Returns `Err(ChainRunError::GatingFailed { .. })` iff any
    /// `Gating` worker errored. The type system forces callers to
    /// handle this arm and prevents them from accidentally treating a
    /// safety-policy failure as legitimate empty extraction.
    ///
    /// # Errors
    ///
    /// Returns [`ChainRunError::GatingFailed`] when a `Gating` worker
    /// returns an error.
    pub async fn run(&self, input: &ExtractInput<'_>) -> Result<ChainResult, ChainRunError> {
        // Initial running eligibility: use what the caller provided.
        // If empty AND body has length, seed with whole-body span so
        // the first worker sees the full body (§4.3 step 2).
        let body_len = match &input.body {
            BodyResolution::Resolved(rb) => rb.text().len(),
            _ => 0,
        };

        // Cast is safe: MAX_BODY_LEN_FOR_REGEX is 64 KiB << u32::MAX.
        #[allow(clippy::cast_possible_truncation)]
        let body_len_u32 = body_len as u32;
        let mut eligible: Vec<TextSpan> = if input.eligible_spans.is_empty() && body_len > 0 {
            vec![TextSpan::new(0, body_len_u32)]
        } else {
            input.eligible_spans.clone()
        };

        let mut outputs: Vec<ExtractOutput> = Vec::new();
        // `discards` is populated by LLM workers in future tasks; kept
        // mutable here so the field flows through `ChainResult` correctly.
        let discards: Vec<DiscardCandidate> = Vec::new();
        let mut failures: Vec<WorkerFailure> = Vec::new();
        let mut truncated = TruncationReason::None;

        for w in &self.workers {
            let sub_input = ExtractInput {
                event: input.event,
                body: input.body.clone(),
                eligible_spans: eligible.clone(),
            };
            match w.extract(&sub_input).await {
                Ok(r) => {
                    // Validate every output against current eligibility;
                    // drop any output whose source_span falls outside.
                    for out in r.outputs {
                        if span_in_eligibility(out.source_span(), &eligible) {
                            outputs.push(out);
                        } else {
                            tracing::warn!(
                                worker = w.name(),
                                "chain: output source_span outside current eligibility; dropping"
                            );
                        }
                    }
                    // Monotonic narrowing: clamp returned eligibility to
                    // the intersection with the current window. Widening
                    // is rejected and logged.
                    let clamped = clamp_intersection(&r.llm_eligible_spans, &eligible);
                    // Warn if a widening attempt was detected: returned was non-empty
                    // and clamping changed it (meaning at least one returned span
                    // extended outside the current eligibility).
                    let widened = !r.llm_eligible_spans.is_empty()
                        && r.llm_eligible_spans.iter().any(|rs| {
                            !eligible
                                .iter()
                                .any(|e| e.start <= rs.start && rs.end <= e.end)
                        });
                    if widened {
                        tracing::warn!(worker = w.name(), "chain.eligibility_widening");
                    }
                    eligible = clamped;
                    if r.truncated != TruncationReason::None {
                        truncated = r.truncated;
                    }
                }
                Err(e) => {
                    let role = w.role();
                    failures.push(WorkerFailure {
                        worker: w.name(),
                        role,
                        error: e,
                    });
                    if role == WorkerRole::Gating {
                        // Fail closed: collapse eligibility so no
                        // subsequent augmenting worker can run on
                        // unchecked input.
                        eligible = vec![];
                    }
                }
            }
        }

        // Partition collected failures into gating vs augmenting.
        let mut gating_failures: Vec<WorkerFailure> = Vec::new();
        let mut aug_failures: Vec<WorkerFailure> = Vec::new();
        for f in failures {
            if f.role == WorkerRole::Gating {
                gating_failures.push(f);
            } else {
                aug_failures.push(f);
            }
        }

        if gating_failures.is_empty() {
            Ok(ChainResult {
                outputs,
                discards,
                failures: aug_failures,
                truncated,
            })
        } else {
            // Fail closed: return Err so callers cannot mistake a safety
            // failure for legitimate empty extraction.
            let partial = ChainResult {
                outputs,
                discards,
                failures: aug_failures,
                truncated,
            };
            Err(ChainRunError::GatingFailed {
                partial,
                failures: gating_failures,
            })
        }
    }
}

/// Returns `true` when `span` falls within at least one of the
/// `eligible` ranges. `None` source spans (hook/tool-frame events
/// without text provenance) are always considered in-eligibility.
fn span_in_eligibility(span: Option<TextSpan>, eligible: &[TextSpan]) -> bool {
    match span {
        None => true,
        Some(s) => eligible
            .iter()
            .any(|e| e.start <= s.start && s.end <= e.end),
    }
}

/// Compute the strict interval-set intersection of `returned` with the
/// `current` eligibility window. Empty `returned` ⇒ empty result, by
/// design: a worker that returns no LLM-eligible spans is reporting
/// suppression, not silence. The chain MUST forward that suppression.
fn clamp_intersection(returned: &[TextSpan], current: &[TextSpan]) -> Vec<TextSpan> {
    let mut out = Vec::new();
    for r in returned {
        for e in current {
            let lo = r.start.max(e.start);
            let hi = r.end.min(e.end);
            if lo < hi {
                out.push(TextSpan::new(lo, hi));
            }
        }
    }
    out
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

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
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

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
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

    // ────────────────────────────────────────
    // Task 5: ExtractChain::run tests
    // ────────────────────────────────────────

    use crate::domain::{
        ActorChainEntry, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, ChainRole,
        Identity, PayloadHash, Rfc3339Timestamp,
    };
    use crate::pipeline::extract::body::BodyResolution;

    fn fixture_event() -> crate::domain::CaptureEvent {
        let payload = CapturePayload::Hook {
            hook_name: "SessionStart".to_owned(),
            tool_name: None,
        };
        let source_family = payload.source_family();
        let ts = Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").expect("valid ts");
        let sensor = Identity::parse("snr:local:hook:test:v1").expect("valid sensor");
        crate::domain::CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid"),
            sensor_id: sensor.clone(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor,
                at: ts.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess-test".into()),
                turn_id: None,
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(format!("sha256:{}", "ab".repeat(32)))
                .expect("valid hash"),
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
            captured_at: ts,
            payload,
            source_family,
        }
    }

    #[tokio::test]
    async fn empty_chain_returns_ok_empty() {
        let chain = ExtractChain::new(vec![]).expect("valid chain");
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let res = chain.run(&input).await.expect("empty chain should succeed");
        assert!(res.outputs.is_empty());
        assert!(res.failures.is_empty());
    }

    struct FailingGate;

    #[async_trait::async_trait]
    impl ExtractorWorker for FailingGate {
        fn name(&self) -> &'static str {
            "failing_gate"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Gating
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::regex_default()
        }

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            Err(ExtractError::SpanOutOfBounds {
                worker: "failing_gate",
            })
        }
    }

    #[tokio::test]
    async fn gating_worker_error_returns_gating_failed() {
        let chain = ExtractChain::new(vec![Box::new(FailingGate)]).expect("valid chain");
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let res = chain.run(&input).await;
        assert!(matches!(res, Err(ChainRunError::GatingFailed { .. })));
    }

    struct FailingAug;

    #[async_trait::async_trait]
    impl ExtractorWorker for FailingAug {
        fn name(&self) -> &'static str {
            "failing_aug"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Augmenting
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::llm_default()
        }

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            Err(ExtractError::SpanOutOfBounds {
                worker: "failing_aug",
            })
        }
    }

    #[tokio::test]
    async fn augmenting_worker_error_recorded_but_run_succeeds() {
        let chain =
            ExtractChain::new(vec![Box::new(StubGate), Box::new(FailingAug)]).expect("valid");
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![],
        };
        let res = chain.run(&input).await.expect("augmenting failure -> Ok");
        assert_eq!(res.failures.len(), 1);
        assert_eq!(res.failures[0].role, WorkerRole::Augmenting);
    }

    // Worker that records the eligible_spans it received, for inspection.
    use std::sync::{Arc, Mutex};

    struct SpyAug {
        received: Arc<Mutex<Vec<Vec<TextSpan>>>>,
    }

    #[async_trait::async_trait]
    impl ExtractorWorker for SpyAug {
        fn name(&self) -> &'static str {
            "spy_aug"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Augmenting
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::llm_default()
        }

        async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            self.received
                .lock()
                .expect("mutex not poisoned")
                .push(input.eligible_spans.clone());
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                llm_eligible_spans: vec![],
            })
        }
    }

    // Gate that returns a narrow eligibility window.
    struct NarrowingGate;

    #[async_trait::async_trait]
    impl ExtractorWorker for NarrowingGate {
        fn name(&self) -> &'static str {
            "narrowing_gate"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Gating
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::regex_default()
        }

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                // Narrows eligibility to bytes 2..8 only.
                llm_eligible_spans: vec![TextSpan::new(2, 8)],
            })
        }
    }

    // Gate that tries to return a WIDER span than it was given — the chain
    // must clamp this to the intersection.
    struct WideningGate;

    #[async_trait::async_trait]
    impl ExtractorWorker for WideningGate {
        fn name(&self) -> &'static str {
            "widening_gate"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Gating
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::regex_default()
        }

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                // Returns span 0..100 which is wider than the initial 3..7.
                llm_eligible_spans: vec![TextSpan::new(0, 100)],
            })
        }
    }

    #[tokio::test]
    async fn eligibility_narrowing_passed_to_next_worker() {
        // The NarrowingGate narrows eligibility from 0..20 to 2..8.
        // The spy aug should receive 2..8 (the intersection of 0..20 and 2..8).
        let received = Arc::new(Mutex::new(Vec::new()));
        let spy = SpyAug {
            received: Arc::clone(&received),
        };
        let chain =
            ExtractChain::new(vec![Box::new(NarrowingGate), Box::new(spy)]).expect("valid chain");
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            // Provide an initial window that the gate can narrow into.
            eligible_spans: vec![TextSpan::new(0, 20)],
        };
        chain.run(&input).await.expect("ok");
        let calls = received.lock().expect("mutex not poisoned");
        // The spy aug should have been called once with the narrowed spans.
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], vec![TextSpan::new(2, 8)]);
    }

    #[tokio::test]
    async fn worker_cannot_widen_eligibility() {
        // Give the chain initial eligibility 3..7. The WideningGate returns
        // 0..100 which is wider. After clamping the aug must see 3..7 (the
        // intersection of 0..100 and 3..7).
        let received = Arc::new(Mutex::new(Vec::new()));
        let spy = SpyAug {
            received: Arc::clone(&received),
        };
        let chain =
            ExtractChain::new(vec![Box::new(WideningGate), Box::new(spy)]).expect("valid chain");
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![TextSpan::new(3, 7)],
        };
        chain.run(&input).await.expect("ok");
        let calls = received.lock().expect("mutex not poisoned");
        assert_eq!(calls.len(), 1);
        // Intersection of 0..100 (returned) ∩ 3..7 (input) = 3..7.
        assert_eq!(calls[0], vec![TextSpan::new(3, 7)]);
    }

    // Gate that emits an output with a source_span inside its eligibility.
    struct OutputInSpanGate;

    #[async_trait::async_trait]
    impl ExtractorWorker for OutputInSpanGate {
        fn name(&self) -> &'static str {
            "output_in_span_gate"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Gating
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::regex_default()
        }

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            use crate::domain::CaptureEventId;
            use crate::domain::taxonomy::MemoryKind;
            use crate::pipeline::extract::draft::{Confidence, KindHint, MemoryDraft};
            Ok(ExtractResult {
                outputs: vec![
                    ExtractOutput::Draft(MemoryDraft {
                        kind_hint: KindHint::from(MemoryKind::User),
                        body: "in-span content".to_owned(),
                        confidence: Confidence::try_from(0.9_f32).expect("valid"),
                        source_event: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                            .expect("valid ulid"),
                        source_span: Some(TextSpan::new(2, 8)), // inside eligibility 0..10
                        trigger_id: None,
                    }),
                    ExtractOutput::Draft(MemoryDraft {
                        kind_hint: KindHint::from(MemoryKind::User),
                        body: "out-of-span content".to_owned(),
                        confidence: Confidence::try_from(0.9_f32).expect("valid"),
                        source_event: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                            .expect("valid ulid"),
                        source_span: Some(TextSpan::new(20, 30)), // OUTSIDE eligibility 0..10
                        trigger_id: None,
                    }),
                ],
                truncated: TruncationReason::None,
                llm_eligible_spans: vec![TextSpan::new(0, 10)],
            })
        }
    }

    #[tokio::test]
    async fn output_outside_eligibility_is_dropped() {
        let chain = ExtractChain::new(vec![Box::new(OutputInSpanGate)]).expect("valid chain");
        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![TextSpan::new(0, 10)], // eligibility 0..10
        };
        let res = chain.run(&input).await.expect("ok");
        // Only the in-span output (2..8) survives; out-of-span (20..30) is dropped.
        assert_eq!(res.outputs.len(), 1);
    }

    struct GateReturnsEmpty;

    #[async_trait::async_trait]
    impl ExtractorWorker for GateReturnsEmpty {
        fn name(&self) -> &'static str {
            "gate_empty"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Gating
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::regex_default()
        }

        async fn extract(&self, _: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                llm_eligible_spans: vec![], // explicit suppression
            })
        }
    }

    struct AugRecorder {
        received: Arc<Mutex<Vec<TextSpan>>>,
    }

    #[async_trait::async_trait]
    impl ExtractorWorker for AugRecorder {
        fn name(&self) -> &'static str {
            "aug_recorder"
        }

        fn role(&self) -> WorkerRole {
            WorkerRole::Augmenting
        }

        fn budget(&self) -> ExtractBudget {
            ExtractBudget::llm_default()
        }

        async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
            *self.received.lock().expect("mutex not poisoned") = input.eligible_spans.clone();
            Ok(ExtractResult {
                outputs: vec![],
                truncated: TruncationReason::None,
                llm_eligible_spans: vec![],
            })
        }
    }

    #[tokio::test]
    async fn gate_returning_empty_eligibility_blocks_downstream_aug() {
        // A gate that succeeds but returns empty llm_eligible_spans should
        // narrow downstream to nothing — the augmenter must see eligible_spans = [].
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let chain = ExtractChain::new(vec![
            Box::new(GateReturnsEmpty),
            Box::new(AugRecorder {
                received: Arc::clone(&recorded),
            }),
        ])
        .expect("valid chain");

        let event = fixture_event();
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            // Start with a non-empty eligible span (would be inherited by first worker).
            eligible_spans: vec![TextSpan::new(0, 20)],
        };

        let _ = chain.run(&input).await.expect("chain should succeed");
        let received_spans = recorded.lock().expect("mutex not poisoned").clone();
        assert!(
            received_spans.is_empty(),
            "augmenter should have received empty eligible_spans, got {received_spans:?}"
        );
    }
}
