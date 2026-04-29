# CLAUDE.md — Agent & Contributor Guide for Cairn

This file orients any coding agent (Claude Code, Codex, Cursor, Gemini, etc.) or
human contributor picking up an issue in this repo. **Read it before touching
code.** When this file and the design brief disagree, the design brief wins —
open a PR updating this file to match.

---

## 1. Source of truth

The canonical design brief lives at:

> **`docs/design/design-brief.md`**

It defines the 8 verbs, 7 contracts, vault layout, pipeline, taxonomy, WAL, hot
memory, sensors, workflows, privacy/consent, evaluation, and tiering. Every
issue implements a slice of that brief.

Shorter companion docs:

- `docs/design/architecture.md` — crate topology and plugin boundary
- `docs/design/2026-04-23-rust-workspace-scaffold-design.md` — workspace scaffold rationale
- `docs/design/traceability.md` — design-section to issue map; update in any PR that materially changes the brief
- `README.md` — public-facing intro + P0 scope

**Rule of thumb for agents:** before proposing a data model, API shape, or
workflow, search the design brief for the relevant section and quote the
section number in your plan. Do not re-invent what the brief already pins down.

---

## 2. What Cairn is (one paragraph)

Cairn is a standalone, harness-agnostic agent-memory framework: a single Rust
binary + a single SQLite file + a markdown vault. It exposes **eight verbs**
(`ingest`, `search`, `retrieve`, `summarize`, `assemble_hot`, `capture_trace`,
`lint`, `forget`) through **four isomorphic surfaces** (CLI, MCP, SDK, skill).
**The CLI is ground truth**; every other surface is a thin wrapper. P0 is pure
Rust + SQLite + markdown + optional local LLM — no Python, no network, no cloud
credentials required.

---

## 3. Workspace topology

Rust workspace, edition 2024, resolver 3, toolchain pinned to `1.95.0`
(`rust-toolchain.toml`). Crates under `crates/`:

| Crate | Role |
|---|---|
| `cairn-core` | Traits, generated types, pure pipeline functions, error enums. **No I/O. No adapter deps.** |
| `cairn-cli` | Terminal entry point (`cairn` binary). Wires adapters into the verb layer. |
| `cairn-sdk` | Typed in-process SDK surface over the eight verbs + `status`/`handshake`. Depends on `cairn-core` only. |
| `cairn-mcp` | MCP adapter (stdio/HTTP). ~300 LOC wrapper over verbs. |
| `cairn-store-sqlite` | SQLite + FTS5 + sqlite-vec record store. |
| `cairn-sensors-local` | Local sensors (hooks, IDE, terminal, clipboard, voice, screen). |
| `cairn-workflows` | Background workflow host (consolidate, promote, expire, evaluate). |
| `cairn-idl` | Canonical IDL + codegen driver (`cairn-codegen` bin). |
| `cairn-test-fixtures` | Dev-only test helpers. **Never a non-dev dep.** |

Dependency rule: **`cairn-core` has zero dependencies on any other workspace
crate, including dev-deps.** Enforced by `scripts/check-core-boundary.sh` —
run it before every PR.

---

## 4. Load-bearing invariants

These come from the design brief §2 and §4. Breaking any of them is a red flag
in review — call it out explicitly if an issue requires it.

1. **Harness-agnostic.** No code path may assume Claude Code, Codex, or any
   specific harness. If you find harness-specific logic, it belongs in a
   sensor or adapter, not in core.
2. **Stand-alone P0.** Fresh laptop, offline, zero cloud credentials → every
   P0 path must work. Network calls are P1+ and gated by config.
3. **CLI is ground truth.** One verb function in `cairn-core`, invoked by CLI,
   MCP, SDK, and skill. Never a parallel implementation.
4. **Seven contracts, pure functions otherwise.** New capability? Either
   implement an existing contract (`MemoryStore`, `LLMProvider`,
   `WorkflowOrchestrator`, `SensorIngress`, `MCPServer`, P1 `FrontendAdapter`,
   P2 `AgentProvider`) or add a pure function. No hidden global state.
5. **WAL + two-phase apply for every mutation.** Every write goes through the
   WAL state machine (§5.6 of the brief). No direct DB mutations.
