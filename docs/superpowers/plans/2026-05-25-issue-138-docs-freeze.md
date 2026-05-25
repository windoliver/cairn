# Issue #138 Docs-Freeze Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the v0.4 docs-freeze artifact set — capability matrix reference page, migration guide framework, beta readiness runbook + script, and an audit pass that aligns every hand-written site page with the current CLI / MCP / config / capability matrix.

**Architecture:** Pure docs + bash. No Rust changes. Single source of truth for "what ships in v0.X" lives in one new reference page mirroring brief §18.c; all other pages link into it. Migration guides scaffolded forward-looking with concrete §19 deltas pinned now and unimplemented sections marked `_To be filled when v0.Y ships._`. Beta-readiness ships as a maintainer-facing runbook plus a bash 3.2-safe automation script that wraps every gate from `CLAUDE.md` §8.

**Tech Stack:** Markdown (mdBook), bash 3.2-safe shell, existing Cargo CLI tools (`cairn-docgen`, `cargo nextest`, `cargo deny`, `cargo machete`).

**Spec:** `docs/superpowers/specs/2026-05-25-issue-138-docs-freeze-design.md`

---

## File Structure

**Create:**
- `docs/site/src/reference/capability-matrix.md` — single source of truth.
- `docs/site/src/usage/migration/index.md` — migration policy + upgrade contract.
- `docs/site/src/usage/migration/v0.1-to-v0.2.md` — scaffold + concrete §19 deltas.
- `docs/site/src/usage/migration/v0.2-to-v0.3.md` — scaffold + concrete §19 deltas.
- `docs/site/src/usage/migration/v0.3-to-v0.4.md` — scaffold + concrete §19 deltas.
- `docs/site/src/maintainers/beta-readiness.md` — canonical runbook.
- `scripts/beta-readiness.sh` — bash 3.2-safe automation, mirrors `scripts/install-smoke.sh` style.

**Modify:**
- `docs/site/src/SUMMARY.md` — add navigation entries for the new pages.
- `docs/site/src/status.md` — refresh `Implemented / Stubbed` against HEAD.
- `docs/site/src/index.md`, `quickstart.md`, `concepts/*.md`, `usage/*.md`, hand-written `reference/*.md`, `maintainers/*.md` — audit-driven inline fixes.
- `docs/design/traceability.md` — add row for #138.

**Untouched:**
- `crates/**`, `Cargo.toml`, `Cargo.lock`, IDL files, generated reference pages, CI workflows.

---

## Task 1: Capability matrix reference page

Establishes the single source of truth that every other page links to. Built first so subsequent tasks can reference it.

**Files:**
- Create: `docs/site/src/reference/capability-matrix.md`
- Modify: `docs/site/src/SUMMARY.md` — add nav entry.

- [ ] **Step 1: Read brief §18.c capability table for verbatim mirror**

Run: `awk 'NR>=4182 && NR<=4194' docs/design/design-brief.md`
Expected: the canonical capability matrix table (8 rows × 4 columns). Note exact column headers and row wording — the new page mirrors them.

- [ ] **Step 2: Create `docs/site/src/reference/capability-matrix.md`**

Write this content:

```markdown
# Capability Matrix

> **Single source of truth.** Other docs link here. When this page and brief
> [§18.c](../../../../docs/design/design-brief.md) disagree, the brief wins —
> open a PR updating this page.

This table mirrors brief §18.c lines 4184-4192. It enumerates which capability
ships in which Cairn release, and is the table every other doc (concepts,
usage, migration guides) links to instead of repeating phase claims.

## Phase legend

| Phase | Priority | One-line goal |
|-------|----------|----------------|
| **v0.1** | P0 | Minimum substrate. Eight verbs, four surfaces, local-only, single static binary. |
| **v0.2** | P1 | Continuous learning + SRE surface. Cold rehydration, session forget, Reflection/REM/Deep, OpenTelemetry, Electron alpha, `cairn bench` public harness. |
| **v0.3** | P2 | Propagation + collective. Federation, share/propagation, source connectors, session-tree extension, EvolutionWorkflow. |
| **v0.4** | P3 | Evaluation + polish. Extended bench corpora, replay cassettes, coherence gates, docs freeze, beta channels. |
| **v1.0** | GA | SLAs, three harnesses shipped, desktop on three OSes, MCP semver freeze. |

## Capability matrix

| Capability | v0.1 ships | v0.2 ships | v0.3+ |
|------------|------------|------------|-------|
| Core verbs 1–8 (`ingest` / `search` / `retrieve` / `summarize` / `assemble_hot` / `capture_trace` / `lint` / `forget`) across all four surfaces (CLI · MCP · SDK · skill) | yes — all 8 | unchanged | unchanged |
| `search` modes | keyword (FTS5) + semantic (`sqlite-vec` + local `candle`) + hybrid (local blend); droppable to keyword-only via `search.local_embeddings: false` (others rejected with `CapabilityUnavailable`) | adds BM25S lexical scoring + swappable cloud embedding provider via `litellm` (OpenAI / Cohere / Voyage / Ollama); `semantic_degraded=true` only on transient outages | adds `cairn.federation.v1` cross-tenant queries via Nexus full hub |
| Session reload | active-session (US2 core) | + cold-storage rehydration (US6) | unchanged |
| `forget` modes | `record` (US8 core) | + `session` fan-out with drain fences | + `scope` mode |
| `ConsolidationWorkflow` | rolling-summary pass only (US4 core) | + Reflection / REM / Deep tiers | + EvolutionWorkflow mutations |
| SRE observability (OpenTelemetry dashboards, tier-migration metrics, rehydration gates) | basic lint + health | full SRE surface | unchanged |
| Extension namespaces | `cairn.admin.v1` (operator verbs) | + `cairn.aggregate.v1` (anonymized agent insights) | + `cairn.federation.v1` (share / accept / revoke — folder-scoped via `subject.path_prefix`) + `cairn.sessiontree.v1` (fork / clone / switch / merge — §5.7) |
| Sensors | hooks + IDE + terminal + clipboard + voice (sherpa-onnx + cpal) + screen (`xcap` + OS-native OCR, off by default per [ADR 0003](../decisions/0003-screen-sensor-packaging.md)) + neuroskill + recording-to-text batch pipeline | unchanged | + GitHub (issues / PRs / commits) + email (IMAP + webhook) + Drive (Google / OneDrive) + Notion + generic web-clipper extension |

## Capability codes

Every row above maps to one or more `cairn.mcp.v1.*` capability codes
advertised in `cairn status`. Clients MUST inspect `status.capabilities`
before issuing a mode; the runtime fails closed with `CapabilityUnavailable`
on any un-advertised code (brief §8.0.a).

| Row | Representative capability codes |
|-----|----------------------------------|
| Core verbs | `cairn.mcp.v1.<verb>` for each of the eight verbs |
| `search` modes | `cairn.mcp.v1.search.keyword`, `.semantic`, `.hybrid`, `.federation` |
| Session reload | `cairn.mcp.v1.retrieve.session`, `.rehydrate` |
| `forget` modes | `cairn.mcp.v1.forget.record`, `.session`, `.scope` |
| Consolidation tiers | `cairn.mcp.v1.summarize.rolling`, `.reflection`, `.rem`, `.deep` |
| Extension namespaces | `cairn.admin.v1.*`, `cairn.aggregate.v1.*`, `cairn.federation.v1.*`, `cairn.sessiontree.v1.*` |
| Sensors | `cairn.sensors.v1.<sensor>` (local + remote) |

## Where this is wired

- **Advertisement source:** `cairn-core::status::advertise` (CLAUDE.md §4
  invariant 6). Flipping a capability on requires both a row change here and
  a `wiring::*_WIRED` constant change in the issue that lands the dispatch
  path.
- **Remediation hints:** `cairn-core::status::REMEDIATION` populates
  `CapabilityUnavailable.data.remediation`. Keep that table in sync when this
  matrix grows.
- **Wire-compat tests:** `crates/cairn-mcp/tests/wire_compat.rs` (§15) asserts
  every advertised capability matches a dispatch path and rejects every
  un-advertised mode.

## Cross-references

- Brief sections: [§8.0.a](../../../../docs/design/design-brief.md) capability codes, [§15](../../../../docs/design/design-brief.md) evaluation gates, [§18.c](../../../../docs/design/design-brief.md) coverage summary, [§19](../../../../docs/design/design-brief.md) sequencing.
- Site pages: [Capability Model](../concepts/capability-model.md), [Migration Guides](../usage/migration/index.md), [Beta Readiness](../maintainers/beta-readiness.md).
```

