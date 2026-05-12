# Cairn docs site and generated usage reference — design

| Field | Value |
|---|---|
| Date | 2026-04-28 |
| Scope | User-facing documentation site, generated CLI/API/package reference, CI drift gates |
| Brief sections | §8.0 four surfaces, §8.0.a preludes, §13.3 commands, §13.5 single-IDL claim, §18.d Cairn skill |
| Existing gates extended | `codegen / no drift`, `docs / cargo doc`, `docs / markdown links (lychee)` |

## 1. Problem

Cairn already has strong generated contract surfaces: `cairn-codegen --check`
re-emits Rust SDK types, CLI clap builders, MCP schemas/tool declarations, and
the `skills/cairn` bundle from the IDL. What is missing is the public docs site
and the same no-drift discipline for usage documentation.

Today the root README still describes earlier scaffold status, detailed docs
live mostly under design/dev folders, and no GitHub Pages/static site structure
exists. The implemented user-facing surfaces are split across packages:

- `cairn-cli`: the `cairn` binary; implemented commands include `status`,
  `handshake`, `bootstrap`, `plugins list`, and `plugins verify`; the eight
  core verbs exist as P0 stub commands.
- `cairn-core`: generated SDK/wire types, config schema types, contract traits,
  plugin registry/conformance APIs.
- `cairn-mcp`: MCP stdio adapter surface and generated MCP tool declarations.
- `cairn-store-sqlite`, `cairn-sensors-local`, `cairn-workflows`: bundled
  plugin crates visible through `cairn plugins list/verify`.
- `cairn-idl`: maintainer-facing IDL and `cairn-codegen`.
- `cairn-test-fixtures`: dev-only test helper crate, not public user docs.

The docs system must make future user-facing changes hard to forget. A new CLI
command, CLI flag, generated verb/schema, plugin capability, config field, or
workspace package should either update generated docs automatically or fail CI
until the docs classification/artefact is updated.

## 2. External research summary

Tools checked:

- **mdBook**. Rust-native Markdown book generator. Its docs describe the core
  layout (`book.toml`, `src/SUMMARY.md`, Markdown chapter files) and CI usage.
  The Rust project itself uses mdBook for major docs. It supports search and
  straightforward static output for GitHub Pages.
- **Docusaurus / VitePress / Starlight**. Modern JS-based documentation shells.
  They are strong for product sites, MDX/components, blogs, custom themes, and
  versioned docs, but add Node/package-manager maintenance that Cairn does not
  otherwise need.
- **MkDocs Material**. Polished Markdown docs with strong navigation/search.
  It is a good Python ecosystem choice, but adds Python tooling and a non-Rust
  install/update lane.
- **GitHub Pages Actions**. GitHub's current recommended path for non-Jekyll
  build systems is a custom Actions workflow: checkout, build static files,
  upload a Pages artifact, then deploy on default-branch pushes.
- **clap / clap-markdown**. `clap::Command` exposes `render_long_help()`,
  `render_usage()`, and command traversal APIs; `clap-markdown` can format a
  `clap::Command` as Markdown. Because Cairn already uses clap and generates
  command builders from IDL, the docs reference should be generated from the
  actual runtime command tree instead of shelling out to a hand-written copy.
- **mdbook-linkcheck**. Optional mdBook backend for validating links inside the
  book. Cairn already has lychee for Markdown links; mdBook build should be the
  required structural gate, while linkcheck can be added later if lychee leaves
  gaps.

Sources:

- mdBook introduction and CI/deployment docs:
  <https://rust-lang.github.io/mdBook/>,
  <https://rust-lang.github.io/mdBook/continuous-integration.html>,
  <https://rust-lang.github.io/mdBook/guide/creating.html>
- GitHub Pages custom workflow docs:
  <https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site>
- clap command help rendering:
  <https://docs.rs/clap/latest/clap/builder/struct.Command.html>
