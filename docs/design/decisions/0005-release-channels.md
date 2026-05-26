# ADR 0005 — Release channels and auto-update policy

- **Status:** Accepted — 2026-05-26
- **Deciders:** Cairn maintainers
- **Issue:** [#141](https://github.com/windoliver/cairn/issues/141)
- **Parent epic:** [#32](https://github.com/windoliver/cairn/issues/32)
- **Design-brief sections:** §2 (design principles), §14 (privacy), §16 (distribution and packaging), §16.b (new subsection introduced by this ADR), §19 (v1.0 production)
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
workflows, electron-updater YAML feed publishing) is tracked as named follow-up
issues under parent epic #32 — each cites this ADR for shape.

## Decision

The following rules govern Cairn releases from v1.0 forward.

### 1. Channel matrix

| Channel | Trigger | Artifact destinations | Update poll feed | Cosign tag | Audience |
|---|---|---|---|---|---|
| **stable** | git tag `vX.Y.Z` (no pre-release suffix) | crates.io · `homebrew-cairn` main tap · winget · scoop · GitHub Releases (DMG/MSI/AppImage/deb/tarball) | `updates/stable/latest-<platform>.yml` on github.io | `cairn-stable` | Default for everyone; what `brew install cairn` gives you |
| **beta** | git tag `vX.Y.Z-beta.N` or `vX.Y.Z-rc.N` | `homebrew-cairn-beta` tap · GitHub Pre-Releases · no crates.io publish (pre-release versions auto-skipped by `cargo install` unless `--version` pinned) | `updates/beta/latest-<platform>.yml` on github.io | `cairn-beta` | Opt-in. Users who want the next release with at least one tagged checkpoint of stability |
| **nightly** | scheduled GHA every 24h off `main`, tagged `nightly-YYYYMMDD` | GitHub Releases ("Nightly" section), **no** package-manager publish | `updates/nightly/latest-<platform>.yml` on github.io | `cairn-nightly` | Developers + dogfooders. No semver promise. Aged off after 30 days. |

One binary per platform per channel. The Rust core compiles
identically; the only difference is the build-time `CAIRN_CHANNEL`
env var the build embeds and `cairn status` reports. Channel-specific
behavior (feed URL, update-check default, signature-verify identity)
is table-driven off that one string.

Channel pinning lives in two places:

- **CLI:** nothing on disk. Channel = whichever artifact the user
  installed. `brew upgrade cairn` walks the stable tap;
  `brew tap cairn/beta && brew upgrade cairn` walks beta.
  `cargo install --locked --bin cairn cairn-cli` is always stable.
- **Desktop:** `desktop-config.json` `update.channel` field (under the
  desktop app's app-support dir per OS — `~/Library/Application
  Support/cairn/` on macOS, mirrors the `vault_registry.json` location
  from #139), default `stable`. Runtime channel switch triggers a
  one-shot fetch of the chosen feed; the change applies on next launch.

### 2. Per-OS update mechanism

| OS | Updater | Feed format | Platform signature |
|---|---|---|---|
| macOS | `electron-updater` reading its native YAML feed | `updates/<channel>/latest-mac.yml` on github.io | Apple Developer ID + notarization (wired in [#139](https://github.com/windoliver/cairn/issues/139)). Cosign `.sig` sidecar as defence-in-depth. |
| Windows | `electron-updater` Squirrel | `updates/<channel>/latest.yml` on github.io | Authenticode signed MSI (EV cert deferred until v1.0-rc). Cosign sidecar. |
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
  arch, and an opaque rotating install ID (generated locally by the desktop
  shell and rotated weekly; never linked to vault contents). No vault data, no
  record IDs, no user identifiers, no IP-derived geo. Logged at `trace`
  only. The `CLAUDE.md` §6.6 rule ("never log raw record bodies above
  `debug`") is extended here to also cover update-poll payloads.
- **Endpoint is a static file** (`updates/<channel>/latest-<platform>.yml` on github.io /
  optional Cloudflare Pages mirror). No server-side application
  logging beyond the hoster's standard access logs.
- **CLI never polls.** Only the desktop shell can be opted in. CLI users
  learn about updates from `brew outdated` / `cargo install --force` /
  the one-shot `cairn status --check-updates` (explicit invocation
  only, never automatic, never recurring).

### 5. Channel migration

1. User changes `update.channel` from stable → beta (or vice versa)
   via the Settings UI or `cairn config set update.channel beta`.
2. Next launch fetches the chosen channel's feed **once**, surfaces a
   "Update to vX.Y.Z-beta.1 available" prompt.
3. User confirms → electron-updater pulls + verifies + restarts.
4. Vault registry stays put. The same vault dirs survive every channel
   switch — channel is a binary-install concept, not a vault concept.
5. Downgrade across a vault-schema bump is **blocked at startup** with
   `VaultSchemaNewerThanBinary` error and a dialog suggesting either
   reinstalling the newer version or picking a different vault.

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
the beta-readiness runbook (new Gate 11 "release channel policy frozen" added by this ADR's PR, which renumbers the existing manual gates 11–15 to 12–16). Concrete
runtime gates ship in named follow-up issues:

- Signed electron-updater YAML feeds + Cosign sidecars per channel — follow-up under
  parent epic #32.
- `cairn release verify` CLI — follow-up under parent epic #32.
- `electron-updater` wiring + `update.channel` config + onboarding
  prompt — follow-up under parent epic #32.
- `cairn config set update.channel <name>` and `cairn status --check-updates` CLI subcommands — follow-up under parent epic #32.
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
  brief §2 invariant #3 ("stand-alone — a single Rust static binary on a fresh laptop with zero cloud credentials works end-to-end"). The default must be zero outbound traffic.

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
