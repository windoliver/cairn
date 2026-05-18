# Issue #100 — `cargo install`, Homebrew formula, install smoke tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the packaging surface that lets a clean machine install Cairn via `cargo install cairn` (once 0.1 is published) or `brew install cairn`, and a smoke test script that exercises the eight P0 verbs against the freshly installed binary. Wire both into CI.

**Architecture:** A Homebrew formula at `packaging/homebrew/cairn.rb` (HEAD-installable today; tagged release block ready for first publish) builds from source via `cargo install --path crates/cairn-cli`. A shell smoke script `scripts/install-smoke.sh` operates against any `cairn` binary on `PATH` or pointed at via `$CAIRN_BIN`, bootstraps an isolated temp vault, runs `status`/`ingest`/`search`/`retrieve`/`lint`/`forget`, and asserts each verb's success contract from §8.0.b. A new `install-smoke` job in `release-dry-run.yml` cargo-installs the workspace into a temp prefix, runs the smoke, and on macOS additionally runs `brew audit --strict` against the formula. The `cairn bootstrap` model-fetch path (§5 of the brief / `crates/cairn-cli/src/main.rs:778`) already implements the one-time-fetch / offline-after-cache contract; we document it but do not change it.

**Tech Stack:** Rust 1.95 / Cargo workspace, Bash, Homebrew DSL (Ruby), GitHub Actions.

**Brief sections touched:** §16 Distribution and Packaging, §19 v0.1 install artifact.
**Invariants touched:** none of the load-bearing seven — purely additive packaging + verification.

---

## File Structure

- **Create:** `packaging/homebrew/cairn.rb` — Homebrew formula for the `cairn` binary. Two install paths: `head` from main, and a versioned release block guarded behind a sha256 placeholder (filled in by the first real release tag).
- **Create:** `packaging/homebrew/README.md` — explains formula layout, how to tap it, how to update the sha256/version on each release.
- **Create:** `scripts/install-smoke.sh` — POSIX shell smoke harness. Takes optional `$CAIRN_BIN`; defaults to `cairn` on `PATH`. Bootstraps a temp vault, runs each verb, asserts exit codes and JSON shape, exits 0 on full pass.
- **Create:** `docs/site/src/usage/installation.md` — public install instructions: `cargo install cairn`, `brew install` (tap path), what the first-run model fetch does, and how to run offline once cached. Linked from `docs/site/src/SUMMARY.md`.
- **Create:** `crates/cairn-cli/tests/install_smoke.rs` — Rust integration test that runs `scripts/install-smoke.sh` against the just-built binary (`env!("CARGO_BIN_EXE_cairn")`), so the smoke contract is also enforced by `cargo nextest`.
- **Modify:** `crates/cairn-cli/Cargo.toml:11` — tighten the `description` field for crates.io presentation; add `categories` and `keywords`.
- **Modify:** `.github/workflows/release-dry-run.yml` — add an `install-smoke` job that does `cargo install --path crates/cairn-cli --root $TMP --locked --no-track`, then runs the smoke script against `$TMP/bin/cairn`. On macOS-only, runs `brew audit --strict --formula packaging/homebrew/cairn.rb`.
- **Modify:** `docs/ci.md:201-202` — delete the "deferred" line for issue #100; replace with a row in the workflow inventory describing the new job.
- **Modify:** `docs/site/src/SUMMARY.md` — link the new installation page.

---

