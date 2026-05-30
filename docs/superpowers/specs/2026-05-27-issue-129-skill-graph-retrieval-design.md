# Issue 129 Dependency-Aware Skill Graph Retrieval Design

## Context

Issue: https://github.com/windoliver/cairn/issues/129

Design sources:

- `docs/design/design-brief.md` §7, Hot Memory
- `docs/design/design-brief.md` §11.a, Graph of Skills
- `docs/design/design-brief.md` §11.b, Skillify and SkillPacks

Dependency issue #128 is closed on `origin/main`. Its Skillify and SkillPack
metadata gives this slice an existing substrate: skill drafts and pack manifests
already carry `requires` and `provides`, `cairn lint --skill` already builds a
vault skill snapshot, and Skillify gate runners already use that snapshot to
reject resolver conflicts.

## Goal

Implement the first dependency-aware retrieval slice for skills: retrieval can
include prerequisite skill context, incompatible skill combinations are avoided
or reported, and skill lint finds broken graph references.

## Non-Goals

- No external or large-scale team skill marketplace.
- No new persistent SQL schema in this slice.
- No change to the public `cairn.mcp.v1` wire schema unless the implementation
  proves an existing explain/debug field cannot carry diagnostics.
- No autonomous mutation of live skills. Fixes remain reviewable lint findings
  or Skillify repair plans.

## Approaches Considered

### Recommended: Pure Core Resolver Over Skill Metadata

Add a pure `SkillGraphResolver` model under `cairn-core` that consumes skill
metadata from the existing Skillify lint snapshot. The resolver builds an
in-memory directed graph from `requires`, `provides`, and `conflicts`, returns
ordered prerequisite closures, and reports broken references, cycles, and
incompatibilities. CLI, hot-memory assembly, search explain diagnostics, and
Skillify gate checks call this shared resolver.

This is the smallest design that satisfies the issue while preserving Cairn's
invariant that the CLI, MCP, SDK, and skill surfaces share core behavior.

### Alternative: CLI-Only Vault Scan

Extend only `cairn lint --skill` and `cairn assemble_hot` to scan skill files.
This is faster to implement but duplicates graph semantics outside core and
would leave SDK/MCP/search behavior behind.

### Alternative: First-Class Store Graph Schema

Add persistent skill graph tables and make retrieval traverse stored edges.
This matches the long-term design brief direction, but it is too large for this
issue because #128 already produced usable metadata and the issue acceptance
criteria can be met without a migration.

## Data Model

Extend `cairn_core::pipeline::skillify::SkillLintSkill` with graph metadata:

- `requires: Vec<String>`: capability ids this skill needs.
- `provides: Vec<String>`: capability ids this skill contributes.
- `conflicts: Vec<String>`: skill ids, lanes, or capability ids that must not be
  selected with this skill.

The CLI snapshot builder reads these values from YAML frontmatter in promoted
skills and candidate bundles. `SkillSpecDraft` and `SkillPackManifest` already
carry `requires` and `provides`; this design adds `conflicts` to skill-level
metadata without requiring every existing skill file to declare it.

Missing `requires`, `provides`, or `conflicts` default to empty lists for legacy
skills. Empty metadata is valid, but lint can only reason about dependencies
for skills that declare them.

## Core Resolver

Create `crates/cairn-core/src/pipeline/skillify/graph.rs`.

Responsibilities:

- Normalize the skill snapshot into deterministic maps:
  - `skill_id -> skill`
  - `lane -> skill_id`
  - `provided capability -> provider skill_ids`
- Resolve each `requires` token to a provider. A token matches in this order:
  - exact `skill_id`
  - exact `lane`
  - exact `provides` capability
- Produce an ordered prerequisite closure for a requested skill.
- Detect and report:
  - missing dependency references
  - ambiguous dependency references where multiple providers match
  - dependency cycles
  - selected skills that conflict with each other

The resolver is pure Rust with no filesystem or store access. It sorts all
outputs by stable string keys so lint and tests do not drift across platforms.

## Retrieval Behavior

### Search

Search already has graph-leg explain plumbing for hybrid record retrieval. This
slice should not alter ranking of ordinary records. For skill/playbook-shaped
results, the search surface should expose dependency diagnostics when explain is
requested:

- the hit skill id
- ordered prerequisite skill ids
- skipped conflicts
- missing or ambiguous references

