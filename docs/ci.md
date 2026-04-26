# CI/CD reference

This document is the contract between contributors, branch protection, and the
GitHub Actions workflows in `.github/workflows/`. If you change a job name,
update this file in the same PR — branch protection references job names
verbatim.

> **Scope.** This is the v0.1 (P0) automation surface only. Specific gates
> (wire compatibility / capability matrix, package smoke tests) are tracked
> under separate issues — see [§ Deferred gates](#deferred-gates) below.
> v1.0 release-channel hardening lives under issue #141.

## Workflows

| File | Purpose | Trigger |
|---|---|---|
| `governance.yml` | Active path freezes (the bespoke reviewed-by gate was removed in ADR 0003 — see `GOVERNANCE.md` §5). | `pull_request_target` against `main`. |
| `ci.yml` | Format, lint, build, test, doctest, core-boundary invariant, IDL codegen drift. | PRs + pushes to `main`, merge queue, manual. |
| `supply-chain.yml` | `cargo-deny` (licenses + bans + sources), `cargo-audit` (RUSTSEC), `cargo-machete` (unused deps). | Manifest/lockfile-touching PRs, pushes to `main`, daily cron, manual. |
| `docs.yml` | `cargo doc` with `-D warnings`; lychee Markdown link check. | PRs + pushes to `main`, weekly cron, manual. |
| `release-dry-run.yml` | tag-version validation; `cargo package --workspace --no-verify` for all crates; full `cargo publish --dry-run` for the two pure-leaf crates; release-mode binary build on Ubuntu + macOS; uploaded artifacts. Downstream-crate publish dry-run is a known v0.1 gap (→ #141). | `v*` tags, manual. |

## Required status checks

Every check listed here must be green for `main` to advance. Branch protection
settings are kept in sync with this list — when a job is renamed, the rule
must be updated in the same PR.

| Job (workflow) | Required? | Notes |
|---|---|---|
| `format / cargo fmt` (`ci.yml`) | ✅ required | rustfmt drift fails fast; takes seconds. |
| `lint / cargo clippy` (`ci.yml`) | ✅ required | `--all-targets -- -D warnings`. |
| `test / ubuntu-latest` (`ci.yml`) | ✅ required | `nextest` + `--doc`. |
| `test / macos-latest` (`ci.yml`) | ✅ required | Same on macOS. |
| `build / ubuntu-latest` (`ci.yml`) | ✅ required | `cargo check --workspace --all-targets`. |
| `build / macos-latest` (`ci.yml`) | ✅ required | Same on macOS. |
| `invariant / cairn-core dep-freeness` (`ci.yml`) | ✅ required | Enforces brief §4 plugin boundary. Adapter or runtime crates depending into core fail closed. |
| `codegen / no drift` (`ci.yml`) | ✅ required | Runs `cargo run -p cairn-idl --bin cairn-codegen -- --check`; fails when generated CLI/MCP/SDK/skill artefacts disagree with the IDL. |
| `plugins / cairn plugins verify` (`ci.yml`) | ✅ required | Runs the bundled-plugin conformance suite (`cairn plugins verify`) and uploads the JSON report as a build artifact. |
| `docs / cargo doc` (`docs.yml`) | ✅ required | Broken intra-doc links fail. |
| `deny / licenses + bans + sources` (`supply-chain.yml`) | ✅ required | Runs on every PR — workflow-level path filtering would leave the required check Pending on non-manifest PRs and either deadlock merges or silently miss manifest-only changes. Cache makes this cheap. |
| `audit / RUSTSEC advisories` (`supply-chain.yml`) | ✅ required | Same reasoning as `deny`. Daily cron catches advisories disclosed after merge. |
| `machete / unused dependencies` (`supply-chain.yml`) | ✅ required | Forward-declared scaffold deps are tracked per-crate via `[package.metadata.cargo-machete] ignored = [...]`; drop entries as their first call site lands. |
| `docs / markdown links (lychee)` (`docs.yml`) | 🟡 advisory | Cron-only by default; flaky external hosts make hard-fail too noisy at v0.1. |
| `freeze / active path freezes` (`governance.yml`) | ✅ required | See `.github/freezes/` and `scripts/check-freeze.sh`. |
| `publish / cargo publish --dry-run` (`release-dry-run.yml`) | ❌ tag-only | Runs on `v*` tags + manual; not part of PR gating. |
| `binary / *` (`release-dry-run.yml`) | ❌ tag-only | Same. |

The names above match the GitHub UI exactly — they are what you paste into
the **Settings → Rules → Branch ruleset** "required status checks" field.

## Local equivalents

Every required CI command has a local equivalent. Reproduce a CI failure
before pushing.

```bash
# format
cargo fmt --all --check

# lint
cargo clippy --workspace --all-targets --locked -- -D warnings

# test (use nextest if installed; falls back to cargo test)
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked

# build (cheap parity with the `build` matrix)
cargo check --workspace --all-targets --locked

# core boundary invariant — fails if cairn-core takes a workspace dep
./scripts/check-core-boundary.sh

# IDL codegen drift — fails if generated CLI/MCP/SDK/skill artefacts disagree
# with the IDL. To regenerate, drop `-- --check` and commit the diff.
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check

# rustdoc — same flags as docs.yml
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked

# supply chain (install once: cargo install cargo-deny cargo-audit cargo-machete)
cargo deny check
cargo audit --deny warnings
cargo machete

# release dry-run (matches release-dry-run.yml)
# 1) Validate every manifest + produce .crate archives. This works on
#    a fresh checkout because --no-verify skips the registry-aware
#    build that downstream crates can't pass until their upstream is
#    on crates.io.
cargo package --workspace --no-verify --locked --allow-dirty
# 2) Full registry-aware dry-run for the two pure-leaf crates.
cargo publish --dry-run --locked --allow-dirty -p cairn-idl
cargo publish --dry-run --locked --allow-dirty -p cairn-core
```

> Tooling install: prefer
> [`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) for the
> `cargo-*` helpers — same artefacts CI uses. CI itself uses
> [`taiki-e/install-action`](https://github.com/taiki-e/install-action),
> which falls back to `cargo binstall` and verifies SHA256 + signatures.

## Caching

All Rust jobs use [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache)
(SHA-pinned). Cache keys include each job's role (`shared-key`) and the runner
OS; only `main` writes the cache (`save-if`). PR runs read but never overwrite,
so a flaky PR can't poison `main`.

`cargo --locked` is used everywhere so an outdated cache cannot mask a
dependency drift — if `Cargo.lock` says one thing and the cache another,
the build fails fast.

## Security posture

| Practice | How it's enforced |
|---|---|
| Least privilege | Every workflow declares `permissions: contents: read` at the workflow level; jobs that need more (none, currently) escalate explicitly. |
| Pinned actions | All third-party `uses:` references are pinned to a full 40-char commit SHA with the human-readable tag in a trailing comment. Dependabot keeps them current (`.github/dependabot.yml`). |
| No PR-controlled secrets | The only workflow that uses `pull_request_target` (which exposes the trusted base context) is `governance.yml`; it never executes PR-contributed code, only files from the base ref. |
| Concurrency cancellation | Each workflow groups by `${{ github.ref }}` and cancels older PR runs to keep CI cost bounded; release dry-runs do **not** cancel — finishing a tag rehearsal matters more than saving minutes. |
| Supply-chain advisories | `cargo audit` runs daily (cron); `cargo deny` covers licenses + bans + duplicate-version warnings on every manifest-touching PR. |
| Yanked dependencies | `deny.toml` denies yanked crates so a yanked release fails the next CI run. |

## Reading a failed run

CI is split so the failed job tells you which kind of bug you have:

| Failed job | Class of failure |
|---|---|
| `format` | rustfmt drift. Run `cargo fmt --all`. |
| `lint` | New clippy warning. Fix the lint or document the `#[allow]` per CLAUDE.md §6.8. |
| `test` (Ubuntu or macOS) | Behaviour regression. Reproduce with `cargo nextest run --workspace`. |
| `build` (Ubuntu or macOS) | Compile failure on a target you didn't test. Build matrix exists exactly for this. |
| `invariant / cairn-core dep-freeness` | Adapter or runtime crate crept into core. Move it to the right crate; see CLAUDE.md §3. |
| `codegen / no drift` | IDL changed without re-running codegen, or a generated file was hand-edited. Run `cargo run -p cairn-idl --bin cairn-codegen` and commit the diff. |
| `plugins / cairn plugins verify` | Bundled-plugin conformance regression. Reproduce with `cargo run -p cairn-cli -- plugins verify --strict`. |
| `docs / cargo doc` | Broken intra-doc link or missing-docs lint. |
| `deny` | License or banned crate. Update the manifest or `deny.toml` (with a justification in the PR). |
| `audit` | RUSTSEC advisory on a transitive dep. Update the lockfile or pin a patched version. |
| `machete` | Unused dep declaration. Drop it or add to machete ignore list with a comment. |
| `links (lychee)` | Dead external URL or broken intra-repo link. |

## Contract invariants

Three gates protect the core/plugin boundary defined in brief §4. They are
documented together so a reviewer can audit them as a set:

1. **IDL codegen drift** — `codegen / no drift` (`ci.yml`). Re-emits the
   generated CLI / MCP / SDK / skill artefacts from the IDL and fails if any
   byte differs. The emitter prunes stale files inside owned roots, so an
   IDL deletion that leaves an orphan generated file also surfaces as drift.
   Tested at unit level by
   `crates/cairn-idl/tests/codegen_check_mode.rs` (clean tree, hand-edit, and
   stale-file scenarios).
2. **Core dependency freeness** — `invariant / cairn-core dep-freeness`
   (`ci.yml`). `scripts/check-core-boundary.sh` reads `cargo metadata` and
   fails when `cairn-core` declares any `cairn-*` workspace dependency
   (normal, build, or dev). Importing a runtime / adapter crate is therefore
   impossible without first changing this manifest, which the script
   catches.
3. **Contract version compatibility** — enforced by the `register_method!`
   macro in `cairn-core::contract::registry`. Plugins declare their
   accepted host range via `supported_contract_versions()`; the registry
   rejects with `PluginError::UnsupportedContractVersion` whenever a
   plugin's range does not include the host's `CONTRACT_VERSION`. Covered
   by `crates/cairn-core/tests/contract_registry.rs::incompatible_plugin_fails_closed`.

All three are exercised by the standard `cargo nextest run --workspace`
plus the boundary script and codegen `--check` invocation listed under
[§ Local equivalents](#local-equivalents).

## Deferred gates

Tracked under their own issues; named here so the gap is explicit:

- **Wire compatibility / schema drift / capability matrix** — issue #98. Add
  snapshot tests for MCP frames + `status` response.
- **`cargo install` and Homebrew formula smoke** — issue #100. Hook into
  `release-dry-run.yml` once the formula exists.
- **MCP/CLI/SDK parity** — depends on codegen #35 + verb implementations.
- **TypeScript build/test** — no `packages/` directory yet; add a `ts.yml`
  (mirroring `ci.yml`) when the first SDK lands.
- **Latency / memory budget / privacy regression gates** — issue #99.

## Branch protection setup

Configure under **Settings → Rules → Rulesets → Branch ruleset → main**:

1. **Restrict deletions** ✅
2. **Block force pushes** ✅
3. **Require a pull request before merging** ✅
   - Required approvals: `1` *once a second maintainer joins* (see
     `CODEOWNERS` and `GOVERNANCE.md` §5; today the repo runs in the
     single-maintainer deviation and self-approval is impossible).
4. **Require status checks to pass** ✅ — for every row above whose
   "Required?" cell is `✅ required`, paste the value of its "Job
   (workflow)" cell into the GitHub UI's required-check field
   verbatim. The current required set is:

   ```
   format / cargo fmt
   lint / cargo clippy
   test / ubuntu-latest
   test / macos-latest
   build / ubuntu-latest
   build / macos-latest
   invariant / cairn-core dep-freeness
   codegen / no drift
   plugins / cairn plugins verify
   docs / cargo doc
   deny / licenses + bans + sources
   audit / RUSTSEC advisories
   machete / unused dependencies
   freeze / active path freezes
   ```
5. **Require branches to be up to date before merging** ✅
6. **Require linear history** ✅
7. **Required workflows: `governance.yml`** (provided by repository ruleset).
8. **Do not allow bypasses** unless explicitly justified in `GOVERNANCE.md`.

Re-confirm after each rename. The list of required checks is the diff that
gates merges — getting it wrong silently lets bad code in.