### Task 1: Tighten `cairn-cli` crates.io metadata

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml:11`

The current `description` reads "Cairn terminal entry point. Wires adapters into the verb layer." That is internal-architecture language. crates.io users searching for a memory framework need a one-line value prop. Also add `categories` and `keywords` so the crate is discoverable.

- [ ] **Step 1: Edit the package metadata block**

Replace lines 1–13 of `crates/cairn-cli/Cargo.toml` with:

```toml
[package]
name = "cairn-cli"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
readme.workspace = true
description = "Cairn — harness-agnostic agent memory: one binary, one SQLite file, one markdown vault. P0 ships offline-after-cache with eight verbs over CLI/MCP/SDK."
categories = ["command-line-utilities", "database", "development-tools"]
keywords = ["memory", "agent", "llm", "mcp", "vault"]
default-run = "cairn"
```

(crates.io enforces ≤5 keywords and ≤5 categories; we use 5 and 3 respectively.)

- [ ] **Step 2: Verify `cargo package` still accepts the manifest**

Run: `cargo package -p cairn-cli --no-verify --locked --allow-dirty`
Expected: exit 0; `target/package/cairn-cli-0.0.1.crate` produced.

- [ ] **Step 3: Verify `cargo metadata` round-trips**

Run: `cargo metadata --no-deps --format-version 1 --locked >/dev/null`
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/Cargo.toml
git commit -m "chore(cairn-cli): crates.io description, categories, keywords (#100)"
```

---

### Task 2: Write the install smoke script

**Files:**
- Create: `scripts/install-smoke.sh`

This is the load-bearing artefact for the issue's acceptance criteria. It must work against any `cairn` binary, regardless of how that binary got installed (cargo, brew, or a CI-side `cargo install --path`).

Contract:
- Reads `$CAIRN_BIN` (default `cairn`).
- Creates an isolated temp vault under `$TMPDIR` (auto-cleaned with a trap).
- Runs `bootstrap → status → ingest → search → retrieve → lint → forget` against the temp vault.
- Each step that succeeds prints `ok: <verb>` on stdout; the script exits 1 with `fail: <verb> — <reason>` on the first failure.
- All verbs use `--json`. Each envelope must include `"status":"committed"` (per §8.0.b) — we grep for that string, no jq required (the script must be runnable on a bare macOS shell).
- The script disables local embeddings by setting `CAIRN_SEARCH_LOCAL_EMBEDDINGS=false` so the smoke does not depend on the 128 MB model fetch — that path is exercised separately in Task 6. **Verify this env var actually exists in `cairn-cli` config (`crates/cairn-cli/src/config.rs`); if not, write the config file directly.**

- [ ] **Step 1: Check the env override actually exists**

Run: `grep -n "CAIRN_SEARCH_LOCAL_EMBEDDINGS\|local_embeddings" crates/cairn-cli/src/config.rs`
Expected: at least one match for `local_embeddings`. If `CAIRN_SEARCH_*` env vars are not honored, plan adjusts: the smoke script writes a literal `.cairn/config.yaml` with `search:\n  local_embeddings: false` *before* `cairn bootstrap` (which respects an existing config). Use the env path if it exists, otherwise the file path.

- [ ] **Step 2: Write the script**

Create `scripts/install-smoke.sh` with content:

```bash
#!/usr/bin/env bash
# scripts/install-smoke.sh — issue #100 install smoke for the cairn binary.
#
# Exercises the P0 verb set against a freshly bootstrapped vault. Honoured by
# both `cargo install` and `brew install` paths: runs against the binary at
# $CAIRN_BIN (default: `cairn` from PATH).
#
# Local embeddings are turned off so the smoke does not depend on the
# one-time model fetch (~128 MB). That path is covered separately by the
# bootstrap integration tests (crates/cairn-cli/tests/bootstrap.rs).
#
# Exit: 0 on full pass; 1 with `fail: <verb> — <reason>` on first failure.

set -euo pipefail

CAIRN_BIN="${CAIRN_BIN:-cairn}"

if ! command -v "$CAIRN_BIN" >/dev/null 2>&1; then
  if [ ! -x "$CAIRN_BIN" ]; then
    echo "fail: cairn binary not found at '$CAIRN_BIN'" >&2
    exit 1
  fi
fi

VAULT="$(mktemp -d -t cairn-smoke-XXXXXX)"
trap 'rm -rf "$VAULT"' EXIT

cd "$VAULT"

# Disable local embeddings before bootstrap so the smoke does not block on a
# model fetch. The config file is created by bootstrap, but we pre-seed it.
mkdir -p .cairn
cat >.cairn/config.yaml <<'EOF'
search:
  local_embeddings: false
EOF

step() {
  local verb="$1"
  shift
  local out
  if ! out="$("$CAIRN_BIN" "$@" 2>&1)"; then
    echo "fail: $verb — non-zero exit"
    echo "$out" >&2
    exit 1
  fi
  if ! grep -q '"status":"committed"' <<<"$out"; then
    # status / lint surface their own committed envelopes; bootstrap returns a
    # BootstrapReceipt without an envelope wrapper, so accept either shape.
    if ! grep -q '"vault_id"\|"dirs_created"\|"capabilities"' <<<"$out"; then
      echo "fail: $verb — no committed envelope; got: $out"
      exit 1
    fi
  fi
  echo "ok: $verb"
}

step bootstrap   bootstrap --vault-path . --json
step status      status --json
step ingest      ingest --kind reference --body "smoke seed body" --json
step search      search --mode keyword --query "smoke" --json
# retrieve needs a record id; pull the most recent one out of the search
# response. We do this with sed/awk because jq is not guaranteed to be on the
# host (jq is not in `brew install cairn`'s runtime closure).
SEARCH_JSON="$("$CAIRN_BIN" search --mode keyword --query "smoke" --json)"
RECORD_ID="$(printf '%s\n' "$SEARCH_JSON" \
  | sed -n 's/.*"record_id":"\([^"]*\)".*/\1/p' | head -1)"
if [ -z "$RECORD_ID" ]; then
  echo "fail: search — no record_id in response; got: $SEARCH_JSON"
  exit 1
fi
step retrieve    retrieve --record-id "$RECORD_ID" --json
step lint        lint --json
step forget      forget --record-id "$RECORD_ID" --json

echo "smoke: all verbs passed (vault: $VAULT)"
```

