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
within seven days counts as approval). A single maintainer may self-approve
only while the clause in §5 applies.

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

While the repository has **exactly one** maintainer in `MAINTAINERS.md`:

- The sole maintainer **may** self-approve PRs that are not load-bearing
  under `CLAUDE.md` §9 (i.e., not touching core traits, WAL, consent journal,
  or config schema).
- Load-bearing PRs still require **external** review — solicit it from the
  community or a trusted reviewer on the relevant issue.
- On the **first PR** after a second maintainer joins, flip branch
  protection to require a second approver and remove this deviation clause
  from this document.

This deviation exists because CODEOWNERS review becomes a deadlock with one
owner and no escape valve; the load-bearing carve-out prevents the deviation
from being a bypass.

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
