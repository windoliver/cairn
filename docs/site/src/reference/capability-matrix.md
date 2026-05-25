# Capability Matrix

> **Single source of truth.** Other docs link here. When this page and brief
> [§18.c](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md) disagree, the brief wins —
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
| `search` modes | keyword (FTS5) + semantic (`sqlite-vec` + local `candle`) + hybrid (local blend); droppable to keyword-only via `search.local_embeddings: false` (others rejected with `CapabilityUnavailable`) | adds BM25S lexical scoring + swappable cloud embedding provider via `litellm`; `semantic_degraded=true` only on transient provider outages | adds `cairn.federation.v1` cross-tenant queries via Nexus full hub |
| Session reload | active-session (US2 core) | + cold-storage rehydration (US6) | unchanged |
| `forget` modes | `record` (US8 core) | + `session` fan-out with drain fences | + `scope` mode |
| `ConsolidationWorkflow` | rolling-summary pass only (US4 core) | + Reflection / REM / Deep tiers | + EvolutionWorkflow mutations |
| SRE observability (OTel dashboards, tier-migration metrics, rehydration gates) | basic lint + health | full SRE surface | unchanged |
| Extension namespaces | `cairn.admin.v1` (operator verbs) | + `cairn.aggregate.v1` (anonymized agent insights) | + `cairn.federation.v1` (share / accept / revoke — folder-scoped via `subject.path_prefix`) + `cairn.sessiontree.v1` (fork / clone / switch / merge — §5.7) |
| Sensors | hooks + IDE + terminal + clipboard + voice (sherpa-onnx + cpal) + screen (`xcap` + OS-native OCR, off by default per [ADR 0003](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0003-screen-sensor-packaging.md)) + neuroskill + recording-to-text batch pipeline | unchanged | + GitHub (issues / PRs / commits) + email (IMAP + webhook) + Drive (Google / OneDrive) + Notion + generic web-clipper extension |

## Capability codes

Every row above maps to one or more `cairn.mcp.v1.*` capability codes
advertised in `cairn status`. Clients MUST inspect `status.capabilities`
before issuing a mode; the runtime fails closed with `CapabilityUnavailable`
on any un-advertised code (brief §8.0.a).

> **Source of truth.** Each bucket below has a runtime gate. The
> snapshot files under
> `crates/cairn-cli/tests/snapshots/status_snapshot_insta__*.snap`
> are the byte-exact reference for each deployment profile; the
> `EXPECTED_PHASE` map in
> `crates/cairn-core/tests/capability_matrix_v1.rs` is the per-phase
> reachability spec. Gate 9 in the
> [beta-readiness checklist](../maintainers/beta-readiness.md) compares
> `cairn status --json` against the snapshot that matches the actual
> deployment profile, not against a one-size-fits-all bucket.

### Out-of-the-box default (`status_snapshot_insta__default_p0_bound_vault.snap`)

What a freshly-bootstrapped P0 vault emits — no embedding model fetched
yet, single-tenant off, no opt-in sensors. Stability: frozen v1.0.

| Capability code |
|-----------------|
| `cairn.mcp.v1.search.keyword` |
| `cairn.mcp.v1.policy_trace` |
| `cairn.mcp.v1.forget.record` |
| `cairn.mcp.v1.forget.session` |
| `cairn.mcp.v1.retrieve.session` |
| `cairn.mcp.v1.retrieve.turn` |
| `cairn.mcp.v1.retrieve.tool_call` |
| `cairn.sensor.v1.screen.xcap` |
| `cairn.sensor.v1.screen.ocr.tesseract` |

### Opt-in: semantic + hybrid search (model loaded)

Added when `search.local_embeddings: true` (default) **and** the
embedding model is on disk **and** `sqlite-vec` reports `vector: true`.

