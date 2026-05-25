# CI

## Docs CI (`docs.yml`)

Docs CI has four lanes:

- `docs / cargo doc`: rustdoc for Rust API references.
- `docs / generated reference`: `cairn-docgen --check` for committed generated
  Markdown drift.
- `docs / mdbook build`: structural docs-site build.
- `docs / markdown links (lychee)`: external link rot checker (advisory — does
  not block PRs because external hosts can rate-limit or block bot user agents).

GitHub Pages deployment is separate from PR checks. It builds and deploys the
mdBook site only on pushes to `main` or manual workflow dispatch.

## Code CI (`ci.yml`)

The main CI workflow runs on every PR and push to `main`. Key jobs:

- `format`: `cargo fmt --all --check`.
- `lint`: `cargo clippy --workspace --all-targets -- -D warnings`.
- `test`: `cargo nextest run --workspace` on Linux and macOS.
- `core-boundary`: `scripts/check-core-boundary.sh` — enforces zero upstream
  deps on `cairn-core`.
- `gates`: latency + memory + privacy + SRE smoke gates (`cairn-bench all`).
- `coherence-gate`: scores extended replay cassettes against the threshold
  manifest (`cairn-bench coherence run --gate beta` on PRs / main;
  `--gate rc` on `release/*` branches). Fails closed on any metric regression
  or floor breach. See `crates/cairn-bench/manifests/coherence.toml`.
- `codegen-drift`: IDL codegen drift gate only — runs `cairn-codegen --check`
  to verify committed generated types match the IDL source.
- `contract-drift` (a.k.a. **v1-freeze gate**, release-blocking on v1.0+):
  wire-compat and capability-matrix gate — runs `cairn-codegen --check`,
  `cairn-docgen --check`, the wire-compat fixtures
  (`crates/cairn-idl/tests/wire_compat_v1.rs`), the capability-matrix
  advertise tests (`crates/cairn-core/tests/capability_matrix_v1.rs`),
  per-surface status snapshots (`crates/cairn-cli/tests/status_snapshot_insta.rs`),
  CLI↔SDK parity (`crates/cairn-cli/tests/sdk_cli_parity.rs`),
  SDK transport filter (`crates/cairn-sdk/tests/surface.rs`),
  and MCP initialize↔status parity (`crates/cairn-mcp/tests/init_status_parity.rs`).
  Fails if any generated output or contract surface drifts from committed
  state. The MCP envelope conformance suite
  (`crates/cairn-mcp/tests/mcp_conformance.rs`) ships as part of the
  regular `test` jobs — separate gate, also branch-protection required.
  See [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
  and [MCP Semver Policy](mcp-semver-policy.md) for the freeze rules
  this gate enforces.
- `bench-full`: manual-trigger BrainBench scorecard run (`bench / world-v1`).

## Supply-chain CI (`supply-chain.yml`)

Runs `cargo deny check`, `cargo audit --deny warnings`, and `cargo machete` on
every PR. Failures block merge.

See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.
