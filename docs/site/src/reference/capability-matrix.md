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

> **Source of truth.** Buckets below mirror
> `crates/cairn-core/tests/capability_matrix_v1.rs::EXPECTED_PHASE`.
> Gate 9 in the [beta-readiness checklist](../maintainers/beta-readiness.md)
> compares `cairn status --json` against the **Default-advertised**
> bucket plus any **non-default platform** entries the operator
> intentionally enabled — never against the deferred-wiring or
> later-phase rows.

### Default-advertised at v0.1 (case A in `advertise()`)

These are the codes `cairn status` emits on a default-P0 binary with
full gates ON. Stability: frozen v1.0.

| Capability code |
|-----------------|
| `cairn.mcp.v1.search.keyword` |
| `cairn.mcp.v1.search.semantic` |
| `cairn.mcp.v1.search.hybrid` |
| `cairn.mcp.v1.retrieve.session` |
| `cairn.mcp.v1.retrieve.turn` |
| `cairn.mcp.v1.retrieve.tool_call` |
| `cairn.mcp.v1.forget.record` |
| `cairn.mcp.v1.policy_trace` |
| `cairn.workflows.v1.consolidation` |
| `cairn.workflows.v1.dream` |
| `cairn.workflows.v1.expiration` |
| `cairn.workflows.v1.evaluation` |
| `cairn.sensor.v1.screen.xcap` |
| `cairn.sensor.v1.screen.ocr.tesseract` |

### Phase-gated under v0.2

Advertised once the runtime reports `contract_phase: V0_2`. Stability:
frozen v1.0 (identifier reserved at v0.1).

| Capability code |
|-----------------|
| `cairn.mcp.v1.summarize.narrative` |
| `cairn.mcp.v1.forget.session` |
| `cairn.mcp.v1.extension.aggregate` |

### Phase-gated under v0.3

Advertised once the runtime reports `contract_phase: V0_3`. Stability:
frozen v1.0 (identifier reserved at v0.1).

| Capability code |
|-----------------|
| `cairn.mcp.v1.forget.scope` |
| `cairn.mcp.v1.extension.federation` |
| `cairn.mcp.v1.extension.sessiontree` |
| `cairn.mcp.v1.extension.coord` |

### Deferred wiring (reserved v1; not advertised today)

Identifier is frozen — the name belongs to v1 and can't be reassigned.
The dispatch path is not yet wired, so `advertise()` does **not** emit
these codes; Gate 9 must not expect to see them.

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
