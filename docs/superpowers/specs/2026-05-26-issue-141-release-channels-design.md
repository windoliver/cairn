# Release Channels and Auto-Update Policy — Design

**Issue:** [#141](https://github.com/windoliver/cairn/issues/141) — child slice of [P3] Ship production packaging, desktop GA, and MCP semver freeze ([#32](https://github.com/windoliver/cairn/issues/32))
**Brief:** §16 Distribution and Packaging · §19 v1.0 Production
**Date:** 2026-05-26
**Status:** Draft — pending implementation
**Scope:** Doc + ADR only. No code, no CI YAML, no Cargo touches. Mirrors the doc-only pattern of [#140 / PR #421](https://github.com/windoliver/cairn/pull/421).

---

## 1. Summary

Codify the release-channel matrix (stable / beta / nightly), the per-OS
update mechanism, the signature scheme, the privacy/offline contract,
and the channel-migration + rollback rules for Cairn v1.0. Land the
policy as ADR 0005 plus a brief subsection §16.b, a maintainer recipe
page, a user-facing usage page, a traceability-matrix update, and a new
beta-readiness **manual gate** entry. No runtime code changes;
enforcement at v1.0 cutover is reviewer-driven via the beta-readiness
runbook, the same shape ADR 0004 / issue #140 used.

## 2. Non-goals

- **Implementation of signed update manifests, electron-updater wiring,
  GitHub Actions release workflows, or `cairn release verify` CLI.**
  These are tracked as named follow-up issues under parent epic #32,
  and each follow-up cites this ADR for shape.
- **Automatic boot-probe rollback** (try-once + auto-restore prior
  bundle). Deferred to v1.1 with rationale in §10.
- **Marketplace distribution of skills** — explicitly out of scope per
  parent epic #32 and brief §17.
- **Mobile, hosted SaaS, MDM enterprise deployment** — out of scope per
  issue #141.

## 3. Design constraints (from CLAUDE.md + brief)

| Constraint | Source | How satisfied |
|---|---|---|
| Harness-agnostic | brief §2 invariant #1 | Channel labels and update poller live in the desktop shell + maintainer-side packaging pipeline; the Rust core only exposes the build-time `CAIRN_CHANNEL` string via `cairn status`. No verb behavior depends on channel. |
| Stand-alone P0 | brief §2 invariant #2 | Update checks default **off**; `CAIRN_OFFLINE=1` and `agent.offline: true` both kill the poller dead. A fresh laptop with no network never makes an outbound request because of release-channel logic. |
| CLI is ground truth | brief §2 invariant #3 | All channel state is readable from `cairn status`. Desktop shell wraps the same string; never invents its own. |
| Fail closed on capability | brief §2 invariant #6 | Verification failures (Cosign sig invalid, channel feed unreachable past retry budget, vault schema newer than binary) return typed errors and non-zero exits — never a silent downgrade. |
| Privacy by construction | brief §14 + invariant #9 | Update poll payload is metadata-only (channel, version, OS, arch, opaque rotating install salt). No record content, no user identity, no IP-derived geo. Logged at `trace` only. |
| `#![forbid(unsafe_code)]`, no `unwrap` in core | CLAUDE.md §6.2 | N/A — no Rust code touched. Pre-emptive note for follow-ups that any `cairn release verify` implementation must obey. |
| Doc convention | CLAUDE.md §1 (brief is source of truth) | Brief §16 gains §16.b pointing at ADR 0005; ADR 0005 is the canonical policy. Maintainer / user docs link both. |

## 4. Deliverables

Six files; doc-only PR.

### 4.1 New files

| Path | Purpose |
|---|---|
| `docs/design/decisions/0005-release-channels.md` | ADR 0005 — frozen channel matrix, update mechanism per OS, signature scheme, privacy/offline contract, channel-migration + rollback rules. Authored against the ADR 0004 template. Status: Accepted. |
| `docs/site/src/maintainers/release-channels.md` | Operator-facing recipes: cutting a stable, cutting a beta, promoting a nightly to beta, publishing a rollback fix, rotating Cosign / Apple / Authenticode / GPG keys, retiring an aged-off channel. Links ADR 0005 from the top. |
| `docs/site/src/usage/updates.md` | User-facing doc: what the channels mean, how to switch, how to disable update checks (`update.check: false`, `CAIRN_OFFLINE=1`), how to verify a downloaded artifact with `cairn release verify` once shipped. Links ADR 0005 as further reading. |

### 4.2 Touched files

| Path | Change |
|---|---|
| `docs/design/design-brief.md` §16 | Append new subsection **§16.b Release Channels and Updates [P3]** — one paragraph naming the three channels + offline-by-default invariant + pointer to ADR 0005. Section count in the table of sections stays consistent (§16, §16.a, §16.b is a clean addition; §17 is already "Non-Goals"). |
| `docs/design/traceability.md` §16 row | Append "#141 (release channels and updates — ADR 0005)" to the Implementation column; coverage note grows to mention "release channels frozen for v1.0". |
| `docs/site/src/SUMMARY.md` | Add nav entries for `maintainers/release-channels.md` and `usage/updates.md` so mdbook ships both pages. |
| `docs/site/src/maintainers/beta-readiness.md` | Add a new **manual gate** row (Gate 11 — renumbers downstream 11→12, 12→13, 13→14, 14→15, 15→16) **"Release channel policy frozen"** — reviewer asserts ADR 0005 present with `Status: Accepted`, §16.b pointer present, and per-channel signing keys actually loaded into CI secrets at v1.0 cutover. |
| `scripts/beta-readiness.sh` | Update the `print_manual_gates` heredoc to add the new Gate 11 line and renumber the existing 11→16. No new check function (mirrors how #140 wired Gate 10 — manual gates are reviewer-driven, not script-automated). |

## 5. Channel matrix (the load-bearing table)

| Channel | Trigger | Artifact destinations | Update poll feed | Cosign tag | Audience |
|---|---|---|---|---|---|
| **stable** | git tag `vX.Y.Z` (no pre-release suffix) | crates.io · `homebrew-cairn` main tap · winget · scoop · GitHub Releases (DMG/MSI/AppImage/deb/tarball) | `updates/stable/latest-<platform>.yml` on github.io | `cairn-stable` | Default for everyone; what `brew install cairn` gives you |
| **beta** | git tag `vX.Y.Z-beta.N` or `vX.Y.Z-rc.N` | `homebrew-cairn-beta` tap · GitHub Pre-Releases · no crates.io publish (pre-release versions auto-skipped by `cargo install` unless `--version` pinned) | `updates/beta/latest-<platform>.yml` | `cairn-beta` | Opt-in. Users who want the next release with at least one tagged checkpoint of stability |
| **nightly** | scheduled GHA every 24h off `main`, tagged `nightly-YYYYMMDD` | GitHub Releases ("Nightly" section), **no** package-manager publish | `updates/nightly/latest-<platform>.yml` | `cairn-nightly` | Developers + dogfooders. No semver promise. Aged off after 30 days |

**One binary per platform per channel.** The Rust core compiles
identically; the only difference is the build-time `CAIRN_CHANNEL` env
var the build embeds and `cairn status` reports. Channel-specific
behavior (feed URL, update-check default, signature-verify identity) is
table-driven off that one string.

**Channel pinning lives in two places.**

- **CLI:** nothing on disk. Channel = whichever artifact the user
  installed. `brew upgrade cairn` walks the stable tap;
  `brew tap cairn/beta && brew upgrade cairn` walks beta.
  `cargo install --locked --bin cairn cairn-cli` is always stable.
- **Desktop:** `desktop-config.json` (under the desktop app's app-support dir per OS — `~/Library/Application Support/cairn/` on macOS, mirrors the `vault_registry.json` location from #139) `update.channel` field, default
  `stable`. Runtime channel switch triggers a one-shot fetch of the chosen
  feed; the change applies on next launch.

## 6. Update mechanism (per OS)

| OS | Updater | Feed format | Platform signature |
|---|---|---|---|
| macOS | `electron-updater` reading its native YAML feed | `updates/<channel>/latest-mac.yml` on github.io | Apple Developer ID + notarization (already wired in #139). Cosign `.sig` sidecar as defence-in-depth. |
| Windows | `electron-updater` Squirrel | `updates/<channel>/latest.yml` on github.io | Authenticode signed MSI (EV cert deferred to v1.0-rc). Cosign sidecar. |
| Linux AppImage | AppImageUpdate (zsync over HTTPS) | embedded `update-information` field | GPG-signed `.sig` next to artifact. Cosign sidecar. |
| brew / cargo / winget / scoop | Owned by the package manager; we publish artifacts + sidecars. | n/a | Package manager's existing verification + Cosign sidecar for users who want to double-check via `cairn release verify`. |

**Cosign uniform layer.** Every released artifact carries a Cosign
keyless OIDC signature (`<artifact>.sig` + `<artifact>.pem`) published
alongside the artifact on its GitHub Release. The shipped CLI verifier
`cairn release verify <path>`:

1. Reads the embedded channel marker.
2. Looks up the matching public identity on the Sigstore Rekor
   transparency log.
3. Verifies the Cosign signature.
4. Additionally invokes the OS-native verifier when running on the
   matching OS (`codesign --verify` / `signtool verify` /
   `gpg --verify`).

Fail-closed: any verification failure → non-zero exit + the
`CapabilityUnavailable` remediation hint pointing back at this ADR.

## 7. Privacy / offline contract

- **Off by default.** No outbound network call for update checks until
  the user opts in via the onboarding prompt or by setting
  `update.check: true` in `desktop-config.json` (under the desktop app's app-support dir per OS — `~/Library/Application Support/cairn/` on macOS, mirrors the `vault_registry.json` location from #139).
- **`CAIRN_OFFLINE=1` and `agent.offline: true` always win.** If either
  is set, the update poller is dead code regardless of `update.check`.
  Tested at config-load time, not at poll time.
- **Payload is metadata-only.** Channel name, current version, OS, arch,
  and an opaque rotating install salt (regenerated weekly via the
  identity service, never linked to vault contents). No vault data, no
  record IDs, no user identifiers, no IP-derived geo. Logged at `trace`
  only (brief §6.6 rule: never log raw record bodies above `debug`; the
  rule is extended here to also cover update-poll payloads).
- **Endpoint is a static file** (`updates/<channel>/latest-<platform>.yml` on
  github.io / a Cloudflare Pages mirror). No server-side application
  logging beyond the hoster's standard access logs, which the user-facing
  doc names so users know what to expect.
- **CLI never polls.** Only the desktop shell can be opted in. CLI users
  learn about updates from `brew outdated` / `cargo install --force` /
  the one-shot `cairn status --check-updates` (explicit invocation only,
  never automatic, never recurring).

## 8. Channel migration + rollback

### 8.1 Channel migration (forward-only)

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
   shape as #139 §6.3).

### 8.2 Rollback (manual, documented)

- `cairn release rollback --to v1.0.3` recipe (shipped with the verifier
  CLI in a follow-up issue): downloads the named prior signed artifact
  from GitHub Releases, runs `cairn release verify`, and instructs the
  user how to drop it in place per OS. Vault is untouched throughout.
- No automatic boot-probe rollback in v1.0 (deferred to v1.1 — see
  §10).
- **Supported-version window.** Every stable release receives signed
  patch updates until the **second** subsequent stable lands (`vN`
  supported until `vN+2`). Past that, the artifact stays on GitHub
  Releases for audit but receives no further security backports. The
  policy is enforced socially via the maintainer recipe doc; no code
  gate.

## 9. Error model

Canonical error strings the docs reference; surfaced to users verbatim
when applicable.

| Failure | Surface | Behavior |
|---|---|---|
| Update poll attempted while `CAIRN_OFFLINE=1` | desktop main process | Skipped silently, logged at `debug`. Never user-visible. |
| Cosign verification fails on a downloaded update | `electron-updater` callback | Download discarded, dialog **"Update verification failed — keeping current version"**. No retry. |
| Apple notarization missing (e.g., a nightly downloaded out-of-band) | macOS Gatekeeper before main runs | Outside Cairn. User doc points to `xattr -d com.apple.quarantine` **only** for the nightly path, with a "you're skipping a check" warning. |
| Channel feed unreachable | desktop main process | One retry with exponential backoff (max 30s), then silent skip until next launch. Never blocks app boot. |
| User sets `update.channel` to an unknown value | config loader | Fail closed at startup with `EX_CONFIG=78`, listing valid values (`stable`, `beta`, `nightly`). |
| `cairn release verify` finds tampered sidecar | CLI | Non-zero exit, prints the Rekor lookup URL so the user can audit the original signature on the transparency log. |
| Vault schema newer than binary after a channel downgrade | sidecar startup | `VaultSchemaNewerThanBinary`. Vault left untouched. Dialog suggests reinstalling the newer version or picking another vault. |
| Cosign sidecar missing on a stable-channel artifact | release-time CI | Release workflow fails before publishing. No artifact ever reaches the channel without a sig. |

## 10. Testing strategy

All doc-level, since the scope is doc-only.

| Test | Mechanism |
|---|---|
| ADR-0005 cross-reference present | Reviewer-enforced via PR checklist. (No automated freeze-check extension — `scripts/check-freeze.sh` is reserved for ADR 0002 split triggers per its header comment; not a general ADR-cross-reference linter.) |
| Beta-readiness gate enforces the policy | Manual gate row added to `print_manual_gates` in `scripts/beta-readiness.sh`. Reviewer asserts the four files exist + Status line + §16.b anchor at v1.0 cutover. |
| Traceability matrix points at #141 | Existing convention; reviewer-enforced (no CI gate today per traceability.md "Enforcement" section). |
| mdbook + rustdoc clean | Same as PR #421 — no new failure modes introduced. |
| Capability matrix unchanged | No capabilities added; no advertise() touch; existing `contract-drift` job stays green by construction. |
| Wire compat unchanged | No envelope, verb, or error-code touch; existing `wire_compat_v1` snapshots stay green by construction. |

What this PR deliberately does **not** test:

- The update poller code path — doesn't exist yet.
- `cairn release verify` — doesn't exist yet.
- electron-updater YAML feed — doesn't exist yet.
- Cosign sidecar layout — doesn't exist yet.

Each is a named follow-up issue under #32 cited from the ADR.

## 11. Acceptance-criteria mapping (from issue #141)

| Issue criterion | How satisfied |
|---|---|
| Define stable, beta, and nightly release channels for CLI and desktop artifacts | §5 channel matrix + ADR 0005 §1; brief §16.b summary. |
| Implement update metadata, signature verification, rollback guidance, and channel migration rules | §6 update mechanism + §8 migration/rollback + ADR 0005 §§2–4. Implementation deferred to named follow-up issues under #32; this PR ships the **rules** they will implement against. |
| Ensure update checks respect privacy/offline expectations | §7 privacy/offline contract + ADR 0005 §5. |
| Users can choose a release channel intentionally | §5 (CLI = pick your tap; desktop = `update.channel`). User-facing doc walks through both. |
| Artifacts are signed and update metadata is verifiable | §6 Cosign-uniform-layer + per-OS platform-native signatures. Verifier shape pinned by ADR 0005 §3. |
| Offline users can disable update checks and still use local vaults | §7 — off by default, and dead-code under `CAIRN_OFFLINE=1` regardless of any other setting. |

Verification checklist items from the issue (update metadata
verification tests, channel migration tests, offline/disabled update
tests) are mapped to follow-up implementation issues — this design PR
ships the **specs they test against**, not the tests themselves.

## 12. Open questions / decisions deferred

- **Boot-probe rollback for v1.1.** Implementing `electron-updater`'s
  try-once boot probe requires preserving the prior `.app` bundle on
  every update and a state machine for "first launch after update". The
  design noise is real engineering; deferring to v1.1 keeps #141 in the
  doc-only lane. A follow-up issue under #32 captures this.
- **Cloudflare Pages mirror of `updates/<channel>/latest-<platform>.yml`.** Default is
  github.io directly. If GitHub Pages bandwidth becomes a concern, the
  mirror is a one-line DNS change; named in the ADR as a permitted
  variant.
- **Aged-off nightly retention.** 30 days is the proposed window. The
  maintainer recipe doc spells out the GHA cleanup job; the actual job
  ships in a follow-up.
- **EV cert acquisition for Windows.** Authenticode signed MSI is
  required for SmartScreen to stop prompting; getting the EV cert is an
  ops step gated on v1.0-rc. ADR 0005 names it as a prerequisite to
  cutting v1.0 stable; the cert acquisition itself is out of scope of
  this PR.
- **Per-channel update-check telemetry.** Off the table for v1.0. If
  ever revisited, it would be opt-in twice (channel opt-in + telemetry
  opt-in) and gated by a brief amendment.

## 13. Sequencing into PRs

Single PR for this slice (~600 lines incl. all six docs + script
extensions). No code touched; no Cargo regen; no wire-compat snapshot
regen.

Logically:

1. ADR 0005 lands first within the PR (it is the policy the other docs
   reference).
2. Brief §16.b patch + traceability matrix update.
3. Maintainer recipe + user-facing usage page; SUMMARY.md nav.
4. Beta-readiness gate row (manual-gate heredoc in
   `scripts/beta-readiness.sh` + matching row in
   `docs/site/src/maintainers/beta-readiness.md`).

Each commit individually buildable; PR mergeable when all four steps
land.

## 14. Cross-references

- **Brief sections:** §16 Distribution and Packaging · §16.b (new
  subsection introduced by this design) · §19 v1.0 Production · §2
  Design principles (invariants #1, #2, #6, #9) · §14 Privacy.
- **Sibling ADRs:** ADR 0004 (`cairn.mcp.v1` semver freeze — same
  doc-only shape; cited as precedent).
- **Sibling specs:** `docs/superpowers/specs/2026-05-25-desktop-packaging-macos-design.md`
  (#139 — provides the macOS signing/notarization wiring this policy
  builds on).
- **Sibling issues:** #139 (desktop packaging, merged) · #140 (MCP
  semver freeze, merged via PR #421) · #142 (v1.0 production readiness,
  depends on this).