- [ ] **Step 3: Make the script executable**

Run: `chmod +x scripts/install-smoke.sh`

- [ ] **Step 4: Smoke-test the script locally against `cargo run`**

Run:
```bash
cargo build --release -p cairn-cli --locked
CAIRN_BIN="$(pwd)/target/release/cairn" ./scripts/install-smoke.sh
```
Expected: exit 0; stdout shows `ok: bootstrap`, `ok: status`, `ok: ingest`, `ok: search`, `ok: retrieve`, `ok: lint`, `ok: forget`, `smoke: all verbs passed`.

If any verb fails: read its stderr, then look up the actual CLI shape in `crates/cairn-cli/tests/cli.rs` — that file is the canonical reference for working argument combinations. Update the script (not the verb).

- [ ] **Step 5: Commit**

```bash
git add scripts/install-smoke.sh
git commit -m "feat(scripts): add install-smoke for P0 verbs (#100)"
```

---

### Task 3: Wrap the smoke script in a Rust integration test

**Files:**
- Create: `crates/cairn-cli/tests/install_smoke.rs`

The script is the user-facing artefact; the Rust test guarantees `cargo nextest` regresses on it.

- [ ] **Step 1: Write the failing test**

```rust
//! Integration cover for `scripts/install-smoke.sh` (issue #100). Runs the
//! script against the just-built binary so the install smoke contract is part
//! of the regular `cargo nextest` run, not just the release-dry-run workflow.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn install_smoke_script_passes_against_built_binary() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scripts")
        .join("install-smoke.sh");
    assert!(
        script.is_file(),
        "install-smoke.sh missing at {}",
        script.display()
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = Command::new("bash")
        .arg(&script)
        .env("CAIRN_BIN", bin)
        .output()
        .expect("run install-smoke.sh");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "install-smoke failed: status={:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    for verb in [
        "ok: bootstrap",
        "ok: status",
        "ok: ingest",
        "ok: search",
        "ok: retrieve",
        "ok: lint",
        "ok: forget",
    ] {
        assert!(
            stdout.contains(verb),
            "missing `{verb}` in smoke output: {stdout}"
        );
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo nextest run -p cairn-cli --locked --test install_smoke`
Expected: PASS. If it fails, the script needs fixing — the verb CLI shapes in `crates/cairn-cli/tests/cli.rs` are the source of truth.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/install_smoke.rs
git commit -m "test(cairn-cli): wrap install-smoke.sh in cargo nextest (#100)"
```

---

### Task 4: Author the Homebrew formula

**Files:**
- Create: `packaging/homebrew/cairn.rb`
- Create: `packaging/homebrew/README.md`

Approach: a single formula with both a versioned release block (commented sha256 placeholder, to be filled by the first 0.1.0 cut) and a `head` block that points at `main`. `brew audit --strict` accepts head-only formulae as long as the structure is correct.

Crucially, the formula builds from source via `cargo install`. This keeps us on a single supported path until we set up signed bottles in #141.

- [ ] **Step 1: Write the formula**

Create `packaging/homebrew/cairn.rb`:

```ruby
# typed: false
# frozen_string_literal: true