- clap-markdown:
  <https://docs.rs/clap-markdown/latest/clap_markdown/>
- mdbook-linkcheck:
  <https://docs.rs/mdbook-linkcheck/latest/mdbook_linkcheck/>
- Docusaurus, VitePress, Starlight, MkDocs Material:
  <https://docusaurus.io/docs/next/docs-introduction>,
  <https://vitepress.dev/guide/what-is-vitepress>,
  <https://starlight.astro.build/>,
  <https://github.com/squidfunk/mkdocs-material>

## 3. Decision

Use **mdBook for the site shell** and a new **`cairn-docgen` maintainer-time
binary** for generated Markdown reference files.

Rationale:

1. **Rust-native operations.** mdBook installs through Rust tooling and fits
   the existing pinned Rust CI model. No Node/Python docs stack is needed.
2. **Markdown remains source of truth.** Human-authored docs stay simple and
   usable in GitHub, editors, `rg`, and future agent context.
3. **Generated reference is checked like codegen.** The docs that mirror
   command flags, package metadata, plugin capabilities, config defaults, and
   IDL schema shape are generated and committed. CI runs `--check`.
4. **Actual runtime CLI is authoritative.** CLI docs come from the same
   `clap::Command` tree used by `main.rs`, including hand-added commands like
   `bootstrap` and `plugins`, not only IDL-generated verbs.
5. **Plain static output.** GitHub Pages can deploy the `mdbook build` output
   via the standard Pages artifact flow.

## 4. Alternatives considered

### Option A: mdBook + generated reference (chosen)

Best fit for a Rust project whose core docs are conceptual/user guides plus
contract reference. Lowest dependency footprint, easiest CI path, and strong
alignment with the current codegen discipline.

Trade-off: mdBook is less visually/product-marketing oriented than Docusaurus
or Starlight. That is acceptable because Cairn's immediate need is precise
developer usage and contract docs, not an interactive landing site.

### Option B: Docusaurus / VitePress / Starlight

Stronger for branded docs, custom components, MDX, version selectors, and richer
product navigation. These would be reasonable once Cairn has a public v1 docs
program with multiple release trains.

Trade-off: adds Node, lockfiles, frontend config, and another supply-chain lane.
It does not improve the most important requirement: keeping CLI/schema docs in
sync with code.

### Option C: MkDocs Material

Polished Markdown documentation with strong navigation and search. Good for
Python-heavy projects or teams already using MkDocs.

Trade-off: adds Python install/configuration. Cairn is currently Rust-only and
already has Rust CI/cache machinery.

## 5. Site structure

Create the mdBook source under `docs/site/`:

```text
docs/site/
|-- book.toml
`-- src/
    |-- SUMMARY.md
    |-- index.md
    |-- quickstart.md
    |-- concepts/
    |   |-- architecture.md
    |   |-- vault-layout.md
    |   `-- capability-model.md
    |-- usage/
    |   |-- cli.md
    |   |-- plugins.md
    |   |-- config.md
    |   |-- mcp.md
    |   `-- skill.md
    |-- reference/
    |   |-- generated/
    |   |   |-- cli.md
    |   |   |-- commands/
    |   |   |   |-- bootstrap.md
    |   |   |   |-- forget.md
    |   |   |   |-- handshake.md
    |   |   |   |-- ingest.md
    |   |   |   |-- plugins-list.md
    |   |   |   |-- plugins-verify.md
    |   |   |   `-- ...
    |   |   |-- config-defaults.md
    |   |   |-- contract-verbs.md
    |   |   |-- mcp-tools.md
    |   |   |-- packages.md
    |   |   `-- plugins.md
    |   |-- idl.md
    |   `-- rust-api.md
    |-- maintainers/
    |   |-- codegen.md
    |   |-- docs.md
    |   `-- ci.md
    `-- status.md
```

`docs/site/src/reference/generated/**` is owned by `cairn-docgen` and carries
an HTML comment header:

