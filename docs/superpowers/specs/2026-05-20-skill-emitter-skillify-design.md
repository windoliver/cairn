# SkillEmitter Skillify Design - Issue #112

**Date:** 2026-05-20
**Issue:** [#112 - Implement SkillEmitter base pipeline](https://github.com/windoliver/cairn/issues/112)
**Brief sections:** section 5.0.b Auto-learning loop; section 10 DreamWorkflow; section 11.b Skillify; section 11.3 promotion predicate
**Status:** Approved

---

## 1. Scope

Implement the full section 11.b Skillify pipeline for issue #112 on top of the
current `origin/main` workflow stack. This is broader than the minimal issue
text: the feature must produce all ten Skillify artifacts, keep candidates
versioned and reversible, run resolver and lint audits, schedule from Deep
Dream and explicit skillify triggers, and prevent unverified trajectories from
becoming live skills.

This PR adds:

- A pure `SkillEmitter` / Skillify domain model in `cairn-core`.
- A store-backed `SkillEmitter` workflow in `cairn-workflows`.
- Candidate bundle materialization under `.cairn/evolution/skillify/`.
- Promotion into live `skills/`, resolver metadata, scripts, tests, evals,
  smoke tests, and filing rules only after all gates pass.
- `cairn lint --skill` checks for completeness, resolver reachability, DRY
  lane overlap, filing rules, rollback integrity, and daily health.
- Fixture coverage for successful trajectories, failed trajectories, missing
  evidence, lint failures, promotion, scheduling, and rollback.

Out of scope: implementing the P2 `AgentProvider` method surface. The first
implementation authors artifacts through bounded `LLMProvider` JSON mode and
keeps a clean extension point for later agent-authored SkillPacks.

---

## 2. Architecture

The implementation uses the existing crate boundaries.

| Layer | Location | Responsibility |
|---|---|---|
| Pure model and validation | `cairn-core::pipeline::skillify` | Candidate extraction rules, bundle schema, artifact model, gate reports, lane and trigger validation. No I/O. |
| Workflow execution | `cairn-workflows::skillify` | Candidate discovery, LLM JSON authoring, bundle materialization, gate execution, promotion and rollback plan generation. |
| CLI and lint | `cairn-cli::verbs::lint`, workflow/admin verbs | `cairn lint --skill`, candidate inspection, explicit enqueue, human-readable findings, and JSON output. |
| Fixtures | `fixtures/v0/skillify/` | Successful and failed trajectories plus malformed bundles used by workflow and lint tests. |
| User-facing skill docs | `skills/cairn/examples/04-skillify-this.md` | Demonstrates the new Skillify pipeline instead of only ingesting `strategy_success`. |

`cairn-core` remains pure data and pure functions. It owns the serialized
contract for candidates, artifacts, and gate reports, but does not read the
vault or run tests. `cairn-workflows` owns all filesystem and store interaction
through existing contracts. `cairn-cli` remains the operator surface.

Durable mutations route through existing `FlushPlan` / WAL semantics. Candidate
bundle files are staged before promotion, but live activation is expressed as a
reviewable plan so rollback can remove resolver rows, deactivate skill metadata,
and archive generated artifacts without dangling scripts.

---

## 3. Candidate Model

`SkillifyCandidate` is the unit SkillEmitter reasons about. It is deterministic
for the same source window so retries and duplicate workflow jobs converge.

Required candidate fields:

- `candidate_id`: stable hash over source record ids, lane, trigger source, and
  normalized success criteria.
- `source_record_ids`: ordered evidence references to `trace`, `reasoning`,
  `strategy_success`, or explicit skillify records.
- `outcome`: closed enum such as `success`, `failure`, `unknown`, and
  `unverified`.
- `lane`: domain/subdomain key used by the DRY audit, for example
  `deploy.hotfix`.
- `triggers`: candidate resolver trigger phrases or patterns.
- `requires` and `provides`: declared dependencies and capabilities inferred
  from the trajectory.
- `scope`: source scope tuple; promotion cannot widen this without the existing
  review gate.
- `evidence`: structured success proof, tests/replay evidence, confidence, and
  source hashes.
- `status`: `candidate`, `blocked`, `ready_for_review`, `live`, `unhealthy`,
  `rolled_back`, or `archived`.

The extractor accepts two paths:

1. Explicit user path: an anchored `skillify this` signal or a user-authored
   `strategy_success` record with enough source context.
2. Background path: Deep Dream windows where successful traces satisfy the same
   evidence gate.

The extractor rejects failed, unknown, or unverified trajectories before LLM
authoring. Rejected trajectories may produce lint findings or
`knowledge_gap`/`strategy_failure` candidates, but they never produce live
skills.

---

## 4. Artifact Bundle

`SkillArtifactBundle` models the ten section 11.b artifacts as typed entries.
Each entry records its logical kind, relative candidate path, content hash,
source evidence refs, generated version, and validation status.

The canonical candidate layout is:

```text
.cairn/evolution/skillify/<candidate_id>/
|-- manifest.json
|-- gate-report.json
|-- bundle/
|   |-- skills/skill_<slug>.md
|   |-- scripts/<slug>.<ext>
|   |-- tests/unit/<slug>.<ext>
|   |-- tests/integration/<slug>.<ext>
|   |-- evals/llm/<slug>.json
|   |-- resolver/triggers.json
|   |-- resolver/eval.json
|   |-- audits/check-resolvable.json
|   |-- smoke/<slug>.json
|   `-- filing-rules.json
`-- versions/
    `-- v1/manifest.json
```

On promotion, the bundle is copied or materialized into the live vault layout:

- `skills/skill_<slug>.md`
- `skills/scripts/<slug>.<ext>`
- `skills/tests/...`
- `.cairn/resolver/skills/<skill_id>.json`
- `.cairn/evals/skills/<skill_id>/...`
- `.cairn/evolution/skillify/<candidate_id>/versions/vN/...`

The live skill frontmatter includes `skill_id`, `version`, `lane`, `triggers`,
`uses`, `requires`, `provides`, `files_to`, `candidate_id`, source evidence ids,
and status. The lane is the primary DRY key. `uses` and `files_to` are required
for lint and rollback to reason about scripts and write destinations.

---

## 5. Authoring Flow

The first implementation authors bundles through `LLMProvider` JSON mode:

1. Build a bounded prompt from the trajectory, source metadata, success
   criteria, tool-call sequence, and existing live skill lanes.
2. Request a JSON value matching the Skillify bundle schema.
3. Validate the JSON locally in `cairn-core`.
4. Render deterministic files from the validated JSON.
5. Hash every artifact and write the candidate manifest.

If no LLM provider is configured, the job exits as a permanent blocked
candidate with a clear lint finding. If the provider exceeds token or wall-clock
budget, the workflow records `blocked: budget_exceeded` and leaves the
candidate non-live. Invalid JSON or schema mismatch is treated as an authoring
failure and is retryable only when the scheduler classifies the error as
transient.

LLM output is never trusted directly. Every field is parsed into typed structs,
all paths are normalized as vault-relative paths, and generated scripts/tests
must remain inside the candidate bundle until promotion.

---

## 6. Gates

The gate report is the promotion boundary. A candidate can become live only
when every required gate passes.

Required gates:

1. `skill_contract`: `skill_*.md` exists and has required frontmatter.
2. `deterministic_script`: referenced script exists, is executable or runnable
   by declared command, and has bounded runtime metadata.
3. `unit_tests`: unit test artifact exists and passes.
4. `integration_tests`: integration test artifact exists and passes. An
   environment-gated integration test remains a blocking health failure until
   it runs successfully in an environment that declares the required capability.
5. `llm_evals`: eval spec exists and passes when LLM evals are configured.
6. `resolver_trigger`: resolver row exists and points to the skill.
7. `resolver_eval`: labeled intents route to the skill without false positives
   or false negatives.
8. `check_resolvable_and_dry`: skill resolves to its script, triggers are not
   ambiguous, and no live skill shares the same lane.
9. `e2e_smoke`: prompt to resolver to script to output succeeds.
10. `filing_rules`: declared `files_to` destinations are valid for writes the
    skill can perform.

Evidence gates are separate and always required: successful outcome evidence,
source record liveness, actor chain, scope, and source hashes must still match
at promotion time. If a source record was forgotten, tombstoned, or its consent
state no longer covers the promotion, the candidate is blocked.

---

## 7. Workflow Integration

Add a new workflow kind, `skillify.emit`, with `SkillifyPayload`:

- `key`: stable dedupe key.
- `trigger`: `explicit`, `deep_dream`, `manual_admin`, or `health_recheck`.
- `candidate_id`: optional when the job is discovering candidates.
- `bound_scope`: optional scope restriction.
- `source_record_ids`: optional explicit source set.

Enqueue paths:

- Explicit `skillify this` capture enqueues `skillify.emit` with source context
  from the active session or trace canvas.
- Deep Dream enqueues `skillify.emit` for successful trajectories discovered in
  the Deep Dream source window.
- `cairn lint --skill --fix-plan` or an admin workflow command can enqueue a
  health recheck for an existing live skill.

The handler is idempotent:

- Existing candidate with the same candidate id and source hashes is reused.
- Existing live skill with the same lane blocks promotion unless the new bundle
  is an explicit version update or rollback candidate.
- Candidate materialization is content-addressed, so retries do not duplicate
  files.

---

## 8. Promotion And Rollback

Promotion produces a `FlushPlan` with a Skillify-specific reason and the
required mutations to activate the bundle. If the existing enum is not expressive
enough, extend `PlanReason` with `Skillify { candidate_id, gate_count }` while
keeping `PlannedMutation::Evolve` for live skill version changes.

Activation must be atomic from the operator's perspective:

- Skill frontmatter status changes to `live`.
- Resolver row is activated.
- Script, tests, evals, smoke spec, and filing rules are installed.
- Candidate manifest records the live version and source hashes.
- Health status is initialized from the passing gate report.

Rollback creates a new plan that:

- Deactivates resolver rows for the rolled-back version.
- Restores the previous live version or marks the skill `rolled_back`.
- Moves superseded artifacts to the version archive.
- Leaves audit metadata and candidate history intact.
- Produces lint-clean state with no dangling script, resolver, or eval refs.

Rollback never deletes source evidence. If source evidence has been forgotten,
rollback still operates on artifact metadata and records the missing evidence as
a lint finding.

---

## 9. Lint And Daily Health

`cairn lint --skill` adds Skillify checks to the existing lint command. The
checks work against both candidate bundles and live skills.

Findings include:

- Missing artifact in the ten-step bundle.
- `uses` points to a missing script.
- Resolver trigger points to a missing skill.
- Skill exists but no resolver trigger reaches it.
- Duplicate or overlapping lane among live skills.
- Resolver eval false negative or false positive.
- Filing rules point outside valid vault subtrees.
- Live skill has failing unit, integration, eval, or smoke health.
- Candidate claims live status without a passing gate report.
- Rollback metadata cannot reconstruct the previous version.

Daily health is the same lint path run on schedule. A failure marks the skill
`unhealthy`, emits a lint finding, and can enqueue a `skillify.emit`
`health_recheck` job. It does not silently remove or mutate the live skill.

---

## 10. Error Handling

Errors are classified by recovery behavior:

- Permanent: missing LLM provider, unsupported script runtime, invalid candidate
  schema, failed evidence gate, consent no longer covers the promotion.
- Retryable: transient store errors, LLM provider unreachable, scheduler lease
  loss, temporary filesystem errors during candidate write.
- Health failure: generated tests or evals fail, resolver eval fails, lane
  overlap exists, smoke fails.

Permanent and health failures leave explicit candidate or skill health metadata.
Retryable failures use the existing scheduler retry/backoff path. No failure
path promotes a candidate with incomplete or failed evidence.

Sensitive record bodies are not logged at info level. Workflow logs and lint
findings use ids, hashes, lane names, and artifact paths.

---

## 11. Testing

Tests are written first.

Core unit tests:

- Successful trajectory data extracts a deterministic `SkillifyCandidate`.
- Failed or unverified trajectories are rejected before authoring.
- Bundle schema requires all ten artifact kinds.
- Lane overlap and resolver trigger ambiguity are detected.
- Path validation rejects absolute paths and `..` escapes.

Workflow tests:

- Successful fixture produces a candidate bundle with evidence and gate report.
- Failed trajectory fixture does not create a live skill.
- Missing tests or failed eval keeps candidate non-live.
- Explicit skillify trigger enqueues exactly one `skillify.emit` job.
- Deep Dream source window enqueues Skillify without duplicate candidates.
- Promotion plan includes version metadata and evidence refs.
- Rollback plan deactivates resolver rows and leaves no orphaned script refs.

CLI/lint tests:

- `cairn lint --skill` reports missing script, missing test, missing resolver,
  duplicate lane, resolver false negative, and invalid filing rules.
- A complete bundle returns no Skillify lint findings.
- A rolled-back skill with archived metadata returns no dangling-artifact
  finding.

Regression checks:

- `cargo test -p cairn-core skillify --locked`
- `cargo test -p cairn-workflows skillify --locked`
- `cargo test -p cairn-cli lint_skill --locked`
- `cargo clippy -p cairn-core -p cairn-workflows -p cairn-cli --all-targets --locked -- -D warnings`
- `scripts/check-core-boundary.sh`

---

## 12. Acceptance Criteria Mapping

| Issue acceptance criterion | Design coverage |
|---|---|
| Successful trajectory fixtures produce candidate skill artifacts with evidence. | Sections 3, 4, 5, and workflow tests. |
| Failed or unverified trajectories do not become durable skills. | Sections 3, 6, 10, and tests for failed/unverified fixtures. |
| Generated skills can be linted and rolled back. | Sections 8, 9, and CLI/lint tests. |
| Full section 11.b ten-artifact pipeline. | Sections 4, 6, and 9. |
| Scheduling from learning workflows. | Section 7. |
| Versioned and reversible artifacts. | Sections 4 and 8. |

---

## 13. Open Constraints

`AgentProvider` remains a P2 forward stub on the current baseline. This design
does not block full Skillify behavior because artifact authoring can use
`LLMProvider` JSON mode and deterministic local validation. A later
AgentProvider implementation can replace the authoring backend without changing
candidate ids, bundle manifests, gate reports, lint checks, promotion plans, or
rollback metadata.

Script runtimes are limited to runtimes already accepted by the repo or
explicitly declared by the generated skill. If a generated artifact requires a
runtime the current binary cannot execute in tests, the integration gate remains
failed until the operator supplies that runtime or the skill is regenerated with
a supported script target.
