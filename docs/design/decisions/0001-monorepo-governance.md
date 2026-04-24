# ADR 0001 — Monorepo shape and maintainer governance model

- **Status:** Accepted
- **Date:** 2026-04-24
- **Closes:** [#145](https://github.com/windoliver/cairn/issues/145)
- **Parent issue:** #3 (Establish repository architecture and contract IDL)
- **Design brief refs:** §4.1 Plugin architecture; §16 Distribution and Packaging; §20 Open Questions (item 1)
- **Supersedes:** —
- **Superseded by:** —

## Context

The design brief leaves "single-repo vs. monorepo organisation; maintainer
model" as open question §20.1. The v0.1 crate roster has eight Rust crates
(`cairn-core`, `cairn-cli`, `cairn-mcp`, `cairn-store-sqlite`,
`cairn-sensors-local`, `cairn-workflows`, `cairn-idl`, `cairn-test-fixtures`)
and will later host generated non-Rust SDKs and skill packages (brief §16:
"Monorepo shape (polyglot: Rust core + TypeScript shell + Electron
renderer)"). The seven contracts (brief §4) already partition the natural
ownership surface: core/traits, store, sensors, API/MCP, frontend, packaging,
docs.

v0.1 has a maintainer roster of 1–3 people. Cargo's multi-package publishing
stabilised in 1.90 (July 2025), so workspace publishing is no longer a
split-justifying pain point. CLAUDE.md §3 already pins the flat `crates/`
layout and the core-boundary invariant.

The acceptance criteria for #145 require (a) a written governance decision
before broad implementation, (b) maintainer boundaries across the seven
contract areas, and (c) reversibility — the decision must not block future
package extraction.

## Decision

**Ship v0.1 as a single public monorepo** — one Cargo workspace in
`crates/`, a sibling `packages/` directory reserved for generated non-Rust
SDKs and skill packages, plus `docs/`, `fixtures/`, and `assets/`. Own the
repository with a **CODEOWNERS-style, team-scoped review model seeded by
contract area**, using a single `@cairn-project/maintainers` team until
additional reviewers exist. Adopt a lightweight governance document derived
from the CNCF maintainer template. Defer splitting any crate or SDK into its
own repository until an explicit trigger fires (see "Split triggers" below).

### Concrete commitments

1. **Layout.** Flat `crates/<crate-name>/` (folder name = crate name; the
   rust-analyzer / matklad pattern). Non-Rust SDKs live under a sibling
   `packages/<package-name>/` when they arrive. No nested hierarchies.
2. **CODEOWNERS.** A `.github/CODEOWNERS` file mirroring the layout. The
   repository is currently user-owned (`windoliver/cairn`); the v0.1 seed
   routes every path to `@windoliver`. When the repo converts to an
   organisation and a second maintainer joins, replace `@windoliver` with
   a `@cairn-project/maintainers` team and split per-contract ownership
   into dedicated teams (see `GOVERNANCE.md` §2). The active seed file
   lives at [`.github/CODEOWNERS`](../../../.github/CODEOWNERS).
3. **Governance doc.** Adopt the CNCF `GOVERNANCE-maintainer.md` template
   as [`GOVERNANCE.md`](../../../GOVERNANCE.md) with one documented
   deviation: while `|maintainers| == 1`, required-reviewer branch
   protection is **not** enabled (GitHub blocks self-approval, so the
   rule would deadlock); load-bearing PRs (as listed in `CLAUDE.md` §9)
   must obtain an external review and record it via a `Reviewed-by:`
   trailer in the PR description. Admin-bypass is not authorised.
   Maintain a parallel [`MAINTAINERS.md`](../../../MAINTAINERS.md)
   listing humans with **contract-area annotations** mirroring the seven
   contracts — this is zero-cost forward compatibility with a future
   SME/domain-lead ladder.
4. **Branch protection.** During the single-maintainer period
   (`GOVERNANCE.md` §5) `main` requires passing CI and blocks force-push,
   but does **not** require a CODEOWNERS / approving review — GitHub
   disallows PR authors from approving their own PRs, so a required-review
   rule would deadlock the repository or force an admin-bypass habit.
   Load-bearing PRs (as listed in `CLAUDE.md` §9 and `GOVERNANCE.md` §5)
   must obtain and record an external review in a `Reviewed-by:` trailer.
   Enable required CODEOWNERS review in branch protection on the first PR
   after a second maintainer joins; that same PR removes `GOVERNANCE.md`
   §5.
5. **Release tooling.** Adopt `release-plz` + `git-cliff` for Rust
   publishing (per-crate independent versioning, conventional commits,
   `cargo-semver-checks` in the Release PR). Non-Rust SDK publishing
   tool choice is deferred to the first SDK issue; candidates are
   Changesets (battle-tested) or Sampo (Rust-native, young). Capture that
   sub-decision in its own ADR when the SDK lands.

### Maintainer boundaries (satisfies AC 2)

Named boundaries used as the seed ownership map. Each maps to a contract in
brief §4; all route to the sole maintainer today (per the seed
`.github/CODEOWNERS`) and will gain a dedicated team as reviewer depth
grows and the repository converts to an organisation.

| Area | Paths | Contract |
| --- | --- | --- |
| Core / traits / IDL | `crates/cairn-core/`, `crates/cairn-idl/` | `MemoryStore`, `LLMProvider`, `WorkflowOrchestrator`, `SensorIngress`, `MCPServer` (brief §4) |
| Store | `crates/cairn-store-sqlite/` | `MemoryStore` |
| Sensors | `crates/cairn-sensors-local/` | `SensorIngress` |
| API / MCP | `crates/cairn-mcp/`, `crates/cairn-cli/` | `MCPServer` + CLI ground truth (brief §8) |
| Frontend | `packages/` (when populated) | `FrontendAdapter` (P1, brief §4) |
| Packaging / release | `.github/workflows/`, `release-plz.toml`, `deny.toml` | Distribution (brief §16) |
| Docs | `docs/`, `README.md`, `CLAUDE.md` | — |

### Split triggers (satisfies AC 3 — reversibility)

Triggers are partitioned into **hard** and **soft**. Any single hard
trigger is sufficient cause to open an extraction ADR immediately; soft
triggers reflect operational discomfort, so a single one opens discussion
and two concurrent soft triggers open an extraction ADR.

**Hard triggers — any one fires, open extraction ADR immediately:**

H1. **Licensing or contribution-policy pressure.** One crate needs a
    different licence, a CLA, or a vendor-only contribution path that
    cannot be satisfied in-repo.
H2. **Security release boundary.** An adapter must ship on a stricter
    release gate (signed-only releases, different maintainers, disclosure
    embargoes) that the monorepo release pipeline cannot enforce without
    compromising the rest.
H3. **External consumer SLA conflict.** An external project depends on a
    single crate with release-cadence, support-window, or stability
    guarantees materially distinct from Cairn itself, and keeping the
    crate in-repo forces us to either break their SLA or over-stabilise
    the rest of the workspace.

**Immediate containment when a hard trigger fires.** Opening the
extraction ADR is the long-running workstream; a discussion cannot be the
only response while risk is outstanding. Within **24 hours** of the
trigger being declared on-repo (issue labelled `governance:hard-trigger`),
the maintainers must:

1. **Assign an owner** — one maintainer takes point, named in the issue.
2. **Set a decision deadline** — no longer than 14 days for the
   extraction ADR to reach `Accepted` or the trigger to be explicitly
   ruled not-applicable with a written rationale.
3. **Apply an enforced path freeze** by committing a file to
   `.github/freezes/YYYY-MM-DD-<trigger>-<slug>.yaml` (see
   `.github/freezes/README.md` for the schema). The required
   `governance / freeze` status check
   (`.github/workflows/governance.yml` + `scripts/check-freeze.sh`) then
   fails any PR whose diff touches a path listed in the freeze,
   including direct merges by the sole maintainer — this is a
   mechanical gate, not a documentation claim.
4. **Apply trigger-specific interim controls on top of the freeze:**
   - **H1 (licensing / CLA):** the freeze alone is sufficient; add a
     note in the freeze file's `reason:` stating the licence question
     and the external counter-party.
   - **H2 (security gate):** additionally disable releases of the
     affected crate — set `publish = false` in its `Cargo.toml` and
     remove it from any active `release-plz` configuration. Open a P0
     issue tracking interim signed-release tooling. Note the reduced
     release surface in `README.md` until extraction or an equivalent
     in-repo mitigation lands.
   - **H3 (SLA conflict):** additionally pin the affected crate's
     semver at its current minor (do not bump while the freeze is
     active), notify the external consumer in writing, and open an
     issue tracking either SLA alignment or extraction — whichever the
     external consumer selects.
5. **Record** the containment steps and the freeze file path in the
   extraction-ADR issue thread; the ADR's `Context` section must cite
   them when it lands.

The freeze is lifted by a PR that deletes the freeze file, labelled
`governance:freeze-removal`; `scripts/check-freeze.sh` exempts that
label from the gate (see `.github/freezes/README.md`).

Soft triggers do **not** require immediate containment — they are
operational-discomfort signals, handled through the normal ADR lifecycle.

**Soft triggers — one opens discussion, two concurrent open extraction ADR:**

S1. Release cadence for one crate has permanently diverged (one ships
    weekly for 6+ months while another ships quarterly, and the Release
    PR is routinely stale).
S2. A dedicated maintainer team has formed for one crate with no overlap
    with the core reviewer pool.
S3. CI wall-clock on the monorepo exceeds the team's tolerance despite
    `cargo nextest` partitioning, sccache, and target-aware caching.
S4. Binary-size or dependency bloat in downstream consumers such that
    vendoring or forking is becoming routine.

### Revisit triggers (scheduled review)

Revisit this ADR when any of the following happens, regardless of the
split-trigger list:

- **Second maintainer joins** — end the single-maintainer deviation in
  `GOVERNANCE.md` §5, enable required CODEOWNERS review in branch
  protection, and update `MAINTAINERS.md`.
- **Third maintainer joins** — consider seeding per-contract ownership
  teams (one team per area in `GOVERNANCE.md` §2). This is optional and
  governed by reviewer depth per area, not by the headcount alone.
- **Repository converts to a GitHub organisation** — migrate CODEOWNERS
  from `@windoliver` to `@cairn-project/maintainers`; this may happen at
  or after the second-maintainer transition.
- **First non-Rust SDK ships** — activate `packages/`, pick an SDK release
  tool (Changesets or Sampo) in a follow-up ADR.
- Any hard or soft split trigger fires.

## Consequences

### Positive

- **Atomic cross-cutting changes.** Contract changes in `cairn-core` + IDL
  + adapters + CLI land in a single PR; `scripts/check-core-boundary.sh`
  continues to enforce the plugin invariant at review time.
- **Low governance overhead.** One CODEOWNERS file, one `GOVERNANCE.md`,
  one release pipeline. Matches the 1–3 maintainer reality of v0.1.
- **Forward compatible.** Per-contract teams, per-crate release cadences,
  and eventual extraction are all reachable without rewriting the model —
  only the CODEOWNERS seed and the governance deviation clause change.
- **Consistent with brief §16.** Keeps "Monorepo shape (polyglot)" from
  §16 as the canonical shape.

### Negative / accepted trade-offs

- **Single point of failure for CI / access.** A broken main branch blocks
  every crate. Mitigation: per-crate test partitioning and the existing
  `check-core-boundary.sh` gate; acceptable at v0.1 scale.
- **Coarse ownership until more reviewers exist.** Every path resolves to
  the sole maintainer initially — CODEOWNERS is informational during the
  single-maintainer period (see `GOVERNANCE.md` §5) and cannot enforce
  contract-specific review until at least two maintainers exist. Mitigation:
  end the single-maintainer period on the second maintainer; seed
  per-contract teams on the third.
- **Repository size grows monotonically.** Accepted; rust-analyzer,
  wasmtime, tauri, and biome all operate comfortably at 10× our expected
  v1 size.

### Known counter-argument

Matt Klein's "Monorepos: Please don't!" argues against organisational
monorepos holding hundreds of services. The evidence (multi-minute
`git status`, VCS scaling) does not apply to a ten-crate OSS library at
our scale. Cited explicitly so reviewers can see the decision was made
with the counter-argument in view.

## Alternatives considered

1. **Polyrepo-from-day-one.** One repo per contract area. Rejected: forces
   cross-repo PRs for any contract change, multiplies CI and governance
   overhead, and `cairn-core` + `cairn-idl` + `cairn-cli` are effectively
   co-evolving. Prisma's 2025 reversal (engines merged back into the
   client repo) is a recent reality check on premature splitting.