6. **Fail closed on capability.** If a mode isn't advertised in `status`, the
   verb rejects with `CapabilityUnavailable`. Never silently downgrade.
7. **`#![forbid(unsafe_code)]`** is workspace-level. Do not add `unsafe`.
8. **No `unwrap()` / `expect()` in `cairn-core`** (deny-linted). Return
   typed errors. `expect("reason")` is tolerated in bins/tests only, and the
   reason must describe the invariant.
9. **Privacy by construction.** Presidio pre-persist + consent journal +
   per-user salt. Never log raw record bodies at `info` or above.
10. **Sources immutable; records LLM-owned; schema co-evolved.** The three
    vault layers (`sources/`, `raw/`+`wiki/`+`skills/`, `purpose.md` +
    `.cairn/config.yaml`) have strict write roles — do not blur them.

---

## 5. Working on an issue

1. **Read the linked design section.** Every issue cites a brief section
   (e.g., "§5.6 WAL"). Open `docs/design/design-brief.md`, read that section
   and its neighbours, then come back.
2. **State a plan in the issue or PR description** before writing code. Cite
   the section numbers you're implementing and the contract(s) you're
   touching.
3. **Keep the diff scoped.** No drive-by refactors. If you spot adjacent
   issues, file them; don't fix them in the same PR.
4. **Implement behind an existing trait.** If no trait fits, escalate — adding
   a contract is a brief-level change, not a PR-level one.
5. **Write tests first.** See §7.
6. **Run the verification checklist.** See §8.
7. **Open a PR** with the brief section numbers, the invariants you touched,
   and the verification output.

---

## 6. Rust conventions

Baseline is already pinned in `Cargo.toml` workspace lints — `pedantic` clippy,
`forbid(unsafe_code)`, `deny(rust_2024_compatibility)`, `warn(missing_docs)`.
Each member crate opts in with `[lints] workspace = true`. Do not locally
disable workspace lints without a code comment explaining why.

### 6.1 Project layout
- **Binaries stay thin.** `cairn-cli` parses args, loads config, dispatches to
  `cairn-core::verbs::*`. No domain logic in `main.rs` or `clap` handlers.
- **Adapters implement one trait.** Keep adapter crates free of cross-adapter
  imports.
- **Feature flags only where needed** (e.g., optional sensor bundles). Default
  features should produce the P0 binary. Avoid per-function `cfg(feature)` —
  gate at the module boundary.

### 6.2 Error handling
- **Libraries (`cairn-core`, adapters, `cairn-workflows`, `cairn-idl`):
  `thiserror`**, one error enum per module boundary. No `anyhow` in libs.
  Preserve context with `#[source]`, not stringification.
- **Binaries (`cairn-cli`, `cairn-codegen`): `anyhow`** in `main` only.
  Return `anyhow::Result<()>` from `main`, `.context("verb: ingest")` at
  call sites, map to shell exit codes at the outer boundary.
- **`?` everywhere.** No `match` on errors purely to re-wrap.
- **Panics are bugs.** Invariants that must hold at runtime: `debug_assert!`
  or `expect("invariant: <what>")`, never a bare `unwrap`.

### 6.3 Async
- **`tokio` is the default runtime** (already in workspace deps). Use
  `#[tokio::main(flavor = "multi_thread")]` for long-lived bins (MCP server,
  workflow host); `flavor = "current_thread"` for short-lived CLI verbs to
  avoid spinning up a threadpool. `#[tokio::test]` in tests.
- **Edition 2024: prefer native `async fn` in traits + RPITIT** over
  `async_trait` for internal traits (stable since 1.75, mature in 1.95).
  Keep `async_trait` only when trait objects (`dyn Trait`) are required.
- **Structured concurrency:** `tokio::task::JoinSet` for fan-out,
  `tokio_util::task::TaskTracker` for graceful shutdown. Every spawned task
  has an owner that awaits or cancels it — no orphan `tokio::spawn`.
- **Cancellation:** propagate `tokio_util::sync::CancellationToken` from the
  orchestrator. Sync or CPU-bound work → `tokio::task::spawn_blocking`.
- **Never `.await` while holding a `std::sync::Mutex`.** Use
  `tokio::sync::Mutex` if the lock must span an await, or
  `parking_lot::Mutex` strictly in non-await regions.
- **No `block_on` inside async.** No mixing `futures::executor` with tokio.