- [ ] **Step 3: Add SUMMARY.md entry**

Edit `docs/site/src/SUMMARY.md`. Find the line `- [Generated CLI Reference](reference/generated/cli.md)`. Insert this line immediately above it (still under the `# Reference` heading):

```markdown
- [Capability Matrix](reference/capability-matrix.md)
```

- [ ] **Step 4: Build mdbook to verify**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: build succeeds, no warning for `capability-matrix.md`.

- [ ] **Step 5: Commit**

```bash
git add docs/site/src/reference/capability-matrix.md docs/site/src/SUMMARY.md
git commit -m "docs(reference): add capability matrix as single source of truth (brief §18.c)"
```

---

## Task 2: Migration policy index

Pins the upgrade contract — what changes between releases and what doesn't.

**Files:**
- Create: `docs/site/src/usage/migration/index.md`
- Modify: `docs/site/src/SUMMARY.md` — add Migration Guides section.

- [ ] **Step 1: Create the migration directory and index file**

Run: `mkdir -p docs/site/src/usage/migration`

Then create `docs/site/src/usage/migration/index.md` with this content:

```markdown
# Migration Guides

Cairn ships forward-looking migration guides for every phase boundary. Each
guide covers what changes between two releases and how to walk a vault from
one to the next.

| Pair | Phase delta | Status |
|------|-------------|--------|
| [v0.1 → v0.2](v0.1-to-v0.2.md) | P0 minimum substrate → P1 continuous learning + SRE | Forward-looking scaffold; concrete §19 deltas pinned. |
| [v0.2 → v0.3](v0.2-to-v0.3.md) | P1 → P2 propagation + collective | Forward-looking scaffold. |
| [v0.3 → v0.4](v0.3-to-v0.4.md) | P2 → P3 evaluation + polish | Forward-looking scaffold. |

The capability deltas in every per-pair guide cross-link back to the
[capability matrix](../../reference/capability-matrix.md) so the
phase-by-phase view stays single-sourced.

## The stability contract

The following surfaces never change shape across releases:

- **The eight verbs** — `ingest`, `search`, `retrieve`, `summarize`,
  `assemble_hot`, `capture_trace`, `lint`, `forget`. New verbs may be added
  under extension namespaces (`cairn.admin.v1.*`, `cairn.federation.v1.*`,
  …); the core eight never disappear.
- **The `cairn status` envelope** — fields and types stay byte-identical
  across an incarnation per brief §8.0.a. New capability codes are added to
  `capabilities[]`; existing ones never change meaning.
- **Vault layout roots** — `sources/`, `raw/`, `wiki/`, `skills/`,
  `purpose.md`, `.cairn/`. New subdirectories may appear under these roots;
  the roots themselves are load-bearing for every release.

## What may change between releases

| Surface | Rule |
|---------|------|
| Capability codes (`cairn.mcp.v1.*`) | Added across phases. Existing codes never change meaning. Removals require one release of deprecation. |
| Config schema (`.cairn/config.yaml`) | Additive. New keys ship with safe defaults. Removals deprecated one release first. |
| CLI flags | Additive same way. |
| WAL state machines | New states append; existing transitions never change semantics ([CLAUDE.md §6.11](../../../../CLAUDE.md)). |
| SQLite migrations | Append-only, never mutated. Each migration is a new file under `crates/cairn-store-sqlite/migrations/`. |
| MCP wire protocol | Frozen at v1.0; v0.x carries the `cairn.mcp.v1` namespace and may add capability codes without breaking incarnation contracts. |

## Standard upgrade steps

These steps apply to every per-pair migration:

1. Read the per-pair guide.
2. Back up `.cairn/cairn.db` and the vault root.
   - `cairn backup register --vault <path>` once `cairn backup` ships.
   - Until then: cold copy the vault directory (`cp -a vault vault.bak`).
3. Install the new binary side-by-side with the old one.
4. Diff `cairn status --json` output between the two binaries; verify
   advertised capabilities meet the deltas in the per-pair guide.
5. Run `cairn doctor` (once shipped) to verify config keys and vault layout.
6. Cut over: point your harness at the new binary, retire the old one.

## Dual-run pattern

Per brief §16.a and §18.b "First month":

1. Install the new binary side-by-side.
2. Point both at the same vault snapshot (read-only on the old, RW on the
   new).
3. Replay recent traffic through both.
4. Diff `search` / `retrieve` outputs. Tolerance bounds are documented in
   each per-pair guide.
5. Retire the old install only after parity is acceptable for a full cycle.

## Import recipes

Importing existing memory from legacy systems is covered by the
`cairn import` verb (lands in v0.2 per §18.b "First four hours" step 3).
Specific recipes will be linked here as connectors ship:

- Claude Code transcripts → `cairn import --from claude-code` (v0.2+)
- Codex sessions → `cairn import --from codex` (v0.2+)
- Generic markdown vault → `cairn import --from markdown` (v0.2+)

## Unsupported migrations

| Skip pattern | Recommendation |
|--------------|----------------|
| _None populated yet — pre-v0.1._ | _To be filled when phases ship and skip-paths become supported or explicitly rejected._ |
```

- [ ] **Step 2: Update SUMMARY.md with the migration section**

Edit `docs/site/src/SUMMARY.md`. Find the line `- [Claude Code Reference Consumer](usage/claude-code-reference.md)`. Insert these lines immediately after it (still under `# Usage`):

```markdown
- [Migration Guides](usage/migration/index.md)
  - [v0.1 → v0.2](usage/migration/v0.1-to-v0.2.md)
  - [v0.2 → v0.3](usage/migration/v0.2-to-v0.3.md)
  - [v0.3 → v0.4](usage/migration/v0.3-to-v0.4.md)
```

- [ ] **Step 3: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds. The three v0.X-to-v0.Y.md files don't exist yet so SUMMARY entries will report missing — that's OK; they land in tasks 3-5.

If the build fails on missing files, temporarily comment out the v0.1-to-v0.2/v0.2-to-v0.3/v0.3-to-v0.4 SUMMARY lines and uncomment them in task 5.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/usage/migration/index.md docs/site/src/SUMMARY.md
git commit -m "docs(migration): add migration policy index + upgrade contract"
```

---

## Task 3: Migration guide — v0.1 → v0.2

**Files:**
- Create: `docs/site/src/usage/migration/v0.1-to-v0.2.md`

- [ ] **Step 1: Create the file**

Write `docs/site/src/usage/migration/v0.1-to-v0.2.md` with this content:

```markdown
# v0.1 → v0.2 Migration