2. **Monorepo with per-crate GitHub repos mirrored via subtree.** Rejected
   as a premature synchronisation cost with no current benefit.
3. **No written governance doc at v0.1.** Rejected: the acceptance
   criteria for #145 explicitly require a written decision before broad
   implementation; undocumented governance tends to ossify into whatever
   the first controversy settles it as.

## References

- Design brief §4.1, §16, §20 (`docs/design/design-brief.md`)
- Architecture summary (`docs/design/architecture.md`)
- Workspace scaffold design (`docs/design/2026-04-23-rust-workspace-scaffold-design.md`)
- Matklad, "Large Rust Workspaces" — <https://matklad.github.io/2021/08/22/large-rust-workspaces.html>
- Tweag, "Publish all your crates everywhere all at once" (2025) — <https://www.tweag.io/blog/2025-07-10-cargo-package-workspace/>
- CNCF project template, `GOVERNANCE-maintainer.md` — <https://github.com/cncf/project-template/blob/main/GOVERNANCE-maintainer.md>
- Rust RFC 3119 — Crate Ownership — <https://rust-lang.github.io/rfcs/3119-rust-crate-ownership.html>
- release-plz — <https://release-plz.dev/>
- Matt Klein, "Monorepos: Please don't!" — <https://medium.com/@mattklein123/monorepos-please-dont-e9a279be011b> (counter-argument, retained for transparency)
- Prisma, "From Rust to TypeScript" (2025) — <https://www.prisma.io/blog/from-rust-to-typescript-a-new-chapter-for-prisma-orm> (cautionary tale on premature split)
- Bevy organisation model — <https://bevy.org/learn/contribute/project-information/bevy-organization/>
- Tauri governance — <https://tauri.app/about/governance/>