### 6.4 Testing
- **Unit tests** in `#[cfg(test)] mod tests { ... }` next to the code.
- **Integration tests** in `crates/<crate>/tests/` — use `cairn-test-fixtures`
  (dev-dep only) for shared setup.
- **Parameterized cases:** `rstest` fixtures for table-driven tests.
- **Property tests (`proptest`)** for IDL round-trips, WAL state-machine
  idempotency, taxonomy parsers, and serialization invariants.
- **Snapshot tests (`insta`)** for CLI output, MCP frames, generated code.
  Review with `cargo insta review`; commit `.snap` files.
- **Runner: `cargo nextest run --workspace`** (faster, better isolation than
  `cargo test`). Doctests separately: `cargo test --doc --workspace`.
- **No mocking the DB.** In-memory SQLite (`sqlite::memory:`) or a
  `tempfile::tempdir()` vault. Integration tests that touch the real store
  are the point — mocking defeats them.
- **Doctests** stay compiling and runnable; mark non-runnable `rust,no_run`.

### 6.5 CLI specifics (`cairn-cli`)
- **`clap` 4.5 derive API** with subcommands, one per verb. Mirror the brief
  §8 table exactly — CLI shape is the contract. Use `ValueEnum` for closed
  sets (search modes, forget modes). Shell completions via `clap_complete`.
- **Config precedence:** CLI flag > env (`CAIRN_*`) > `.cairn/config.yaml` >
  user file > defaults. Layer with `figment` (or a small hand-rolled merge)
  — centralize in one function, test as a single unit.
- **Exit codes:** return `std::process::ExitCode` from `main`. `0` success,
  `1` generic failure, `2` clap usage error, `64-78` sysexits-style
  (`EX_UNAVAILABLE=69` for `CapabilityUnavailable`, `EX_CONFIG=78` for bad
  config). Fail closed on unknown verbs, args, and config keys.
- **TTY detection:** `std::io::IsTerminal` (stable) to suppress colors and
  progress bars when piping. Never hard-code ANSI escapes.
- **`--log-format json`** flips `tracing-subscriber` to JSON layer; default
  is human-readable. See §6.6. Every verb supports `--json` for machine
  output where it has human output.

### 6.6 Logging & observability
- **`tracing`** for all diagnostics. Instrument boundaries with
  `#[tracing::instrument(skip(...), err, fields(verb, actor, scope, request_id))]`.
  Prefer structured fields over formatted strings.
- **Never log record bodies above `debug`.** Metadata only. Consent-sensitive
  fields (user text, source content) never leave `trace`.
- **`RUST_LOG` honored via `tracing_subscriber::EnvFilter`**; default filter
  for `cairn-cli` is `warn,cairn=info`. Document the common filters in
  `cairn --help` long output.
- **Metrics land in `.cairn/metrics.jsonl`** (brief §3). Structured,
  append-only, one JSON object per line.
- **OpenTelemetry export** (`opentelemetry` + `tracing-opentelemetry`) is
  feature-gated — P0 binaries ship without it.

### 6.7 Dependencies
- **Workspace deps** (`[workspace.dependencies]`) — members reference as
  `serde = { workspace = true }`. Inline only when exactly one crate uses it
  and it's unlikely to spread.
- **Disable `default-features` on heavy crates** (`reqwest`, `tokio`, `sqlx`);
  opt into exactly what's needed. Use `dep:` syntax to avoid implicit
  features leaking out.
- **New dep = justify in PR.** Minimal deps beat shaving 20 LOC. Check the
  transitive tree with `cargo tree -e normal --depth 2`.
- **Licensing:** `deny.toml` allowlist controls acceptance. Adding a license
  needs maintainer sign-off.
- **Supply chain:** `cargo deny check`, `cargo audit`, `cargo machete` before
  every PR.

### 6.8 Style
- **rustfmt** near-defaults. `imports_granularity = "Crate"` is fine if
  agreed; don't diverge otherwise.
- **Clippy:** workspace lints include `pedantic`. Pragmatic allows
  (`module_name_repetitions`, `missing_errors_doc`, `missing_panics_doc`)
  already live in the workspace; don't add local `#[allow(...)]` without a
  one-line reason comment. `clippy::unwrap_used`, `clippy::expect_used`,
  `clippy::dbg_macro`, `clippy::todo` should stay `warn` or `deny`.
