# Issue #141 — Release Channels + Auto-Update Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the v1.0 release-channel + auto-update policy as a single doc-only PR — ADR 0005 + brief §16.b + maintainer recipe page + user-facing usage page + traceability matrix update + new beta-readiness manual gate. No runtime code changes; no Cargo touches; no CI YAML touches.

**Architecture:** Mirrors the doc-only shape of ADR 0004 / issue #140 (PR #421). Six files; six commits, one per task; each commit individually buildable. Enforcement at v1.0 cutover is reviewer-driven through the beta-readiness runbook — no new automated gate is needed because the policy is a frozen document, not a runtime constraint.

**Tech Stack:** Markdown, mdbook (`mdbook build docs/site`), rustdoc (clean by construction since no Rust touched), bash (`scripts/beta-readiness.sh` heredoc edit).

**Spec:** `docs/superpowers/specs/2026-05-26-issue-141-release-channels-design.md`

---

## Pre-flight

- Worktree: already on `worktree-fluttering-munching-hartmanis` from `main`.
- Working dir: `/Users/tafeng/cairn/.claude/worktrees/fluttering-munching-hartmanis` — every command in this plan assumes this is `pwd`.
- The spec (above) is already committed at `4a1277d6`. Tasks below add the six deliverables on top of it.

---

## Task 1: Author ADR 0005 — release channels and auto-update policy

**Files:**
- Create: `docs/design/decisions/0005-release-channels.md`

- [ ] **Step 1: Verify the target path is free**

```bash
ls docs/design/decisions/0005-release-channels.md 2>&1
```

Expected: `ls: docs/design/decisions/0005-release-channels.md: No such file or directory`

- [ ] **Step 2: Write the ADR with full content**

Write the file with exactly this content (the template, headers, and tone match ADR 0004):

````markdown
# ADR 0005 — Release channels and auto-update policy