**Phase delta:** P0 minimum substrate → P1 continuous learning + SRE surface
+ richer search backends.

## Phase summary

v0.2 keeps the v0.1 substrate intact and adds three layers on top: a Python
sidecar (Nexus `sandbox`) for BM25S lexical scoring and swappable cloud
embedding providers, the full Reflection/REM/Deep consolidation tiers, and a
full SRE observability surface (OpenTelemetry, tier-migration dashboards,
rehydration latency gates). Existing v0.1 vaults migrate in place — the
SQLite file stays as the sole authority, Nexus indexes are derived
projections alongside.

## Capability deltas

The rows that flip from "no" to "yes" between v0.1 and v0.2 per the
[capability matrix](../../reference/capability-matrix.md):

| Capability row | v0.1 → v0.2 change |
|----------------|--------------------|
| `search` modes | adds BM25S lexical scoring + cloud embedding provider via `litellm` (OpenAI / Cohere / Voyage / Ollama); `semantic_degraded=true` only on transient outages mid-call |
| Session reload | adds cold-storage rehydration (US6) — `retrieve(session_id, rehydrate: true)` unpacks Nexus snapshot bundles transparently; budget ≤ 3 s p95 for sessions ≤ 10 MB |
| `forget` modes | adds `session` fan-out with `reader_fence` closure in the last chunk's transaction and exclusive session lock |
| `ConsolidationWorkflow` | adds Reflection / REM / Deep tiers; `DreamWorker` gains `hybrid` mode |
| SRE observability | full surface — OpenTelemetry export, per-tier latency histograms, archive/hydration counts, storage-cost metrics |
| Extension namespaces | adds `cairn.aggregate.v1` (anonymized agent insights, gated on `agent.enable_aggregate: true`) |

Plus: second consumer wired (Codex), Electron alpha desktop, `cairn bench`
public harness ships, `promote` WAL state machine.

## Config schema deltas

_To be filled when v0.2 ships._

Expected new keys (per brief §19):

- `search.bm25s.enabled` — bool, default `false`.
- `search.embedding_provider` — enum: `local-candle` (default) | `litellm`.
- `nexus.profile` — enum: `disabled` (default) | `sandbox`.
- `agent.enable_aggregate` — bool, default `false`.
- `consolidation.reflection.*`, `consolidation.rem.*`, `consolidation.deep.*` — cadence + window settings.
- OpenTelemetry exporter block (`otel.endpoint`, `otel.headers`).

## CLI / MCP / SDK / skill deltas

_To be filled when v0.2 ships._

Expected additions:

- `cairn forget --session <id>` and MCP `forget` `mode: "session"`.
- `cairn retrieve --rehydrate` flag and `RetrieveArgs.rehydrate: true`.
- `cairn import --from <legacy>` for migration imports.
- `cairn bench` corpus runner.
- New capability codes advertised: `cairn.mcp.v1.search.bm25s`, `.forget.session`, `.retrieve.rehydrate`, `cairn.aggregate.v1.*`.

## WAL / store deltas

_To be filled when v0.2 ships._

Expected:

- New WAL state-machine rows: `forget_session` (with drain fences), `promote`.
- SQLite migration adds `session_locks` and `cold_storage_pointers` tables (append-only — never modify existing v0.1 migrations).
- Nexus profile drops indexes under `.cairn/nexus/`; the SQLite file remains authoritative.

## Sensor deltas

No new sensors in v0.2. The local sensor suite from v0.1 (hooks, IDE,
terminal, clipboard, voice, screen, neuroskill, recording-to-text) carries
forward unchanged.

## Upgrade steps

_To be filled when v0.2 ships._

Expected sequence:

```bash
# 1. Back up v0.1 vault.
cairn backup register --vault "$VAULT"   # or cp -a "$VAULT" "${VAULT}.bak.v0.1"

# 2. Install v0.2 side-by-side.
cargo install cairn --version 0.2.0      # or brew upgrade cairn

# 3. Diff capabilities.
diff <(cairn --version 0.1 status --json) <(cairn --version 0.2 status --json)

# 4. Verify config compatibility.
cairn-0.2 doctor --vault "$VAULT"

# 5. Optional: enable Nexus sandbox.
cairn-0.2 config set nexus.profile sandbox

# 6. Cut over: point harness at the new binary.
cairn setup claude-code --binary "$(which cairn-0.2)"
```
```

- [ ] **Step 2: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: build succeeds for this file (v0.2-to-v0.3 / v0.3-to-v0.4 still missing — covered in tasks 4 and 5).

- [ ] **Step 3: Commit**

```bash
git add docs/site/src/usage/migration/v0.1-to-v0.2.md
git commit -m "docs(migration): scaffold v0.1 → v0.2 guide with §19 deltas"
```

---

## Task 4: Migration guide — v0.2 → v0.3

**Files:**
- Create: `docs/site/src/usage/migration/v0.2-to-v0.3.md`

- [ ] **Step 1: Create the file**

Write `docs/site/src/usage/migration/v0.2-to-v0.3.md` with this content:

```markdown
# v0.2 → v0.3 Migration

**Phase delta:** P1 continuous learning + SRE surface → P2 propagation +
collective.

## Phase summary

v0.3 adds cross-tenant propagation: consent-gated share/accept/revoke,
federation queries across vaults the caller has been granted access to, and
the full source-connector suite (GitHub, IMAP email, Drive, Notion,
web-clipper). The `EvolutionWorkflow` mutation layer becomes live with
canary rollout.

## Capability deltas

The rows that flip between v0.2 and v0.3 per the
[capability matrix](../../reference/capability-matrix.md):

| Capability row | v0.2 → v0.3 change |
|----------------|--------------------|
| `search` modes | adds `cairn.federation.v1` cross-tenant queries via Nexus full hub |
| `forget` modes | adds `scope` mode (fan-out across a vault subtree) |
| `ConsolidationWorkflow` | adds `EvolutionWorkflow` mutations |
| Extension namespaces | adds `cairn.federation.v1` (share / accept / revoke — folder-scoped via `subject.path_prefix`) + `cairn.sessiontree.v1` (fork / clone / switch / merge — §5.7) |
| Sensors | adds GitHub (issues / PRs / commits) + email (IMAP + webhook) + Drive (Google / OneDrive) + Notion + generic web-clipper extension |

Plus: `PromotionWorkflow`, `PropagationWorkflow`, `evolve` WAL state machine
with canary rollout, `cairn.admin.v1` grows `connector_enable` /
`connector_disable` / `connector_backfill` verbs.

## Config schema deltas

_To be filled when v0.3 ships._

Expected new keys (per brief §19):

- `federation.enabled` — bool, default `false`.
- `federation.peers[]` — list of `{vault_id, share_link}` entries.
- `propagation.canary.*` — canary rollout config for evolve mutations.
- `connectors.<name>.*` — per-connector enable + OAuth/webhook config for each of GitHub / email / Drive / Notion / web-clipper.
- `share.default_path_prefix` — folder scope for share-link grants.

## CLI / MCP / SDK / skill deltas

_To be filled when v0.3 ships._

Expected additions:

- `cairn share propose <subject> --to <peer> --path-prefix <prefix>` plus accept / revoke verbs.
- `cairn search --federation on` flag and `SearchArgs.federation: "on"`.
- `cairn forget --scope <path-prefix>`.
- `cairn admin connector_enable <name>`, `connector_disable`, `connector_backfill`.
- `cairn session-tree fork|clone|switch|merge` (`cairn.sessiontree.v1.*`).
- New capability codes advertised: `cairn.federation.v1.*`, `cairn.sessiontree.v1.*`, `cairn.mcp.v1.forget.scope`, `cairn.sensors.v1.<connector>` for each new connector.

