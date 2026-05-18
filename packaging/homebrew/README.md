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
brew install --HEAD cairn    # only supported install path pre-release
```

Homebrew discovers formulae at one of three locations in a tap repo —
`Formula/`, `HomebrewFormula/`, or the tap root. We use `Formula/` so
the tap layout is unambiguous.

## Formula structure (`Formula/cairn.rb`)

The formula is **HEAD-only** until v0.1.0 ships. The `head` block points
at `https://github.com/windoliver/cairn.git@main`. There is no stable
`url`/`sha256` block today — advertising one with a placeholder sha256
would make `brew install cairn` (the default stable path) fail at
checksum verification on every user's machine.

## Adding the stable block on the first release

When release #141 cuts `v0.1.0`:

1. Compute the tarball sha256:

   ```bash
   curl -sL https://github.com/windoliver/cairn/archive/refs/tags/vX.Y.Z.tar.gz \
     | shasum -a 256
   ```

2. Add the stable block to `Formula/cairn.rb` immediately after
   `homepage`:

   ```ruby
   url "https://github.com/windoliver/cairn/archive/refs/tags/vX.Y.Z.tar.gz"
   sha256 "<digest from step 1>"
   ```

   Homebrew parses `X.Y.Z` from `vX.Y.Z`, so no explicit `version` line
   is needed. After this lands, `brew install cairn` becomes the
   default install path.

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
