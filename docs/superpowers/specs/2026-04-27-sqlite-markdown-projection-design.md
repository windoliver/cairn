# SQLite → Markdown Projection & Resync

**Issue:** #43 — [P0] Project SQLite records to markdown and rebuild from markdown safely  
**Brief sections:** §3, §13.5.a, §13.5.c  
**Dependencies:** #37 (MemoryRecord schema), #42 (vault registry)  
**Date:** 2026-04-27

---

## Scope

Implement one-way projection from authoritative SQLite rows to markdown files and YAML
frontmatter under the vault tree, plus `ingest --resync <path>` for re-ingesting out-of-band
markdown edits through the normal validation and WAL paths.

**Out of scope:** actual SQLite store ops (land in #46), WAL background job wiring, daemon
file-watcher, `FrontendAdapter` trait impl (P1, #113), desktop GUI.

---

## Architecture

```
cairn-core/src/domain/
  projection.rs          pure MarkdownProjector + value types — no I/O, no store dep

cairn-core/src/contract/
  memory_store.rs        add get/upsert/list_active method signatures to MemoryStore trait
                         (bump CONTRACT_VERSION; SQLite impl bodies land in #46)

cairn-cli/src/
  commands/ingest.rs     add --resync <path> flag
  commands/lint.rs       add --fix-markdown flag

cairn-test-fixtures/src/
  store.rs               FixtureStore: HashMap-backed MemoryStore test double
```

### Data flow — projection (DB → markdown)

```
MemoryRecord (from store)
  → MarkdownProjector::project()
  → ProjectedFile { path, content }
  → fs::write (caller, not projector)
```

### Data flow — resync (markdown → DB)

```
fs::read(path) (caller)
  → MarkdownProjector::parse()
  → ParsedProjection
  → MarkdownProjector::check_conflict(parsed, current_record)
  → ConflictOutcome::Clean   → MemoryStore::upsert (WAL path)
  → ConflictOutcome::Conflict → write .cairn/quarantine/ marker, return error
```

**Invariant:** `MarkdownProjector` is a zero-field unit struct. Zero I/O, zero async, zero
store deps — pure `&str`/`&MemoryRecord` in, typed values out.

---

## Types

### `ProjectedFile`

```rust
pub struct ProjectedFile {
    /// Vault-relative path, e.g. `raw/feedback_abc.md`
    pub path: PathBuf,
    /// Full file content: YAML frontmatter block + blank line + markdown body
    pub content: String,
}
```

### `ParsedProjection`

```rust
pub struct ParsedProjection {
    pub target_id: String,
    pub version: u32,
    pub kind: MemoryKind,
    pub visibility: MemoryVisibility,
    pub body: String,
    pub tags: Vec<String>,
    pub raw_frontmatter: BTreeMap<String, serde_yaml::Value>,
}
```

### `ConflictOutcome`

```rust
pub enum ConflictOutcome {
    Clean,
    Conflict {
        marker: String,
        file_version: u32,
        store_version: u32,
    },
}
```

### `ResyncError`

```rust
pub enum ResyncError {
    ParseFailed(String),
    MissingId,
    ImmutableFieldMutated(String),
    Conflict { file_version: u32, store_version: u32 },
}
```

Note: `Io(std::io::Error)` is injected by the CLI caller, not in `cairn-core`.

---

## Frontmatter fields

| Field | Projected | Frontend-mutable |
|---|---|---|
| `id` (target_id) | yes | no — identity |
| `version` | yes | no — backend-owned |
| `kind`, `class`, `visibility` | yes | no — classification |
| `scope`, `confidence`, `salience` | yes | no — classification |
| `created`, `updated` | yes | no — audit |
| `tags`, `links` | yes | **yes** |
| body | markdown body section | **yes** |
| `actor_chain`, `signature` | no | no — backend-only |
| `operation_id` | no | no — WAL-internal |

### Projected file format

```
---
id: 01JXXXXXXXXXXXXXXXXXXXXXXXXX
version: 3
kind: feedback
class: episodic
visibility: private
scope: ["default", "default", "default", "default"]
confidence: 0.9
salience: 0.7
tags: [rust, auth]
links: []
created: 2026-04-01T00:00:00Z
updated: 2026-04-27T12:00:00Z
---

Body text here.
```

---

## `MemoryStore` trait additions

Three method signatures added to the existing `MemoryStore` trait (bumps `CONTRACT_VERSION`).
SQLite impl bodies land in #46; `FixtureStore` in `cairn-test-fixtures` provides the test impl.

```rust
async fn get(&self, target_id: &str) -> Result<Option<MemoryRecord>, StoreError>;
async fn upsert(&self, record: MemoryRecord) -> Result<(), StoreError>;
async fn list_active(&self) -> Result<Vec<MemoryRecord>, StoreError>;
```

`StoreError` is a new `thiserror` enum in `cairn-core::contract::memory_store`.

---

## `MarkdownProjector`

```rust
pub struct MarkdownProjector;

impl MarkdownProjector {
    pub fn project(&self, record: &MemoryRecord) -> ProjectedFile;

    pub fn parse(&self, content: &str) -> Result<ParsedProjection, ResyncError>;

    pub fn check_conflict(
        &self,
        parsed: &ParsedProjection,
        current: Option<&MemoryRecord>,
    ) -> ConflictOutcome;
}
```

### `check_conflict` rules

1. `current = None` (new record) → `Clean`
2. `parsed.version < current.version` → `Conflict` (file is stale)
3. `parsed.version == current.version` → `Clean` (optimistic version match)
4. `parsed.version > current.version` → `Conflict` (version is immutable from frontend; increment is invalid)
5. Any immutable field differs (`kind`, `class`, `visibility`, `scope`) → `Conflict` regardless of version

---

## CLI surface

### `ingest --resync <path>`

```
cairn ingest --resync <vault-relative-or-absolute-path>
```

Caller flow:

1. `fs::read(path)` → content
2. `MarkdownProjector::parse(content)` → `ParsedProjection` | error → exit 1
3. `MemoryStore::get(target_id)` → `current: Option<MemoryRecord>`
4. `MarkdownProjector::check_conflict(parsed, current)`:
   - `Clean` → build `MemoryRecord` from parsed → `MemoryStore::upsert` → exit 0
   - `Conflict` → write `.cairn/quarantine/<timestamp>-<target_id>.rejected`
               → stderr: `"conflict: file version N, store version M; see .cairn/quarantine/"`
               → exit 1
5. `--json` flag → `{ status, path, target_id, version }`

### `lint --fix-markdown`

```
cairn lint --fix-markdown
```

Caller flow:

1. `MemoryStore::list_active()` → `Vec<MemoryRecord>`
2. For each record: `MarkdownProjector::project(record)` → write if missing or content differs
3. Report: `N written, M already current`
4. `--json` → `{ written: [...], current: N }`

### Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | resync conflict or parse failure |
| 78 (EX_CONFIG) | vault not initialised |

---

## Error handling

- `cairn-core` owns `ResyncError` and `ConflictOutcome` — no I/O, no `anyhow`
- `cairn-cli` maps `ResyncError → anyhow::Error` with `.context("ingest --resync: <path>")`
- Conflict writes quarantine file then returns `ResyncError::Conflict`
- `ParseFailed` and `MissingId` print a human message pointing at the offending line; exit 1

---

## Testing

### `cairn-core` unit tests (in `projection.rs` `#[cfg(test)]`)

- `project` → `parse` round-trip preserves all mutable fields
- `project` → mutate body → `parse` → `check_conflict` → `Clean`
- `project` → mutate `kind` → `parse` → `check_conflict` → `Conflict`
- `project` → decrement version → `parse` → `check_conflict` → `Conflict`
- `parse` on missing `id` → `MissingId`
- `parse` on malformed YAML → `ParseFailed`

### `cairn-cli` integration tests (`crates/cairn-cli/tests/resync.rs`) using `FixtureStore`

- Write projected file to `tempdir`, run resync → record updated in store
- Write projected file, corrupt `kind` field → conflict quarantine file written
- Run `lint --fix-markdown` → missing files recreated, current files untouched
- `--json` flag produces valid JSON on both success and conflict

### `FixtureStore` (`cairn-test-fixtures`)

`HashMap<String, MemoryRecord>` behind the `MemoryStore` trait — `get`, `upsert`,
`list_active`. No SQLite required.

---

## Invariants touched

- §3.0: DB is sole authority; markdown is repairable projection
- §4 invariant 3: CLI is ground truth — `ingest --resync` routes through verb layer
- §4 invariant 5: WAL + two-phase apply — resync upsert goes through `MemoryStore::upsert`
- §4 invariant 6: fail closed — `ImmutableFieldMutated` rejects the edit rather than silently applying it
