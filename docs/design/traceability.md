# Cairn Design-to-Issue Traceability Matrix

This document maps top-level numbered sections of the Cairn design
brief (`docs/design/design-brief.md`), plus a curated list of
subsections that have their own dedicated issue threads, to the
GitHub issues that own their implementation, decisions, or
documentation. It is a roadmap coverage aid — not a guarantee that
every sentence in the brief is built, that every numbered subsection
is individually tracked, or that every section has an implementation
owner. Subsections inherit the ownership of their parent unless an
explicit row is listed below.

Sections without an owning implementation issue are recorded with
`_none_` and listed in the residual-risk section so the gap is
visible rather than hidden. Coverage is maintained by convention:
PRs that change the brief are expected to update this matrix in the
same diff, but no CI check enforces that today (see "Enforcement"
below).

This document supersedes the historical matrix drafted inline in
issue [#157](https://github.com/windoliver/cairn/issues/157). When
this file merges, #157 closes and the issue body becomes a frozen
historical record — the file in the repo is the only source of
truth going forward, so PRs that change the brief can update
coverage in the same diff.

## Column semantics

- **Implementation issues** — concrete `kind:task` or `kind:epic`
  tickets whose deliverable is code, schema, fixtures, or a published
  artifact. Open decision issues (`kind:decision`), pure documentation
  audits (`kind:audit`), and meta/tracking tickets do not belong here.
- **Decisions / docs** — `kind:decision` and doc-freeze tickets that
  shape the section but are not themselves implementation. Resolved
  decisions stay listed with their ADR; open decisions are flagged so
  reviewers know the section is not yet ready to implement.
- **Coverage notes** — short sentence on what is and is not covered.
  When the implementation cell is empty, the note must say so.

## Maintenance rules

- When the design brief gains a new section, add a row before merging.
- When a brief section is materially rewritten, re-check the cited issues
  and adjust coverage notes.
- When an issue is split or renumbered, update the matrix in the same PR.
- PRs that touch `docs/design/design-brief.md` should state in the
  description whether this matrix needs an update, and link the row(s).
- Audit, tracking, and meta issues (such as #157, which owns this
  document itself) are excluded from both issue columns so coverage
  cannot be self-justifying.
- A row whose Implementation cell is empty must say so explicitly in
  the coverage note rather than borrow an unrelated ticket.

## Resolved decisions baked into the matrix

- **v0.1 search modes:** keyword, semantic, and hybrid all ship in v0.1
  using SQLite FTS5 + statically linked `sqlite-vec` + pure-Rust `candle`
  embeddings. v0.2 adds richer providers and Nexus/BM25S projections —
  it does not introduce baseline semantic or hybrid search.
- **Semantic fallback:** strict fail-closed when the embedding model is
  unavailable (resolved in [#179](https://github.com/windoliver/cairn/pull/179)).

## Traceability matrix

| Design section | Implementation issues | Decisions / docs | Coverage notes |
|---|---|---|---|
| §1 Thesis / KISS / first principles | #3, #5, #7, #8, #9, #10, #11, #18, #19 | — | Contract-first, local-first, inspectable vault, and reference consumer covered. |
| §2 Design principles | #3, #4, #5, #7, #8, #17, #18, #158 | #145 (resolved) | Non-negotiable boundaries enforced through architecture, schema, privacy, WAL, and plugin gates. |
| §3 Vault layout / SQLite / Nexus | #5, #6, #20, #41–#49, #104–#106 | — | P0 authority remains SQLite; P1 Nexus is derived/additive. |
| §4 Contracts / plugins / identity | #3, #7, #10, #23, #27, #50–#53, #113, #124, #143 | — | Plugin registry and conformance covered. |
| §5 Pipeline / WAL / sessions | #8, #12, #13, #16, #54–#58, #71–#79, #89–#92 | #146 (resolved) | Capture, extract, filter, classify, plan/apply, WAL, hooks, and session capture covered. |
| §6 Taxonomy / provenance | #4, #37–#40 | — | Canonical kinds, classes, visibility, and provenance owned by core schema. |
| §6.a Multi-modal memory | #15, #29, #84–#88, #130–#132 | — | P0 local sensors plus P2 connectors and aggregate memory. |
| §7 Hot memory / profile | #14, #80–#83 | — | Budgeted hot prefix, profile, cache, and lint coverage. |
| §8 CLI / MCP / SDK / skill contract | #9, #10, #11, #59–#70 | — | IDL-generated surfaces and parity checks covered. |
| §8.1 Session lifecycle | #13, #76–#79 | — | Auto-discovery, trace storage, retrieve variants, and hooks. |
| §9 Sensors | #15, #84–#88 | #150 (resolved) | Local hooks, IDE, terminal, clipboard, voice, screen, recording. |
| §10 Continuous learning | #16, #22, #24, #27, #28, #89–#92, #110–#112, #124–#127 | — | P0 rolling workflows, P1 reflection and dreaming, P2 agent and evolution. |
| §11 Evolution | #28, #127–#129 | — | EvolutionWorkflow, Skillify, and skill graph covered. |
| §11.a Skill graph | #28, #129 | — | Dependency-aware structural retrieval covered. |
| §11.b Skillify | #22, #28, #112, #128 | #148 (open) | Base SkillEmitter plus P2 Skillify; artifact format still pending. |
| §12 Deployment tiers | #20, #23, #26, #104–#106, #113–#115, #121–#123 | — | P0 embedded, P1 local and Nexus and frontend, P2 federation. |
| §12.a Distribution model | #26, #29, #121–#123, #130–#132 | #149 (open) | ReBAC, share links, propagation, connectors, aggregate memory; transport boundary still pending. |
| §13 UI / frontend | #5, #23, #32, #43–#44, #113–#115, #139 | #147 (open) | P0 markdown, P3 desktop GA; P1 GUI alpha timing still pending. |
| §14 Privacy and consent | #17, #26, #58, #88, #93–#96, #121–#122 | — | Redaction, consent, policy traces, lint, forget, ReBAC, share consent. |
| §15 Evaluation | #18, #24, #31, #97–#100, #116–#118, #136–#137 | #138 (docs freeze) | P0 replay and gates, P1 bench and SRE, v0.4 cassette and doc freeze. |
| §16 Packaging | #18, #32, #100, #139–#142, #158 | — | Cargo and Homebrew, static smoke tests, desktop production packaging, release channels. |
| §16.a Existing memory systems | #120, #151–#156 | — | Explicit P2 migration bridge epic and child issues. |
| §17 Non-goals | _none_ | _none_ | No code deliverable and no current docs owner; non-goals language is reviewed ad-hoc in design-brief PRs. |
| §18 Success criteria / adoption / consumer blueprint | #11, #18, #19, #25, #31, #32, #68–#70, #97–#103, #119–#120, #139–#142 | #136, #137 | User stories, skill adoption, reference consumers, and production criteria. |
| §18.d Cairn skill | #11, #68–#70 | — | Install, conventions, compatibility checks. |
| §19 Sequencing | _none_ | — | Process-only row; phased milestones and `priority:` / `phase:` labels encode the sequence. |
| §19.a KISS v0.1 subset | #3–#19, #33–#103 | #145, #146, #150 (resolved) | P0 substrate coverage with the resolved all-three-search-mode decision. |
| §20 Open questions | _none_ | #147, #148, #149 (open); #145, #146, #150 (resolved) | Decision-only section by definition. |
| Appendix glossary | _none_ | _none_ | No dedicated glossary owner today; consistency is checked ad-hoc when terms change. |

## §20 open question decision status

| # | Title | Status |
|---|---|---|
| #145 | Monorepo governance and maintainer model | Resolved ([ADR 0002](decisions/0002-monorepo-governance.md) — PR #166) |
| #146 | Default LLM setup for local tier | Resolved ([ADR 0001](decisions/0001-llm-default.md) — PR #165) |
| #147 | Desktop GUI alpha timing and scope | Open |
| #148 | Skillify artifact format and compatibility policy | Open |
| #149 | Propagation transport boundary | Open |
| #150 | Screen sensor packaging and opt-in model | Resolved ([ADR 0003](decisions/0003-screen-sensor-packaging.md) — PR #168) |

## Enforcement

There is currently no automated check that pairs a `design-brief.md`
edit with a `traceability.md` update. Until such a guardrail exists
(for example, a CI step that fails when one file changes without the
other, or a PR-template checkbox), this document should be treated
as best-effort coverage rather than a hard release-signoff control.
Reviewers can compensate by inspecting the matrix on any PR that
touches `docs/design/design-brief.md`.

## Residual risk

- Later-phase issues are intentionally less granular than P0 and should
  be subdivided again before v0.2 / v0.3 implementation starts.
- The matrix must be updated whenever the design brief changes
  materially. PR reviewers should reject brief edits that leave this
  document stale.
- Sections marked `_none_` in the Implementation column are real gaps:
  §17 non-goals, §19 sequencing, §20 open questions, and the appendix
  glossary do not have dedicated implementation tickets today. New
  brief content that lands inside those sections must come with an
  implementation issue or an explicit waiver.
