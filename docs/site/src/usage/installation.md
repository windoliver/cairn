# Installation

Cairn v0.1 is a single Rust binary. Two supported install paths.

## `cargo install` (any platform)

```bash
cargo install cairn-cli
```

Requires Rust 1.95+. The crate name is `cairn-cli`; it builds a binary named
`cairn` into `~/.cargo/bin/cairn`. No other runtime dependencies.

> The shorter `cairn` crate name on crates.io is already taken by an unrelated
> project; renaming `cairn-cli → cairn` is tracked under issue #141.

## `brew install` (macOS / Linux)

```bash
brew tap windoliver/cairn https://github.com/windoliver/cairn
brew install --HEAD cairn
```

Builds from source via the bundled `cargo install` step. The formula is
**HEAD-only** until v0.1.0 is tagged — there is no stable `url`/`sha256`
block today, so `--HEAD` is the only supported install path. Bottles
(pre-built binaries) and the first stable release are tracked under
issue #141; once v0.1.0 ships, `brew install cairn` will work without
`--HEAD`.

## First-run model fetch

`cairn bootstrap` initialises a vault and, when `search.local_embeddings`
is `true` in `.cairn/config.yaml` (the default), downloads the local
embedding model (`bge-small-en-v1.5`, ~128 MB) to
`<vault>/.cairn/models/`. This happens **once per vault**; subsequent
runs use the cache and require no network access.

- The fetch uses the Hugging Face Hub over HTTPS. Set `HF_ENDPOINT` to
  point at a mirror if your network blocks `huggingface.co`.
- **No API keys are required.** The model is public; Cairn never sends
  user data to any third party as part of the fetch.
- To skip the fetch entirely, set `search.local_embeddings: false` in
  `.cairn/config.yaml` before running `bootstrap`. Keyword search still
  works; semantic and hybrid search are then rejected with
  `CapabilityUnavailable` (brief §8.0.b).

## Verifying the install

```bash
cairn --version
cairn bootstrap --vault-path /tmp/cairn-vault
cd /tmp/cairn-vault
cairn status --json
```

A complete verification run is `scripts/install-smoke.sh` in the
source tree — point it at your installed binary:

```bash
CAIRN_BIN="$(which cairn)" scripts/install-smoke.sh
```

It bootstraps a temp vault, exercises the eight P0 verbs (`status`, `ingest`,
`search`, `retrieve`, `summarize`, `assemble_hot`, `capture_trace`, `lint`,
`forget`), and exits 0 on full pass.

## Offline after the first run

Once a vault is bootstrapped and the embedding model is cached, Cairn
runs fully offline: no network calls are made by any of the eight verbs
during a normal session. Sensors, workflows, and MCP serving all stay
local (brief §19).
