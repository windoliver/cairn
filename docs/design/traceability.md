# Cairn Design-to-Issue Traceability Matrix

This document maps each section of the Cairn design brief
(`docs/design/design-brief.md`) to the GitHub issues that own its
implementation. It is the auditable source of roadmap coverage —
not a claim that every sentence is built, but a claim that every
major design surface has at least one owning issue path.

The matrix originates from issue
[#157](https://github.com/windoliver/cairn/issues/157) and lives here so
PRs that change the brief can update coverage in the same diff.

## Maintenance rules

- When the design brief gains a new section, add a row before merging.
- When a brief section is materially rewritten, re-check the cited issues
  and adjust coverage notes.
- When an issue is split or renumbered, update the matrix in the same PR.
- PRs that touch `docs/design/design-brief.md` should state in the
  description whether this matrix needs an update, and link the row(s).

## Resolved decisions baked into the matrix

- **v0.1 search modes:** keyword, semantic, and hybrid all ship in v0.1
  using SQLite FTS5 + statically linked `sqlite-vec` + pure-Rust `candle`
  embeddings. v0.2 adds richer providers and Nexus/BM25S projections —
  it does not introduce baseline semantic or hybrid search.
- **Semantic fallback:** strict fail-closed when the embedding model is
  unavailable (resolved in [#179](https://github.com/windoliver/cairn/pull/179)).

## Traceability matrix

| Design section | Owning issues | Coverage notes |
|---|---|---|
| §0 Priority legend | #3–#32, #143–#157 | Labels and milestones encode phase and priority. |
| §1 Thesis / KISS / first principles | #3, #5, #7, #8, #9, #10, #11, #18, #19 | Contract-first, local-first, inspectable vault, and reference consumer are covered. |
| §2 Design principles | #3, #4, #5, #7, #8, #17, #18, #143, #158 | Non-negotiable boundaries enforced through architecture, schema, privacy, WAL, and plugin gates. |
| §3 Vault layout / SQLite / Nexus | #5, #6, #20, #41–#49, #104–#106 | P0 authority remains SQLite; P1 Nexus is derived/additive. |
| §4 Contracts / plugins / identity | #3, #7, #10, #23, #27, #50–#53, #113, #124, #143 | Plugin registry and conformance coverage included. |
| §5 Pipeline / WAL / sessions | #8, #12, #13, #16, #54–#58, #71–#79, #89–#92 | Capture, extract, filter, classify, plan/apply, WAL, hooks, and session capture covered. |
| §6 Taxonomy / provenance | #4, #37–#40 | Canonical kinds, classes, visibility, and provenance owned by core schema. |
| §6.a Multi-modal memory | #15, #29, #84–#88, #130–#132 | P0 local sensors plus P2 connectors and aggregate memory. |
| §7 Hot memory / profile | #14, #80–#83 | Budgeted hot prefix, profile, cache, and lint coverage. |
| §8 CLI / MCP / SDK / skill contract | #9, #10, #11, #59–#70 | IDL-generated surfaces and parity checks covered. |
| §8.1 Session lifecycle | #13, #76–#79 | Auto-discovery, trace storage, retrieve variants, and hooks. |
| §9 Sensors | #15, #84–#88, #149 | Local hooks, IDE, terminal, clipboard, voice, screen, recording, plus screen packaging decision. |
| §10 Continuous learning | #16, #22, #24, #27, #28, #89–#92, #110–#112, #124–#127 | P0 rolling workflows, P1 reflection and dreaming, P2 agent and evolution. |
| §11 Evolution | #28, #127–#129 | EvolutionWorkflow, Skillify, and skill graph covered. |
| §11.a Skill graph | #28, #129 | Dependency-aware structural retrieval covered. |
| §11.b Skillify | #22, #28, #112, #128, #147 | Base SkillEmitter plus P2 Skillify; format decision covered. |
| §12 Deployment tiers | #20, #23, #26, #104–#106, #113–#115, #121–#123 | P0 embedded, P1 local and Nexus and frontend, P2 federation. |
| §12.a Distribution model | #26, #29, #121–#123, #130–#132, #148 | ReBAC, share links, propagation, connectors, aggregate memory. |
| §13 UI / frontend | #5, #23, #32, #43–#44, #113–#115, #139, #146 | P0 markdown, P1 GUI alpha, P3 desktop GA. |
| §14 Privacy and consent | #17, #26, #58, #88, #93–#96, #121–#122 | Redaction, consent, policy traces, lint, forget, ReBAC, share consent. |
| §15 Evaluation | #18, #24, #31, #97–#100, #116–#118, #136–#138, #154 | P0 replay and gates, P1 bench and SRE, v0.4 cassette and doc freeze. |
| §16 Packaging | #18, #32, #100, #158, #139–#142 | Cargo and Homebrew, static smoke tests, desktop production packaging, release channels. |
| §16.a Existing memory systems | #120, #151–#156 | Explicit P2 migration bridge epic and child issues. |
| §17 Non-goals | #138, #154 | Documentation freeze and traceability audit preserve non-goals. |
| §18 Success criteria / adoption / consumer blueprint | #11, #18, #19, #25, #31, #32, #68–#70, #97–#103, #119–#120, #136–#142 | User stories, skill adoption, reference consumers, and production criteria. |
| §18.d Cairn skill | #11, #68–#70 | Install, conventions, compatibility checks. |
| §19 Sequencing | #3–#32, #33–#158 | Phased milestones and parent and sub-issue hierarchy encode sequence. |
| §19.a KISS v0.1 subset | #3–#19, #33–#103, #143–#146, #150, #157, #158 | P0 substrate coverage with the resolved all-three-search-mode decision. |
| §20 Open questions | #145–#150 | Every open question has a decision issue with a recommended default. |
| Appendix glossary | #138 | Docs freeze owns glossary consistency. |

## §20 open question decision status

| # | Title | Status |
|---|---|---|
| #145 | Monorepo governance and maintainer model | Resolved (ADR 0001 — PR #166) |
| #146 | Default LLM setup for local tier | Resolved (PR #165) |
| #147 | Desktop GUI alpha timing and scope | Open |
| #148 | Skillify artifact format and compatibility policy | Open |
| #149 | Propagation transport boundary | Open |
| #150 | Screen sensor packaging and opt-in model | Resolved (ADR 0003 — PR #168) |

## Residual risk

- Later-phase issues are intentionally less granular than P0 and should
  be subdivided again before v0.2 / v0.3 implementation starts.
- The matrix must be updated whenever the design brief changes
  materially. PR reviewers should reject brief edits that leave this
  document stale.