## WAL / store deltas

_To be filled when v0.3 ships._

Expected:

- New WAL state-machine rows: `evolve` (with canary rollout), `share` /
  `propagate`.
- SQLite migrations add `share_grants`, `federation_peers`, `connector_state`
  tables (append-only).
- Source-connector incremental-sync cursors persisted under
  `.cairn/connectors/<name>/state.db`.

## Sensor deltas

v0.3 adds the full source-connector suite — each is a separate L2 crate keyed
off a stable OAuth or webhook payload format:

| Connector | Capability code | OAuth / payload |
|-----------|------------------|-----------------|
| GitHub | `cairn.sensors.v1.github` | GitHub App + webhook |
| IMAP email | `cairn.sensors.v1.email.imap` | IMAP IDLE + per-message hash |
| Email webhook | `cairn.sensors.v1.email.webhook` | SES / Postmark / Mailgun |
| Google Drive | `cairn.sensors.v1.drive.google` | OAuth + Drive Changes API |
| OneDrive | `cairn.sensors.v1.drive.onedrive` | OAuth + Delta API |
| Notion | `cairn.sensors.v1.notion` | OAuth + page version cursor |
| Web clipper | `cairn.sensors.v1.web` | Browser extension → local HTTP receiver |

## Upgrade steps

_To be filled when v0.3 ships._

Expected sequence:

```bash
cairn backup register --vault "$VAULT"
cargo install cairn --version 0.3.0

# Enable federation if needed.
cairn-0.3 config set federation.enabled true
cairn-0.3 share accept <share-link>

# Enable source connectors progressively.
cairn-0.3 admin connector_enable github --token <token>
cairn-0.3 admin connector_backfill github --since 30d
```
```

- [ ] **Step 2: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds for this file.

- [ ] **Step 3: Commit**

```bash
git add docs/site/src/usage/migration/v0.2-to-v0.3.md
git commit -m "docs(migration): scaffold v0.2 → v0.3 guide with §19 deltas"
```

---

## Task 5: Migration guide — v0.3 → v0.4

**Files:**
- Create: `docs/site/src/usage/migration/v0.3-to-v0.4.md`

- [ ] **Step 1: Create the file**

Write `docs/site/src/usage/migration/v0.3-to-v0.4.md` with this content:

```markdown
# v0.3 → v0.4 Migration

**Phase delta:** P2 propagation + collective → P3 evaluation + polish.

## Phase summary

v0.4 is the docs / eval / packaging freeze. No new runtime capabilities ship;
the work is hardening: extended `cairn bench` corpora for research /
engineering / support domains, replay cassettes covering every v0.1-v0.3
capability, coherence gate floors per #137, full docs freeze (this issue),
and beta distribution channels.

## Capability deltas

No new runtime capabilities flip in v0.4 — the
[capability matrix](../../reference/capability-matrix.md) is unchanged from
v0.3. The deltas are in the eval surface, not the runtime surface.