# Cairn — harness-agnostic agent memory framework.
#
# Distribution path: brief §16. v0.1 ships a single Rust binary built from
# source via `cargo install`. Bottles are deferred to #141.
class Cairn < Formula
  desc "Harness-agnostic agent memory: one binary, one SQLite file, one vault"
  homepage "https://github.com/windoliver/cairn"
  license "Apache-2.0"
  head "https://github.com/windoliver/cairn.git", branch: "main"

  # First stable release block. URL and sha256 are populated by release #141
  # when the v0.1.0 tag is cut. Until then `brew install --HEAD cairn` is the
  # only supported install path; the stable block is present so `brew audit`
  # has a release URL to validate.
  url "https://github.com/windoliver/cairn/archive/refs/tags/v0.1.0.tar.gz"
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  depends_on "rust" => :build

  def install
    # Builds the `cairn` binary out of crates/cairn-cli. `--locked` keeps
    # dependency resolution identical to CI; `--no-track` keeps `cargo
    # install` from leaving a metadata file in the build sandbox.
    system "cargo", "install", *std_cargo_args(path: "crates/cairn-cli"),
           "--locked", "--no-track"
  end

  test do
    # Brew's own smoke: bin runs, vault bootstraps, status returns a
    # well-formed envelope. Mirrors scripts/install-smoke.sh in miniature.
    assert_match(/cairn /, shell_output("#{bin}/cairn --version"))
    ENV["CAIRN_VAULT"] = testpath.to_s
    (testpath/".cairn").mkpath
    (testpath/".cairn/config.yaml").write("search:\n  local_embeddings: false\n")
    system "#{bin}/cairn", "bootstrap", "--vault-path", testpath, "--json"
    status = shell_output("#{bin}/cairn status --json")
    assert_match(/"capabilities"/, status)
  end
end
```

- [ ] **Step 2: Write the formula README**

Create `packaging/homebrew/README.md`:

```markdown
# Homebrew formula

`cairn.rb` defines `brew install cairn` for the Rust binary. The formula
builds from source via `cargo install`; bottles (pre-built binaries) are
deferred to issue #141.

## Layout

- `head` → `https://github.com/windoliver/cairn.git@main` — installable
  today with `brew install --HEAD cairn`.
- `url` / `version` / `sha256` → tagged release tarball; the sha256 is a
  placeholder until the first `v0.1.0` tag is cut.

## Updating on a release

1. After the release workflow uploads the tarball at
   `https://github.com/windoliver/cairn/archive/refs/tags/vX.Y.Z.tar.gz`,
   compute its sha256:

   ```bash
   curl -sL https://github.com/windoliver/cairn/archive/refs/tags/vX.Y.Z.tar.gz \
     | shasum -a 256
   ```

2. Edit `cairn.rb`:
   - `url` → the tagged URL above.
   - `version` → `X.Y.Z`.
   - `sha256` → the computed digest.

3. Validate locally before tapping:

   ```bash
   brew audit --strict --formula packaging/homebrew/cairn.rb
   brew install --build-from-source --HEAD packaging/homebrew/cairn.rb
   brew test cairn
   ```

The `install-smoke` job in `release-dry-run.yml` runs `brew audit --strict`
on every workflow_dispatch and tag push.
```

- [ ] **Step 3: Audit the formula locally if `brew` is on PATH**

Run:
```bash
if command -v brew >/dev/null 2>&1; then
  brew audit --strict --formula packaging/homebrew/cairn.rb || \
    echo "audit findings noted; recheck under CI"