- **Status:** Accepted — 2026-05-26
- **Deciders:** Cairn maintainers
- **Issue:** [#141](https://github.com/windoliver/cairn/issues/141)
- **Parent epic:** [#32](https://github.com/windoliver/cairn/issues/32)
- **Design-brief sections:** §2 (design principles — invariants 1, 2, 6, 9), §14 (privacy), §16 (distribution and packaging), §16.b (new subsection introduced by this ADR), §19 (v1.0 production)
- **Supersedes:** none

## Context

Brief §19 commits v1.0 to "Beta distribution channels" (v0.4 setup) and
"Three harnesses shipped. Desktop GUI on three OSes" (v1.0). Brief §16
ships the artifact list (DMG / MSI / AppImage / deb / static tarball /
brew / cargo / winget / scoop) but leaves the **channel** semantics
(what does "beta" mean? how does a user pick one? what gets signed?
when does the desktop poll?) undefined.

Issue #141 asks for a single policy document that:

1. Defines stable / beta / nightly channels for CLI and desktop.
2. Names the update-metadata format, signature verification scheme,
   rollback recipe, and channel-migration rules.
3. Specifies the privacy / offline contract for any update checks.

The doc has to be in place before #142 ("validate v1.0 production
readiness across three harnesses") can certify a release.

This ADR is that policy. Implementation (signing infrastructure,
electron-updater wiring, `cairn release verify` CLI, GHA release
workflows, Sparkle feed publishing) is tracked as named follow-up
issues under parent epic #32 — each cites this ADR for shape.

## Decision

The following rules govern Cairn releases from v1.0 forward.

### 1. Channel matrix

| Channel | Trigger | Artifact destinations | Update poll feed | Cosign tag | Audience |
|---|---|---|---|---|---|
| **stable** | git tag `vX.Y.Z` (no pre-release suffix) | crates.io · `homebrew-cairn` main tap · winget · scoop · GitHub Releases (DMG/MSI/AppImage/deb/tarball) | `updates/stable.xml` (Sparkle feed) on github.io | `cairn-stable` | Default for everyone; what `brew install cairn` gives you |
| **beta** | git tag `vX.Y.Z-beta.N` or `vX.Y.Z-rc.N` | `homebrew-cairn-beta` tap · GitHub Pre-Releases · no crates.io publish (pre-release versions auto-skipped by `cargo install` unless `--version` pinned) | `updates/beta.xml` | `cairn-beta` | Opt-in. Users who want the next release with at least one tagged checkpoint of stability |
| **nightly** | scheduled GHA every 24h off `main`, tagged `nightly-YYYYMMDD` | GitHub Releases ("Nightly" section), **no** package-manager publish | `updates/nightly.xml` | `cairn-nightly` | Developers + dogfooders. No semver promise. Aged off after 30 days. |

One binary per platform per channel. The Rust core compiles
identically; the only difference is the build-time `CAIRN_CHANNEL`
env var the build embeds and `cairn status` reports. Channel-specific
behavior (feed URL, update-check default, signature-verify identity)
is table-driven off that one string.

Channel pinning lives in two places:

- **CLI:** nothing on disk. Channel = whichever artifact the user
  installed. `brew upgrade cairn` walks the stable tap;
  `brew tap cairn/beta && brew upgrade cairn` walks beta.
  `cargo install cairn` is always stable unless `--version` overrides.
- **Desktop:** `desktop-config.json` `update.channel` field (under the
  desktop app's app-support dir per OS — `~/Library/Application
  Support/cairn/` on macOS, mirrors the `vault_registry.json` location
  from #139), default `stable`. Runtime channel switch triggers a
  one-shot fetch of the chosen feed; the change applies on next launch.

### 2. Per-OS update mechanism

| OS | Updater | Feed format | Platform signature |
|---|---|---|---|
| macOS | `electron-updater` reading a Sparkle appcast XML | `updates/<channel>.xml` on github.io | Apple Developer ID + notarization (wired in [#139](https://github.com/windoliver/cairn/issues/139)). Cosign `.sig` sidecar as defence-in-depth. |
| Windows | `electron-updater` Squirrel | `updates/<channel>.xml` | Authenticode signed MSI (EV cert deferred until v1.0-rc). Cosign sidecar. |
| Linux AppImage | AppImageUpdate (zsync over HTTPS) | embedded `update-information` field | GPG-signed `.sig` next to artifact. Cosign sidecar. |
| brew / cargo / winget / scoop | Owned by the package manager; we publish artifacts + sidecars. | n/a | Package manager's existing verification + Cosign sidecar for users who want to double-check via `cairn release verify`. |

### 3. Signature scheme

Every released artifact carries a **Cosign keyless OIDC signature**
(`<artifact>.sig` + `<artifact>.pem`) published alongside the artifact
on its GitHub Release. The shipped CLI verifier `cairn release verify
<path>`:

1. Reads the embedded channel marker (`CAIRN_CHANNEL` in the binary
   stamp).
2. Looks up the matching public identity on the Sigstore Rekor
   transparency log.
3. Verifies the Cosign signature.
4. Additionally invokes the OS-native verifier when running on the
   matching OS (`codesign --verify` / `signtool verify` /
   `gpg --verify`).

Fail-closed: any verification failure → non-zero exit + the
`CapabilityUnavailable` remediation hint pointing back at this ADR.

### 4. Privacy / offline contract

- **Off by default.** No outbound network call for update checks until
  the user opts in via the onboarding prompt or by setting
  `update.check: true` in `desktop-config.json`.
- **`CAIRN_OFFLINE=1` and `agent.offline: true` always win.** If either
  is set, the update poller is dead code regardless of `update.check`.
  Evaluated at config-load time, not at poll time.
- **Payload is metadata-only.** Channel name, current version, OS,
  arch, and an opaque rotating install salt (regenerated weekly via the
  identity service, never linked to vault contents). No vault data, no
  record IDs, no user identifiers, no IP-derived geo. Logged at `trace`
  only. The brief §6.6 rule ("never log raw record bodies above
  `debug`") is extended here to also cover update-poll payloads.
- **Endpoint is a static file** (`updates/<channel>.xml` on github.io /
  optional Cloudflare Pages mirror). No server-side application
  logging beyond the hoster's standard access logs.
- **CLI never polls.** Only the desktop shell can be opted in. CLI users
  learn about updates from `brew outdated` / `cargo install --force` /
  the one-shot `cairn status --check-updates` (explicit invocation
  only, never automatic, never recurring).

### 5. Channel migration (forward-only)

1. User changes `update.channel` from stable → beta (or vice versa)
   via the Settings UI or `cairn config set update.channel beta`.
2. Next launch fetches the chosen channel's feed **once**, surfaces a
   "Update to vX.Y.Z-beta.1 available" prompt.
3. User confirms → electron-updater pulls + verifies + restarts.
4. Vault registry stays put. The same vault dirs survive every channel
   switch — channel is a binary-install concept, not a vault concept.
5. Downgrade across a vault-schema bump is **blocked at startup** with
   `VaultSchemaNewerThanBinary` error and a dialog suggesting either
   reinstalling the newer version or picking a different vault (same
   shape as the [#139 desktop packaging spec §6.3](../../superpowers/specs/2026-05-25-desktop-packaging-macos-design.md)).

### 6. Rollback

- `cairn release rollback --to v1.0.3` recipe (shipped with the
  verifier CLI in a follow-up issue): downloads the named prior signed
  artifact from GitHub Releases, runs `cairn release verify`, and
  instructs the user how to drop it in place per OS. Vault is untouched
  throughout.
- **No automatic boot-probe rollback in v1.0.** Implementing
  electron-updater's try-once boot probe requires preserving the prior
  `.app` bundle on every update plus a state machine for "first launch
  after update"; the design noise is real engineering and would push
  this ADR out of doc-only scope. Deferred to v1.1 as a named follow-up
  issue.
- **Supported-version window.** Every stable release receives signed
  patch updates until the **second** subsequent stable lands (`vN`
  supported until `vN+2`). Past that, the artifact stays on GitHub
  Releases for audit but receives no further security backports. The
  policy is enforced socially via the maintainer recipe doc; no code
  gate.

### 7. Enforcement

Unlike ADR 0004 (which is enforced by the runtime `contract-drift` CI
job), this ADR is a **policy document** — the rules apply to release
operations, not to compiled code. Enforcement is reviewer-driven via
the beta-readiness runbook (Gate 11 added by this ADR's PR). Concrete
runtime gates ship in named follow-up issues:

- Signed Sparkle feeds + Cosign sidecars per channel — follow-up under
  parent epic #32.
- `cairn release verify` CLI — follow-up under parent epic #32.
- `electron-updater` wiring + `update.channel` config + onboarding
  prompt — follow-up under parent epic #32.
- GHA scheduled nightly cut + 30-day age-off — follow-up under parent
  epic #32.
- Boot-probe automatic rollback — v1.1 follow-up.

Each follow-up cites this ADR for shape.

## Alternatives considered

- **Cairn-native signed manifest** (single `update.json` signed with
  Cosign, both CLI and desktop poll). Rejected because it reinvents the
  wheel and competes with brew / cargo / winget / scoop's existing
  channel semantics. The per-OS native-updater path matches what users
  already know.
- **GPG-only across the board.** Rejected because it concentrates risk
  on a single private key and has no transparency-log story. Cosign
  keyless OIDC + Rekor gives us supply-chain auditability the
  single-key path cannot.
- **No auto-update at v1.0 (manual reinstall only).** Rejected because
  desktop GA is a v1.0 success criterion (brief §18) and shipping a
  desktop app with no update story is a poor user experience. The
  off-by-default poll preserves the offline-first contract while
  letting users opt in.
- **On-by-default update poll.** Rejected as a direct violation of
  brief §2 invariant #2 ("stand-alone P0 — fresh laptop, offline,
  zero cloud credentials"). The default must be zero outbound traffic.

## Consequences

- Downstream packagers (brew, winget, scoop, AUR, Flatpak) can pin
  against a written, versioned channel policy instead of inferring
  cadence from release notes.
- Future contributors learn the channel + signing rules from one ADR
  rather than reading PR threads.
- Small docs surface to maintain (this ADR + maintainer page + user
  page); no new code paths to debug in this PR.
- Risk: follow-up implementation issues are non-trivial (signing
  infra, electron-updater wiring, scheduled GHA). Acceptable — the
  policy has to be frozen before the implementations have a
  specification to build against.

## Cross-references

- Brief: [§2 design principles](../design-brief.md), [§14 privacy](../design-brief.md), [§16 distribution and packaging](../design-brief.md), [§16.b release channels and updates](../design-brief.md), [§19 sequencing](../design-brief.md).
- Sibling ADRs: [ADR 0004 — `cairn.mcp.v1` semver freeze](./0004-mcp-v1-semver-freeze.md) (same doc-only shape; precedent for this ADR's tone).
- Sibling specs: [`docs/superpowers/specs/2026-05-25-desktop-packaging-macos-design.md`](../../superpowers/specs/2026-05-25-desktop-packaging-macos-design.md) (#139 — macOS signing/notarization wiring this policy builds on).
- Maintainer recipes: [`docs/site/src/maintainers/release-channels.md`](../../site/src/maintainers/release-channels.md).
- User-facing usage: [`docs/site/src/usage/updates.md`](../../site/src/usage/updates.md).
- Sibling issues: [#139](https://github.com/windoliver/cairn/issues/139) (desktop packaging, merged), [#140](https://github.com/windoliver/cairn/issues/140) (MCP semver freeze, merged via PR #421), [#142](https://github.com/windoliver/cairn/issues/142) (v1.0 production readiness, depends on this).
- Beta readiness gate: [`docs/site/src/maintainers/beta-readiness.md`](../../site/src/maintainers/beta-readiness.md) — Gate 11.
- Repo invariant: [`CLAUDE.md` §4 invariant 2 (stand-alone P0)](../../../CLAUDE.md).
````

- [ ] **Step 3: Verify the file was written**

```bash
wc -l docs/design/decisions/0005-release-channels.md
grep -c "^### [1-7]\." docs/design/decisions/0005-release-channels.md
```

Expected: ~180 lines, 7 section headers.

- [ ] **Step 4: Verify mdbook still builds (no broken links from the new file)**

```bash
mdbook build docs/site 2>&1 | tail -5
```

Expected: `Book building has finished` with no warnings about the new ADR (it isn't in SUMMARY yet, which is intentional — ADRs aren't surfaced via mdbook; only the maintainer/usage pages added later are).

- [ ] **Step 5: Commit**

```bash
git add docs/design/decisions/0005-release-channels.md
git commit -m "$(cat <<'EOF'
docs(adr): ADR 0005 release channels and auto-update policy (#141)

Defines the stable/beta/nightly channel matrix, per-OS update mechanism
(electron-updater + Sparkle/Squirrel/AppImageUpdate + Cosign uniform
layer), signature scheme, privacy/offline contract (off by default,
CAIRN_OFFLINE wins), channel migration (forward-only), and rollback
(manual recipe; auto deferred to v1.1).

Doc-only; mirrors ADR 0004 / PR #421 shape. Implementation tracked as
named follow-ups under parent epic #32.

Brief §2 invariants 1/2/6/9 · §14 · §16 · §16.b (new) · §19.
EOF
)"
```

Expected: commit succeeds; no pre-commit hook failure.

---

## Task 2: Add brief §16.b subsection

**Files:**
- Modify: `docs/design/design-brief.md` (insert new subsection between §16.a and §17, around line 3917)

- [ ] **Step 1: Locate the insertion point**

```bash
grep -n "^## 16\.\|^## 17\." docs/design/design-brief.md
```

Expected:
```
3778:## 16. Distribution and Packaging [P0 binary · P3 full channels]
3818:## 16.a Replacing Existing Memory Systems [P2]
3919:## 17. Non‑Goals (what Cairn will never be)
```

The new §16.b subsection slots in **after** §16.a content ends (line 3917 is the blank line before `---` separator that precedes `## 17`).

- [ ] **Step 2: Read the exact lines that bracket the insertion**

```bash
sed -n '3914,3920p' docs/design/design-brief.md
```

Confirm the last lines of §16.a + the `---` separator + the `## 17.` header. Use whatever you see to construct an Edit `old_string` that anchors the insertion uniquely on the `---` line right before `## 17.`.

- [ ] **Step 3: Insert §16.b before the `---` that precedes §17**

Use the Edit tool to replace the exact `---\n\n## 17.` block with the new subsection followed by the same `---\n\n## 17.` separator. Concretely the insertion content is:

```markdown
## 16.b Release Channels and Updates [P3]

Cairn ships under three named release channels: **stable** (tagged
`vX.Y.Z` — crates.io / brew main tap / winget / scoop / GitHub
Releases), **beta** (tagged `vX.Y.Z-beta.N` or `-rc.N` — `homebrew-cairn-beta`
tap and GitHub Pre-Releases), and **nightly** (scheduled GHA cut off
`main`, tagged `nightly-YYYYMMDD`, GitHub Releases only). One binary
per platform per channel; `cairn status` reports the channel via the
build-time `CAIRN_CHANNEL` stamp. Desktop users pin a channel via
`update.channel` in their desktop-config; CLI users pick a channel
by which artifact / package-manager tap they installed.

**Update checks are off by default.** No outbound poll runs until the
user opts in (`update.check: true`), and `CAIRN_OFFLINE=1` or
`agent.offline: true` always wins. When enabled, the desktop shell
reads a Sparkle appcast at `updates/<channel>.xml`; per-OS native
updaters (electron-updater on macOS / Windows, AppImageUpdate on
Linux) handle the download. Every artifact additionally carries a
Cosign keyless OIDC signature on the Sigstore Rekor transparency log;
the shipped `cairn release verify <path>` CLI checks both the platform
signature and the Cosign sidecar. Verification is fail-closed.

**Channel migration is forward-only.** Switching channels changes the
binary on next launch; vault data is untouched. Downgrade across a
vault-schema bump is blocked. **Rollback is documented but manual** at
v1.0 (`cairn release rollback --to <ver>` recipe); automatic
boot-probe rollback is deferred to v1.1.

Full rules live in [ADR 0005](decisions/0005-release-channels.md).
The maintainer recipe (cutting a stable, promoting nightly, rotating
signing keys) lives in
[`docs/site/src/maintainers/release-channels.md`](../site/src/maintainers/release-channels.md);
the user-facing guide (picking a channel, disabling update checks,
verifying a downloaded artifact) lives in
[`docs/site/src/usage/updates.md`](../site/src/usage/updates.md).

---

```

The Edit tool call (use `replace_all=false`; the `---\n\n## 17.` pattern is unique because there is exactly one §17):

```
old_string:
---

## 17. Non‑Goals (what Cairn will never be)

new_string:
## 16.b Release Channels and Updates [P3]

Cairn ships under three named release channels: **stable** (tagged
`vX.Y.Z` — crates.io / brew main tap / winget / scoop / GitHub
Releases), **beta** (tagged `vX.Y.Z-beta.N` or `-rc.N` — `homebrew-cairn-beta`
tap and GitHub Pre-Releases), and **nightly** (scheduled GHA cut off
`main`, tagged `nightly-YYYYMMDD`, GitHub Releases only). One binary
per platform per channel; `cairn status` reports the channel via the
build-time `CAIRN_CHANNEL` stamp. Desktop users pin a channel via
`update.channel` in their desktop-config; CLI users pick a channel
by which artifact / package-manager tap they installed.

**Update checks are off by default.** No outbound poll runs until the
user opts in (`update.check: true`), and `CAIRN_OFFLINE=1` or
`agent.offline: true` always wins. When enabled, the desktop shell
reads a Sparkle appcast at `updates/<channel>.xml`; per-OS native
updaters (electron-updater on macOS / Windows, AppImageUpdate on
Linux) handle the download. Every artifact additionally carries a
Cosign keyless OIDC signature on the Sigstore Rekor transparency log;
the shipped `cairn release verify <path>` CLI checks both the platform
signature and the Cosign sidecar. Verification is fail-closed.

**Channel migration is forward-only.** Switching channels changes the
binary on next launch; vault data is untouched. Downgrade across a
vault-schema bump is blocked. **Rollback is documented but manual** at
v1.0 (`cairn release rollback --to <ver>` recipe); automatic
boot-probe rollback is deferred to v1.1.

Full rules live in [ADR 0005](decisions/0005-release-channels.md).
The maintainer recipe (cutting a stable, promoting nightly, rotating
signing keys) lives in
[`docs/site/src/maintainers/release-channels.md`](../site/src/maintainers/release-channels.md);
the user-facing guide (picking a channel, disabling update checks,
verifying a downloaded artifact) lives in
[`docs/site/src/usage/updates.md`](../site/src/usage/updates.md).

---

## 17. Non‑Goals (what Cairn will never be)
```

- [ ] **Step 4: Verify the insertion**

```bash
grep -n "^## 16\.b\|^## 17\." docs/design/design-brief.md
```

Expected:
```
3917:## 16.b Release Channels and Updates [P3]
3951:## 17. Non‑Goals (what Cairn will never be)
```

(line numbers will be ~33 higher than before; exact numbers depend on the insertion length).

- [ ] **Step 5: Commit**

```bash
git add docs/design/design-brief.md
git commit -m "$(cat <<'EOF'
docs(brief): add §16.b release channels and updates (#141)

Inserts the §16.b subsection that names the three channels, the
off-by-default update poll, and the rollback policy. Points at
ADR 0005 for full rules.

Brief §16 grows §16.b.
EOF
)"
```

---

## Task 3: Update traceability matrix §16 row

**Files:**
- Modify: `docs/design/traceability.md` (the `| §16 Packaging |` row)

- [ ] **Step 1: Locate the row**

```bash
grep -n "§16 Packaging" docs/design/traceability.md
```

Expected: one match, around line 93.

- [ ] **Step 2: Read the row**

```bash
sed -n '93p' docs/design/traceability.md
```

Expected:
```
| §16 Packaging | #18, #32, #100, #139–#142, #158 | — | Cargo and Homebrew, static smoke tests, desktop production packaging, release channels. |
```

- [ ] **Step 3: Replace the row to cite ADR 0005**

Edit the file (`replace_all=false`):

```
old_string:
| §16 Packaging | #18, #32, #100, #139–#142, #158 | — | Cargo and Homebrew, static smoke tests, desktop production packaging, release channels. |

new_string:
| §16 Packaging | #18, #32, #100, #139–#142, #158 | ADR 0005 (resolved) | Cargo and Homebrew, static smoke tests, desktop production packaging, release channels. #141 lands ADR 0005 (release channels + auto-update policy) — stable/beta/nightly matrix, per-OS update mechanism, Cosign signing scheme, off-by-default privacy contract, forward-only channel migration, manual rollback. Implementation of signing infra / electron-updater wiring / `cairn release verify` is tracked as named follow-ups under parent epic #32, each citing ADR 0005. |
```

- [ ] **Step 4: Verify**

```bash
grep -A0 "§16 Packaging" docs/design/traceability.md
```

Expected: row now contains "ADR 0005 (resolved)" in the decisions column and the new coverage note.

- [ ] **Step 5: Commit**

```bash
git add docs/design/traceability.md
git commit -m "$(cat <<'EOF'
docs(traceability): cite ADR 0005 in §16 row (#141)

Updates the §16 packaging row to point at ADR 0005 and the follow-up
implementation issues under parent epic #32.
EOF
)"
```

---

## Task 4: Author maintainer recipe page

**Files:**
- Create: `docs/site/src/maintainers/release-channels.md`

- [ ] **Step 1: Verify the target path is free**

```bash
ls docs/site/src/maintainers/release-channels.md 2>&1
```

Expected: `No such file or directory`.

- [ ] **Step 2: Write the maintainer doc with full content**

Write the file with exactly this content:

````markdown
# Release Channels

This page is the operator runbook for cutting Cairn releases. The
governing policy is [ADR 0005](../../../design/decisions/0005-release-channels.md);
the user-facing summary lives in [Updates](../usage/updates.md).

## Channels at a glance

| Channel | Tag pattern | Cadence | Where it goes |
|---|---|---|---|
| **stable** | `vX.Y.Z` (no suffix) | When milestone is ready | crates.io · `homebrew-cairn` tap · winget · scoop · GitHub Releases |
| **beta** | `vX.Y.Z-beta.N` / `-rc.N` | Every 2–4 weeks during a release cycle | `homebrew-cairn-beta` tap · GitHub Pre-Releases |
| **nightly** | `nightly-YYYYMMDD` | Daily scheduled GHA off `main` | GitHub Releases ("Nightly" section); aged off after 30 days |

## Recipes

### Cut a stable release

1. Update `CHANGELOG.md` with the user-visible delta since the previous
   stable. Use the "Changed / Added / Removed / Fixed / Security"
   conventional sections.
2. Bump `[workspace.package].version` in the root `Cargo.toml`.
   Commit with `release: vX.Y.Z`.
3. Tag the release commit: `git tag -s vX.Y.Z -m 'Cairn vX.Y.Z'`.
4. Push branch and tag: `git push origin main && git push origin vX.Y.Z`.
5. Wait for `release-dry-run` to go green on the tag. **Do not push
   any artifacts before this completes.**
6. Run `scripts/beta-readiness.sh --full` locally. All automatable
   gates must pass.
7. Walk Gates 9–15 of [Beta Readiness](beta-readiness.md) manually,
   including Gate 11 (this page is the evidence for Gate 11 — confirm
   ADR 0005 status is `Accepted`).
8. Trigger the `release-stable.yml` workflow (added in follow-up
   under #32) with the tag as input. It builds + signs + publishes
   to all stable destinations + updates the `homebrew-cairn` tap +
   updates `updates/stable.xml` Sparkle feed.
9. Verify artifacts on a clean machine: `cairn release verify
   <downloaded>.dmg` must print `ok: cosign + apple-developer-id`.
10. Post the release notes to GitHub Releases; mark as latest.

### Cut a beta / release candidate

1. Bump version to `vX.Y.Z-beta.N` (or `-rc.N`) in `Cargo.toml`.
   Commit, tag, push.
2. Pre-release tags skip crates.io publishing automatically (verified
   in `release-dry-run`).
3. Trigger `release-beta.yml` workflow — builds, signs, publishes to
   the `homebrew-cairn-beta` tap, marks the GitHub Release as
   "Pre-release", updates `updates/beta.xml`.
4. Announce in the release thread; ask beta testers for feedback.

### Promote a nightly to beta

1. Pick the `nightly-YYYYMMDD` tag that passed all bench gates.
2. Cherry-pick its tip commit onto a fresh `release/vX.Y.Z-beta.N`
   branch.
3. Run the beta recipe above against the new branch's tip.

### Publish a rollback fix (security or critical regression)

A rollback is a **new patch release** that reverts the bad change.
There is no "delete the broken version" path — once an artifact is
signed and uploaded, it stays for audit. Steps:

1. Identify the bad commit. Revert via `git revert <sha>` on `main`.
2. Bump patch version (`v1.0.3 → v1.0.4`). Tag, push.
3. Cut as a normal stable release (recipe above).
4. Edit the bad release's GitHub Releases description to add a
   prominent **DEPRECATED — upgrade to vX.Y.Z** banner with link to
   the fix.
5. Notify affected users via the desktop in-app update prompt (next
   poll will see the new stable as the latest) and via the
   `homebrew-cairn` tap (`brew upgrade cairn` picks up the fix).

For users on a broken release who cannot upgrade automatically:
`cairn release rollback --to v1.0.2` (the recipe shipped with the
verifier CLI) downloads + verifies + dropps the prior signed artifact
in place. Vault is untouched.

### Rotate signing keys

| Key | Provider | Rotation cadence | Procedure |
|---|---|---|---|
| Cosign keyless | Sigstore (OIDC ephemeral) | n/a — every signing operation mints a fresh short-lived cert via OIDC | No rotation required; verify the cert chain on every release. |
| Apple Developer ID | Apple | Cert valid 5 years; rotate at 4y mark | Generate new cert in Apple Developer portal, update `APPLE_*` GitHub secrets, sign a smoke build, verify with `codesign --verify`. |
| Authenticode (EV) | DigiCert / Sectigo | Cert valid 1–3 years | Acquire new EV cert from CA, update `WINDOWS_CERT_*` secrets, sign + verify with `signtool verify`. |
| GPG (AppImage) | Maintainer-held | Annual | Generate new key, publish to keyservers, update `GPG_*` secret, sign a smoke build, document the new fingerprint in this page. |

After any key rotation, update the documented fingerprint in
[Updates](../usage/updates.md) so users can verify by hand.

### Retire an aged-off nightly

Scheduled GHA `nightly-prune.yml` (added in follow-up under #32)
deletes nightly releases older than 30 days. Manual override:

```bash
gh release delete nightly-20260301 --repo windoliver/cairn --yes
```

This is destructive — the artifact and its Cosign sidecar both
disappear. Only run for nightlies; never for stable or beta releases.

## Common failures

| Failure | First place to look |
|---|---|
| `release-dry-run` red on tag push | The job log; usually `[workspace.package].version` doesn't match the tag literal. |
| Apple notarization fails | `notarytool log <submission-id>` — most often a missing entitlement on a helper binary. |
| Cosign signature missing on the release | The signing step in `release-stable.yml`; verify `id-token: write` permission is granted. |
| `cairn release verify` fails on a downloaded artifact | Re-download (the file may have been corrupted in transit); if the failure reproduces, escalate — possible supply-chain compromise. |
| AppImageUpdate doesn't see the new version | The embedded `update-information` field in the AppImage points at the wrong URL; rebuild with the correct `APPIMAGE_UPDATE_INFORMATION`. |

## Supported version window

Each stable release is supported with patch updates until the
**second** subsequent stable lands (`vN` supported until `vN+2`).
After that, the artifact stays on GitHub Releases for audit but
receives no further security backports. This is socially enforced —
no code gate.

If a CVE lands on an unsupported version, document the affected
versions in a GitHub Security Advisory and explicitly state which
supported versions contain the fix.
````

- [ ] **Step 3: Verify the file was written and rendered correctly**

```bash
wc -l docs/site/src/maintainers/release-channels.md
```

Expected: ~130 lines.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/maintainers/release-channels.md
git commit -m "$(cat <<'EOF'
docs(maintainers): add release-channels recipe page (#141)

Operator runbook for cutting stable / beta / nightly releases,
promoting nightlies, publishing rollback fixes, rotating signing keys,
and retiring aged-off nightlies. Companion to ADR 0005.
EOF
)"
```

---

## Task 5: Author user-facing updates page

**Files:**
- Create: `docs/site/src/usage/updates.md`

- [ ] **Step 1: Verify the target path is free**

```bash
ls docs/site/src/usage/updates.md 2>&1
```

Expected: `No such file or directory`.

- [ ] **Step 2: Write the user doc with full content**

Write the file with exactly this content:

````markdown
# Updates

Cairn ships under three release channels. Pick whichever fits how
much stability and risk you want.

| Channel | Who it's for | How you get it |
|---|---|---|
| **stable** (default) | Everyone. Tagged releases only. | `brew install cairn` · `cargo install cairn` · DMG / MSI / AppImage / deb from [GitHub Releases](https://github.com/windoliver/cairn/releases) |
| **beta** | Users who want the next release with at least one tagged checkpoint of stability. | `brew tap cairn/beta && brew install cairn` · GitHub Pre-Releases · `cargo install cairn --version vX.Y.Z-beta.N` |
| **nightly** | Developers and dogfooders. No semver promise. Aged off after 30 days. | GitHub Releases "Nightly" section only — no package-manager publish. |

## Switching channels (desktop)

1. Open the Cairn desktop app.
2. Settings → Updates → Channel.
3. Pick `stable`, `beta`, or `nightly`. The change applies on next
   launch.

From the command line, the desktop config is at:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/cairn/desktop-config.json` |
| Linux | `$XDG_CONFIG_HOME/cairn/desktop-config.json` (default `~/.config/cairn/`) |
| Windows | `%APPDATA%\cairn\desktop-config.json` |

Set `update.channel` to `stable`, `beta`, or `nightly`. Restart the
app.

## Switching channels (CLI)

The CLI doesn't have a runtime channel selector — your channel is
whichever artifact you installed. To switch:

```bash
# Stable → beta (Homebrew):
brew uninstall cairn
brew tap cairn/beta
brew install cairn

# Beta → stable:
brew uninstall cairn
brew untap cairn/beta
brew install cairn

# Stable → specific beta (cargo):
cargo install cairn --force --version vX.Y.Z-beta.N
```

Your vault is **not** touched by any of these — channel switches are
binary-only.

## Disabling update checks

**Update checks are off by default.** No outbound network call runs
until you opt in.

To make sure they stay off:

- Set `CAIRN_OFFLINE=1` in your shell environment. Wins over every
  other setting; Cairn will never poll.
- Or set `agent.offline: true` in `.cairn/config.yaml`. Same effect.
- Or leave `update.check: false` (the default) in your desktop
  config.

The desktop app's onboarding asks once whether you want update
checks. You can change your mind any time in Settings → Updates.

When checks are enabled, the desktop app polls
`https://windoliver.github.io/cairn/updates/<channel>.xml` once per
24 hours. The payload it sends is metadata-only: channel name,
current version, OS, arch, and an opaque rotating install ID that
resets weekly. No vault content, no user identity, no IP-derived
geo.

## Verifying a downloaded artifact

Every artifact on a Cairn GitHub Release ships with a Cosign keyless
OIDC signature (`<artifact>.sig` + `<artifact>.pem`) committed to the
Sigstore Rekor transparency log. The shipped CLI verifier:

```bash
cairn release verify ~/Downloads/Cairn-1.0.0-universal.dmg
```

Prints `ok: cosign + apple-developer-id` (or the OS-equivalent line)
when both signatures verify. On any failure, the command exits
non-zero and prints the Rekor lookup URL so you can audit the
original signature on the transparency log.

You can also verify manually using upstream tooling:

```bash
# Cosign verification (any OS):
cosign verify-blob \
  --certificate Cairn-1.0.0-universal.dmg.pem \
  --signature Cairn-1.0.0-universal.dmg.sig \
  --certificate-identity-regexp '^https://github\.com/windoliver/cairn/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  Cairn-1.0.0-universal.dmg

# macOS notarization (on macOS):
codesign --verify --deep --strict --verbose=2 /Applications/Cairn.app
spctl --assess --type execute --verbose=2 /Applications/Cairn.app

# Windows Authenticode (on Windows):
signtool verify /pa /v Cairn-1.0.0.msi

# Linux GPG (on Linux):
gpg --verify Cairn-1.0.0.AppImage.sig Cairn-1.0.0.AppImage
```

The maintainer's GPG fingerprint and Cosign identity regex are
documented in [release channels](../maintainers/release-channels.md)
under "Rotate signing keys".

## Rolling back to a previous version

If a release breaks something for you, the supported recipe is:

```bash
cairn release rollback --to v1.0.3
```

The verifier CLI downloads the named prior signed artifact from
GitHub Releases, runs `cairn release verify`, and instructs you how
to drop it in place per OS. Your vault is **not** touched.

There is no automatic boot-probe rollback at v1.0. If you cannot
launch the app at all after an update, downgrade manually from
GitHub Releases.

Downgrade across a vault-schema bump is blocked at startup with a
`VaultSchemaNewerThanBinary` error — you'll be prompted to either
reinstall the newer version or pick a different vault.

## What about the package managers?

Brew, cargo, winget, and scoop own their own update cadence:

- `brew upgrade cairn` — pulls the latest from the tap you have
  installed (stable tap by default, beta tap if you ran `brew tap
  cairn/beta`).
- `cargo install cairn --force` — pulls the latest non-pre-release
  from crates.io. Pin a pre-release with `--version`.
- `winget upgrade cairn` / `scoop update cairn` — both walk the
  upstream feed.

These commands work whether or not you have Cairn's own update
checks enabled — Cairn doesn't interfere with your package
manager.

## Further reading

- [ADR 0005 — Release channels and auto-update policy](../../../design/decisions/0005-release-channels.md) (the policy this page summarizes)
- [Beta Readiness](../maintainers/beta-readiness.md) (the runbook
  every release passes)
- [Installation](installation.md) (first-time install instructions
  per channel)
````

- [ ] **Step 3: Verify the file was written**

```bash
wc -l docs/site/src/usage/updates.md
```

Expected: ~150 lines.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/usage/updates.md
git commit -m "$(cat <<'EOF'
docs(usage): add updates user-facing page (#141)

Explains how to pick a channel, switch channels (desktop and CLI),
disable update checks (CAIRN_OFFLINE, agent.offline, update.check),
verify downloaded artifacts with cairn release verify + upstream
tools, and roll back to a previous version. Companion to ADR 0005.
EOF
)"
```

---

## Task 6: Wire SUMMARY nav + beta-readiness gate

**Files:**
- Modify: `docs/site/src/SUMMARY.md`
- Modify: `docs/site/src/maintainers/beta-readiness.md`
- Modify: `scripts/beta-readiness.sh`

This task does three small wiring changes that belong together logically (they all surface the new docs through existing structure).

- [ ] **Step 1: Add `usage/updates.md` to SUMMARY**

Read the current SUMMARY entry block for `usage/`:

```bash
sed -n '14,29p' docs/site/src/SUMMARY.md
```

Expected:
```
# Usage

- [Installation](usage/installation.md)
- [CLI](usage/cli.md)
- [Backup Registry](usage/backup.md)
- [Configuration](usage/config.md)
- [Plugins](usage/plugins.md)
- [MCP](usage/mcp.md)
- [Claude Code](usage/claude-code.md)
- [Codex](usage/codex.md)
- [Cairn Skill](usage/skill.md)
- [Claude Code Reference Consumer](usage/claude-code-reference.md)
- [Migration Guides](usage/migration/index.md)
  - [v0.1 → v0.2](usage/migration/v0.1-to-v0.2.md)
  - [v0.2 → v0.3](usage/migration/v0.2-to-v0.3.md)
  - [v0.3 → v0.4](usage/migration/v0.3-to-v0.4.md)
```

Edit (insert "Updates" right after "Installation"):

```
old_string:
- [Installation](usage/installation.md)
- [CLI](usage/cli.md)

new_string:
- [Installation](usage/installation.md)
- [Updates](usage/updates.md)
- [CLI](usage/cli.md)
```

- [ ] **Step 2: Add `maintainers/release-channels.md` to SUMMARY**

```bash
sed -n '79,85p' docs/site/src/SUMMARY.md
```

Expected:
```
# Maintainers

- [Codegen](maintainers/codegen.md)
- [Docs](maintainers/docs.md)
- [CI](maintainers/ci.md)
- [Beta Readiness](maintainers/beta-readiness.md)
- [MCP Semver Policy](maintainers/mcp-semver-policy.md)
```

Edit (append "Release Channels" after "MCP Semver Policy"):

```
old_string:
- [MCP Semver Policy](maintainers/mcp-semver-policy.md)

new_string:
- [MCP Semver Policy](maintainers/mcp-semver-policy.md)
- [Release Channels](maintainers/release-channels.md)
```

- [ ] **Step 3: Add Gate 11 row to `beta-readiness.md` body and renumber 11→16**

Read the existing section list for gates 11–15:

```bash
grep -n "^### 1[1-5]\." docs/site/src/maintainers/beta-readiness.md
```

Expected (line numbers approximate):
```
196:### 11. Migration guide review (manual)
209:### 12. Known limitations (manual)
217:### 13. Cassette replay (manual)
226:### 14. Privacy posture (manual)
240:### 15. Release notes draft (manual)
```

Renumber each header by **incrementing all by 1** (11→12, 12→13, 13→14, 14→15, 15→16). Use the Edit tool with `replace_all=false` once per header (the strings are unique):

```
old_string: ### 11. Migration guide review (manual)
new_string: ### 12. Migration guide review (manual)
```
```
old_string: ### 12. Known limitations (manual)
new_string: ### 13. Known limitations (manual)
```
```
old_string: ### 13. Cassette replay (manual)
new_string: ### 14. Cassette replay (manual)
```
```
old_string: ### 14. Privacy posture (manual)
new_string: ### 15. Privacy posture (manual)
```
```
old_string: ### 15. Release notes draft (manual)
new_string: ### 16. Release notes draft (manual)
```

**Apply the renumbers in REVERSE order** (15 first, then 14, then 13, …, then 11) so that earlier edits do not accidentally re-collide with later-numbered originals. After all five renumbers, the section list reads 12, 13, 14, 15, 16.

- [ ] **Step 4: Insert the new Gate 11 body right before the renumbered Gate 12**

Edit (`replace_all=false`):

```
old_string:
### 12. Migration guide review (manual)

new_string:
### 11. Release channel policy frozen (manual)

Verify the release-channel policy ADR is present and accepted, and
the brief subsection points at it.

```bash
test -f docs/design/decisions/0005-release-channels.md && \
  grep -q '^- \*\*Status:\*\* Accepted' docs/design/decisions/0005-release-channels.md && \
  grep -q '^## 16\.b Release Channels and Updates' docs/design/design-brief.md && \
  echo "ok: release channel policy frozen"
```

**Pass:** prints `ok: release channel policy frozen` and exits 0.
**Failure:** ADR file missing, ADR status is not `Accepted`, or brief
§16.b anchor is missing. Authoring drift; fix the missing piece.

At v1.0 cutover also confirm the per-channel signing secrets are
loaded into CI (Apple Developer ID, Authenticode EV cert, GPG
keyring, Cosign OIDC permission). The implementation issue under
parent epic #32 owns the secret-loading workflow; this gate only
verifies the policy doc is in place.

See [ADR 0005](../../../design/decisions/0005-release-channels.md)
for the authoritative channel + signing + privacy rules.

### 12. Migration guide review (manual)
```

- [ ] **Step 5: Renumber the sign-off block at the bottom**

Read it:

```bash
sed -n '270,290p' docs/site/src/maintainers/beta-readiness.md
```

Look for the sign-off `- [ ] Gate 11:` through `- [ ] Gate 15:` lines. They need to grow to `Gate 11: release channel policy frozen (manual)` plus 12–16 mirroring the renumbered section headers.

Edit (`replace_all=false`):

```
old_string:
- [ ] Gate 11: migration guide review (manual)
- [ ] Gate 12: known limitations (manual)
- [ ] Gate 13: cassette replay (manual)
- [ ] Gate 14: privacy posture (manual)
- [ ] Gate 15: release notes draft (manual)

new_string:
- [ ] Gate 11: release channel policy frozen (manual)
- [ ] Gate 12: migration guide review (manual)
- [ ] Gate 13: known limitations (manual)
- [ ] Gate 14: cassette replay (manual)
- [ ] Gate 15: privacy posture (manual)
- [ ] Gate 16: release notes draft (manual)
```

- [ ] **Step 6: Update the script `print_manual_gates` heredoc**

Read it:

```bash
sed -n '104,115p' scripts/beta-readiness.sh
```

Expected:
```
print_manual_gates() {
  cat <<'EOF'
manual gates remaining (see docs/site/src/maintainers/beta-readiness.md):
  - 9:  capability sync (cairn status --json vs reference/capability-matrix.md)
  - 10: contract freeze verified (contract-drift CI job + ADR 0004)
  - 11: migration guide review (usage/migration/v0.X-to-v0.Y.md)
  - 12: known limitations (status.md vs capability matrix)
  - 13: cassette replay (cargo run -p cairn-bench -- coherence run --gate beta)
  - 14: privacy posture (forget round-trip + presidio scrub)
  - 15: release notes draft
EOF
}
```

Edit (`replace_all=false`):

```
old_string:
  - 11: migration guide review (usage/migration/v0.X-to-v0.Y.md)
  - 12: known limitations (status.md vs capability matrix)
  - 13: cassette replay (cargo run -p cairn-bench -- coherence run --gate beta)
  - 14: privacy posture (forget round-trip + presidio scrub)
  - 15: release notes draft

new_string:
  - 11: release channel policy frozen (ADR 0005 + brief §16.b)
  - 12: migration guide review (usage/migration/v0.X-to-v0.Y.md)
  - 13: known limitations (status.md vs capability matrix)
  - 14: cassette replay (cargo run -p cairn-bench -- coherence run --gate beta)
  - 15: privacy posture (forget round-trip + presidio scrub)
  - 16: release notes draft
```

- [ ] **Step 7: Verify the SUMMARY entries resolve and mdbook builds clean**

```bash
mdbook build docs/site 2>&1 | tail -10
```

Expected: `Book building has finished` with no warnings. If you see "Link points to a nonexistent file" pointing at `usage/updates.md` or `maintainers/release-channels.md`, those files weren't committed in Tasks 4 / 5 — go back.

- [ ] **Step 8: Verify the script still runs and lists Gate 11**

```bash
bash -n scripts/beta-readiness.sh
bash -c 'source scripts/beta-readiness.sh; print_manual_gates' 2>&1 || true
```

The `bash -n` syntax check must exit 0. The `source` invocation will run the gates and likely fail somewhere — that's fine; we're only checking the script parses and the heredoc renders. To see the heredoc in isolation, run the same `cat <<'EOF' ... EOF` block by extracting it:

```bash
awk '/^print_manual_gates\(\)/,/^}/' scripts/beta-readiness.sh
```

Expected: 11 is "release channel policy frozen", 12 is "migration guide review", 16 is "release notes draft".

- [ ] **Step 9: Verify rustdoc still clean (no Rust touched, but the workspace lint runs in CI)**

```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked 2>&1 | tail -5
```

Expected: builds without warnings. (If your local environment has stale `target/doc`, you may need to `cargo clean -p cairn-cli` first.)

- [ ] **Step 10: Commit**

```bash
git add docs/site/src/SUMMARY.md \
        docs/site/src/maintainers/beta-readiness.md \
        scripts/beta-readiness.sh
git commit -m "$(cat <<'EOF'
docs(nav): wire release-channels docs + add beta-readiness Gate 11 (#141)

- SUMMARY.md: add Updates (usage) and Release Channels (maintainers)
  nav entries so mdbook ships the two new pages.
- beta-readiness.md: insert Gate 11 "Release channel policy frozen"
  (asserts ADR 0005 status Accepted + brief §16.b anchor present);
  renumber 11→16 in body sections and sign-off block.
- scripts/beta-readiness.sh: update print_manual_gates heredoc to
  match the renumbered gate list. Manual-gate row, no new automated
  check function — same shape as #140 / ADR 0004 wired Gate 10.
EOF
)"
```

---

## Task 7: Run the full verification sweep

This is the final check before opening the PR. All commands run from the repo root.

- [ ] **Step 1: Format / clippy / check (no Rust touched, but CI runs these)**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | tail -5
cargo check --workspace --all-targets --locked 2>&1 | tail -5
```

Expected: all three exit 0. No Rust touched, so any failure points at unrelated drift from `main` — investigate before continuing.

- [ ] **Step 2: Doc gates from CLAUDE.md §8**

```bash
mdbook build docs/site 2>&1 | tail -5
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked 2>&1 | tail -5
```

Expected: both clean.

- [ ] **Step 3: Codegen + docgen drift checks (no IDL or CLI touched)**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check 2>&1 | tail -5
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check 2>&1 | tail -5
```

Expected: both report "no diff".

- [ ] **Step 4: Beta-readiness quick run (sanity check the script edit didn't break parsing)**

```bash
scripts/beta-readiness.sh --quick 2>&1 | tail -20
```

Expected: the gate output ends with `manual gates remaining (...)` listing Gate 11 as "release channel policy frozen". The automated gates above it may pass or fail depending on other workspace state — that's not what this task is verifying. If `beta-readiness: N ok, 0 fail, M skip` then all automated gates pass; either way the manual-gate listing must be present and correctly numbered.

- [ ] **Step 5: Confirm the six commits + spec commit are on the branch**

```bash
git log --oneline main..HEAD
```

Expected: 7 commits in this order (top = newest):

```
<sha> docs(nav): wire release-channels docs + add beta-readiness Gate 11 (#141)
<sha> docs(usage): add updates user-facing page (#141)
<sha> docs(maintainers): add release-channels recipe page (#141)
<sha> docs(traceability): cite ADR 0005 in §16 row (#141)
<sha> docs(brief): add §16.b release channels and updates (#141)
<sha> docs(adr): ADR 0005 release channels and auto-update policy (#141)
<sha> docs(spec): design for issue #141 release channels + auto-update policy
```

If any are missing, find the task they belong to and re-run it.

- [ ] **Step 6: Confirm no unexpected file changes**

```bash
git diff --stat main..HEAD
```

Expected file list (and roughly the right line counts):

```
docs/design/decisions/0005-release-channels.md              | ~180 +++
docs/design/design-brief.md                                  |  ~34 ++
docs/design/traceability.md                                  |   2 +-
docs/site/src/SUMMARY.md                                     |   2 ++
docs/site/src/maintainers/beta-readiness.md                  |  ~30 ++
docs/site/src/maintainers/release-channels.md                | ~130 +++
docs/site/src/usage/updates.md                               | ~150 +++
docs/superpowers/specs/2026-05-26-issue-141-release-channels-design.md | 280 +++
scripts/beta-readiness.sh                                    |   8 +-
```

Nothing under `crates/`, `frontend/`, `.github/`, `Cargo.toml`,
`Cargo.lock`. If something else moved, investigate.

---

## Task 8: Push branch + open the PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin worktree-fluttering-munching-hartmanis
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --repo windoliver/cairn \
  --title "docs: release channels + auto-update policy (#141)" \
  --body "$(cat <<'EOF'
## Summary

Codifies the v1.0 release-channel + auto-update policy for issue #141
(parent epic #32). Doc-only — mirrors the shape of #140 / PR #421.

- **ADR 0005** `docs/design/decisions/0005-release-channels.md` —
  stable / beta / nightly channel matrix, per-OS update mechanism
  (electron-updater + Sparkle / Squirrel / AppImageUpdate + Cosign
  uniform layer), signature scheme, privacy / offline contract (off by
  default, `CAIRN_OFFLINE` wins), channel migration (forward-only),
  rollback (manual; auto deferred to v1.1).
- **Brief §16.b** new subsection summarizing the policy and pointing
  at ADR 0005.
- **`docs/site/src/maintainers/release-channels.md`** — operator
  recipes (cutting a stable, promoting a nightly, publishing a
  rollback fix, rotating signing keys, retiring aged-off nightlies).
- **`docs/site/src/usage/updates.md`** — user-facing guide (picking a
  channel, switching channels, disabling update checks, verifying
  artifacts with `cairn release verify` + upstream tools, rolling
  back).
- **`docs/site/src/SUMMARY.md`** — nav entries for both new pages.
- **Traceability matrix §16 row** — cites ADR 0005 + the follow-up
  implementation issues under #32.
- **Beta readiness Gate 11** — manual gate "release channel policy
  frozen" added to both the `beta-readiness.md` runbook and the
  `scripts/beta-readiness.sh` heredoc; renumbers 11→16 downstream.
  Manual-gate row only — same shape as #140 / ADR 0004 wired Gate 10.

No code, no schema, no CI workflow YAML, no Cargo touches.
Implementation of signing infrastructure, electron-updater wiring,
`cairn release verify` CLI, scheduled nightly cuts, and Sparkle
feed publishing is tracked as named follow-up issues under parent
epic #32.

## Brief sections

- §16 — Distribution and Packaging gains §16.b.
- §2 invariants 1 (harness-agnostic), 2 (stand-alone P0), 6 (fail
  closed on capability), 9 (privacy by construction) — preserved.
- §14 — Privacy contract extended to update-poll payloads.
- §19 — v1.0 production sequencing references this policy.

## Invariants touched

- **Invariant 2 (stand-alone P0).** Update checks default off; the
  config loader fails closed under any of `CAIRN_OFFLINE=1`,
  `agent.offline: true`, or unknown channel value.
- **Invariant 6 (fail closed on capability).** `cairn release verify`
  is fail-closed; any signature failure → non-zero exit +
  `CapabilityUnavailable` remediation pointing at ADR 0005.

## Verification

- `cargo fmt --all --check` — clean.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` —
  clean.
- `cargo check --workspace --all-targets --locked` — clean.
- `mdbook build docs/site` — clean; both new pages resolve from
  SUMMARY.
- `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo
  doc --workspace --no-deps --document-private-items --locked` —
  clean.
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` —
  no diff.
- `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` —
  no diff.
- `scripts/beta-readiness.sh --quick` ends with manual-gate listing
  including the new Gate 11.

## Test plan

- [ ] CI `docs.yml` job green.
- [ ] CI `ci.yml` job green (no Rust touched, should be by
  construction).
- [ ] CI `contract-drift` job green (no schema touched, should be by
  construction).
- [ ] mdbook preview shows both new pages in nav.
- [ ] At v1.0 cutover, a maintainer ticks Gate 11 in the sign-off
  block after confirming the per-channel signing secrets are loaded.
EOF
)"
```

- [ ] **Step 3: Capture the PR URL for the user**

The command prints the PR URL on success. Save it for the final report.

---

## Self-review

After the plan is implemented, walk this checklist before declaring done:

1. **Spec coverage check.**
   - §4.1 (new files): ADR 0005 → Task 1. Maintainer recipe →
     Task 4. User doc → Task 5. ✓
   - §4.2 (touched files): brief §16.b → Task 2. Traceability →
     Task 3. SUMMARY → Task 6 Step 1+2. Beta-readiness MD → Task 6
     Step 3+4+5. Beta-readiness sh → Task 6 Step 6. ✓
   - §5 channel matrix → ADR 0005 §1 + brief §16.b ✓
   - §6 update mechanism → ADR 0005 §2 + maintainer recipe ✓
   - §7 privacy/offline → ADR 0005 §4 + user doc "Disabling update
     checks" ✓
   - §8 migration + rollback → ADR 0005 §5 + §6 + user doc
     "Rolling back" ✓
   - §9 error model → ADR 0005 §3 + §4 fail-closed clauses ✓
   - §10 testing strategy → Task 7 (verification sweep) ✓
   - §11 acceptance-criteria mapping → PR body ✓
   - §12 open questions → all noted in ADR 0005 alternatives /
     consequences sections ✓
   - §13 sequencing → 6 commits in this plan ✓
   - §14 cross-references → ADR 0005 cross-references section ✓

2. **Placeholder scan.** Re-grep the plan body for "TBD", "TODO",
   "fill in later" — none present.

3. **Type consistency.** No types in this plan (doc-only). File
   paths cross-referenced from Tasks 1, 2, 4, 5, 6 all use the
   identical literal `0005-release-channels.md` /
   `release-channels.md` / `updates.md` strings.

4. **Renumber sanity.** Renumbering happens in Task 6 Step 3 (body
   headers) + Step 5 (sign-off block) + Step 6 (script heredoc).
   The reverse-order edit instruction in Step 3 is the only
   load-bearing detail — flag if the order isn't followed.
