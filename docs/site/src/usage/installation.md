# Installation

Cairn v0.1 is a single Rust binary. Two supported install paths.

## `cargo install` (any platform)

Until v0.1.0 is published to crates.io (issue #141), install from git:

```bash
cargo install --locked --git https://github.com/windoliver/cairn \
  --branch main --bin cairn cairn-cli
```

Requires Rust 1.95+. The package name is `cairn-cli`; `--bin cairn` selects the
single user-facing binary (the package also contains an internal
`cairn-docgen` doc generator that is not part of the supported surface).
`--locked` matches the lockfile that CI tests against. The result is a `cairn`
binary in `~/.cargo/bin/cairn` with no other runtime dependencies.

Once v0.1.0 is on crates.io the command becomes:

```bash
cargo install --locked --bin cairn cairn-cli
```

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
  user data to any third party as part of the fetch, and any cached HF
  bearer token in `$HF_HOME/token` is explicitly cleared before the
  request so a hostile mirror cannot harvest it.
- **Trust model (v0.1):** Cairn trusts the configured endpoint
  (huggingface.co or your `HF_ENDPOINT` mirror) to serve untampered
  bytes for the public model repos. The cache records a blake3 digest
  of what was fetched, but v0.1 does not pin to a known-good digest in
  the binary; if you operate under a stricter threat model, point
  `HF_ENDPOINT` at an internal mirror you control. Tightening this to
  per-file pinned digests is tracked under a follow-up issue.
- To skip the fetch entirely, set `search.local_embeddings: false` in
  `.cairn/config.yaml` before running `bootstrap`. Keyword search still
  works; semantic and hybrid search are then rejected with
  `CapabilityUnavailable` (brief §8.0.b).

## Signing-key storage

`cairn ingest` and the other mutating verbs auto-provision a default issuer
the first time they run. The signing key is written to a keystore.

- **macOS:** the OS keychain (Keychain Access). You may be prompted to allow
  access on the first run.
- **Windows:** the Windows Credential Manager.
- **Linux:** the kernel keyutils (per the `keyring` crate's `linux-native`
  backend — **not** Secret Service / gnome-keyring). On most distributions
  keyutils entries are session-scoped: they survive across processes within
  one login session but are wiped on reboot or logout. If you need keys
  that outlive a reboot, switch to the file backend below.
- **Headless / CI / Docker / WSL / reboot-persistent setups:** set
  `CAIRN_KEYSTORE=file` before running any mutating verb — the key is
  stored at `<vault>/.cairn/keystore.json` instead of the OS keystore:

  ```bash
  export CAIRN_KEYSTORE=file
  ```

  `<vault>/.cairn/keystore.json` should be backed up alongside the vault
  and treated as sensitive (`chmod 600`). The
  `scripts/install-smoke.sh` test harness sets `CAIRN_KEYSTORE=file`
  automatically because CI runners are headless.

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

See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.