If the current public wire shape cannot express these diagnostics directly,
surface code should include them in existing explain/debug JSON rather than
adding a new `cairn.mcp.v1` field.

### Hot Memory

The active playbook source currently picks the single newest admissible playbook.
Update that path so callers can provide dependency candidates along with the
active playbook candidate. When budget allows, the assembler includes
prerequisite playbooks before the active playbook body, ordered from oldest
prerequisite to leaf skill.

Budget behavior is fail-closed and predictable:

- If the active playbook alone does not fit, return the existing
  over-budget behavior.
- If prerequisites do not fit, omit the lowest-priority prerequisite first and
  include an explain/debug exclusion when debug is enabled.
- Never include a conflicting prerequisite with the active playbook.

### Lint

Extend `lint_skill_snapshot` so `cairn lint --skill` reports graph health:

- missing dependency: `requires` points at no known skill, lane, or provided
  capability
- ambiguous dependency: `requires` matches more than one provider
- dependency cycle: following `requires` reaches an already-active node
- conflict violation: a selected prerequisite closure contains a declared
  conflict

The existing generated lint enum has no skill-graph-specific variants. To keep
this slice narrow, map graph issues onto existing skill lint kinds:

- missing or ambiguous dependency -> `SkillMissingArtifact`
- cycle or conflict violation -> `SkillDuplicateLane`

Messages must name the exact skill id and reference token so operators can fix
frontmatter without reading code.

## File Responsibilities

- `crates/cairn-core/src/pipeline/skillify/graph.rs`: pure graph types,
  resolver, closure ordering, and graph diagnostics.
- `crates/cairn-core/src/pipeline/skillify/lint.rs`: wire graph diagnostics
  into existing skill lint output.
- `crates/cairn-core/src/pipeline/skillify/mod.rs`: re-export graph resolver
  types needed by CLI, workflow gates, and tests.
- `crates/cairn-cli/src/verbs/lint.rs`: parse `requires`, `provides`, and
  `conflicts` frontmatter into `SkillLintSkill`.
- `crates/cairn-workflows/src/skillify/snapshot.rs`: include graph metadata in
  workflow-local snapshots so gate runners catch broken dependencies before
  promotion.
- `crates/cairn-core/src/verbs/assemble_hot/inputs.rs` and
  `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`: allow the
  active playbook source to receive and render prerequisite playbooks.
- `crates/cairn-core/src/verbs/search.rs` or the existing search explain path:
  surface skill graph diagnostics for explain-mode skill results if an existing
  extension point can carry them.

## Testing Strategy

Follow TDD. Every behavior starts with a failing test.

Core unit tests:

- resolver returns prerequisite closure in dependency order
- missing dependency produces a diagnostic
- ambiguous provider produces a diagnostic
- dependency cycle produces a diagnostic and terminates
- conflict in a selected closure produces a diagnostic

Skill lint tests:

- `lint_skill_snapshot` reports missing `requires`
- `lint_skill_snapshot` reports cycles and conflicts
- CLI `cairn lint --skill --json` includes graph messages from YAML
  frontmatter

Hot memory tests:

- active playbook includes prerequisite playbooks when budget allows
- conflicting prerequisite is excluded
- prerequisite over budget is omitted while active playbook remains

Search tests:

- explain-mode skill retrieval exposes prerequisite diagnostics without
  changing ordinary keyword or hybrid result ordering

Verification commands:

- `cargo test -p cairn-core skillify_graph --locked`
- `cargo test -p cairn-core assemble_hot --locked`
- `cargo test -p cairn-cli lint_skill --locked`
- `cargo test -p cairn-workflows skillify_gate_runners --locked`
- `cargo fmt --all --check`
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
- `./scripts/check-core-boundary.sh`

## Open Implementation Decision

Search diagnostics should use an existing explain/debug extension point if
possible. If the implementation discovers that no stable field can carry skill
graph diagnostics without schema abuse, the first implementation plan task
should document that gap and keep search behavior limited to core resolver tests
plus lint/hot-memory integration until an IDL-backed follow-up is approved.

## Self-Review

- No placeholder requirements remain.
- The design maps all issue acceptance criteria to core resolver, hot memory,
  search diagnostics, and lint behavior.
- Scope stays inside existing Skillify metadata and avoids a migration.
- Legacy skills remain valid because new graph fields default to empty lists.