- **Naming:** verbs are verbs (`ingest`), contracts are nouns
  (`MemoryStore`), errors are `<Domain>Error`, traits describe capability
  (`Fetchable`, `Consented`), not implementation (`FooImpl`). Acronyms are
  words (`IdlParser`, not `IDLParser`).
- **Module layout:** group by cohesion, not one-type-per-file.
- **Edition 2024 niceties:** let-chains (`if let Some(x) = a && x.ok()`),
  native async-fn-in-traits where trait objects aren't needed.

### 6.9 Performance
- **No premature `.clone()`.** Pass `&str`, `&[T]`, `&Path`. `Cow<'_, str>`
  when both borrowed and owned are common. Enable
  `clippy::clone_on_ref_ptr` and `clippy::redundant_clone` locally when
  tuning a hot path.
- **`Arc<Mutex<T>>` is a last resort.** Prefer message passing
  (`tokio::sync::mpsc`), `Arc<RwLock>`, or `arc-swap` for read-mostly
  config. `Arc<T>` alone is fine for shared immutable data.
- **Pre-size collections:** `Vec::with_capacity` when the size is known.
- **Iterator chains** over `for`+`push` where it stays readable; collect only
  at boundaries.
- **Measure before optimizing.** `criterion` benches for hot paths; commit
  the baseline in the PR.

### 6.10 API design
- **Newtypes for IDs** (`RecordId(String)`, `SessionId(String)`,
  `WorkflowId(Uuid)`) — never leak primitives across crate boundaries.
  Derive `Debug, Clone, PartialEq, Eq, Hash`; hand-roll `Display`.
- **`From` / `Into`** for infallible conversions; `TryFrom` for fallible.
- **Builders via `bon`** (derive-based) for anything with >3 optional fields
  (verb args, hot-memory recipes). Avoid hand-rolled builders or
  `typed-builder` in new code.
- **Sealed traits** for contract extensions we don't want third-party
  implementations of (use a private supertrait).
- **`#[non_exhaustive]`** on all public enums that may grow (error variants,
  capability codes) — unless exhaustiveness is part of the contract.
- **Semver discipline:** adding a required field is a breaking change. Use
  `#[serde(default)]` + optional fields for forward compat in wire types.

### 6.11 SQLite / store (`cairn-store-sqlite`)
- **`rusqlite` or `sqlx`** — pick one per the scaffold design doc. If
  `sqlx`, use compile-time `query!` / `query_as!` macros with
  `SQLX_OFFLINE=true` and commit the `.sqlx/` metadata.
- **Migrations** in `crates/cairn-store-sqlite/migrations/`, run via
  `sqlx::migrate!()` or equivalent. Every migration is append-only; never
  mutate a committed migration — add a new one.
- **WAL state machine** (brief §5.6) lives in `cairn-core` as pure
  functions; the adapter only persists its outputs. Store tests verify the
  state machine's invariants against the real SQLite schema.

### 6.12 MCP (`cairn-mcp`)
- **Official `rmcp` crate** (modelcontextprotocol/rust-sdk) is the canonical
  Rust MCP SDK. Pin tightly — the spec still evolves.
- **Transports:** stdio for CLI-embedded servers, SSE / streamable-HTTP for
  network. Keep transport selection in `cairn-cli`; `cairn-mcp` stays
  protocol-only.
- **JSON schemas** for tool inputs/outputs: `schemars` derives them from the
  same Rust types the verbs accept — never hand-write schemas.
- **Wire compat:** the `status` response must be byte-identical across an
  incarnation (brief §8.0.a) — snapshot-test it.

---

## 7. Test-driven workflow

1. Write the failing test at the smallest layer that captures the behaviour
   (usually unit in `cairn-core`).
2. Promote to integration when a trait boundary is involved.
3. Add a CLI snapshot test for any new verb surface or output change.
4. If the issue is a bug: write the test that reproduces it **before** the
   fix, commit the failing test in its own commit, then fix.

---

## 8. Verification checklist (run before pushing)

These are the same commands the `ci.yml`, `docs.yml`, and `supply-chain.yml`
workflows run. See `docs/ci.md` for the job-by-job mapping.

