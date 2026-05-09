# Folder Ingest Live Issue Design

**Date:** 2026-05-07
**Issue:** [#188 - `cairn ingest --folder <path>` folder scanning and knowledge-base builder](https://github.com/windoliver/cairn/issues/188)
**Status:** Draft for review
**Supersedes:** `docs/superpowers/specs/2026-05-06-folder-ingest-design.md` for live issue completion

---

## 1. Scope

This spec completes the live issue #188 behavior, not only the earlier P0
keyword-only validation slice. The earlier branch proved deterministic folder
scanning, keyword extraction counts, cache keys, dry-run, and summaries on an
older base where `MemoryStore`, WAL apply, and `FlushPlan` were still stubs.

Current `origin/main` now contains the required store and `FlushPlan` domain
work referenced by the issue comment. Completion must therefore start by
rebasing or merging this branch onto `origin/main`; building a parallel local
store or fake WAL on the old branch would not satisfy the issue's intended
architecture.

The completed feature must provide:

- `cairn ingest --folder <path>` as a real CLI path.
- Offline `--mode keyword` folder ingest with no LLM, embeddings, network, or
  cloud dependency.
- Deterministic include/exclude scanning and extraction cache behavior.
- `--batch-size N`, default `64`.
- One `FlushPlan` per batch of cache misses.
- Deterministic per-batch `operation_id` derived from folder path, batch index,
  and sorted content hashes.
- Dry-run output that materializes the planned batches without applying them.
- Non-dry-run behavior that applies or persists those batches through the
  existing current-main store/flush path so the summary reports durable work
  honestly.
- Java structural extraction in keyword mode.

`--mode semantic` and `--mode full` remain accepted modes but are not
implemented by this change. They fail closed with `CapabilityUnavailable`.
Issue #188's P0 acceptance requires `keyword` to work offline; it does not
require inventing a new LLM stack in this change.

---

## 2. Base Integration

The branch is currently behind `origin/main` by hundreds of commits. `origin/main`
contains:

- `crates/cairn-core/src/domain/flush_plan/`
- a non-stub `MemoryStore` contract
- a real `cairn-store-sqlite` implementation
- store, graph, and flush support expected by the issue comment

Implementation starts by rebasing or merging the existing folder-ingest commits
onto `origin/main`, then resolving conflicts by preserving current-main
store/domain code and reapplying the folder ingest behavior on top.

The design target is the current-main architecture. If conflicts reveal that
main already has adjacent folder or flush commands, integrate with those
surfaces instead of creating duplicate modules.

---

## 3. CLI And IDL Contract

`IngestArgs` keeps the existing source XOR contract and adds the live issue
folder options:

```rust
pub folder: Option<String>,
pub recursive: Option<bool>,
pub include: Option<Vec<String>>,
pub exclude: Option<Vec<String>>,
pub mode: Option<IngestMode>,
pub dry_run: Option<bool>,
pub batch_size: Option<u16>,
```

CLI behavior:

- exactly one of positional `source`, `--body`, `--file`, `--url`, or
  `--folder` is required
- `--folder` combined with another source exits usage `64`
- missing or non-directory `--folder` exits usage `64`
- `--batch-size 0` exits usage `64`
- default `--batch-size` is `64`
- default `--recursive` is `true`
- default includes are `*.md,*.txt,*.rst,*.rs,*.py,*.ts,*.js,*.go,*.java`
- default excludes are `.git,node_modules,target`
- default mode is `keyword`

Generated SDK, MCP schemas, and skill docs are updated only through the IDL
code generator.

---

## 4. Scan, Cache, And Extraction

Folder scanning preserves the validated earlier behavior:

- walk relative to the supplied folder root
- sort relative paths lexicographically before processing
- prune excluded directories and files
- include hidden files when they match includes
- follow symlinked files
- skip symlinked directories
- warn and skip broken symlinks
- warn and skip unsupported files explicitly included by a glob
- warn and skip invalid UTF-8 text files

Cache keys remain portable:

```text
sha256(body_below_yaml_frontmatter + "\0" + relative_path_from_folder_root)
```

Cache entries remain under `.cairn/cache/{sha256hex}.json` relative to the
current vault root. Cache checks happen before batch planning. Cache hits never
enter a `FlushPlan`.

Keyword extraction remains deterministic and offline. It expands code support
to `.java`:

| Extension | Keyword structural patterns |
|---|---|
| `.rs` | `fn`, `struct`, `enum`, `trait`, `impl`, `mod` |
| `.py` | `def`, `class` |
| `.ts`, `.js` | `function`, `class`, `interface`, `type`, `const name =` |
| `.go` | `package`, `func`, `type` |
| `.java` | `class`, `interface`, `enum`, method declarations |

Markdown/text extraction continues to detect headings, wiki links, TODO/FIXME
markers, and conservative capitalized phrases.

---

## 5. FlushPlan Batching

After cache filtering, remaining files are chunked into batches of
`batch_size`.

Each batch produces exactly one `FlushPlan`:

- `mode` is `DryRun` for `--dry-run`, otherwise `Autonomous`
- `reason` is `UserIngest`
- `mutations` contains one `PlannedMutation::Upsert` per processed file
- `operation_id` is deterministic from:
  - normalized folder path
  - zero-based batch index
  - sorted cache/content hashes for files in the batch

The deterministic ID function must be isolated and unit-tested. The final
encoded ID must fit the repository's `Ulid` newtype constraints. If the current
ULID type requires canonical Crockford formatting, derive a stable 128-bit value
from SHA-256 and encode it as a valid ULID-shaped string.

Dry-run materializes the same plans the non-dry-run path would apply, but it
does not write cache entries, `FlushPlan` files, WAL rows, store records, graph
edges, or derived projections.

Non-dry-run applies planned batches through a real store-backed apply helper.
The helper takes one `FlushPlan` and a `MemoryStore`/SQLite store handle, writes
the batch through the current-main WAL tables, and calls `MemoryStore::upsert`
for every `PlannedMutation::Upsert` in the plan. If current-main does not expose
this helper publicly after rebase, this change adds a narrow folder-ingest apply
helper instead of using `cairn flush apply`'s metadata-only placeholder path.
`records_written` is incremented only for accepted store upserts.

Crash/retry behavior is content-addressed:

- an unchanged batch derives the same `operation_id`
- applied batches are skipped or no-op on retry
- a crash after an earlier batch commits and before later batches commit resumes
  from the first unapplied batch
- no duplicate records are written on retry

---

## 6. Store Records And Graph Shape

Each processed file maps to one durable source/record upsert. The
`MemoryRecord` taxonomy follows current-main constructors and validation rules.
The record body preserves the source body, and the frontmatter or metadata
includes enough provenance to identify:

- folder root
- relative path
- content/cache hash
- extraction mode
- extracted entity and edge counts

Entities and lightweight edges produced by keyword extraction are written
through the current-main bitemporal graph methods when
`MemoryStoreCapabilities.graph_edges` is true. Graph write failures fail the
batch; summaries do not count graph entities or edges as durable unless those
calls succeed.

`records_written` means durable store upserts attempted and accepted for cache
misses. Cache writes are not counted as records.

---

## 7. Output

Human output keeps the stable shape:

```text
Scanning ./docs (142 files)...
  Cached  89 (no changes detected)
  Processed 53 files
    Entities: 214 new · 37 merged
    Edges:    891 new · 12 contradictions resolved
    Records:  53 written to store
Elapsed: 2.3s
```

For dry-run:

```text
    Records:  0 written to store (dry-run)
    Plans:    3 materialized (dry-run)
```

JSON output includes the existing fields and adds batch/plan visibility:

```json
{
  "scanned": 142,
  "cached": 89,
  "processed": 53,
  "skipped": 0,
  "warnings": 0,
  "entities_new": 214,
  "entities_merged": 37,
  "edges_new": 891,
  "contradictions_resolved": 12,
  "records_written": 53,
  "plans": 3,
  "batch_size": 64,
  "operation_ids": ["01..."],
  "elapsed_ms": 2300,
  "dry_run": false,
  "mode": "keyword"
}
```

`operation_ids` are included so agents can inspect or correlate planned/applied
batches. The list is ordered by batch index.

---

## 8. Testing And Verification

The implementation must be test-first. Required coverage:

- IDL/generated-wire test for `folder`, `recursive`, `include`, `exclude`,
  `mode`, `dry_run`, and `batch_size`
- CLI help exposes `--folder`, `--batch-size`, `--mode`, `--dry-run`, and
  `--json`
- source XOR errors include `--folder`
- missing folder exits `64`
- `--batch-size 0` exits `64`
- keyword dry-run over a temp folder extracts entities and writes no cache,
  plans, WAL, records, or graph rows
- non-dry-run over a temp folder writes cache entries, applies plans, and
  reports `records_written > 0`
- second non-dry-run over unchanged files reports all supported files cached and
  `processed == 0`
- deterministic operation ID test for identical folder content and batch index
- changed body or changed relative path changes operation ID
- `--batch-size 2` over 5 cache misses produces exactly 3 plans
- crash/retry proptest or deterministic integration test proves already-applied
  batch IDs are not duplicated and later batches can resume
- `.java` keyword extraction produces structural entities
- explicitly included unsupported files are warned and skipped before cache or
  plan construction
- symlinked files, symlinked directories, broken symlinks, hidden files, and
  excludes remain covered
- repo-local integration test ingests `docs/` with `--mode keyword --dry-run
  --json` and asserts `entities_new > 0`
- human summary snapshot includes plans and record lines
- real CLI E2E script or test covers `docs/`, cache hit, batch size, and Java
  paths

Final verification commands:

```bash
cargo run -p cairn-idl --bin cairn-codegen -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/check-core-boundary.sh
git diff --check
```

Manual real CLI validation must run against the built binary after tests:

```bash
target/debug/cairn ingest --folder docs --mode keyword --dry-run --json
target/debug/cairn ingest --folder <temp-project> --mode keyword --batch-size 2 --json
target/debug/cairn ingest --folder <temp-project> --mode keyword --batch-size 2 --json
```

---

## 9. Non-Goals

- Building a new LLM provider or embedding stack for `semantic` or `full`.
- Replacing current-main store, WAL, or FlushPlan architecture.
- Implementing MCP graph traversal tools enabled by the ingested graph.
- Adding a full glob crate; the local pattern matcher remains sufficient for
  issue #188 defaults and tested edge cases.

---

## 10. Risks

- Rebase conflicts are likely because `origin/main` changed core/domain/store
  APIs substantially.
- Current-main apply APIs may not expose one public helper for
  `FlushPlan -> MemoryStore`; this change must either reuse the existing helper
  or add a narrowly scoped one that is store-backed and tested.
- Entity graph writes add more store calls per file; batch failures must abort
  cleanly without reporting partial graph counts as durable.
- The deterministic `operation_id` needs careful encoding so it is stable and
  valid for the repository's generated `Ulid` type.