| Capability code | Gated by |
|-----------------|----------|
| `cairn.mcp.v1.search.semantic` | `config.semantic_search` + `model_present` + `embedding_provider_ready` + `store.vector` |
| `cairn.mcp.v1.search.hybrid` | same as above (drops with either pillar) |

### Opt-in: workflows (single-tenant + runtime ready)

Added when the deployment is single-tenant and the workflow runtimes
report ready. Stability: frozen v1.0.

| Capability code | Gated by |
|-----------------|----------|
| `cairn.workflows.v1.consolidation` | `consolidation_runtime_ready` |
| `cairn.workflows.v1.dream` | `llm_configured` + `dream_runtime_ready` |
| `cairn.workflows.v1.expiration` | `expiration_runtime_ready` |
| `cairn.workflows.v1.evaluation` | `evaluation_runtime_ready` |

### Phase-gated under v0.2

Advertised once the runtime reports `contract_phase: V0_2` (gated on
`llm_configured` and `FORGET_SESSION_WIRED` per
`capability_matrix_v1::phase_v02_adds_summarize_narrative_and_forget_session`).
Stability: frozen v1.0 (identifier reserved at v0.1).

| Capability code |
|-----------------|
| `cairn.mcp.v1.summarize.narrative` |
| `cairn.mcp.v1.forget.session` |

### Phase-gated under v0.3 (today: empty)

At `Phase::V0_3`, today's `advertise()` returns the same set as `V0_2`
(per `capability_matrix_v1::phase_v03_matches_v02_until_v03_wiring_lands`):
`FORGET_SCOPE_WIRED=false` and every `COORD_*_WIRED=false`. The reserved
codes below move out of the deferred-wiring bucket when the matching
wiring constant flips.

| Capability code | Held back by |
|-----------------|--------------|
| `cairn.mcp.v1.forget.scope` | `FORGET_SCOPE_WIRED=false` |
| `cairn.mcp.v1.extension.coord` | `coord_extension_ready()=false` |

### Deferred wiring (reserved v1; not advertised today)

Identifier is frozen — the name belongs to v1 and can't be reassigned.
The dispatch path is not yet wired, so `advertise()` does **not** emit
these codes at any phase; Gate 9 must not expect to see them.

| Capability code |
|-----------------|
| `cairn.mcp.v1.retrieve.record` |
| `cairn.mcp.v1.retrieve.folder` |
| `cairn.mcp.v1.retrieve.scope` |
| `cairn.mcp.v1.retrieve.profile` |
| `cairn.mcp.v1.sensors.pre_compact` |
| `cairn.mcp.v1.replay.sequence` |
| `cairn.mcp.v1.replay.challenge` |
| `cairn.mcp.v1.extension.admin` |
| `cairn.mcp.v1.extension.aggregate` |
| `cairn.mcp.v1.extension.federation` |
| `cairn.mcp.v1.extension.sessiontree` |
| `cairn.sensor.v1.screen.ocr.vision` |

### Non-default platform (cfg / feature / OS-gated)

Advertised only when the operator opts in (config or build feature)
and the host OS supports the producer. Stability: reserved v1; the
sensors namespace itself is independent.

| Capability code | Gated by |
|-----------------|----------|
| `cairn.sensor.v1.screen.screenpipe` | `--features screenpipe-runtime` + `sensors.screen.backend: screenpipe` |
| `cairn.sensor.v1.screen.ocr.winrt` | Windows |

Stability tiers and the freeze rules are governed by
[ADR 0004 — `cairn.mcp.v1` semver freeze](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md).
See [MCP Semver Policy](../maintainers/mcp-semver-policy.md) for the
operator-facing summary.

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

- Brief sections: [§8.0.a](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md) capability codes, [§15](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md) evaluation gates, [§18.c](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md) coverage summary, [§19](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md) sequencing.
- Site pages: [Capability Model](../concepts/capability-model.md), [Migration Guides](../usage/migration/index.md), [Beta Readiness](../maintainers/beta-readiness.md).
