# Updates

Cairn ships under three release channels. Pick whichever fits how
much stability and risk you want.

| Channel | Who it's for | How you get it |
|---|---|---|
| **stable** (default) | Everyone. Tagged releases only. | `brew install cairn` · `cargo install --locked --bin cairn cairn-cli` · DMG / MSI / AppImage / deb from [GitHub Releases](https://github.com/windoliver/cairn/releases) |
| **beta** | Users who want the next release with at least one tagged checkpoint of stability. | `brew tap cairn/beta && brew install cairn` · GitHub Pre-Releases |
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
```

Beta releases are not published to crates.io (per ADR 0005 §1). To switch the CLI to beta, use the Homebrew tap shown above, or download the signed beta artifact from [GitHub Pre-Releases](https://github.com/windoliver/cairn/releases) and verify it manually.

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
`https://windoliver.github.io/cairn/updates/<channel>/latest-<platform>.yml` once per
24 hours. The payload it sends is metadata-only: channel name,
current version, OS, arch, and an opaque rotating install ID that
resets weekly. No vault content, no user identity, no IP-derived
geo.

## Verifying a downloaded artifact

Every artifact on a Cairn GitHub Release ships with a Cosign keyless
OIDC signature (`<artifact>.sig` + `<artifact>.pem`) committed to the
Sigstore Rekor transparency log. The shipped CLI verifier:

*Note: `cairn release verify` and `cairn release rollback` ship with v1.0. Check your installed version with `cairn --version`. Until then, use the manual `cosign verify-blob` recipe in the next subsection.*

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

*Note: the `cairn release rollback` recipe ships with v1.0 alongside the verifier CLI. Until then, download the prior signed artifact from [GitHub Releases](https://github.com/windoliver/cairn/releases) and install it via your package manager or by replacing `/Applications/Cairn.app` directly.*

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
- `cargo install --locked --bin cairn cairn-cli --force` — pulls the latest non-pre-release
  from crates.io.
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
