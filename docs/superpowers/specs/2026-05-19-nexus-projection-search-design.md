# Nexus Projection Search Design - Issue #105

**Date:** 2026-05-19
**Issue:** [#105 - Add BM25S lexical projection and richer parser projections](https://github.com/windoliver/cairn/issues/105)
**Brief sections:** section 3.0 Storage topology; section 8.0 search and lint verbs; section 19 v0.2 richer search backends
**Status:** Approved direction

## 1. Scope

Implement the full issue #105 scope on top of the Nexus sandbox sidecar from #104.
The feature adds rebuildable P1 projection behavior without changing the authority
model: `.cairn/cairn.db` remains the only source of truth for records, frontmatter,
edges, WAL state, and consent state.

This PR adds:

- A Cairn-side projection ledger and rebuild controller for Nexus sandbox derived
  indexes.
- BM25S lexical projection as an additional search ranking signal when Nexus is
  configured, healthy, and current for the relevant record hashes.
- Rich parser projections for configured PDF, DOCX, video-frame, and vision-caption
  source inputs.
- Projection lag, rebuild, and parser-failure reporting in `status` and `lint`.
- Tests proving rebuilds are derived-only and parser failures remain scoped to the
  projection layer.

Out of scope: Nexus full hub federation, shared search hubs, replacing SQLite as the
record store authority, and creating authoritative records from parser output. Parser
output remains projection data unless a separate ingest workflow explicitly promotes it.

## 2. Architecture

The design keeps existing ownership boundaries.

| Layer | Location | Responsibility |
|---|---|---|
| Projection domain | `cairn-core::domain::projection` | Pure structs for projection targets, cursors, item state, parser state, lag, and rebuild summaries. |
| Store/search contract | `cairn-core::contract::memory_store` | Search request/result types and optional projection capability declarations needed by all surfaces. |
| SQLite authority adapter | `cairn-store-sqlite` | Read authoritative records, hashes, source references, FTS candidates, and projection ledger rows from `.cairn/cairn.db`. |
| Nexus projection adapter | `cairn-cli::nexus` initially, promotable to `cairn-store-nexus` when adapter crates are split | HTTP/MCP calls to the Nexus sandbox sidecar; never opens `nexus-data/` directly. |
| CLI verbs | `cairn-cli::verbs::{search,lint,status}` | Dispatch search, rebuild, lag diagnostics, and human/JSON output. |
| IDL/codegen | `cairn-idl` | Wire schema changes for search diagnostics, status projection health, and lint findings. |

The implementation includes the minimum store/search substrate that #105 needs. If the
target branch already has broader `MemoryStore` CRUD/search methods, this work extends
those methods rather than adding a parallel path.

## 3. Projection Model

Projection state is derived state plus control metadata. It may record what was applied,
when, and with which source hashes, but it must not mutate authoritative record rows.

Core projection types:

```rust
pub enum ProjectionTarget {
    Bm25sLexical,
    Parser(ParserProjectionKind),
}

pub enum ParserProjectionKind {
    PdfText,
    DocxText,
    VideoFrameText,
    VisionCaption,
}

pub struct ProjectionCursor {
    pub record_id: RecordId,
    pub wal_sequence: u64,
    pub record_hash: String,
    pub source_hash: Option<String>,
}

pub enum ProjectionItemState {
    Current,
    Stale,
    Failed { reason: String },
    Missing,
}
```

The SQLite authority layer stores projection ledger rows keyed by
`(projection_target, record_id, record_hash, source_hash)`. A ledger row means "this
record hash has been attempted for this projection"; it does not mean the projected
content is authoritative. The sidecar owns the physical BM25S and parser indexes under
`nexus-data/`.

Lag is computed by comparing current record/source hashes from the authoritative tables
with the latest successful ledger rows. If hashes differ, the projection is stale even
when the sidecar is reachable.

## 4. Rebuild Flow

`cairn reindex --from-db` is the operator surface for a full projection rebuild. It is a
management command, not a ninth core MCP verb. Existing core verbs stay unchanged; MCP and
SDK callers observe projection state through `status`, `lint`, and `search` outputs.

Rebuild steps:

1. Load config and require `store.kind: nexus-sandbox`.
2. Probe the sidecar through the #104 health boundary.
3. Enumerate authoritative records, body hashes, source references, and source hashes
   from `.cairn/cairn.db`.
4. Send rebuild batches to the sidecar over the narrow Cairn-facing Nexus projection API.
5. The sidecar updates BM25S/parser projection state under `nexus-data/`.
6. Cairn records successful or failed item states in the projection ledger without
   modifying record bodies, frontmatter, edges, or WAL rows.
7. Re-running the rebuild with the same hashes is idempotent.

The projection API is batch-oriented and hash-keyed:

```json
{
  "operation_id": "01H...",
  "target": "bm25s_lexical",
  "items": [
    {
      "record_id": "01H...",
      "wal_sequence": 42,
      "record_hash": "sha256:...",
      "body": "...",
      "source_path": "sources/example.pdf",
      "source_hash": "sha256:..."
    }
  ]
}
```

The sidecar response returns per-item state. Partial failures are committed as projection
failures and surfaced by `lint`; they do not fail or roll back authoritative records.

## 5. Parser Projections

Parser projections are configured under the Nexus profile. Defaults are conservative:
PDF and DOCX text extraction are enabled when the sidecar advertises support; video frame
text and vision captions require explicit config because they can be compute- or
provider-heavy.

Parser outputs are stored only in Nexus projection state and addressed by source hash.
They can contribute searchable text and snippets, but they are not written back as
`MemoryRecord` bodies. This keeps parser bugs and provider outages from corrupting
authority.

Parser failure semantics:

- Unsupported file type: skipped with a `Missing` or `Unsupported` projection finding.
- Parser crash or malformed source: `Failed { reason }` for that source hash.
- Vision provider unavailable: failed parser projection only; search falls back to
  available non-vision signals unless the caller explicitly requires that parser target.
- Source hash changed: old projection is stale and ignored until rebuilt.

Fixtures cover representative PDF, DOCX, video-frame metadata, and vision-caption inputs.
The default CI path uses deterministic mock sidecar responses. Optional real-sidecar tests
can run behind an environment variable, but they are not required for normal workspace CI.

## 6. Search Integration

BM25S is an additional ranking signal, not a replacement search mode. The baseline search
contract remains the brief section 8.0 shape: `keyword`, `semantic`, and `hybrid` are
capability-gated modes. BM25S participates only when the Nexus sandbox projection is
healthy and current for returned record hashes.

Search flow for `keyword`:

1. Query SQLite FTS5 for the authoritative candidate set.
2. If Nexus BM25S is healthy and current, request BM25S scores for the query and candidate
   record hashes.
3. Merge scores deterministically, dropping any BM25S hit whose returned `record_hash`
   does not match SQLite.
4. Return hits with `ranking_signals` so tests and operators can see whether BM25S was
   used.

Search flow for `hybrid`:

1. Apply the existing semantic/vector and FTS candidate logic.
2. Add BM25S as a lexical signal when available and current.
3. Preserve fail-closed behavior for semantic/vector capability failures. A BM25S outage
   does not silently satisfy an explicitly required BM25S request.

The search args gain an optional ranking preference:

```json
{
  "ranking": {
    "bm25s": "auto | required | disabled"
  }
}
```

`auto` is the default and uses BM25S when current. `required` rejects with
`CapabilityUnavailable` if the BM25S projection is absent, degraded, or stale.
`disabled` keeps the exact P0 ranking path for debugging and deterministic comparisons.

## 7. Status And Lint

`status` extends the #104 split health object with projection detail:

- sidecar state: disabled, healthy, degraded
- BM25S state: current, stale, failed, missing
- parser states by kind
- last successful rebuild timestamp
- last successful cursor or WAL sequence
- lag counts by projection target
- recent failure reasons, capped to a small fixed number for stable output

Human output stays compact. JSON output carries full structured detail for CI and tools.

`lint` adds projection findings:

- `ProjectionStale`: a record/source hash is newer than the last successful projection.
- `ProjectionMissing`: Nexus is active but a configured projection target has never been
  built.
- `ProjectionParserFailed`: parser output failed for a scoped source hash.
- `ProjectionHashMismatch`: sidecar returned a result for a hash that no longer matches
  the authoritative record.
- `ProjectionSidecarUnavailable`: configured Nexus projection cannot be probed.

`lint --fix` may invoke a rebuild for missing/stale projection items when the Nexus
profile is active. It must not rewrite authoritative records. If rebuild fails, lint
returns a failed projection finding rather than hiding the failure.

## 8. Error Handling

Projection errors are deliberately scoped.

- Config errors fail fast with the existing config validation path.
- Sidecar unavailable errors become degraded projection health and lint findings.
- Per-item parser failures are item failures, not command-wide authority failures.
- Search drops stale optional BM25S signals in `auto` mode and records that BM25S was not
  used. In `required` mode, stale or unavailable BM25S is `CapabilityUnavailable`.
- Hash mismatches are never reconciled by trusting Nexus. SQLite wins every time.

No projection error may mutate record bodies, frontmatter, edges, WAL rows, consent rows,
or tombstone state.

## 9. Testing

Tests are written first and grouped by behavior.

1. Rebuild immutability:
   - Rebuild BM25S and parser projections from fixture records.
   - Assert authoritative record rows and body hashes are unchanged.
   - Assert projection ledger rows reflect current hashes.

2. Search ranking:
   - With a healthy mock BM25S sidecar, `search --mode keyword` reports BM25S in
     `ranking_signals` and changes ordering only through deterministic score merge.
   - With a stale returned hash, BM25S for that hit is ignored.
   - With `ranking.bm25s = required`, degraded or stale BM25S returns
     `CapabilityUnavailable`.

3. Parser fixtures:
   - PDF and DOCX text parser outputs become projection text.
   - Video-frame and vision-caption fixtures are indexed only when configured.
   - Malformed parser fixture produces a projection failure without record mutation.

4. Status and lint:
   - Healthy current projection reports zero lag.
   - Missing sidecar reports degraded projection health while SQLite authority remains
     visible.
   - Stale record hash appears in both status lag counts and lint findings.
   - `lint --fix` rebuilds stale projection items and leaves records unchanged.

5. Regression checks:
   - `cargo nextest run -p cairn-core -p cairn-cli -p cairn-store-sqlite --locked`
   - `cargo test --doc --workspace --locked`
   - `./scripts/check-core-boundary.sh`
   - `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`

## 10. Acceptance Mapping

- Projection rebuild does not mutate authoritative records: covered by rebuild
  immutability tests and hash assertions.
- BM25S can be used as an additional search signal when available: covered by healthy
  mock sidecar search ranking tests and `ranking_signals` output.
- Parser failures are visible and scoped to the projection layer: covered by malformed
  parser fixtures, status detail, and lint findings.
- Projection lag and rebuild status appear in `lint` and `status`: covered by stale hash
  and missing projection tests.