```markdown
<!-- @generated by cairn-docgen — DO NOT EDIT. -->
```

The generated source is committed. The mdBook output directory is not.

## 6. Per-package documentation plan

Every workspace package gets an entry in `docs/site/src/reference/generated/packages.md`.
Packages that expose a user or maintainer surface also get a stable human page.

| Package | Audience | Docs responsibility |
|---|---|---|
| `cairn-cli` | Users/operators | CLI quickstart, command reference, JSON/human output behavior, exit codes, config bootstrap. |
| `cairn-core` | SDK/plugin authors | Contract traits, generated wire types, config model, plugin registry and conformance expectations. |
| `cairn-mcp` | MCP host/integration authors | MCP transport status, generated tool list, stdio caveats, current P0 stub behavior. |
| `cairn-store-sqlite` | Operators/plugin authors | Bundled `MemoryStore` plugin page; clearly mark current capabilities as P0 stub false until storage lands. |
| `cairn-sensors-local` | Operators/plugin authors | Bundled `SensorIngress` plugin page; current P0 stub false capabilities. |
| `cairn-workflows` | Operators/plugin authors | Bundled `WorkflowOrchestrator` plugin page; current P0 stub false capabilities. |
| `cairn-idl` | Maintainers/integration authors | IDL authoring rules, codegen command, schema layout, generated surfaces. |
| `cairn-test-fixtures` | Contributors only | Mention in maintainer package index as dev-only; no public user page. |

Add a docs coverage manifest:

```toml
# docs/site/docs-coverage.toml
[packages.cairn-cli]
audience = "user"
page = "usage/cli.md"
generated_reference = "reference/generated/cli.md"

[packages.cairn-core]
audience = "sdk-author"
page = "reference/rust-api.md"

[packages.cairn-test-fixtures]
audience = "internal-test"
page = ""
```

`cairn-docgen --check` reads `cargo metadata --no-deps` and fails if any
workspace package is absent from this manifest. This gives future packages an
explicit docs classification gate.

## 7. Generated reference sources

`cairn-docgen` lives as a second binary in `crates/cairn-cli`:

```text
crates/cairn-cli/src/
|-- command.rs              # shared `build_command()`
|-- main.rs                 # parses and dispatches through command.rs
`-- bin/cairn-docgen.rs     # writes/checks generated docs
```

`build_command()` must move from `main.rs` into `cairn_cli::command` so the
runtime and doc generator use exactly the same command tree. This is the key
drift-prevention boundary.

Generated inputs:

1. **CLI command tree.** `cairn_cli::command::build_command()` traversed as
   `clap::Command`.
   - Generate a full `reference/generated/cli.md`.
   - Generate one page per command/subcommand under
     `reference/generated/commands/`.
   - Include usage, flags, positional args, help text, and subcommands.
   - Use `clap`'s own help/usage rendering or `clap-markdown`; the first
     implementation should prefer a small Cairn-owned renderer that traverses
     `clap::Command` instead of introducing `clap-markdown` immediately. If
     that grows awkward, switch to `clap-markdown`.
2. **Config defaults.** Serialize `CairnConfig::default()` to YAML into
   `reference/generated/config-defaults.md`. Future config fields change the
   generated sample and trip `--check`.
3. **Plugin registry.** Run `plugins::host::register_all()`, reuse the
   existing list/verify renderers, and generate `reference/generated/plugins.md`
   with plugin names, contracts, version ranges, capabilities, and current
   conformance summary.
4. **IDL / contract.** Read `crates/cairn-idl/schema/index.json` and verb
   schema files to generate `reference/generated/contract-verbs.md`, including
   verb id, auth model, capability, CLI source metadata, skill triggers, and
   `$defs.Args` / `$defs.Data` schema links.
5. **MCP tool declarations.** Read `cairn_mcp::generated::TOOLS` to generate
   `reference/generated/mcp-tools.md`, including tool name, description,
   root auth/capability, auth overrides, capability overrides, and schema path.
6. **Package metadata.** Use `cargo metadata --format-version 1 --no-deps` to
   generate `reference/generated/packages.md` and validate
   `docs-coverage.toml`.

Generated docs are intentionally reference-style. Human-authored pages under
`usage/` and `concepts/` explain workflows and link to generated reference.

## 8. Future-change enforcement

`cairn-docgen --check` must fail on these drift classes:

| Change | How it is caught |
|---|---|
| New CLI command/subcommand | Runtime `build_command()` renders different generated CLI docs. |
| New CLI flag/positional/help text | Generated command page diff. |
| New config field/default | Generated default YAML diff. |
| New bundled plugin | Generated plugin/package docs diff; coverage manifest must classify package. |
| Plugin capability change | Generated plugin docs diff. |
| New user-facing package | `docs-coverage.toml` missing package entry. |
| New IDL verb/prelude/schema metadata | `cairn-codegen --check` catches generated code drift; `cairn-docgen --check` catches generated contract docs drift. |
| New MCP tool metadata | Generated MCP tool docs diff. |
| User-facing Rust public API docs | `cargo doc -D warnings` remains the Rust API gate; package coverage manifest points users to rustdoc/API pages. |

This does not try to infer whether every public Rust symbol deserves a hand
guide. That is not robust. Instead:

- rustdoc stays the authoritative API reference for public Rust symbols;
- generated site pages summarize package-level and contract-level surfaces;
- new packages require classification;
- new CLI/config/plugin/IDL/MCP surfaces update generated docs automatically.

## 9. CI changes

Extend `.github/workflows/docs.yml`:

1. **`docs-generated` job** (required):

```yaml
name: docs / generated reference
run: cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