else
  echo "brew not on PATH; CI macOS runner will audit"
fi
```

Expected: either an audit pass, or a list of findings to address before committing. Common findings to fix inline:
- `Formula in wrong location`: ignore — we are not yet in a tap.
- `Stable url and HEAD url should not be the same`: ignore.
- `Audit problems`: any unrelated to placeholder sha256 → fix.

- [ ] **Step 4: Commit**

```bash
git add packaging/homebrew/
git commit -m "feat(packaging): Homebrew formula for cairn binary (#100)"
```

---

### Task 5: Document install + model-fetch behavior

**Files:**
- Create: `docs/site/src/usage/installation.md`
- Modify: `docs/site/src/SUMMARY.md`

The brief mandates "Model cache behavior is explicit and does not require API keys." (acceptance #2). The code already implements this in `crates/cairn-cli/src/main.rs:778-840` — the doc names the behaviour for users.

- [ ] **Step 1: Write the installation page**

Create `docs/site/src/usage/installation.md`:

```markdown
# Installation

Cairn v0.1 is a single Rust binary. Two supported install paths.

## `cargo install` (any platform)

```bash
cargo install cairn
```

Requires Rust 1.95+. Pulls `cairn-cli` from crates.io and builds the `cairn`
binary into `~/.cargo/bin/cairn`. No other runtime dependencies.

## `brew install` (macOS / Linux)

```bash
brew tap windoliver/cairn https://github.com/windoliver/cairn
brew install cairn
```

Builds from source via the bundled `cargo install` step. Bottles (pre-built
binaries) are tracked under issue #141 and not yet available.

To track `main` instead of the latest tagged release:

```bash
brew install --HEAD cairn
```

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

It bootstraps a temp vault, exercises every P0 verb (`status`, `ingest`,
`search`, `retrieve`, `lint`, `forget`), and exits 0 on full pass.

## Offline after the first run

Once a vault is bootstrapped and the embedding model is cached, Cairn
runs fully offline: no network calls are made by any of the eight verbs
during a normal session. Sensors, workflows, and MCP serving all stay
local (brief §19).
```

- [ ] **Step 2: Link the page from `SUMMARY.md`**

Read `docs/site/src/SUMMARY.md`, then add a line under the "Usage" section pointing to `usage/installation.md` (place it before `usage/cli.md` so install is the first usage doc readers see).

The exact edit is:

```markdown
- [Installation](usage/installation.md)
```

inserted as the first child under the existing Usage section header.

- [ ] **Step 3: Build the docs to verify the link resolves**

Run: `mdbook build docs/site`
Expected: exit 0; `docs/site/book/usage/installation.html` exists.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/usage/installation.md docs/site/src/SUMMARY.md
git commit -m "docs: installation page + model-fetch behaviour (#100)"
```

---

### Task 6: Wire `install-smoke` into release-dry-run.yml

**Files:**
- Modify: `.github/workflows/release-dry-run.yml`

Add a new job that runs `cargo install --path crates/cairn-cli` into a temp prefix, then runs `scripts/install-smoke.sh` against the installed binary. On macOS, also run `brew audit --strict` against the formula. The job runs on every workflow_dispatch and tag push, alongside the existing `publish-dry-run` and `binary` jobs.

- [ ] **Step 1: Read the current bottom of `release-dry-run.yml` to find the right insertion point**

Run: `wc -l .github/workflows/release-dry-run.yml` to know the line count, then read the last `jobs:` entry. The new job goes after the existing `binary` job.

- [ ] **Step 2: Append the new job**

Edit `.github/workflows/release-dry-run.yml`. After the `binary:` job's last step (the `Upload binary` step), add:

