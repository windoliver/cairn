# Docs Freeze, Migration Guides, and Beta Readiness Checklist — Design

**Issue**: [#138](https://github.com/windoliver/cairn/issues/138) — `[P3] Freeze docs, migration guides, and beta readiness checklist`
**Parent**: [#31](https://github.com/windoliver/cairn/issues/31) — `[P3] Harden replay cassettes, coherence benchmarks, and documentation freeze`
**Phase**: v0.4 (Evaluation + docs freeze) · priority P3
**Brief sections**: §19 v0.4 docs freeze, §18.b Consumer blueprint, §18.c Capability matrix, §15 Evaluation
**Status**: Spec — pending implementation plan

---

## 1. Goal

Land the v0.4 docs-freeze artifact set:

1. A **capability matrix** reference page mirroring brief §18.c — the single source of truth other docs link to instead of repeating phase claims.
2. A **migration guide framework** (`usage/migration/`) — one policy doc plus three per-pair guides (`v0.1→v0.2`, `v0.2→v0.3`, `v0.3→v0.4`) with concrete deltas pulled from §19 and scaffold sections marked `_To be filled when v0.Y ships._` for items not yet decided.
3. A **beta readiness checklist** — `docs/site/src/maintainers/beta-readiness.md` (canonical runbook) plus `scripts/beta-readiness.sh` (executes every automatable gate from CLAUDE.md §8) so a maintainer can walk a build to beta-eligible with one command + a short manual list.
4. An **audit pass** over every hand-written page under `docs/site/src/` so no doc disagrees with the current CLI, MCP surface, config schema, or capability matrix.

The pre-v0.1 repo state (no released versions yet) shapes the migration framing: guides are **forward-looking templates** with the upgrade contract pinned now, per-version content filled when each release ships.

## 2. Non-goals

- **CI lint that fails the build when §8, §18.c, and §19 disagree** (brief §18.c calls for this). Separate follow-up issue — the lint needs a parser over the brief and capability matrix that is its own design.
- **Marketing-site polish** (v1.0 work, per issue out-of-scope clause).
- **Per-domain consumer templates** (`templates/personal/`, `templates/engineering/`, etc., from §18.b) — sibling work, not this issue.
- **Filling speculative migration content.** Pre-v0.1: anything not already pinned in §19 is left as `_To be filled when v0.Y ships._` so the scaffold is honest.
- **New Rust code.** Docs + bash only. No IDL, no codegen, no new clippy lint, no new deps.

## 3. Source of truth in the brief

| Brief excerpt | This design's response |
|---|---|
| §19 v0.4 — "Documentation freeze. Beta distribution channels." | Capability matrix page + migration framework + beta-readiness runbook + script. |
| §18.b — "Migration recipe — Step-by-step: import existing memory, dual-run, cut over" | `usage/migration/index.md` enumerates the upgrade contract (vault, WAL, config, CLI, MCP wire compat) and the dual-run / cut-over playbook. |
| §18.c — Authoritative capability table (lines 4184-4192) | `reference/capability-matrix.md` mirrors the table verbatim with footnotes pointing back to the brief and to `cairn-core::status::advertise`. |
| §18.c — "a CI lint fails the build if §8, §18.c, and §19 disagree on what ships when" | Out of scope for this PR; tracked as a follow-up issue. The capability matrix page calls out manually that it must mirror the brief, with a process note. |
| §15 — Eval gates, coherence floors, regression budgets | Beta-readiness runbook lists `cargo run -p cairn-bench --release -- all` and `coherence run --gate beta` as required gates 4. |
| CLAUDE.md §6 invariant 6 — "Fail closed on capability… advertisement decisions live in `cairn-core::status::advertise`" | Capability matrix page footnotes link to `cairn-core::status::advertise` so maintainers know where to flip rows. |

## 4. Architecture

### 4.1 Files added

```
docs/site/src/
  reference/
    capability-matrix.md             # single source of truth, mirrors §18.c
  usage/
    migration/
      index.md                       # policy: what changes, what doesn't, dual-run, cut-over
      v0.1-to-v0.2.md                # scaffold + concrete §19 deltas
      v0.2-to-v0.3.md                # scaffold + concrete §19 deltas
      v0.3-to-v0.4.md                # scaffold + concrete §19 deltas
  maintainers/
    beta-readiness.md                # canonical runbook (~250 lines)
scripts/
  beta-readiness.sh                  # bash 3.2-safe, mirrors install-smoke.sh style
```

### 4.2 Files edited

```
docs/site/src/SUMMARY.md             # add navigation entries
docs/site/src/status.md              # refresh "Implemented / Stubbed" to match repo HEAD
docs/site/src/index.md               # add capability-matrix link
docs/site/src/quickstart.md          # consistency fixes from audit
docs/site/src/concepts/architecture.md
docs/site/src/concepts/vault-layout.md
docs/site/src/concepts/capability-model.md
docs/site/src/usage/installation.md
docs/site/src/usage/cli.md
docs/site/src/usage/config.md
docs/site/src/usage/plugins.md
docs/site/src/usage/mcp.md
docs/site/src/usage/claude-code.md
docs/site/src/usage/codex.md
docs/site/src/usage/skill.md
docs/site/src/usage/backup.md
docs/site/src/usage/claude-code-reference.md
docs/site/src/reference/rust-api.md
docs/site/src/reference/policy-gates.md
docs/site/src/reference/idl.md
docs/site/src/reference/bench/index.md
docs/site/src/maintainers/codegen.md
docs/site/src/maintainers/docs.md
docs/site/src/maintainers/ci.md
docs/design/traceability.md          # add row for #138
```

Edits are inline fixes from the audit pass (see §6). Generated pages under `reference/generated/` are not touched by hand.

### 4.3 Files not touched

- `crates/**` — no Rust changes.
- `Cargo.toml`, `Cargo.lock` — no dependency changes.
- IDL files under `crates/cairn-idl/` — no schema changes.
- `.github/workflows/*.yml` — the beta-readiness script is a maintainer tool, not a new CI job (existing `ci.yml` already runs the gates the script wraps).

## 5. Component specs

### 5.1 `reference/capability-matrix.md`

**Purpose:** single source of truth for "what ships in v0.X." Other docs link here.

**Structure (~150 lines):**

1. **Heading + SoT banner** — "When this page and brief §18.c disagree, the brief wins. Open a PR updating this page."
2. **Phase legend** — v0.1 P0, v0.2 P1, v0.3 P2, v0.4 P3, v1.0 GA, with one-line goal per phase from §19.
3. **Capability table** — mirrors §18.c table (lines 4184-4192) verbatim:

   | Capability | v0.1 ships | v0.2 ships | v0.3+ |
   |---|---|---|---|
   | Core verbs 1–8 across all 4 surfaces | yes — all 8 | unchanged | unchanged |
   | `search` modes | keyword + semantic + hybrid via local `candle` | + BM25S + cloud embedding providers | + `cairn.federation.v1` |
   | Session reload | active-session | + cold rehydration | unchanged |
   | `forget` modes | `record` | + `session` fan-out | + `scope` |
   | `ConsolidationWorkflow` | rolling-summary | + Reflection / REM / Deep | + EvolutionWorkflow |
   | SRE observability | basic lint + health | full SRE surface (OTel) | unchanged |
   | Extension namespaces | `cairn.admin.v1` | + `cairn.aggregate.v1` | + `cairn.federation.v1` + `cairn.sessiontree.v1` |
   | Sensors | hooks + IDE + terminal + clipboard + voice + screen + neuroskill + recording-to-text | unchanged | + GitHub / email / Drive / Notion / web-clipper connectors |

4. **Capability-code map** — table mapping each row to its `cairn.mcp.v1.*` capability code so readers know `status.capabilities[]` advertisement equals behavior.
5. **Footnotes:**
   - Capability advertisement lives in `cairn-core::status::advertise` (CLAUDE.md §4 invariant 6); flipping a row on requires a `wiring::*_WIRED` constant change.
   - Remediation hints flow from `cairn-core::status::REMEDIATION`.
   - This table mirrors brief §18.c lines 4184-4192 — keep in sync; brief wins on conflict.
6. **Cross-references:** §8.0.a (capability codes), §15 (eval gates), §19 (sequencing), `usage/migration/index.md`.

### 5.2 `usage/migration/index.md` — migration policy (~200 lines)

**Sections:**

1. **The stability contract.** What never changes across releases:
   - The 8 verbs (`ingest` / `search` / `retrieve` / `summarize` / `assemble_hot` / `capture_trace` / `lint` / `forget`).
   - The `cairn status` envelope shape (§8.0.b).
   - Vault layout roots (`sources/`, `raw/`, `wiki/`, `skills/`, `purpose.md`, `.cairn/`).
2. **What may change.** Versioned per release, with rules:
   - **Capability codes** (`cairn.mcp.v1.*`) — added across phases; existing codes never change meaning.
   - **Config schema** — additive; new keys ship with safe defaults. Removals require one release of deprecation warning.
   - **CLI flags** — additive same way.
   - **WAL state machines** — new states append; existing transitions never change semantics (CLAUDE.md §6.11).
   - **SQLite migrations** — append-only, never mutated (CLAUDE.md §6.11).
3. **Standard upgrade steps:**
   1. Read the per-pair guide (`v0.X-to-v0.Y.md`).
   2. Back up `.cairn/cairn.db` and the vault root (`cairn backup register` when shipped; cold copy until then).
   3. Install the new binary side-by-side.
   4. Run `cairn status --json` against both — diff capabilities.
   5. Run `cairn doctor` (when shipped) to verify config keys + vault layout.
   6. Cut over.
4. **Dual-run pattern** — per §16.a / §18.b "First month": point new binary at a vault snapshot, replay recent traffic, compare `search` / `retrieve` outputs before retiring the old install.
5. **Import recipes** — placeholder list with status (per §18.b "First four hours" step 3: `v0.2 cairn import --from <legacy>`).
6. **Unsupported migrations** — table of "if you skipped >2 phases, do this instead" pointers (initially empty; populated as phases ship).

### 5.3 Per-pair migration guides (~120 lines each)

Common skeleton for `v0.1-to-v0.2.md`, `v0.2-to-v0.3.md`, `v0.3-to-v0.4.md`:

1. **Phase summary** — one-paragraph pull from brief §19.
2. **Capability deltas** — table linking back to `capability-matrix.md` rows that flipped from "no" to "yes" between these phases.
3. **Config schema deltas** — new keys, defaults, deprecations. Stub now, filled when v0.Y ships.
4. **CLI / MCP / SDK / skill deltas** — new flags, new verb args, new capability codes.
5. **WAL / store deltas** — new state-machine rows, new migrations, on-disk layout changes.
6. **Sensor deltas** — what `SensorIngress` connectors gained per §19.
7. **Upgrade steps** — copy-pasteable bash for the cut-over (placeholders for binary paths until release URLs exist).

**Concrete content per pair (already pinned in §19, written now):**

- **v0.1 → v0.2** (P0 → P1): BM25S + cloud embeddings via Nexus sandbox profile, `forget` session mode + drain fences, `Reflection`/REM/Deep tiers, full SRE observability with OpenTelemetry, `cairn bench` public harness, Electron alpha desktop, cold rehydration (US6), Codex consumer wired, `cairn.aggregate.v1` extension namespace, `promote` WAL state machine.
- **v0.2 → v0.3** (P1 → P2): `cairn.federation.v1` cross-tenant queries, `PromotionWorkflow` + `PropagationWorkflow`, consent-gated team/org share, source connectors (GitHub / IMAP email / Google Drive / OneDrive / Notion / web-clipper), `cairn.sessiontree.v1` (fork/clone/switch/merge), `cairn.admin.v1` grows `connector_enable` / `connector_disable` / `connector_backfill` verbs, `evolve` WAL state machine with canary rollout, `forget` scope mode, `EvolutionWorkflow` mutations.
- **v0.3 → v0.4** (P2 → P3): extended `cairn bench` corpora for research / engineering / support domains, coherence gate floors (per #137), replay cassettes covering every v0.1-v0.3 capability (per #136), docs freeze (per this issue), beta distribution channels.

Sections 3–6 of each per-pair guide are marked `_To be filled when v0.Y ships._` so the file is an honest scaffold, not a fiction.

### 5.4 `maintainers/beta-readiness.md` — canonical runbook (~250 lines)

**Sections:**

1. **Purpose.** "If every gate passes, the build is beta-eligible. If any gate fails, the build is not beta-eligible."
2. **Quick start.** `scripts/beta-readiness.sh` runs gates 1–8 (automatable). Gates 9–14 remain manual.
3. **Gate categories** — each row has: name, command (or "manual"), pass criterion, common failure modes.

| # | Category | Items |
|---|---|---|
| 1 | Code quality | `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo check --workspace --all-targets --locked`, `cargo nextest run --workspace --locked --no-fail-fast`, `cargo test --doc --workspace --locked` |
| 2 | Core boundary | `scripts/check-core-boundary.sh`, `scripts/check-no-os-locks.sh`, `scripts/check-lint-readonly-sources.sh` |
| 3 | Generated code drift | `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`, `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` |
| 4 | Eval gates | `cargo run -p cairn-bench --release --locked -- all`, `cargo run -p cairn-bench --release --locked -- coherence run --gate beta` |
| 5 | Docs build | `mdbook build docs/site`, `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked` |
| 6 | Supply chain | `cargo deny check`, `cargo audit --deny warnings`, `cargo machete` |
| 7 | Install smoke | `scripts/install-smoke.sh` against `target/release/cairn` |
| 8 | Package dry-run (only when touching publish-affecting metadata) | `cargo package --workspace --no-verify --locked --allow-dirty`, `cargo publish --dry-run --locked --allow-dirty -p cairn-idl`, `cargo publish --dry-run --locked --allow-dirty -p cairn-core` |
| 9 | Capability sync (manual) | `cairn status --json` advertised capabilities must equal `capability-matrix.md` row for the target phase. Reviewer attestation. |
| 10 | Migration guide review (manual) | `v0.X-to-v0.Y.md` for the target phase has all 7 sections populated for surfaces that actually ship in v0.Y. |
| 11 | Known limitations (manual) | `status.md` "Stubbed or pending" reviewed against current capability matrix; release notes call out anything still stubbed. |
| 12 | Cassette replay (manual) | All replay cassettes (#136) pass under the beta gate; coherence floors per #137 met. |
| 13 | Privacy posture (manual) | Consent journal write path exercised on a real session; `cairn forget` round-trip; presidio scrub spot-check. |
| 14 | Release notes draft (manual) | Per-phase template populated; capability deltas cross-linked to migration guide. |

4. **Manual gate guidance** — for each manual gate, exact command + what "pass" looks like.
5. **Failure remediation** — table mapping failed gate → first place to look (e.g. clippy fail → recent PR diff; docgen drift → re-run with `--write`; capability sync fail → `cairn-core::status::advertise`).
6. **Sign-off block** — checklist a maintainer copy-pastes into the release issue.

### 5.5 `scripts/beta-readiness.sh` — automation harness (~250 lines)

**Style:** bash 3.2-safe, mirrors `scripts/install-smoke.sh` (POSIX-y, no `jq`, no `mapfile`, no associative arrays). `set -euo pipefail`.

**Modes:**

- `--quick` (default, ~3 min): fmt, clippy, check, nextest, doc tests, `scripts/check-*.sh`, codegen `--check`, docgen `--check`, `cargo deny`, `cargo machete`.
- `--full` (~15 min): everything in `--quick` + `cargo bench all` + `coherence run --gate beta` + `mdbook build` + `cargo doc` with broken-link check + `cargo audit` + `install-smoke.sh` against release build + `cargo package` + per-leaf-crate `cargo publish --dry-run`.

**Behavior:**

- Logs `ok: <gate>` / `fail: <gate> — <reason>` / `skip: <gate> — install <subcommand>`.
- Fails fast on the first failure; prints summary nonetheless.
- Skipped gates (missing toolchain) flagged but don't fail the run; final summary shows skip count and remediation hint.
- Final block prints the manual gates list — script never claims those passed.
- Honors `CAIRN_BIN`, `CARGO_TARGET_DIR`, `RUST_LOG` env passthrough.
- Exits 0 only when fail count is 0.

**Output shape:**

```
ok: fmt
ok: clippy
ok: check
ok: nextest (412 tests)
ok: doc-tests
ok: core-boundary
ok: no-os-locks
ok: lint-readonly-sources
ok: codegen --check
ok: docgen --check
ok: cargo deny
skip: cargo audit — install cargo-audit
ok: cargo machete
---
beta-readiness: 12 ok, 0 fail, 1 skip
manual gates remaining: 6
  - 9: capability sync (cairn status --json vs capability-matrix.md)
  - 10: migration guide review (usage/migration/v0.X-to-v0.Y.md)
  - 11: known limitations (status.md vs capability matrix)
  - 12: cassette replay (cargo run -p cairn-bench -- coherence run --gate beta)
  - 13: privacy posture (forget round-trip + presidio scrub)
  - 14: release notes draft
```

## 6. Audit pass

### 6.1 Methodology

Walk every page under `docs/site/src/` not in `reference/generated/`. For each page check:

1. **Capability claims** — does the page name a v0.X capability inconsistent with `capability-matrix.md`?
2. **CLI claims** — every command + flag matches `cairn <verb> --help` on HEAD.
3. **Config claims** — every config key referenced exists in `reference/generated/config-defaults.md`.
4. **MCP claims** — every tool name matches `reference/generated/mcp-tools.md`.
5. **Cross-doc contradictions** — page disagrees with `status.md` or `index.md`.

### 6.2 Page list (hand-edited)

`index.md`, `quickstart.md`, `status.md`, `concepts/architecture.md`, `concepts/vault-layout.md`, `concepts/capability-model.md`, `usage/installation.md`, `usage/cli.md`, `usage/config.md`, `usage/plugins.md`, `usage/mcp.md`, `usage/claude-code.md`, `usage/codex.md`, `usage/skill.md`, `usage/backup.md`, `usage/claude-code-reference.md`, `reference/rust-api.md`, `reference/policy-gates.md`, `reference/idl.md`, `reference/bench/index.md`, `maintainers/codegen.md`, `maintainers/docs.md`, `maintainers/ci.md`.

~23 pages. Each gets a 2-line audit note in the PR description (`page → finding → action`). Fixes inline; if a fix is bigger than a paragraph, file a follow-up issue and add a `> [!NOTE]` callout on the page pointing at the issue.

### 6.3 Audit acceptance bar

- No page references a capability the matrix marks as a future phase without an explicit "_v0.X+_" badge.
- No page shows a CLI flag or config key that doesn't exist on HEAD.
- Every page links to `capability-matrix.md` at least once so the SoT is discoverable from any entry point.
- `status.md` "Implemented / Stubbed" reflects what `cargo run -p cairn-cli -- status --json` reports today.
- `traceability.md` has a row for #138.

## 7. Tests and verification

Docs + bash; no Rust changes. Verification gates run on this branch before push:

1. `mdbook build docs/site` — no broken intra-doc links, no missing pages.
2. `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` — generated pages unchanged (we don't touch them).
3. `scripts/beta-readiness.sh --quick` — the script we're shipping passes on our own branch (dogfood).
4. `shellcheck scripts/beta-readiness.sh` — clean.
5. **Manual:** `mdbook serve docs/site`, click through SUMMARY.md; verify nav + cross-refs work.
6. Full CLAUDE.md §8 list before push.

## 8. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Capability matrix page drifts from brief §18.c silently. | Page header banner: "When this page and brief §18.c disagree, the brief wins. Open a PR updating this file." Follow-up issue tracks the actual CI lint. |
| Per-pair migration guides feel like fiction because v0.2+ doesn't exist yet. | Sections that depend on unimplemented behavior marked `_To be filled when v0.Y ships._` Concrete §19 deltas are written now; speculative deltas aren't. |
| Beta-readiness script silently passes because a gate is skipped. | Skipped gates are flagged in the summary with explicit remediation hints. Maintainer must confirm skip is intentional. |
| Audit pass surfaces large inconsistencies that need code changes. | Out-of-scope fix → follow-up issue + page callout. PR stays docs-only. |
| New script breaks on macOS bash 3.2. | Style-matches `install-smoke.sh` which already runs on macOS default bash; CI runs on Linux + macOS. |

## 9. Out of scope (explicit)

- CI lint that fails the build when §8, §18.c, and §19 disagree (brief §18.c calls for this). Tracked as a follow-up issue.
- Marketing-site polish (v1.0 work, per issue out-of-scope).
- Per-domain consumer templates from §18.b (`templates/personal/`, etc.).
- Filling speculative migration content not pinned in §19.
- New Rust code, dependencies, IDL changes, codegen changes.

## 10. Open questions

None. All scope decisions made during brainstorming. Manual audit findings are recorded as the audit runs.