| Eval surface | v0.3 → v0.4 change |
|--------------|---------------------|
| `cairn bench` corpora | adds research / engineering / support domain suites |
| Replay cassettes | extended coverage of every v0.1-v0.3 capability (per #136) |
| Coherence gate floors | enforced per #137 (`coherence run --gate beta` / `--gate rc`) |
| Docs | frozen per this issue (#138) — capability matrix + migration guides + beta readiness runbook all canonical |
| Distribution | beta channels open (`brew install --beta cairn` and `cargo install --version 0.4.0-beta.N`) |

## Config schema deltas

_To be filled when v0.4 ships._

Expected:

- `bench.domain.<name>.enabled` — bool per domain corpus.
- `eval.coherence.gate` — enum: `beta` (default) | `rc`.
- No runtime config additions.

## CLI / MCP / SDK / skill deltas

_To be filled when v0.4 ships._

Expected:

- `cairn bench <domain>` runs a single domain corpus.
- `cairn bench coherence run --gate <beta|rc>` runs the coherence gate per #137.
- No new MCP verbs; no new SDK surfaces; no new capability codes advertised.

## WAL / store deltas

None expected. v0.4 does not introduce new WAL state machines or SQLite
migrations.

## Sensor deltas

None expected. v0.4 carries the v0.3 sensor suite unchanged.

## Upgrade steps

_To be filled when v0.4 ships._

Expected sequence:

```bash
cairn backup register --vault "$VAULT"
cargo install cairn --version 0.4.0-beta.1   # or brew install --beta cairn

# Run the beta-readiness gate on the new install before cutting over.
cairn-0.4-beta bench all
cairn-0.4-beta bench coherence run --gate beta
```

## Notes

- The beta readiness checklist ([maintainers/beta-readiness.md](../../maintainers/beta-readiness.md)) is the maintainer-side gate that decides which `0.4.0-beta.N` build is eligible for the public beta channel.
- Operators upgrading from v0.3 to v0.4 should not see behavioral changes — the upgrade is a packaging and evaluation event.
```

- [ ] **Step 2: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: all three migration guides + index now resolve cleanly; no missing-file warnings.

- [ ] **Step 3: Commit**

```bash
git add docs/site/src/usage/migration/v0.3-to-v0.4.md
git commit -m "docs(migration): scaffold v0.3 → v0.4 guide with §19 deltas"
```

---

## Task 6: Beta readiness runbook

The maintainer-facing canonical checklist. Pairs with the script in task 7 — runbook explains; script automates.

**Files:**
- Create: `docs/site/src/maintainers/beta-readiness.md`
- Modify: `docs/site/src/SUMMARY.md` — add nav entry.

- [ ] **Step 1: Create `docs/site/src/maintainers/beta-readiness.md`**

Write this content:

```markdown
# Beta Readiness

This checklist is the maintainer gate every Cairn build must pass before it
is eligible for the public beta channel. If any item fails, the build is not
beta-eligible.

## Quick start

```bash
# Runs gates 1-8 (automatable). ~3 min.
scripts/beta-readiness.sh --quick

# Runs gates 1-8 plus the long-running eval + package + audit gates. ~15 min.
scripts/beta-readiness.sh --full
```

The script honors `CAIRN_BIN`, `CARGO_TARGET_DIR`, and `RUST_LOG`. Gates 9-14
are manual and listed at the end of the script output.

## Gate categories

### 1. Code quality

| Item | Command |
|------|---------|
| Format | `cargo fmt --all --check` |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Compile | `cargo check --workspace --all-targets --locked` |
| Unit + integration tests | `cargo nextest run --workspace --locked --no-fail-fast` |
| Doc tests | `cargo test --doc --workspace --locked` |

**Pass:** every command exits 0.
**Common failure:** a recent PR added a clippy violation. First place to look: latest diff to the failing crate.

### 2. Core boundary

| Item | Command |
|------|---------|
| Core dep freeness | `scripts/check-core-boundary.sh` |
| No OS locks in core | `scripts/check-no-os-locks.sh` |
| Lint reads readonly sources | `scripts/check-lint-readonly-sources.sh` |

**Pass:** all three exit 0.
**Common failure:** `cairn-core` grew a dependency on an adapter crate. Move the dependency to the adapter and call back via a trait.

### 3. Generated code drift

| Item | Command |
|------|---------|
| Codegen | `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` |
| Docgen | `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` |

**Pass:** both exit 0.
**Common failure:** an IDL or CLI change shipped without re-running codegen / docgen. Re-run with `--write` and commit.

### 4. Eval gates

| Item | Command |
|------|---------|
| Full bench | `cargo run -p cairn-bench --release --locked -- all` |
| Coherence gate | `cargo run -p cairn-bench --release --locked -- coherence run --gate beta` |

**Pass:** all bench scores within floor, no metric regression > 2 % from baseline (forget_completeness: 0 % tolerance per #137).
**Common failure:** a metric dropped > 2 %. Investigate the failing metric; if intentional, update the baseline trend file in a separate commit.

### 5. Docs build

| Item | Command |
|------|---------|
| mdBook | `mdbook build docs/site` |
| Rustdoc + broken-link check | `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked` |

**Pass:** both exit 0.
**Common failure:** a renamed item left a stale intra-doc link. Fix the link target.

### 6. Supply chain

| Item | Command |
|------|---------|
| License + advisory | `cargo deny check` |
| Audit | `cargo audit --deny warnings` |
| Unused deps | `cargo machete` |

**Pass:** all three exit 0.
**Common failure:** a transitive dep ships under a license not in `deny.toml`. Either pin the dep to a compatible version or update the allowlist (maintainer sign-off required).

### 7. Install smoke

| Item | Command |
|------|---------|
| End-to-end CLI smoke | `CAIRN_BIN=target/release/cairn scripts/install-smoke.sh` |

**Pass:** all eight P0 verbs round-trip on a fresh temp vault.
**Common failure:** a verb regressed against the default-off `search.local_embeddings` profile.

### 8. Package dry-run (only when touching publish-affecting metadata)

| Item | Command |
|------|---------|
| Workspace package | `cargo package --workspace --no-verify --locked --allow-dirty` |
| Publish dry-run (idl) | `cargo publish --dry-run --locked --allow-dirty -p cairn-idl` |
| Publish dry-run (core) | `cargo publish --dry-run --locked --allow-dirty -p cairn-core` |

**Pass:** all three exit 0.
**Common failure:** a workspace package added a path-only dep without a version constraint. Add the version per CLAUDE.md §9 publish-order rules.

### 9. Capability sync (manual)

Run `target/release/cairn status --json` and compare the `capabilities[]`
array against the [capability matrix](../reference/capability-matrix.md) row
for the target phase.

**Pass:** the advertised set equals the matrix row exactly. No extras, no omissions.
**Failure:** the runtime advertises a capability the matrix says shouldn't ship yet (or vice versa). Reconcile in `cairn-core::status::advertise` plus the matching `wiring::*_WIRED` constant per CLAUDE.md §4 invariant 6.

### 10. Migration guide review (manual)

Open the per-pair migration guide for the target phase
([usage/migration/](../usage/migration/index.md)). Verify all seven sections
(phase summary, capability deltas, config deltas, CLI/MCP/SDK/skill deltas,
WAL/store deltas, sensor deltas, upgrade steps) are populated for surfaces
that actually ship in the target phase.

**Pass:** no `_To be filled when v0.Y ships._` markers remain for capabilities
the runtime now advertises.
**Failure:** a capability advertised by `cairn status` has no migration
content. Fill the section.

### 11. Known limitations (manual)

Review [status.md](../status.md) "Stubbed or pending" against the current
capability matrix. Anything still stubbed must be either:

- removed from the stubbed list (because it now ships), or
- explicitly called out in the release notes as a known limitation.

### 12. Cassette replay (manual)

```bash
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

**Pass:** all replay cassettes from #136 pass under the beta gate; all five
coherence metrics (per #137) meet their floors.

### 13. Privacy posture (manual)

Exercise the consent + forget round-trip on a real session:

```bash
cairn ingest --kind user --body "test memory"
RECORD=$(cairn search "test memory" --json | jq -r '.hits[0].id')
cairn forget --record "$RECORD"
cairn search "test memory" --json | jq '.hits | length'   # 0
```

Spot-check `.cairn/consent.log` for the `delete` entry. Verify the presidio
scrub pass redacts at least one PII pattern in a known-PII fixture.

### 14. Release notes draft (manual)

Populate the per-phase release notes template. Cross-link every capability
delta to the matching row in the [migration guide](../usage/migration/index.md).

## Failure remediation

| Failed gate | First place to look |
|-------------|----------------------|
| Format | `cargo fmt --all` then commit. |
| Clippy | Latest PR diff to the failing crate. |
| Nextest | `cargo nextest run --workspace --no-capture` for the failing test. |
| Doc tests | The failing doctest's source file; the example may have drifted from the API. |
| Codegen / docgen drift | Re-run with `--write` and commit the result. |
| Bench / coherence | `crates/cairn-bench/baseline/trend.json` for the prior baseline; the failing metric's source data. |
| mdBook | Search for the broken link's target file. |
| Rustdoc | The `[broken_link]` target — usually a renamed item. |
| Deny / audit / machete | `Cargo.lock` for the offending dep; pin or remove. |
| Install smoke | The verb that printed `fail: <verb>` and the temp vault path. |
| Package dry-run | The crate that failed; check its `Cargo.toml` for missing version metadata. |
| Capability sync (gate 9) | `cairn-core::status::advertise` and the `wiring::*_WIRED` constants. |

## Sign-off block

Copy this block into the release issue:

```markdown
## Beta readiness sign-off — v0.X.Y-beta.N

- [ ] Gate 1: code quality
- [ ] Gate 2: core boundary
- [ ] Gate 3: generated code drift
- [ ] Gate 4: eval gates
- [ ] Gate 5: docs build
- [ ] Gate 6: supply chain
- [ ] Gate 7: install smoke
- [ ] Gate 8: package dry-run
- [ ] Gate 9: capability sync (manual)
- [ ] Gate 10: migration guide review (manual)
- [ ] Gate 11: known limitations (manual)
- [ ] Gate 12: cassette replay (manual)
- [ ] Gate 13: privacy posture (manual)
- [ ] Gate 14: release notes draft (manual)

Reviewed by: <maintainer>
Date: <YYYY-MM-DD>
Commit: <sha>
```
```

- [ ] **Step 2: Add SUMMARY.md entry**

Edit `docs/site/src/SUMMARY.md`. Find the line `- [CI](maintainers/ci.md)`. Insert this line immediately after it (still under `# Maintainers`):

```markdown
- [Beta Readiness](maintainers/beta-readiness.md)
```

- [ ] **Step 3: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds, no broken links.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/maintainers/beta-readiness.md docs/site/src/SUMMARY.md
git commit -m "docs(maintainers): add beta readiness runbook"
```

---

## Task 7: Beta readiness script

Bash 3.2-safe. Mirrors `scripts/install-smoke.sh` style. Pairs with the runbook from task 6.

**Files:**
- Create: `scripts/beta-readiness.sh`

- [ ] **Step 1: Check that shellcheck is available**

Run: `command -v shellcheck && shellcheck --version | head -1`
Expected: prints a path + version. If not installed, run `brew install shellcheck` (macOS) or `apt-get install shellcheck` (linux).

- [ ] **Step 2: Create the script**

Write `scripts/beta-readiness.sh` with this content:

```bash
#!/usr/bin/env bash
# scripts/beta-readiness.sh — issue #138 beta readiness gate runner.
#
# Wraps every automatable gate from CLAUDE.md §8 plus the
# docs/site/src/maintainers/beta-readiness.md runbook. Exits 0 only when
# every required gate passes; manual gates (9-14) are listed at the end of
# the run and never claimed as passed.
#
# Modes:
#   --quick (default): fmt, clippy, check, nextest, doc tests,
#     scripts/check-*.sh, codegen --check, docgen --check, deny, machete.
#   --full: --quick + cargo bench all + coherence beta gate + mdbook +
#     rustdoc + audit + install-smoke + cargo package + leaf publish dry-run.
#
# Env passthrough: CAIRN_BIN, CARGO_TARGET_DIR, RUST_LOG.
#
# POSIX-y bash; targets macOS default /bin/bash 3.2 — no associative arrays,
# no `mapfile`. Style follows scripts/install-smoke.sh.

set -euo pipefail

MODE="quick"
case "${1:-}" in
  --quick) MODE="quick" ;;
  --full) MODE="full" ;;
  "") ;;
  -h|--help)
    cat <<'EOF'
