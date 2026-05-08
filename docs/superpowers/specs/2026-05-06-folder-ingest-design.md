# Folder Ingest Design — Issue #188

**Date:** 2026-05-06  
**Issue:** [#188 — `cairn ingest --folder <path>` folder scanning and knowledge-base builder](https://github.com/windoliver/cairn/issues/188)  
**Brief sections:** §3 Vault Layout · §5.2 Write path · §5.2.a ExtractorWorker · §5.6 WAL · §8 CLI contract  
**Status:** Approved

---

## 1. Scope

Add a P0 vertical slice for `cairn ingest --folder <path>` that is useful before
the full SQLite `MemoryStore`, WAL apply path, entity graph, and three-tier
entity resolution are implemented. The slice scans folders deterministically,
plans offline keyword extraction, maintains a content-addressed extraction
cache, supports dry-run, and emits stable human and JSON summaries.

The implemented mode is `--mode keyword`. It runs fully offline, with no
`LLMProvider`, embeddings, Python sidecar, network access, or cloud credential.
`--mode semantic` and `--mode full` are accepted by the contract but fail closed
with the existing capability-unavailable behavior until their dependencies
exist.

The feature does not pretend to commit records. Because `MemoryStore` is still a
scaffold, non-dry-run folder ingest may write extraction cache entries but must
not report WAL, record-store, or graph writes as durable DB commits. Summary
fields such as `records_written`, `entities_merged`, and
`contradictions_resolved` remain zero or explicitly planned-only until the real
store path lands.

---

## 2. Architecture

The design has three layers.

| Layer | Files | Responsibility |
|---|---|---|
| Contract | `crates/cairn-idl/schema/verbs/ingest.json` and generated artifacts | Add folder ingest fields through the canonical IDL so CLI, SDK, MCP schema, and skill output stay in sync. |
| Folder planner | `crates/cairn-cli/src/verbs/ingest/` modules | Scan folders, match include/exclude patterns, compute cache keys, run keyword extraction, read/write cache entries, and render summaries. |
| Dispatcher | `crates/cairn-cli/src/verbs/ingest.rs` | Route `--folder` separately from the existing body/file/url/stdin stub while preserving current single-source ingest behavior. |

Folder ingest starts in `cairn-cli` because there is not yet a real core ingest
verb or store trait method to call. The planner should avoid CLI-specific data
types internally so it can move into `cairn-core` once the store/WAL work
exists.

Generated files must not be edited by hand. Changing the IDL requires running:

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

---

## 3. CLI And Args Contract

`IngestArgs` gains these fields:

```rust
pub folder: Option<String>,
pub recursive: Option<bool>,
pub include: Option<Vec<String>>,
pub exclude: Option<Vec<String>>,
pub mode: Option<IngestMode>,
pub dry_run: Option<bool>,
```

`IngestMode` is a closed enum:

```rust
pub enum IngestMode {
    Keyword,
    Semantic,
    Full,
}
```

The generated SDK type may represent optional CLI inputs as `Option<T>` because
the current codegen treats non-required fields that way. The folder dispatcher
normalizes those values into an internal options struct before scanning:

```rust
pub struct FolderIngestOptions {
    pub folder: PathBuf,
    pub recursive: bool,
    pub include: Vec<GlobPattern>,
    pub exclude: Vec<GlobPattern>,
    pub mode: IngestMode,
    pub dry_run: bool,
}
```

The input XOR changes from exactly one of `[body, file, url]` to exactly one of
`[body, file, url, folder]`. The positional `source` continues to count as one
of the non-folder sources in the CLI dispatcher. Supplying `--folder` with
`source`, `--body`, `--file`, or `--url` is a usage error.

Defaults:

| Arg | Default |
|---|---|
| `recursive` | `true` |
| `include` | `*.md,*.txt,*.rs,*.py,*.ts,*.js,*.go` |
| `exclude` | `.git,node_modules,target` |
| `mode` | `keyword` |
| `dry_run` | `false` |

`--json` remains the existing shared CLI output flag rather than an `IngestArgs`
field.

---

## 4. Folder Scanning

The scanner walks relative to the supplied folder root and sorts candidate paths
lexicographically for deterministic output. Recursive scanning is enabled by
default. When recursion is disabled, only direct children are considered.

Path matching applies to slash-normalized relative paths from the folder root.
Include patterns decide which files are eligible. Exclude patterns prune both
files and directories. Default excludes remove `.git`, `node_modules`, and
`target`; hidden files are otherwise eligible if they match an include pattern.

Symlink policy:

- Symlinked files may be read if their resolved target is a regular file.
- Symlinked directories are not traversed.
- Broken symlinks are skipped and counted as warnings.

Unsupported binary or media files are skipped with warning counts rather than
failing the whole run. Invalid UTF-8 in an otherwise included file is also
skipped with a warning. The scanner never logs or prints raw file bodies.

The initial pattern matcher should be small and dependency-light:

- `*.ext` matches a file basename suffix.
- A bare path segment like `.git` or `target` matches that segment anywhere in
  the relative path.
- A pattern containing `/` matches the normalized relative path prefix or exact
  path.

This covers the issue defaults and tested edge cases without adding a glob
dependency before the project has a broader dependency policy for path matching.

---

## 5. Cache

Cache entries live under `.cairn/cache/{sha256hex}.json`. For this vertical
slice, the vault root is the current working directory, so the cache root is
`$PWD/.cairn/cache`. This matches the P0 vault layout and can later switch to
the configured vault root when vault selection lands. The implementation creates
this directory only for non-dry-run runs that need to write at least one cache
entry.

The cache key is:

```text
sha256(body_below_yaml_frontmatter + "\0" + relative_path_from_folder_root)
```

For markdown files, only a leading YAML frontmatter block is stripped for cache
hashing. Frontmatter changes therefore do not bust cache; body changes do.
Non-markdown files hash their full body. The relative path participates in the
hash so identical content in two locations produces two independent cache
entries.

Cache hit behavior:

- If `.cairn/cache/{hash}.json` exists, count the file as cached and skip
  extraction.
- In dry-run, cache hits are read but missing cache entries are not written.
- In non-dry-run, cache entries are written through a same-directory temporary
  file followed by `std::fs::rename`.

Cache write failures are fatal in non-dry-run because the run would otherwise
look repeatable while failing to record extraction state.

---

## 6. Keyword Extraction

`--mode keyword` runs a conservative, deterministic extractor.

Markdown and text files extract:

- headings (`#`, `##`, etc.) as candidate entities
- wiki links (`[[name]]`) as candidate entities and lightweight edges
- capitalized multi-word phrases as candidate entities
- `TODO` and `FIXME` markers as signal entities

Code files extract structural declarations by extension:

| Extension | Patterns |
|---|---|
| `.rs` | `fn`, `struct`, `enum`, `trait`, `impl`, `mod` declarations |
| `.py` | `def`, `class` declarations |
| `.ts`, `.js` | `function`, `class`, `interface`, `type`, `const name =` declarations |
| `.go` | `func`, `type`, `package` declarations |

Extraction produces counts and cache-entry payloads only in this vertical slice.
All extracted entities are tagged as `EXTRACTED`; `INFERRED` and `AMBIGUOUS`
remain reserved for future semantic/full extraction.

Entity merge, contradiction resolution, WAL apply, and record-store writes are
not implemented in this slice. The JSON fields for those operations are present
for contract shape but report `0`.

---

## 7. Output

Human output for `cairn ingest --folder ./docs` uses this stable shape:

```text
Scanning ./docs (142 files)...
  Cached  89 (no changes detected)
  Processed 53 files
    Entities: 214 new · 0 merged
    Edges:    891 new · 0 contradictions resolved
    Records:  0 written to store
Elapsed: 2.3s
```

If this is a dry-run, the records line says:

```text
    Records:  0 written to store (dry-run)
```

JSON output includes the required fields:

```json
{
  "cached": 89,
  "processed": 53,
  "entities_new": 214,
  "entities_merged": 0,
  "edges_new": 891,
  "contradictions_resolved": 0,
  "elapsed_ms": 2300
}
```

It may include additional stable fields such as `scanned`, `skipped`,
`warnings`, `records_written`, `dry_run`, and `mode`. The required fields remain
top-level numbers.

---

## 8. Errors

Folder ingest uses explicit fail-closed errors:

| Case | Exit |
|---|---|
| Missing or non-directory `--folder` | `64` usage error |
| `--folder` combined with `source`, `--body`, `--file`, or `--url` | `64` usage error |
| Malformed include or exclude pattern | `64` usage error |
| `--mode semantic` or `--mode full` without required dependencies | `78` config/capability error |
| Cache write failure in non-dry-run | `1` generic failure |
| Unsupported file type, invalid UTF-8, or broken symlink | warning count, not fatal |

Dry-run guarantees zero cache directory creation and zero cache file writes.

---

## 9. Tests

Tests are written before implementation.

CLI tests:

- `cairn ingest --folder <dir> --mode keyword --dry-run --json` succeeds and
  emits the required summary fields.
- `--folder` conflicts with positional `source`, `--body`, `--file`, and
  `--url`.
- Missing/non-directory folder exits `64`.
- `--mode semantic` and `--mode full` fail closed while dependencies are absent.

Scanner tests:

- Default include/exclude handles `.md`, `.txt`, Rust, Python, TypeScript,
  JavaScript, and Go files.
- `.git`, `node_modules`, and `target` are excluded by default.
- Hidden files are included when the include pattern matches.
- Symlinked files are processed; symlinked directories are not traversed.

Cache tests:

- Markdown frontmatter changes do not change the key.
- Markdown body changes change the key.
- Relative path changes change the key.
- Dry-run creates no `.cairn/cache` directory.
- Second non-dry-run over unchanged files reports all files cached, zero files
  processed, and `elapsed_ms < 100` on the small fixture.

Output tests:

- Human output has an insta snapshot.
- JSON output includes
  `{ cached, processed, entities_new, entities_merged, edges_new, contradictions_resolved, elapsed_ms }`.
- A repo-local integration test scans `docs/` in keyword dry-run mode and
  asserts `entities_new > 0`.

---

## 10. Follow-On Work

When the store dependencies land, this slice should move the planner/extractor
into the core ingest verb and replace planned-only counts with real operations:

- WAL `upsert` transactions in `.cairn/cairn.db`
- record rows and FTS rows
- graph edge rows
- three-tier entity resolution
- `records_written > 0` for non-dry-run runs

The folder CLI contract and cache-key semantics should remain stable.