2. **`mdbook` job** (required):

```yaml
name: docs / mdbook build
run: mdbook build docs/site
```

Install mdBook using the official docs' Rust path:

```bash
cargo install mdbook --version 0.5.2 --no-default-features --features search --locked
```

Use the same Rust cache policy as existing docs jobs. Pin action SHAs in the
same style as the current workflows.

3. **Existing `docs / markdown links (lychee)`** remains advisory. It should
include the mdBook source Markdown and the generated Markdown because both are
committed. mdBook build catches missing `SUMMARY.md` pages; lychee catches link
rot when hosts behave.

4. **Optional later gate:** add `mdbook-linkcheck` if lychee does not catch
mdBook-specific cases. Do not block this first docs-site PR on it.

Update `docs/ci.md` and `CLAUDE.md` verification lists:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
```

## 10. GitHub Pages deployment

Add `.github/workflows/pages.yml` after the docs site builds locally and CI
passes. Trigger on pushes to `main` and manual runs only; PRs run build checks
but do not deploy.

Deployment shape follows GitHub's custom Pages workflow:

1. checkout
2. install Rust/mdBook
3. run `cairn-docgen --check`
4. run `mdbook build docs/site --dest-dir ../../target/site`
5. upload `target/site` via `actions/upload-pages-artifact`
6. deploy on `main` via `actions/deploy-pages`

Permissions:

```yaml
permissions:
  contents: read
  pages: write
  id-token: write
