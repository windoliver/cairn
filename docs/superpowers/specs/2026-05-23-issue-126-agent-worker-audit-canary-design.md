# Agent Worker Audit And Canary Controls Design - Issue #126

**Date:** 2026-05-23
**Issue:** [#126 - Add agent worker audit, cost, and canary controls](https://github.com/windoliver/cairn/issues/126)
**Brief sections:** section 11.3 Constraint gates; section 15 Evaluation
**Related sections:** section 4 AgentProvider; section 5.2.a AgentExtractor; section 10.2 DreamWorker
**Status:** Approved direction, pending written-spec review

---

## 1. Scope

Implement issue #126 as the control and reporting layer that sits above the
AgentProvider contract from issue #124 and the agent-mode extractor/dream
workers from issue #125.

This PR adds:

- A body-free agent-worker audit model that records per-worker cost, tool calls,
  generated candidates, accepted candidates, acceptance rate, failure modes,
  identity, and scope.
- Pure aggregation helpers that summarize multiple agent-worker audit records
  for operator-facing reports.
- Canary rollout controls that can keep agent-mode workers paused, enable them
  for a bounded canary percentage, promote them to enabled, or roll them back
  when audit-derived thresholds fail.
- Lint and evaluation report data that exposes agent-mode cost and benefit
  without leaking record bodies or tool output bodies.
- Tests covering audit metrics, canary pause/rollback behavior, and report
  projection.

Out of scope:

- Production rollout automation, schedulers, remote control planes, or
  fleet-wide deployment automation.
- New external agent runtime adapters.
- Changing how the AgentProvider executes tools or models.
- Broadening agent-mode worker enablement beyond the already configured
  extractor/dream worker paths.

## 2. Current Context

Issue #124 has already established the pure AgentProvider spawn contract in
`cairn-core::contract::agent_provider`. That contract records run status,
typed abort errors, budget consumed, attempted tool calls, and policy trace.

Issue #125 is closed and has unblocked issue #126. The remaining gap is not
whether a single agent run can be constrained; it is whether operators can see
the cost and benefit of agent-mode workers over time and whether agent mode can
be paused or rolled back before it becomes broadly active.

The design should preserve the current AgentProvider result shape instead of
adding issue-specific reporting fields directly to `AgentRun`. `AgentRun`
remains the per-spawn contract artifact. Issue #126 adds host-owned audit and
rollout projections over those runs.

## 3. Architecture

Keep this work in deterministic, pure layers first.

| Layer | Location | Responsibility |
|---|---|---|
| Audit model | `cairn-core::domain::agent_audit` | Body-free event and aggregate types for agent-worker outcomes. |
| Canary model | `cairn-core::domain::agent_canary` | Pure rollout state, threshold evaluation, pause, promote, and rollback decisions. |
| Agent run mapping | `cairn-core::domain::agent_audit` | Convert AgentProvider run facts plus worker metadata into audit records. |
| Lint projection | `cairn-core::verbs::lint` and CLI lint report rendering | Include agent audit summary in JSON and markdown report data. |
| Evaluation projection | `cairn-workflows::evaluation` | Include agent audit summary in deterministic evaluation reports. |

The first implementation should not require a database migration. Existing
report paths can consume in-memory audit records in tests. Persistent metrics
or workflow event wiring belongs to production automation follow-up work.

## 4. Audit Record Shape

An agent-worker audit record represents one logical worker invocation, not an
entire deployment window.

Required fields:

- `operation_id`: stable operation or workflow id for correlation.
- `worker_kind`: `extractor` or `dream`.
- `worker_name`: stable worker label, such as `agent_extractor` or
  `agent_dream`.
- `agent_identity`: the `agt:` identity used by the worker.
- `scope`: tenant, workspace, user, and agent dimensions available to the
  caller, represented with the existing body-free scope shape where possible.
- `status`: completed, rejected, aborted, or rolled back.
- `generated_candidates`: number of candidates produced by the worker.
- `accepted_candidates`: number of candidates accepted by the host pipeline.
- `budget_consumed`: turns, tool calls, and cost units.
- `failure_mode`: optional typed failure mode for failed runs.
- `canary_label`: optional rollout cohort label used for canary accounting.

The model must not include:

- Raw prompt text.
- Raw record body text.
- Tool output bodies.
- Generated candidate bodies.

Candidate counts are sufficient for issue #126. Quality and body-level review
belong to existing evidence, flush-plan, and evaluation fixtures.

## 5. Aggregate Metrics

The aggregate view computes operator-facing cost and benefit.

Per worker and per canary cohort, report:

- total runs
- completed runs
- rejected or aborted runs
- generated candidates
- accepted candidates
- acceptance rate
- turns consumed
- tool calls consumed
- cost units consumed
- failure mode counts

Acceptance rate is:

```text
accepted_candidates / generated_candidates
```

When `generated_candidates == 0`, the aggregate reports `None` instead of `0`.
That avoids implying poor quality when the worker never had an opportunity to
produce candidates.

Failure modes should stay typed and compact. Initial values should map cleanly
from `AgentProviderError` and host outcomes:

- `budget_exceeded`
- `wall_clock_exceeded`
- `tool_not_allowed`
- `mutating_verb_not_scoped`
- `invalid_output`
- `provider_unavailable`
- `host_rejected_candidates`
- `unknown`

## 6. Canary Controls

Canary control is a pure state machine used by hosts before dispatching an
agent-mode worker and after summarizing its audit records.

States:

- `paused`: do not dispatch agent-mode workers.
- `canary`: dispatch only for a bounded percentage or explicit cohort.
- `enabled`: dispatch for all configured eligible traffic.
- `rolled_back`: do not dispatch until an operator explicitly resets the state.

Inputs:

- `rollout_percent`: integer 0 through 100.
- `min_runs`: minimum completed or failed runs before judging the canary.
- `min_acceptance_rate`: optional minimum acceptance rate.
- `max_failure_rate`: optional maximum failed-run ratio.
- `max_cost_units_per_accepted_candidate`: optional cost ceiling.
- `pause_requested`: explicit operator pause.
- `rollback_requested`: explicit operator rollback.

Decisions:

- `paused` and `rolled_back` always deny dispatch.
- `canary` allows dispatch only when the candidate is in cohort.
- `enabled` allows dispatch for all eligible agent-mode traffic.
- Explicit rollback wins over all metric decisions.
- Explicit pause wins over metric promotion.
- A canary with insufficient runs remains in canary.
- A canary that violates any threshold rolls back with a compact reason.
- A canary that satisfies every configured threshold may promote to enabled.

The state machine should be deterministic and side-effect free. Persistence and
automatic scheduling are future production automation.

## 7. Report Surfaces

Lint reports should expose the audit aggregate so local operators can answer:

- How much did agent mode cost?
- How many tool calls did it use?
- How many candidates did it generate?
- How many were accepted?
- What failure modes occurred?
- Is agent mode paused, canarying, enabled, or rolled back?

Evaluation reports should expose the same aggregate because brief section 15
requires every new workflow, adapter, or contract behavior to ship with
evaluation. Evaluation output should use deterministic, body-free data so it is
safe for CI snapshots and local operator review.

The JSON shape should favor stable machine-readable fields. Markdown rendering
can be concise, for example:

```text
## Agent worker audit

- state: canary
- runs: 10
- accepted candidates: 7 / 12
- cost units: 850
- tool calls: 18
- failures: budget_exceeded=1, provider_unavailable=1
```

If no agent audit data exists, reports should state that no agent-worker audit
records were observed rather than treating that as success or failure.

## 8. Identity And Scope

Audit records must preserve both identity and scope:

- The worker identity must remain an `agt:` identity.
- Scope must be body-free and suitable for report rendering.
- Scope must be included even on rejected and aborted runs.
- Canary cohort labels must not replace identity or scope; they are an
  additional rollout dimension.

This keeps issue #126 aligned with the acceptance criterion that audit records
preserve identity and scope.

## 9. Error Handling

Audit construction should be infallible where possible. Invalid identities,
invalid canary percentages, and impossible thresholds should be rejected when
building the policy configuration.

Report rendering should not fail because audit data is empty. Empty input
returns an empty aggregate with `acceptance_rate = None`.

Canary evaluation should return a typed decision that includes a short reason:

- `dispatch_allowed`
- `dispatch_denied_paused`
- `dispatch_denied_rolled_back`
- `dispatch_denied_outside_canary`
- `remain_canary_insufficient_data`
- `promote_to_enabled`
- `rollback_threshold_failed`

Reasons must be safe to include in lint and evaluation reports.

## 10. Testing

The implementation should use test-first coverage at the smallest layer that
captures each behavior.

Core audit tests:

- Aggregates cost, tool calls, generated candidates, accepted candidates, and
  failure modes per worker.
- Computes acceptance rate when generated candidates are nonzero.
- Reports no acceptance rate when no candidates were generated.
- Preserves agent identity and scope for completed and aborted runs.

Core canary tests:

- Paused rollout denies dispatch.
- Rolled-back rollout denies dispatch.
- Canary rollout denies traffic outside the cohort.
- Canary rollout remains in canary before `min_runs`.
- Canary rollout rolls back when failure rate exceeds threshold.
- Canary rollout promotes when all thresholds pass.
- Explicit rollback wins over promotion.

Report tests:

- Lint JSON includes the agent audit summary.
- Lint markdown renders the agent audit summary without record bodies.
- Evaluation report includes the agent audit summary.
- Empty audit input renders an explicit no-data state.

## 11. Non-Goals And Follow-Ups

This issue should not add production rollout automation. A P3 task can
persist canary state, schedule canary windows, integrate with a remote control
plane, and connect rollback to fleet deployment workflows.

This issue also should not add a new agent runtime. It should consume the
existing AgentProvider run data and host-level candidate acceptance outcomes.

Future work can add:

- Persistent metrics rows for agent-worker audit records.
- CLI operator commands for viewing and changing canary state.
- SRE dashboard panels over the same aggregate.
- Workflow automation that flips canary state after a configured observation
  window.
