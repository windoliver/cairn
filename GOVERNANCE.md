# Cairn Governance

This document describes how the Cairn project is governed. The shape follows
the [CNCF maintainer-template governance model][cncf-template] with a small,
time-limited deviation called out below. Governance evolves by PR against this
file.

**Decision record:** [ADR 0001 — Monorepo shape and maintainer governance
model](docs/design/decisions/0001-monorepo-governance.md). Close any proposed
governance change by updating both this file and the ADR.

---

## 1. Maintainers

Maintainers are listed in [`MAINTAINERS.md`](MAINTAINERS.md). The `Maintainer`
role carries:

- Write access to the repository.
- Final review authority under the [CODEOWNERS](.github/CODEOWNERS) map.
- Release authority (`release-plz` Release-PR merges).
- A vote in project-level decisions (§4).

Maintainers are expected to:

- Uphold the contracts and invariants in [`CLAUDE.md`](CLAUDE.md) §4 and the
  design brief.
- Review PRs in their CODEOWNERS area within a reasonable window.
- Disclose conflicts of interest and recuse on votes where they apply.

### Adding a maintainer

A nomination opens as a PR that adds the candidate to `MAINTAINERS.md` and
updates `CODEOWNERS` to grant the relevant ownership. The PR requires approval
by a simple majority of the current maintainers (lazy consensus: no objection
within seven days counts as approval). During the single-maintainer period
(§5), the sole maintainer solicits and records an external `Reviewed-by:` on
the nomination PR — an explicit reviewer rather than a self-approval — and
the merge of that PR is itself the event that ends §5.

### Removing a maintainer

A maintainer may step down at any time by PR. Involuntary removal (inactivity
over twelve months with no response to outreach, or a Code of Conduct breach)
follows the same simple-majority rule. A removed maintainer with a positive
history is granted **Emeritus** status, recorded in `MAINTAINERS.md`.

---

## 2. Contract areas and sub-maintainers

The project partitions ownership along the seven contracts (design brief §4).
Each contract area may grow a **sub-maintainer** or domain expert ladder once
it has more than one active reviewer. Sub-maintainers hold merge authority
scoped to their area but do not hold a project-level vote.

| Area | Paths |
| --- | --- |
| Core / traits / IDL | `crates/cairn-core/`, `crates/cairn-idl/` |
| Store | `crates/cairn-store-sqlite/` |
| Sensors | `crates/cairn-sensors-local/` |
| API / MCP | `crates/cairn-mcp/`, `crates/cairn-cli/` |
| Workflows | `crates/cairn-workflows/` |
| Packaging / release | `.github/`, `scripts/`, `Cargo.toml`, `deny.toml` |
| Docs | `docs/`, `README.md`, `CLAUDE.md` |

Frontend (`FrontendAdapter`, design brief §4, P1) is a future area; `packages/`
is reserved for it.

---

## 3. Decision making

Default mode is **lazy consensus**: a maintainer proposes a change as a PR or
issue, other maintainers have seven days to object. Silence is consent.

**Simple majority vote** is the escape hatch. Any maintainer may call a vote
on any PR or issue; the vote runs for seven days; majority of
currently-active maintainers wins; the proposer does not get a tiebreaker.

**Super-majority (two-thirds)** is required for:

- Amending this document.
- Amending the design brief's load-bearing invariants (brief §2, `CLAUDE.md`
  §4).
- Changing the project licence.
- Reorganising the repository (e.g., splitting a crate into its own repo — see
  ADR 0001 split triggers).

---

## 4. Design decisions and ADRs

Substantive decisions land as Architecture Decision Records under
[`docs/design/decisions/`](docs/design/decisions/). The sequence is:

1. Open an issue proposing the decision.
2. Post a draft ADR (status `Proposed`) as a PR.
3. Discuss until lazy consensus or a called vote resolves.
4. Merge with `Status: Accepted`; reference the closing issue.
5. Supersede by opening a new ADR that sets the old one's status to
   `Superseded by #N`. ADRs are append-only; never rewrite history.

The design brief (`docs/design/design-brief.md`) remains the source of truth
for scope; ADRs close open questions or amend the brief through a PR that
touches both.

---

## 5. Time-limited deviation — single-maintainer period

While the repository has **exactly one** maintainer in `MAINTAINERS.md`,
required-reviewer branch protection cannot be satisfied by the sole
maintainer on their own PRs (GitHub disallows PR authors from approving
their own pull requests). The deviation resolves this without relying on an
admin bypass:

- **Branch protection does not require a CODEOWNERS or approving review**
  during this period. CODEOWNERS remains in place as a reviewer-request
  hint and an informational ownership map, not an enforcement gate.
- **Required status checks remain on** (CI must pass) and **force-push to
  `main` stays blocked**; the deviation is scoped narrowly to the "require
  N approvals" rule.
- **Load-bearing PRs** (as listed in `CLAUDE.md` §9 — core traits, WAL,
  consent journal, config schema; plus any change to this document or to
  `docs/design/decisions/`) **must** solicit and obtain at least one
  external review before merge. The sole maintainer records the reviewer
  (GitHub handle or email) in the PR description under a `Reviewed-by:`
  trailer. Merging a load-bearing PR without a recorded external review is
  a governance breach.
- **On the first PR after a second maintainer joins**, enable required
  CODEOWNERS review in branch protection, remove this §5 from the document
  by PR (that PR itself benefits from the newly-available second
  approver), and update `MAINTAINERS.md` to note the end of the
  single-maintainer period in its change log.

No admin-bypass or ruleset-bypass path is authorised for the single
maintainer during this period. The hard floor is the `Reviewed-by:` trailer
for load-bearing changes; everything else relies on community scrutiny of
the public commit log.

---

## 6. Repository shape

The project is one public monorepo (Cargo workspace + sibling `packages/`
for non-Rust SDKs) per ADR 0001. The conditions for splitting a crate or
package into its own repository are enumerated in ADR 0001 §"Split triggers".
Any split proposal requires a super-majority vote (§3).

The repository is currently **user-owned** (`windoliver/cairn`). Conversion to
a GitHub organisation and migration of CODEOWNERS from `@windoliver` to a
`@cairn-project/maintainers` team is a scheduled revisit trigger in ADR 0001;
it fires when a second maintainer joins.

---

## 7. Code of Conduct

The project will adopt the [Contributor Covenant][covenant] as its Code of
Conduct in a follow-up PR. Until then, contributors are expected to behave
consistently with the Contributor Covenant 2.1. Report concerns privately to
the maintainers listed in `MAINTAINERS.md`.

---

## 8. Amendments

Any change to this file requires a super-majority (§3) of current
maintainers. PRs that touch this file should also update ADR 0001 when the
change affects the decisions recorded there.

[cncf-template]: https://github.com/cncf/project-template/blob/main/GOVERNANCE-maintainer.md
[covenant]: https://www.contributor-covenant.org/version/2/1/code_of_conduct/
