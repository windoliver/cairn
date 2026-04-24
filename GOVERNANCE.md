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
required-approval branch protection cannot be satisfied by the sole
maintainer on their own PRs (GitHub disallows PR authors from approving
their own pull requests). The deviation resolves this without relying on an
admin bypass, and keeps a **mechanically-enforced gate** on load-bearing
changes.

### 5.1. Branch-protection settings during this period

- **Required status checks remain on** (CI must pass — including the
  `governance / reviewed-by` check defined in §5.3).
- **Force-push to `main` is blocked.**
- **Signed commits are recommended** but not required.
- **"Required approvals = 1" is disabled** — see rationale above; this is
  the narrow scope of the deviation.
- **Admin / ruleset bypass is NOT granted** to the sole maintainer. The
  `governance / reviewed-by` required status check is the hard gate.

### 5.2. Load-bearing paths

A PR is **load-bearing** if its diff touches any of the following (kept in
sync with `CLAUDE.md` §9 and `scripts/check-reviewed-by.sh`):

- `crates/cairn-core/src/` or `crates/cairn-core/Cargo.toml`
- `crates/cairn-idl/` (IDL source and generated artefacts)
- `crates/cairn-store-sqlite/migrations/` (append-only migrations)
- `docs/design/design-brief.md`
- `docs/design/decisions/` (any ADR)
- `GOVERNANCE.md`, `MAINTAINERS.md`, `.github/CODEOWNERS`
- `.github/workflows/governance.yml`, `.github/freezes/`
- `scripts/check-reviewed-by.sh`, `scripts/check-freeze.sh`

### 5.3. Enforceable external-review rule

For every load-bearing PR during this period:

1. An **external reviewer** (GitHub account other than the PR author) must
   leave an **Approved** review on the PR before merge. The review is
   permanently recorded in the PR timeline and is queryable via the
   GitHub API.
2. The PR must carry a `Reviewed-by:` trailer naming the external
   reviewer, either in the **merge commit body** or in the **PR
   description**:

   ```
   Reviewed-by: Jane Doe <@janedoe>
   ```

3. The `governance / reviewed-by` CI job
   (`.github/workflows/governance.yml` + `scripts/check-reviewed-by.sh`)
   inspects the PR diff; if it touches load-bearing paths and no valid
   `Reviewed-by:` trailer is present (or the trailer names the author),
   the check fails and the PR cannot merge. This check is listed in
   branch protection as a required status check.

The combination of the GitHub Approved review (social + auditable), the
commit trailer (audit artefact surviving in `git log`), and the required
status check (mechanical gate) makes the rule enforceable without relying
on the 1-approval branch-protection rule that GitHub's self-approval ban
would otherwise deadlock.

Merging a load-bearing PR without the above is a governance breach and
must be reverted and re-submitted under the proper process.

### 5.4. Transition off the deviation — ordered runbook

GitHub evaluates `CODEOWNERS` and branch-protection rules against the
**base branch** at PR-review time, so enabling "Require review from Code
Owners" **before** the nominee is written into `main`'s CODEOWNERS would
lock the repository: the sole existing owner can't self-approve, and the
nominee isn't yet a code owner on base. The runbook therefore deliberately
changes branch-protection rules **after** the nomination PR merges, not
before, and prevents an intermediate-state gap with a merge freeze plus a
transition-specific active path freeze.

Steps (executed in order, by the sole maintainer unless stated):

1. **Grant the nominee write access** to the repository (Settings →
   Collaborators). This allows them to leave a non-stale Approved review
   on the nomination PR but does **not** make them a code owner yet.
2. **Open a pre-transition freeze PR** that adds
   `.github/freezes/<date>-transition.yaml` freezing every path except
   `MAINTAINERS.md`, `.github/CODEOWNERS`, `GOVERNANCE.md`, and
   `docs/design/decisions/`. This prevents any non-transition work
   merging during the window. The freeze PR is itself load-bearing and
   goes through the §5.3 Approved-review + `Reviewed-by:` gate, with the
   nominee as the approving reviewer.
3. **Open the nomination PR** containing, in one commit or squashed:
   (a) `MAINTAINERS.md` addition with contract-area annotations,
   (b) `.github/CODEOWNERS` update adding the nominee to every affected
   path, (c) removal of this §5 (and update of every cross-reference:
   ADR 0001, brief §20.1, CODEOWNERS comment header,
   `scripts/check-reviewed-by.sh` load-bearing path list if obsolete),
   (d) `MAINTAINERS.md` change-log entry recording the date and PR
   number that ended the single-maintainer period,
   (e) removal of the transition freeze file from
   `.github/freezes/`. The PR is labelled `governance:freeze-removal`
   so the freeze gate exempts it. The nominee leaves an Approved
   review; a `Reviewed-by:` trailer names them.
4. **Merge the nomination PR** (squash). The new CODEOWNERS is now on
   `main`.
5. **Update branch protection** (Settings → Branches): enable
   `Require a pull request before merging`, `Required approvals = 1`,
   `Dismiss stale pull request approvals when new commits are pushed`,
   `Require review from Code Owners`, and keep the `governance /
   reviewed-by` required status check active (the script's load-bearing
   gate now overlaps with CODEOWNERS but the redundancy is cheap and
   fails closed). Remove the `governance / freeze` requirement only if
   there are no active freezes.
6. **Announce the transition** in the `MAINTAINERS.md` change-log entry
   (already committed in step 3) and, if the project has any external
   communication channel, there as well.

Until step 5 completes, no PR other than the transition PR may merge
(enforced by the freeze file committed in step 2). If any of the steps
fail or stall, the fallback is to reopen the freeze and re-run from the
last successful step — the transition is resumable because every step
leaves a committed artefact.

A future "third maintainer joins" transition is not gated by
`GOVERNANCE.md` at all — it triggers a separate, optional per-contract
team seed under §2 without touching §5 (by then removed) or branch
protection.

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