Usage: beta-readiness.sh [--quick|--full]

  --quick   (default) ~3 min: fmt, clippy, check, nextest, doctests,
            core-boundary scripts, codegen/docgen --check, deny, machete.
  --full    ~15 min: --quick + bench + coherence gate + mdbook + rustdoc +
            audit + install-smoke + cargo package + leaf publish dry-run.

Honors CAIRN_BIN, CARGO_TARGET_DIR, RUST_LOG.

Manual gates (9-14 in docs/site/src/maintainers/beta-readiness.md) are
listed after the automated run and never claimed as passed.
EOF
    exit 0
    ;;
  *)
    echo "fail: unknown arg '$1'. Try --help." >&2
    exit 2
    ;;
esac

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OK=0
FAIL=0
SKIP=0
FIRST_FAIL=""

# `gate <name> <command>` runs the command, logs ok/fail, increments counters.
# A failure short-circuits later gates via set -e; we capture the first
# failure name to print at the end before exiting.
gate() {
  name="$1"
  shift
  printf 'run: %s\n' "$name"
  if "$@" >/tmp/cairn-beta-readiness.$$.log 2>&1; then
    OK=$((OK + 1))
    printf 'ok: %s\n' "$name"
  else
    FAIL=$((FAIL + 1))
    if [ -z "$FIRST_FAIL" ]; then
      FIRST_FAIL="$name"
    fi
    printf 'fail: %s\n' "$name" >&2
    sed 's/^/    /' /tmp/cairn-beta-readiness.$$.log >&2
    rm -f /tmp/cairn-beta-readiness.$$.log
    print_summary
    exit 1
  fi
  rm -f /tmp/cairn-beta-readiness.$$.log
}

# `gate_optional <name> <required-binary> <command>` skips when the binary is
# missing. Prints a remediation hint.
gate_optional() {
  name="$1"
  bin="$2"
  shift 2
  if ! command -v "$bin" >/dev/null 2>&1; then
    SKIP=$((SKIP + 1))
    printf 'skip: %s — install %s\n' "$name" "$bin"
    return 0
  fi
  gate "$name" "$@"
}

print_summary() {
  printf -- '---\n'
  printf 'beta-readiness: %d ok, %d fail, %d skip\n' "$OK" "$FAIL" "$SKIP"
  if [ -n "$FIRST_FAIL" ]; then
    printf 'first failure: %s\n' "$FIRST_FAIL"
  fi
  print_manual_gates
}

print_manual_gates() {
  cat <<'EOF'
manual gates remaining (see docs/site/src/maintainers/beta-readiness.md):
  - 9: capability sync (cairn status --json vs reference/capability-matrix.md)
  - 10: migration guide review (usage/migration/v0.X-to-v0.Y.md)
  - 11: known limitations (status.md vs capability matrix)
  - 12: cassette replay (cargo run -p cairn-bench -- coherence run --gate beta)
  - 13: privacy posture (forget round-trip + presidio scrub)
  - 14: release notes draft
EOF
}

# ---- Gate 1: code quality ----
gate "fmt" cargo fmt --all --check
gate "clippy" cargo clippy --workspace --all-targets --locked -- -D warnings
gate "check" cargo check --workspace --all-targets --locked
gate_optional "nextest" cargo-nextest cargo nextest run --workspace --locked --no-fail-fast
gate "doc-tests" cargo test --doc --workspace --locked

# ---- Gate 2: core boundary ----
gate "check-core-boundary" scripts/check-core-boundary.sh
gate "check-no-os-locks" scripts/check-no-os-locks.sh
gate "check-lint-readonly-sources" scripts/check-lint-readonly-sources.sh

# ---- Gate 3: generated code drift ----
gate "codegen --check" cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
gate "docgen --check" cargo run -p cairn-cli --bin cairn-docgen --locked -- --check

# ---- Gate 6 (partial — fast): supply chain that's quick ----
gate_optional "cargo deny" cargo-deny cargo deny check
gate_optional "cargo machete" cargo-machete cargo machete

if [ "$MODE" = "quick" ]; then
  print_summary
  exit 0
fi

# ---- Gate 4: eval gates (full only) ----
gate "bench all" cargo run -p cairn-bench --release --locked -- all
gate "coherence beta gate" cargo run -p cairn-bench --release --locked -- coherence run --gate beta

# ---- Gate 5: docs build (full only) ----
gate_optional "mdbook" mdbook mdbook build docs/site
gate "rustdoc" env RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked

# ---- Gate 6 (rest, slower) ----
gate_optional "cargo audit" cargo-audit cargo audit --deny warnings

# ---- Gate 7: install smoke (full only) ----
# Builds the release binary if not already present, then runs the smoke
# script against it.
gate "release build" cargo build --release --locked -p cairn-cli --bin cairn
gate "install smoke" env CAIRN_BIN="$REPO_ROOT/target/release/cairn" scripts/install-smoke.sh

# ---- Gate 8: package dry-run (full only) ----
gate "cargo package" cargo package --workspace --no-verify --locked --allow-dirty
gate "publish dry-run (cairn-idl)" cargo publish --dry-run --locked --allow-dirty -p cairn-idl
gate "publish dry-run (cairn-core)" cargo publish --dry-run --locked --allow-dirty -p cairn-core

print_summary
exit 0
```

- [ ] **Step 3: Make the script executable**

Run: `chmod +x scripts/beta-readiness.sh`

- [ ] **Step 4: Lint the script**

Run: `shellcheck scripts/beta-readiness.sh`
Expected: no output (clean). If shellcheck flags an issue, fix it inline and re-run.

- [ ] **Step 5: Verify the help text and arg parsing**

Run: `scripts/beta-readiness.sh --help`
Expected: prints the usage block; exits 0.

Run: `scripts/beta-readiness.sh bogus 2>&1; echo "exit=$?"`
Expected: prints `fail: unknown arg 'bogus'. Try --help.` and `exit=2`.

- [ ] **Step 6: Dogfood — quick mode on this branch**

Run: `scripts/beta-readiness.sh --quick 2>&1 | tail -30`
Expected: prints `ok:` for each gate; exits 0 with `beta-readiness: N ok, 0 fail, M skip` and the manual-gates list.

If a gate fails, fix the underlying issue in the repo (not the script), then re-run.

- [ ] **Step 7: Commit**

```bash
git add scripts/beta-readiness.sh
git commit -m "feat(scripts): add beta-readiness.sh gate runner (issue #138)"
```

---

## Task 8: Audit pass — `status.md`, `index.md`, `quickstart.md`

Refreshes the three top-level pages against HEAD.

**Files:**
- Modify: `docs/site/src/status.md`
- Modify: `docs/site/src/index.md`
- Modify: `docs/site/src/quickstart.md`

- [ ] **Step 1: Establish ground truth from the CLI**

Run: `cargo run -p cairn-cli --locked -- --help 2>&1 | head -40`
Run: `cargo run -p cairn-cli --locked -- status --json 2>&1 | head -40` (if the binary is buildable; if `status` requires a vault, skip and rely on `--help` instead)

Record:
- The list of top-level subcommands that are wired (= "Implemented" candidates).
- Anything visibly absent that was on the old `status.md` "Implemented" list (= candidates for "Stubbed or pending").

- [ ] **Step 2: Read current `status.md`**

Run: `cat docs/site/src/status.md`

- [ ] **Step 3: Update `status.md` "Implemented" / "Stubbed or pending" lists**

Edit `docs/site/src/status.md`. Reconcile the bullet lists with what step 1 showed. Concretely:

- Move any verb that now dispatches (not stubbed) from "Stubbed or pending" to "Implemented".
- Add a "See [Capability Matrix](reference/capability-matrix.md) for what ships in each phase." line under the heading.
- Keep the GitHub query links at the bottom intact.

Insert this line under the page title `# Status`, immediately above the `Cairn is pre-v0.1.` paragraph:

```markdown
> See the [capability matrix](reference/capability-matrix.md) for the
> authoritative per-phase capability list.
```

- [ ] **Step 4: Update `index.md` to cross-link the capability matrix**

Read `docs/site/src/index.md`. Add a "Phase scope" section right above the existing first navigation block (or near the top — wherever the existing TL;DR ends). The section reads:

```markdown
## Phase scope

The [capability matrix](reference/capability-matrix.md) is the authoritative
view of which capability ships in which Cairn release. Concept and usage
pages link into it rather than restating phase claims.
```

- [ ] **Step 5: Update `quickstart.md` for capability-matrix link**

Read `docs/site/src/quickstart.md`. Add a single line near the bottom (just before any "Next steps" section, or as the final line if none exists):

```markdown
For what each release supports, see the [capability matrix](reference/capability-matrix.md).
```

- [ ] **Step 6: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds.

- [ ] **Step 7: Commit**

```bash
git add docs/site/src/status.md docs/site/src/index.md docs/site/src/quickstart.md
git commit -m "docs: align top-level pages with capability matrix (issue #138)"
```

---

## Task 9: Audit pass — `concepts/*` (3 pages)

Cross-link the matrix and fix any drift in claims.

**Files:**
- Modify: `docs/site/src/concepts/architecture.md`
- Modify: `docs/site/src/concepts/vault-layout.md`
- Modify: `docs/site/src/concepts/capability-model.md`

- [ ] **Step 1: Read each page and note drift**

Run for each file:
- `cat docs/site/src/concepts/architecture.md`
- `cat docs/site/src/concepts/vault-layout.md`
- `cat docs/site/src/concepts/capability-model.md`

For each: identify any claim that
- (a) names a capability not in the capability matrix for the page's stated phase, or
- (b) names a config key not present in `docs/site/src/reference/generated/config-defaults.md`, or
- (c) names a CLI flag not in `docs/site/src/reference/generated/cli.md`.

Record findings as `- <page>: <finding> → <fix>` in a scratch note for the PR description.

- [ ] **Step 2: Append a "See also" line to each concept page**

For each of the three files, add this exact line as the new last line (before any trailing blank line):

```markdown
See the [capability matrix](../reference/capability-matrix.md) for which capabilities ship in which release.
```

- [ ] **Step 3: Apply any drift fixes identified in step 1**

If a concept page named a flag / config key / capability that doesn't exist on HEAD: edit the page to match HEAD. If the fix is larger than a paragraph, leave a `> [!NOTE]` callout pointing at a follow-up issue and file that issue (`gh issue create --title "docs(concepts): <page> drift — <one-line>" --body "<details>"`). Record the issue URL in the PR description.

- [ ] **Step 4: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds.

- [ ] **Step 5: Commit**

```bash
git add docs/site/src/concepts/architecture.md docs/site/src/concepts/vault-layout.md docs/site/src/concepts/capability-model.md
git commit -m "docs(concepts): cross-link capability matrix + fix drift (issue #138)"
```

---

## Task 10: Audit pass — `usage/*` (10 pages, excluding migration subdir)

**Files:**
- Modify: `docs/site/src/usage/installation.md`, `cli.md`, `config.md`, `plugins.md`, `mcp.md`, `claude-code.md`, `codex.md`, `skill.md`, `backup.md`, `claude-code-reference.md`.

- [ ] **Step 1: Establish reference ground truth**

For each, keep handy:
- CLI flags: `docs/site/src/reference/generated/cli.md` and `commands/*.md`
- Config keys: `docs/site/src/reference/generated/config-defaults.md`
- MCP tools: `docs/site/src/reference/generated/mcp-tools.md`
- Bundled plugins: `docs/site/src/reference/generated/plugins.md`

- [ ] **Step 2: For each of the 10 pages — read, identify drift, fix inline**

For each `docs/site/src/usage/<name>.md`:

1. `cat docs/site/src/usage/<name>.md`.
2. Cross-check every command, flag, config key, capability code, and MCP tool name against the generated references.
3. Fix any drift inline. If too large, leave a `> [!NOTE]` callout + file a follow-up issue.
4. If the page lacks a link to the capability matrix, add this line at the bottom: `See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.`

Record findings as `- usage/<name>.md: <finding> → <fix>` in the PR description scratch note.