```yaml

  install-smoke:
    name: install-smoke / ${{ matrix.os }}
    # Proves `cargo install cairn` + the eight-verb smoke pass on a clean
    # prefix. On macOS, additionally audits the Homebrew formula. Closes the
    # last v0.1 packaging gap called out in docs/ci.md (issue #100).
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - name: Checkout
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
      - name: Install toolchain (rust-toolchain.toml)
        run: rustup show active-toolchain || rustup toolchain install
      - name: Cache cargo build
        uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4 # v2.9.1
        with:
          shared-key: install-smoke-${{ matrix.os }}
          cache-bin: "false"
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - name: cargo install into a clean prefix
        shell: bash
        run: |
          set -euo pipefail
          prefix="$(mktemp -d -t cairn-install-XXXXXX)"
          echo "CAIRN_PREFIX=$prefix" >>"$GITHUB_ENV"
          cargo install --path crates/cairn-cli --locked --no-track \
            --root "$prefix"
          ls "$prefix/bin"
      - name: Run install smoke
        shell: bash
        run: |
          set -euo pipefail
          CAIRN_BIN="$CAIRN_PREFIX/bin/cairn" ./scripts/install-smoke.sh
      - name: Audit Homebrew formula
        if: matrix.os == 'macos-latest'
        shell: bash
        run: |
          set -euo pipefail
          # `brew audit --strict` validates formula structure offline; we do
          # not pass `--online` because the v0.1.0 tarball does not yet exist
          # (sha256 is a placeholder until release #141 fills it in).
          brew audit --strict --formula packaging/homebrew/cairn.rb || {
            echo "::warning::brew audit found issues; see log above"
            exit 1
          }
```

- [ ] **Step 3: Validate the workflow YAML parses**

Run:
```bash
python3 -c 'import yaml,sys; yaml.safe_load(open(".github/workflows/release-dry-run.yml"))'
```
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release-dry-run.yml
git commit -m "ci(release-dry-run): install-smoke job + brew audit (#100)"
```

---

### Task 7: Update the CI doc inventory

**Files:**
- Modify: `docs/ci.md:201-202`

The current copy says the smoke is deferred. After Task 6 it is no longer deferred.

- [ ] **Step 1: Read the deferred-gates section**

Read `docs/ci.md` from line 197 onwards, find the bullet that names issue #100.

- [ ] **Step 2: Delete the deferred line, add an inventory row**

The deferred bullet at lines 201–202 reads:

```markdown
- **`cargo install` and Homebrew formula smoke** — issue #100. Hook into
  `release-dry-run.yml` once the formula exists.