```bash
# ci.yml
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check

# docs.yml
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked

# supply-chain.yml (install once: cargo install cargo-deny cargo-audit cargo-machete)
cargo deny check
cargo audit --deny warnings
cargo machete

# release-dry-run.yml (only when touching publish-affecting metadata)
cargo package --workspace --no-verify --locked --allow-dirty
cargo publish --dry-run --locked --allow-dirty -p cairn-idl
cargo publish --dry-run --locked --allow-dirty -p cairn-core
```

A PR that touches generated code must also re-run `cargo run -p cairn-idl
--bin cairn-codegen` and commit the result.

A PR that touches CLI flags, config defaults, bundled plugins, IDL/MCP
metadata, workspace package membership, or user-facing docs must also re-run
`cargo run -p cairn-cli --bin cairn-docgen -- --write` and commit the generated
Markdown under `docs/site/src/reference/generated/`.

---

## 9. Commit & PR etiquette

- **Commit messages:** imperative subject ≤72 chars, body explains *why*, not
  *what*. Reference brief section numbers when relevant (e.g.,
  `feat(verbs): wire ingest state machine (brief §5.6)`).
- **PR description:** link the issue, cite brief sections, list invariants
  touched, paste verification output. Small PRs over big ones.
- **Draft PR early** if you want review on direction before finishing.
- **Never merge your own PR without review** on load-bearing changes (core
  traits, WAL, consent journal, config schema).
- **MSRV:** declared in `[workspace.package] rust-version = "1.95.0"`. Bumping
  MSRV is a minor-version release and must be called out in the PR + changelog.
- **Publish order** (when we ship to crates.io): leaf crates first —
  `cairn-idl`, `cairn-core`, `cairn-test-fixtures` — then `cairn-sdk`,
  then adapters (`cairn-store-sqlite`, `cairn-sensors-local`, `cairn-mcp`,
  `cairn-workflows`), finally `cairn-cli`. Dry-run with
  `cargo publish --dry-run`.

---

## 10. Quick map — where things live

```
cairn/
├── CLAUDE.md                       ← you are here
├── README.md                       ← public intro
├── Cargo.toml                      ← workspace, lints, shared deps
├── rust-toolchain.toml             ← pinned channel (1.95.0)
├── deny.toml                       ← cargo-deny policy
├── crates/
│   ├── cairn-core/                 ← traits, pure pipeline, errors (no I/O)
│   ├── cairn-cli/                  ← `cairn` binary
│   ├── cairn-sdk/                  ← typed in-process SDK over the verbs
│   ├── cairn-mcp/                  ← MCP stdio/http adapter
│   ├── cairn-store-sqlite/         ← SQLite + FTS5 + sqlite-vec
│   ├── cairn-sensors-local/        ← hooks/IDE/term/clipboard/voice/screen
│   ├── cairn-workflows/            ← background workflow host
│   ├── cairn-idl/                  ← IDL + codegen. Run `cargo run -p cairn-idl --bin cairn-codegen` after IDL edits; CI gates on no-diff.
│   └── cairn-test-fixtures/        ← dev-only test helpers
├── docs/
│   ├── ci.md                       ← CI/CD reference + branch protection
│   ├── design/
│   │   ├── design-brief.md         ← SOURCE OF TRUTH
│   │   ├── architecture.md         ← crate topology summary
│   │   ├── traceability.md         ← design-section → issue map
│   │   └── 2026-04-23-rust-workspace-scaffold-design.md
│   └── superpowers/
├── scripts/
│   └── check-core-boundary.sh      ← enforces core dep-freeness
├── fixtures/                       ← golden fixtures
└── assets/                         ← logos, static assets
```

---

## 11. When in doubt

- **Data-model question** → design brief §3 (vault), §4 (contracts), §6 (taxonomy).
- **Verb shape question** → brief §8. CLI is the contract; MCP/SDK/skill mirror.
- **Workflow / lifecycle question** → brief §5 (pipeline), §10 (workflows).
- **Privacy / consent question** → brief §14.
- **Capability advertisement / wire compat** → brief §8.0.a, §15.
- **Scale-up / Nexus / federation question** → brief §3.0, §12.

If the brief is silent or contradictory, raise it on the issue — do not guess.
Updating the brief is a legitimate outcome of an implementation PR.