- [ ] **Step 3: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/usage/installation.md docs/site/src/usage/cli.md docs/site/src/usage/config.md docs/site/src/usage/plugins.md docs/site/src/usage/mcp.md docs/site/src/usage/claude-code.md docs/site/src/usage/codex.md docs/site/src/usage/skill.md docs/site/src/usage/backup.md docs/site/src/usage/claude-code-reference.md
git commit -m "docs(usage): align with capability matrix + fix drift (issue #138)"
```

---

## Task 11: Audit pass — hand-written `reference/*` + `maintainers/*`

**Files:**
- Modify: `docs/site/src/reference/rust-api.md`, `policy-gates.md`, `idl.md`, `bench/index.md`.
- Modify: `docs/site/src/maintainers/codegen.md`, `docs.md`, `ci.md`.

- [ ] **Step 1: Read each page and identify drift**

For each:
- `docs/site/src/reference/rust-api.md`
- `docs/site/src/reference/policy-gates.md`
- `docs/site/src/reference/idl.md`
- `docs/site/src/reference/bench/index.md`
- `docs/site/src/maintainers/codegen.md`
- `docs/site/src/maintainers/docs.md`
- `docs/site/src/maintainers/ci.md`

Cross-check claims against HEAD code paths. For `policy-gates.md` and
`bench/index.md`, confirm the gate names match `cargo run -p cairn-bench --
help` output. For `idl.md`, confirm the schema layout matches
`crates/cairn-idl/schema/` directory listing. For `maintainers/ci.md`,
confirm the CI job list matches `.github/workflows/*.yml`.

- [ ] **Step 2: Fix drift inline; cross-link the matrix where appropriate**

Apply fixes. For pages that talk about phase-scoped behavior (`policy-gates.md`, `bench/index.md`), add the standard footer:

```markdown
See the [capability matrix](capability-matrix.md) for what ships in each release.
```

(Adjust the relative path: `../reference/capability-matrix.md` from `maintainers/*` files.)

- [ ] **Step 3: Build mdbook**

Run: `mdbook build docs/site 2>&1 | tail -20`
Expected: succeeds.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/reference/rust-api.md docs/site/src/reference/policy-gates.md docs/site/src/reference/idl.md docs/site/src/reference/bench/index.md docs/site/src/maintainers/codegen.md docs/site/src/maintainers/docs.md docs/site/src/maintainers/ci.md
git commit -m "docs(reference,maintainers): align with capability matrix + fix drift (issue #138)"
```

---

## Task 12: Traceability map

**Files:**
- Modify: `docs/design/traceability.md`

- [ ] **Step 1: Read the file and find the canonical row format**

Run: `head -60 docs/design/traceability.md`

Note: the file maps brief sections to issue numbers. The new row covers
brief §19 (v0.4 docs freeze) + §18.b (consumer blueprint) → issue #138.

- [ ] **Step 2: Add the row**

Edit `docs/design/traceability.md`. Find the appropriate table (likely the
"§19" or "Sequencing / v0.4" row). Add a row using the file's existing
format:

```markdown
| §19 v0.4 docs freeze · §18.b consumer blueprint | #138 — docs freeze, migration guides, beta readiness | landed |
```

If the existing format differs, mirror it exactly — column count, capitalization, status verbiage.

- [ ] **Step 3: Commit**

```bash
git add docs/design/traceability.md
git commit -m "docs(traceability): add #138 docs freeze row"
```

---

## Task 13: Final verification — CLAUDE.md §8 full checklist

Runs every required gate. No commit unless a fix is needed; this is the
pre-push verification.

- [ ] **Step 1: Format + lint + compile**

Run in parallel where possible:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
```

Expected: all exit 0. If any fail, fix in a separate commit (probably touched a Rust file inadvertently — should not happen on a docs-only branch).

- [ ] **Step 2: Tests**

```bash
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
```

Expected: all pass. Doctests sometimes break if a doc example was edited — unlikely on this PR but possible.

- [ ] **Step 3: Boundary + drift gates**

```bash
scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Expected: all exit 0. If `docgen --check` fails, re-run with `--write` and commit the regenerated pages.

- [ ] **Step 4: Eval gates**

```bash
cargo run -p cairn-bench --release --locked -- all
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

Expected: all pass.

- [ ] **Step 5: Docs build**

```bash
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: both exit 0. If mdbook reports a broken intra-doc link from the
audit pass, fix it inline and commit.

- [ ] **Step 6: Supply chain**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: all exit 0 (if installed).

- [ ] **Step 7: Dogfood the script we shipped**

```bash
scripts/beta-readiness.sh --quick
```

Expected: exits 0 with the manual-gates list at the end.

- [ ] **Step 8: Spot-check the rendered site**

Run: `mdbook serve docs/site -p 3001 &`
Then in a browser open `http://localhost:3001`:
- Navigate to `Reference → Capability Matrix`. Verify the table renders + cross-links work.
- Navigate to `Usage → Migration Guides → v0.1 → v0.2`. Verify the structure.
- Navigate to `Maintainers → Beta Readiness`. Verify the gates table.
- Click at least three intra-doc links to verify they resolve.
- Kill the server: `kill %1`.

- [ ] **Step 9: No commit (verification only)**

If every gate passes, proceed to task 14. If any gate failed and was fixed, the fix was committed in its own commit during the gate step.

---

## Task 14: Open PR

- [ ] **Step 1: Sync the branch**

Run: `git status` — verify clean working tree, on `worktree-cosmic-skipping-pebble`.
Run: `git log --oneline main..HEAD` — verify all the commits from this plan are present.

- [ ] **Step 2: Push the branch**

```bash
git push -u origin worktree-cosmic-skipping-pebble
```

- [ ] **Step 3: Open the PR**

```bash
gh pr create --title "docs: v0.4 freeze — capability matrix, migration guides, beta readiness (#138)" --body "$(cat <<'EOF'
## Summary

Closes #138.

- Adds `docs/site/src/reference/capability-matrix.md` as the single source of truth mirroring brief §18.c. Other pages link into it instead of restating phase claims.
- Adds the migration guide framework: `docs/site/src/usage/migration/index.md` (upgrade contract + dual-run pattern) plus three per-pair scaffolds (`v0.1→v0.2`, `v0.2→v0.3`, `v0.3→v0.4`) with concrete §19 deltas pinned and unimplemented sections marked `_To be filled when v0.Y ships._`
- Adds the beta readiness gate: `docs/site/src/maintainers/beta-readiness.md` (canonical runbook for gates 1-14) plus `scripts/beta-readiness.sh` (bash 3.2-safe runner that wraps every gate from CLAUDE.md §8 and lists the six manual gates separately).
- Audit pass over every hand-written page under `docs/site/src/` — drift fixed inline; large fixes filed as follow-up issues with `> [!NOTE]` callouts.

**Brief sections touched:** §19 v0.4 docs freeze, §18.b consumer blueprint, §18.c capability matrix, §15 evaluation (eval gates in the runbook).

**Invariants touched:** none — docs + bash only. Capability matrix page explicitly notes that advertisement decisions remain in `cairn-core::status::advertise` per CLAUDE.md §4 invariant 6.

**Out of scope (called out explicitly in the spec):**
- CI lint that fails the build when §8, §18.c, and §19 disagree — separate follow-up issue (brief §18.c flags this).
- Per-domain consumer templates from §18.b.
- Marketing-site polish (v1.0 work).

**Spec:** `docs/superpowers/specs/2026-05-25-issue-138-docs-freeze-design.md`
**Plan:** `docs/superpowers/plans/2026-05-25-issue-138-docs-freeze.md`

## Verification

CLAUDE.md §8 checklist run on this branch; all gates green:

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --locked -- -D warnings` ✅
- `cargo check --workspace --all-targets --locked` ✅
- `cargo nextest run --workspace --locked --no-fail-fast` ✅
- `cargo test --doc --workspace --locked` ✅
- `scripts/check-core-boundary.sh` ✅
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` ✅
- `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` ✅
- `cargo run -p cairn-bench --release --locked -- all` ✅
- `cargo run -p cairn-bench --release --locked -- coherence run --gate beta` ✅
- `mdbook build docs/site` ✅
- `RUSTDOCFLAGS="..." cargo doc --workspace --no-deps ...` ✅
- `cargo deny check` / `cargo audit --deny warnings` / `cargo machete` ✅
- `scripts/beta-readiness.sh --quick` ✅ (dogfood)
- `shellcheck scripts/beta-readiness.sh` ✅

## Test plan

- [ ] `mdbook serve docs/site` — click through new pages; verify cross-links and the capability-matrix table render.
- [ ] `scripts/beta-readiness.sh --quick` — script exits 0; manual gates listed at end.
- [ ] `scripts/beta-readiness.sh --full` (long-running) — script exits 0 on a clean release build.
EOF
)"
```

- [ ] **Step 4: Record the PR URL**

The `gh pr create` command prints the PR URL. Save it; report it to the user.

---

## Notes for the executor

- **Caveman mode** is active for this session per the SessionStart hook. Use terse status messages between tasks; the runbook and migration content goes into the doc files in full prose (the doc readers are not in caveman mode).
- **No Rust changes expected.** If a task requires editing a Rust file, stop and surface the situation — it indicates a drift the audit caught that needs design discussion, not a quiet fix.
- **Audit findings are recorded in the PR description as you go**, not as separate commits. The audit-pass commits (tasks 8-11) carry the fixes; the findings narrative belongs in the PR.
- **Follow-up issues** for large drift fixes use the title pattern `docs(<area>): <page> drift — <one-line>` and link to the discovery point with a permalink. Add the issue numbers to the PR description.
