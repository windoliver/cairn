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
7. Walk Gates 9–16 of [Beta Readiness](beta-readiness.md) manually,
   including Gate 11 (this page is the evidence for Gate 11 — confirm
   ADR 0005 status is `Accepted`).
8. Trigger the `release-stable.yml` workflow (added in follow-up
   under #32) with the tag as input. It builds + signs + publishes
   to all stable destinations + updates the `homebrew-cairn` tap +
   updates `updates/stable/latest-{mac,windows}.yml` electron-updater feed
   (for macOS and Windows; the Linux AppImage's embedded
   `update-information` field is regenerated at build time).
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
   "Pre-release", updates `updates/beta/latest-{mac,windows}.yml`
   (macOS and Windows; Linux AppImage carries embedded metadata).
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
verifier CLI) downloads + verifies + drops the prior signed artifact
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

### Current trust anchors

| Channel | Cosign identity | Cosign issuer |
|---|---|---|
| stable | `https://github.com/windoliver/cairn/.github/workflows/release-stable.yml@refs/tags/v*` | `https://token.actions.githubusercontent.com` |
| beta | `https://github.com/windoliver/cairn/.github/workflows/release-beta.yml@refs/tags/v*-{beta,rc}.*` | same |
| nightly | `https://github.com/windoliver/cairn/.github/workflows/release-nightly.yml@refs/tags/nightly-*` | same |
| GPG (AppImage) | _Fingerprint to be published in the v1.0 release notes._ | |

These identities are frozen under [ADR 0005 §3.a](../../../design/decisions/0005-release-channels.md). Any change to the workflow path or ref pattern requires `cairn.update.v2`.

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