```

The repository must set Pages source to GitHub Actions. The workflow should
publish only built static files, never commit generated HTML to a branch.

## 11. Current implementation prerequisite

Before `cairn-docgen` can build inside `cairn-cli`, the workspace compile
blocker in `cairn-mcp` must be fixed:

- `crates/cairn-mcp/Cargo.toml` references code using `rmcp` but does not
  declare an `rmcp` dependency.
- `crates/cairn-mcp/src/lib.rs` re-exports `TransportError` while the type in
  `error.rs` is named `McpTransportError`, and docs still reference the old
  name.
- `serve_stdio()` maps rmcp failures to `TransportError::Service`, but the
  actual enum currently has `Initialize` and `Io` variants only. The fix should
  either add the intended service variant or map to the correct existing
  variant before docs generation relies on this crate compiling.
- Current crate docs mention `cairn mcp`, but the runtime CLI does not expose
  an `mcp` subcommand yet. The first docs pass should describe MCP as the
  `cairn-mcp` crate/plugin surface and generated tool declaration surface, not
  as a runnable CLI command, unless the implementation intentionally adds that
  command and lets `cairn-docgen --check` capture it.

This is not a docs design choice; it is a prerequisite for any `cargo run -p
cairn-cli ...` docs generator.

## 12. Initial content pass

The first implementation should add user-facing pages with current truth:

- `index.md`: concise product intro, current pre-v0.1 status, links to
  quickstart and command reference.
- `quickstart.md`: install/build from source, `cairn status --json`,
  `cairn handshake --json`, `cairn bootstrap`, `cairn plugins list`, and clear
  note that eight core verbs are wired as P0 stubs until store/verb dispatch
  lands.
- `usage/cli.md`: workflow-oriented CLI docs; links to generated command pages.
- `usage/config.md`: bootstrap/default config and environment interpolation.
- `usage/plugins.md`: list/verify behavior, strict mode, exit code 69 for
  strict pending cases.
- `usage/mcp.md`: generated tools are present through the `cairn-mcp`
  crate/plugin surface, transport is P0, dispatch is stubbed until the
  MCP/verb issue lands, and there is no `cairn mcp` CLI subcommand in the
  current runtime command tree.
- `usage/skill.md`: generated Cairn skill bundle, install/use expectations.
- `reference/rust-api.md`: rustdoc link targets and package-level API map.
- `maintainers/docs.md`: how to regenerate docs and fix the CI gate.

Update the root `README.md` to link to the docs site and correct scaffold
status. The README should say that some preludes/management commands are
implemented, while memory-mutating/read verbs still fail closed as P0 stubs.

## 13. Verification commands

Initial PR verification:

```bash
cargo run -p cairn-cli --bin cairn-docgen -- --write
cargo run -p cairn-cli --bin cairn-docgen -- --check
mdbook build docs/site
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Targeted test additions:

- `crates/cairn-cli/tests/docgen.rs`: tempdir write + check mode clean.
- `docgen_detects_cli_flag_drift`: write docs, mutate a generated command doc,
  assert check reports drift.
- `docgen_requires_package_coverage`: temp coverage manifest missing one
  workspace package, assert check fails with the package name.
- `docgen_command_tree_matches_runtime`: generated command list includes all
  current runtime top-level commands (`ingest`, `search`, `retrieve`,
  `summarize`, `assemble_hot`, `capture_trace`, `lint`, `forget`,
  `handshake`, `status`, `plugins`, `bootstrap`).

## 14. Risks

- **Generated docs build depends on `cairn-cli`.** This is intentional because
  the CLI is the ground truth. The current `cairn-mcp` compile issue must be
  fixed first.
- **Generated reference can be noisy.** Keep generated pages under one
  `reference/generated/` tree and keep human guide pages stable.
- **mdBook version drift.** Use semver-pinned install (`^0.4` initially) with
  `--locked` in CI. If mdBook output changes, source Markdown still remains the
  committed artefact; build output is not committed.
- **Manual guide coverage cannot be fully automated.** The gate guarantees
  reference coverage and package classification; review still decides whether a
  new surface needs a guide/tutorial page.

## 15. Invariants touched

- **CLI is ground truth.** Strengthened: CLI usage docs are generated from the
  actual runtime `clap::Command`.
- **Single-IDL claim.** Strengthened: IDL schema changes drive generated
  contract docs in addition to code/MCP/skill outputs.
- **Fail closed on capability.** Strengthened: plugin and MCP capability docs
  derive from the registry/generated tool declarations and drift-check in CI.
- **Harness-agnostic.** Preserved: docs describe CLI/MCP/SDK/skill surfaces
  without coupling Cairn to a specific agent harness.
