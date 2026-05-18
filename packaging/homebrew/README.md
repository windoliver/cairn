# Homebrew formula — maintainer notes

The user-facing formula lives at the repo-root `Formula/cairn.rb` — that
is the canonical path Homebrew searches when a tap points at this
repository. This directory holds only the maintainer documentation; do
not ship a duplicate formula here.

The formula builds from source via `cargo install`; bottles (pre-built
binaries) are deferred to issue #141.

## Tap layout

This repo *is* the tap. End users:

```bash
brew tap windoliver/cairn https://github.com/windoliver/cairn
brew install --HEAD cairn    # supported pre-release path
brew install cairn           # only after v0.1.0 ships (sha256 placeholder today)
```

Homebrew discovers formulae at one of three locations in a tap repo —
`Formula/`, `HomebrewFormula/`, or the tap root. We use `Formula/` so
the tap layout is unambiguous.

## Formula structure (`Formula/cairn.rb`)

- `head` → `https://github.com/windoliver/cairn.git@main` — installable
  today with `brew install --HEAD cairn`.
- `url` / `sha256` → tagged release tarball; the sha256 is a placeholder
  until the first `v0.1.0` tag is cut. Homebrew infers the `version`
  from the `vX.Y.Z` tag in the URL, so no explicit `version` line is
  needed.

## Updating on a release

1. After the release workflow uploads the tarball at
   `https://github.com/windoliver/cairn/archive/refs/tags/vX.Y.Z.tar.gz`,
   compute its sha256:

   ```bash
   curl -sL https://github.com/windoliver/cairn/archive/refs/tags/vX.Y.Z.tar.gz \
     | shasum -a 256
   ```

2. Edit `Formula/cairn.rb`:
   - `url` → the tagged URL above (Homebrew parses `X.Y.Z` from `vX.Y.Z`).
   - `sha256` → the computed digest.

3. Validate locally before tapping. Homebrew 5.x rejects `brew audit
   <path>` and `brew install <path>`, so stage the formula into a temp
   tap first:

   ```bash
   # 1. Stage the formula into a local tap.
   tap="$(brew --repository)/Library/Taps/local/homebrew-cairn"
   mkdir -p "$tap/Formula"
   cp Formula/cairn.rb "$tap/Formula/cairn.rb"

   # 2. Audit + install + test through the tap.
   brew audit --strict local/cairn/cairn
   brew install --build-from-source --HEAD local/cairn/cairn
   brew test cairn

   # 3. Tear down.
   rm -rf "$tap"
   ```

The `install-smoke` job in `release-dry-run.yml` runs the same audit
recipe in CI on every `workflow_dispatch` and tag push.