```

Remove it. Then locate the table in `docs/ci.md` that lists `release-dry-run.yml` jobs (search for `release-dry-run` in the file). Add a row for the new `install-smoke` job describing what it asserts and whether it is required.

If no such table exists, append a paragraph under the existing description of `release-dry-run.yml`:

```markdown
The `install-smoke` job (issue #100) runs on the Linux and macOS matrix:
`cargo install --path crates/cairn-cli` into a temp prefix, then
`scripts/install-smoke.sh` against the installed binary. The macOS leg
additionally runs `brew audit --strict` against `packaging/homebrew/cairn.rb`.
Advisory until release #141 wires real publishes; promotes to required when
bottles ship.
```

- [ ] **Step 3: Commit**

```bash
git add docs/ci.md
git commit -m "docs(ci): record install-smoke job, retire #100 deferred note (#100)"
```

---

### Task 8: End-to-end verification

Run the same gates CI will run, in the same order:

- [ ] **Step 1: Lints + format**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```
Expected: both exit 0.

- [ ] **Step 2: Workspace tests (includes the new install-smoke test)**

```bash
cargo nextest run --workspace --locked --no-fail-fast
```
Expected: exit 0; `install_smoke_script_passes_against_built_binary` passes.

- [ ] **Step 3: Doctests**

```bash
cargo test --doc --workspace --locked
```
Expected: exit 0.

- [ ] **Step 4: Core dep-freeness + codegen check**

```bash
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```
Expected: both exit 0.

- [ ] **Step 5: Docs build**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
```
Expected: both exit 0.

- [ ] **Step 6: Cargo package**

```bash
cargo package --workspace --no-verify --locked --allow-dirty
```
Expected: exit 0.

- [ ] **Step 7: Smoke against a real `cargo install`**

```bash
prefix="$(mktemp -d -t cairn-install-XXXXXX)"
cargo install --path crates/cairn-cli --locked --no-track --root "$prefix"
CAIRN_BIN="$prefix/bin/cairn" ./scripts/install-smoke.sh
```
Expected: exit 0; full `ok: <verb>` list and `smoke: all verbs passed`.

- [ ] **Step 8: If `brew` is installed, audit the formula**

```bash
if command -v brew >/dev/null 2>&1; then
  brew audit --strict --formula packaging/homebrew/cairn.rb
fi
```
Expected: exit 0 (or only the expected placeholder-sha256 warnings).

- [ ] **Step 9: Open PR**

```bash
git push -u origin HEAD
gh pr create --title "feat(packaging): cargo install + Homebrew formula + install smoke (closes #100)" \
  --body "$(cat <<'EOF'
## Summary

- Adds `packaging/homebrew/cairn.rb` plus updater README.
- Adds `scripts/install-smoke.sh` exercising the eight P0 verbs against a freshly bootstrapped vault, wired into `cargo nextest` via `crates/cairn-cli/tests/install_smoke.rs`.
- Adds an `install-smoke` job to `release-dry-run.yml` that `cargo install`s into a temp prefix on Linux + macOS and runs the smoke, plus `brew audit --strict` on macOS.
- Ships `docs/site/src/usage/installation.md` explaining `cargo install` / `brew install`, the one-time model fetch (no API keys), and offline-after-cache behaviour.
- Tightens `cairn-cli` crates.io metadata (description, keywords, categories).
- Retires the issue #100 deferred note in `docs/ci.md`.

Brief sections: §16 Distribution and Packaging, §19 v0.1 install artifact.
Invariants touched: none of the load-bearing seven (additive packaging only).

## Test plan

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo nextest run --workspace --locked` — including the new install smoke test
- [ ] `cargo test --doc --workspace --locked`
- [ ] `./scripts/check-core-boundary.sh`
- [ ] `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
- [ ] `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
- [ ] `mdbook build docs/site`
- [ ] `cargo package --workspace --no-verify --locked --allow-dirty`
- [ ] Local end-to-end: `cargo install --path crates/cairn-cli --root <tmp>` + `CAIRN_BIN=<tmp>/bin/cairn ./scripts/install-smoke.sh`
- [ ] `release-dry-run.yml` green on workflow_dispatch (both matrix legs)

Closes #100.
EOF
)"
```

---

## Self-review

**Spec coverage** (mapped from issue #100):
- "Prepare `cargo install cairn` packaging metadata" → Task 1 (description, keywords, categories) + Task 8 step 6 (`cargo package`) + Task 6 (real `cargo install` in CI).
- "Homebrew formula path" → Task 4.
- "Model fetch/cache behavior + offline-after-cache" → Task 5 docs (code already implements it).
- "Install smoke tests that bootstrap a vault and run status/search/retrieve on a fixture" → Tasks 2 + 3 + 6. Acceptance also lists `ingest` / `lint` / `forget`; the script covers all six.
- "Clean machine can install via cargo or brew" → covered by CI matrix in Task 6 once 0.1 is published; the formula and metadata are ready today.
- "Run cargo package/build checks" → Task 8 steps 1, 2, 6.
- "Run Homebrew formula audit" → Task 6 macOS leg + Task 8 step 8.
- "Run install smoke script in a clean temp prefix" → Task 8 step 7 + Task 6.

**Out of scope confirmation:** desktop packages and release channels (#139, #141) are not touched. Linux/Windows package managers (winget, scoop, deb) are not in the issue and not in v0.1 scope (brief §16 lists them as future).

**Placeholder scan:** all code blocks contain real content. The Homebrew sha256 is a placeholder by design (filled at first release) and is called out in the formula comment, the README, and Task 4 step 2.

**Type consistency:** `CAIRN_BIN` is the script's input contract everywhere (script, Rust test, CI job). The verb argument shapes (`--kind reference --body ...`, `--mode keyword --query ...`, `--record-id`) match `crates/cairn-cli/tests/cli.rs:32–47` and `crates/cairn-cli/tests/cli.rs:1113`.

**Risk note:** Task 2 step 1 contains a verification step — if `local_embeddings` is configurable via a YAML key (verified in the source) but not via env, the script falls back to writing the config file. The plan handles both branches.
