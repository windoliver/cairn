# Folder Sidecars, Backlinks, and Policy Inheritance

**Issue:** #44 — [P0] Implement folder metadata, sidecars, backlinks, and folder summaries
**Brief sections:** §3.4 Folders are first-class · §3.4.a Obsidian prior art
**Parent epic:** #5 Vault layout, registry, and markdown projection
**Depends on:** #43 (MarkdownProjector + resync — merged in `b1cddd0`)
**Date:** 2026-04-27

---

## Scope

Define the folder-sidecar schema and ship the P0 helpers that project SQLite
state into per-folder `_index.md` files. Backlinks are derived from markdown
links inside record bodies. Policy walk-up resolution is a pure function so
the eventual Filter stage and inheritance tests can call it directly.

`_summary.md` ships as schema-only types plus a `FolderSummaryWriter` trait
stub on `WorkflowOrchestrator`; no `_summary.md` files are written at P0.
Actual LLM-driven summary generation is P1 (brief §3.4 table).

**Out of scope:**

- PostToolUse hook wiring (sensors issue).
- Filter-stage enforcement of `allowed_kinds` / `visibility_default` (downstream).
- LLM-driven `_summary.md` body generation (P1).
- An `edges` table or `links` field on `MemoryRecord` (separate, future issue).
- Promotion of records into `wiki/` paths (separate issue).

---

## Architecture

```
cairn-core/src/domain/
  folder.rs              ← NEW — pure types + functions
                            FolderPolicy, EffectivePolicy
                            FolderIndex, FolderSummary (schema only)
                            Backlink, FolderState, RawLink
                            project_index() · parse_policy() · resolve_policy()
                            extract_links() · materialize_backlinks() · aggregate_folders()
  projection.rs          ← unchanged

cairn-core/src/contract/
  workflow_orchestrator.rs ← add FolderSummaryWriter trait stub
                              (default body returns Unimplemented)

cairn-cli/src/verbs/
  lint.rs                ← add --fix-folders flag
                            walks store records, calls folder projector,
                            atomically writes _index.md per non-empty folder

cairn-cli/src/vault/
  bootstrap.rs           ← unchanged (sidecars emitted on first lint, not bootstrap)
```

`cairn-core::domain::folder` keeps zero dependencies on other workspace crates.
All filesystem I/O lives in `cairn-cli`. The pure-function shape mirrors the
existing `MarkdownProjector` so the same callers (`lint`, future hooks) reuse
one pattern.

### Data flow — `cairn lint --fix-folders`

```
1. store.list_active() ──────────────► Vec<StoredRecord>

2. for each record:
     MarkdownProjector::project()  ──► record_paths: BTreeMap<RecordId, PathBuf>

3. fs walk for files named `_policy.yaml` in vault:
     parse_policy(content)         ──► policies_by_dir: BTreeMap<PathBuf, FolderPolicy>

4. materialize_backlinks(records, record_paths)
                                   ──► backlinks_by_target: BTreeMap<PathBuf, Vec<Backlink>>

5. aggregate_folders(records, record_paths,
                     policies_by_dir, backlinks_by_target)
                                   ──► Vec<FolderState>

6. for each FolderState:
     project_index(state)          ──► ProjectedFile { path: <folder>/_index.md, content }
     write atomically (write_once helper from bootstrap)

7. report: { written: [...], unchanged: N, policy_errors: [...] }
```

A single bad `_policy.yaml` does not abort the whole rebuild — that subtree is
skipped, the error is recorded in the report, the rest of the vault still
rebuilds. Otherwise one corrupt file would break vault-wide repair.

### Backlink derivation

Backlinks are derived (not authoritative): `extract_links` parses
`[label](path)` and `[[target]]` forms from each record body, resolves
relative paths against the source folder, drops external URLs, and stitches
the reverse map. This matches the brief's "rebuildable from SQLite state"
phrasing without introducing an edge table.

### Empty folders

`aggregate_folders` only emits a `FolderState` for folders that contain at
least one record OR a descendant subfolder that does. Empty `wiki/*`
subdirectories created by bootstrap therefore receive no `_index.md`. This
matches brief §3.4: "every non-empty folder has an `_index.md`."

Stray `_index.md` files at empty folders are left alone — never deleted by
lint, to avoid surprise removals. Operators who want them gone can `rm`
manually.

---

## Types

### `FolderPolicy` (deserialized from `_policy.yaml`)

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderPolicy {
    pub purpose: Option<String>,
    pub allowed_kinds: Option<Vec<MemoryKind>>,
    pub visibility_default: Option<MemoryVisibility>,
    pub consolidation_cadence: Option<ConsolidationCadence>,
    pub owner_agent: Option<String>,
    pub retention: Option<RetentionPolicy>,
    pub summary_max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsolidationCadence { Hourly, Daily, Weekly, Monthly, Manual }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RetentionPolicy { Days(u32), Unlimited }
```

`Option<T>` everywhere encodes "inherit from parent." `allowed_kinds: Some(vec![])`
explicitly forbids all kinds; `None` means "no opinion."

### `EffectivePolicy` (result of walk-up merge)

```rust
pub struct EffectivePolicy {
    pub purpose: Option<String>,
    pub allowed_kinds: Option<Vec<MemoryKind>>,
    pub visibility_default: MemoryVisibility,
    pub consolidation_cadence: ConsolidationCadence,
    pub owner_agent: Option<String>,
    pub retention: RetentionPolicy,
    pub summary_max_tokens: u32,
    /// Folder paths that contributed, shallowest first, deepest last.
    pub source_chain: Vec<PathBuf>,
}
```

Defaults applied when no policy chain sets a value:

| Field | Default |
|---|---|
| `visibility_default` | `MemoryVisibility::Private` |
| `consolidation_cadence` | `ConsolidationCadence::Daily` |
| `retention` | `RetentionPolicy::Unlimited` |
| `summary_max_tokens` | `200` |

`EffectivePolicy` has a hand-rolled `Default` impl that bakes in the table
above (and `purpose`/`allowed_kinds`/`owner_agent` = `None`,
`source_chain` = empty). Used directly when no policies exist anywhere on
the chain.

### `FolderIndex` (logical document, before serialization)

```rust
pub struct FolderIndex {
    pub folder: PathBuf,                 // vault-relative
    pub purpose: Option<String>,         // from EffectivePolicy
    pub updated_at: Rfc3339Timestamp,
    pub records: Vec<RecordEntry>,       // sorted: kind asc, then id asc
    pub subfolders: Vec<SubfolderEntry>, // sorted: name asc
    pub backlinks: Vec<Backlink>,        // sorted: source path asc
}

pub struct RecordEntry {
    pub path: PathBuf,
    pub id: RecordId,
    pub kind: MemoryKind,
    pub updated_at: Rfc3339Timestamp,
    pub backlink_count: u32,
}

pub struct SubfolderEntry {
    pub name: String,
    pub record_count: u32,
    pub last_updated: Option<Rfc3339Timestamp>,
}

pub struct Backlink {
    pub source_path: PathBuf, // file containing the link
    pub target_path: PathBuf, // file being pointed at (vault-relative)
    pub anchor: Option<String>,
}
```

Deterministic sort orders make repeat runs byte-identical for the
"rebuild from empty markdown tree" test.

### `RawLink` (intermediate from `extract_links`)

```rust
pub struct RawLink {
    pub target_path: PathBuf,
    pub anchor: Option<String>,
}
```

### `FolderSummary` (P1 schema; types only at P0)

```rust
pub struct FolderSummary {
    pub folder: PathBuf,
    pub generated_at: Rfc3339Timestamp,
    pub generated_by: AgentId,
    pub covers_records: u32,
    pub summary_tokens: u32,
    pub body: String,
}
```

`FolderSummaryWriter` trait stub lives on `WorkflowOrchestrator`:

```rust
#[async_trait::async_trait]
pub trait FolderSummaryWriter: Send + Sync {
    async fn write_summary(
        &self,
        summary: FolderSummary,
    ) -> Result<(), WorkflowError> {
        Err(WorkflowError::Unimplemented)
    }
}
```

P1 implementations override the default; P0 callers never invoke this path.

### `FolderState` (input to `project_index`)

```rust
pub struct FolderState {
    pub path: PathBuf,
    pub records: Vec<StoredRecord>,
    pub subfolders: Vec<SubfolderEntry>,
    pub backlinks: Vec<Backlink>,
    pub effective_policy: EffectivePolicy,
}
```

### `FolderError`

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FolderError {
    #[error("policy parse failed: {source}")]
    PolicyParse { #[source] source: serde_yaml::Error },
}
```

`extract_links` is best-effort and returns `Vec<RawLink>` directly — links
that do not match either supported syntactic form are skipped silently
(standard markdown extractor behaviour). Semantic validation of policy
fields beyond YAML shape is not done at P0; downstream Filter-stage
enforcement will own that check.

---

## Functions

### Public API in `cairn-core::domain::folder`

```rust
pub fn project_index(state: &FolderState) -> ProjectedFile;

pub fn parse_policy(yaml: &str) -> Result<FolderPolicy, FolderError>;

pub fn resolve_policy(
    target: &Path,
    policies_by_dir: &BTreeMap<PathBuf, FolderPolicy>,
) -> EffectivePolicy;

pub fn extract_links(source_path: &Path, body: &str) -> Vec<RawLink>;

pub fn materialize_backlinks(
    records: &[StoredRecord],
    record_paths: &BTreeMap<RecordId, PathBuf>,
) -> BTreeMap<PathBuf, Vec<Backlink>>;

pub fn aggregate_folders(
    records: &[StoredRecord],
    record_paths: &BTreeMap<RecordId, PathBuf>,
    policies_by_dir: &BTreeMap<PathBuf, FolderPolicy>,
    backlinks_by_target: &BTreeMap<PathBuf, Vec<Backlink>>,
) -> Vec<FolderState>;
```

### `extract_links` rules

- markdown form: `[label](target)` — label discarded, target taken as path.
- wiki form: `[[target]]` — `.md` appended if no extension.
- wiki anchor: `[[target#anchor]]` → `anchor` populated.
- relative paths: resolved against the source file's parent directory.
- absolute paths inside the vault: kept as vault-relative.
- external URLs: dropped (`http://`, `https://`, `mailto:`).
- inside code fences (```` ``` ```` blocks): ignored.
- escaped (`\[`): ignored.

### `resolve_policy` algorithm

Walk from `target`'s parent up to the vault root, collecting every entry in
`policies_by_dir` along the way. Reduce shallowest-to-deepest with
"deepest wins per key": for every `Option` field on `FolderPolicy`, the
deepest non-`None` value wins. Apply defaults to fields that are still
`None` after the walk. Record the contributing folder paths in
`source_chain`, shallowest first.

`resolve_policy` is associative under the deepest-wins fold — a property
test confirms this so chunked or memoized resolution stays valid later.

---

## CLI surface

### `cairn lint --fix-folders [--json]`

Additive flag — `--fix-markdown` from #43 is unchanged. `--fix-markdown` and
`--fix-folders` may be combined; records run first, then folders.

Caller flow:

1. `MemoryStore::list_active()` → records
2. project each record → `record_paths` map
3. fs-walk vault for `_policy.yaml` → `policies_by_dir`
4. `materialize_backlinks` → reverse map
5. `aggregate_folders` → states
6. for each state: `project_index` → atomic write via `write_once`
7. report counts + policy errors

### Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | one or more `FolderError`s recorded; partial rebuild succeeded |
| `78` (`EX_CONFIG`) | vault not initialized |

### `--json` shape

```json
{
  "written": ["raw/_index.md", "raw/projects/_index.md"],
  "unchanged": 4,
  "policy_errors": [
    { "path": "raw/broken/_policy.yaml", "reason": "<message>" }
  ]
}
```

---

## Output format — `_index.md`

```markdown
---
folder: wiki/entities/people
kind: folder_index
updated_at: 2026-04-22T14:02:11Z
record_count: 42
subfolder_count: 3
purpose: "people Cairn knows about"
---
# entities/people

## Records (42)
- [<vault-relative-path>](<vault-relative-path>) — <kind> · updated <date> · <N> backlinks
- ...

## Subfolders (3)
- [<name>/](<name>/) — <N> records · last updated <date>
- ...

## Backlinks into this folder (17)
- [<source-path>](<source-path>)
- ...
```

`purpose` is omitted from the frontmatter when the resolved
`EffectivePolicy.purpose` is `None`. Each `##` section is omitted when its
list is empty.

---

## Edge cases

| Case | Behaviour |
|---|---|
| No `_policy.yaml` anywhere in chain | `EffectivePolicy::default()` (private, daily, unlimited, 200 tokens) |
| `_policy.yaml` parse fails | error recorded; that folder + descendants skipped; lint exits `1` |
| Two records project to same path | unreachable — `MarkdownProjector` uses `<kind>_<id>`; guarded with `debug_assert!` in `materialize_backlinks` |
| Markdown link to non-existent file | included in backlinks anyway (derivative); `cairn lint`'s drift report is responsible for flagging dangling links — out of scope here |
| Markdown link is external URL | dropped at `extract_links` |
| Wiki-style `[[target]]` link | resolved relative to source folder; `.md` appended if missing |
| Anchor in link (`#section`) | preserved on the `Backlink`, not stripped from target path |
| Folder rename | next `--fix-folders` rebuilds correctly from store; no migration |
| Stray `_index.md` for now-empty folder | left alone — lint never deletes files |
| Pre-existing `_index.md` matches projection | counted as `unchanged`; no write |

---

## Error handling

- `cairn-core::domain::folder` returns `FolderError` (thiserror,
  `#[non_exhaustive]`). No I/O, no `anyhow`, no `unwrap`/`expect` in core.
- `cairn-cli::verbs::lint` wraps with `.context("lint --fix-folders: <path>")`
  at every call site; returns `anyhow::Result<()>` from the verb entry point.
- A single corrupt file does not abort the rebuild; partial success exits `1`.
- All file writes go through the existing `write_once` helper —
  random-named tempfile + `persist`, parent-symlink rejected, target
  re-validated post-rename.

---

## Testing

### `cairn-core` unit tests (in `folder.rs` `#[cfg(test)]`)

**`parse_policy`**
- happy round-trip preserves every field
- unknown key → `PolicyParse` (struct uses `deny_unknown_fields`)
- malformed YAML → `PolicyParse`
- empty YAML → `FolderPolicy::default()` (all `None`)

**`resolve_policy` (inheritance — verification criterion)**
- target with no policies → defaults
- single policy at root → echoed
- child overrides parent on one key, inherits others
- three-deep chain (`a/b/c` → `a/b` → `a`) — deepest wins per key
- `source_chain` shallowest-first, deepest-last

**`extract_links`**
- `[label](path.md)` extracted, label discarded
- `[[target]]` → `target.md`
- `[[target#anchor]]` → anchor populated
- relative `../alice.md` resolved against source folder
- `https://`, `mailto:` dropped
- code-fenced links ignored
- escaped `\[not a link\]` ignored

**`materialize_backlinks`**
- empty record set → empty map
- record links to existing target → backlink emitted
- record links to non-existent path → backlink still emitted
- two records → same target → both appear, sorted by source path

**`aggregate_folders`**
- single record at `raw/x.md` → one `FolderState` for `raw/`
- nested records → parents include subfolder aggregates
- empty folder, empty descendants → no `FolderState`
- subfolder counts and `last_updated` propagate

**`project_index`**
- determinism: same `FolderState` → byte-identical output
- frontmatter contains `folder`, `kind: folder_index`, `updated_at`,
  `record_count`, `subfolder_count`
- `purpose` field present iff policy supplied one
- record / subfolder / backlink sections omitted when empty

### `cairn-cli` integration tests (`crates/cairn-cli/tests/lint_folders.rs`)

Use `FixtureStore` from `cairn-test-fixtures` + `tempfile::tempdir()` vault.

**Folder fixture projection** *(verification criterion)*
- Bootstrap vault → seed `FixtureStore` with a known set → run
  `lint --fix-folders` → assert each `_index.md` matches an `insta` snapshot.

**Backlink rebuild from empty markdown tree** *(verification criterion)*
- Seed store with cross-linking records → vault contains only `.cairn/` and
  empty `raw/`/`wiki/` → run `lint --fix-folders` → assert backlink sections
  in every emitted `_index.md` match expected.

**Nested-folder config inheritance** *(verification criterion)*
- The unit-level `resolve_policy` cases above own this assertion. No CLI
  debug surface is added at P0.

**`_policy.yaml` parse failure does not abort lint**
- Two folders, one with valid policy and records, one with garbled
  `_policy.yaml` → run `lint --fix-folders` → exit `1`, valid folder's
  `_index.md` still written, error appears in `--json` `policy_errors`.

**Atomic writes**
- Pre-place `_index.md` with stale content → run lint → final file matches
  projection; no temp leftovers in the directory.

**`--json` output**
- Healthy vault → assert valid JSON shape: `{ written, unchanged, policy_errors }`.

### Property tests (`proptest`)

- `parse_policy ∘ serde_yaml::to_string` round-trips for arbitrary
  `FolderPolicy`.
- `resolve_policy` is associative under the deepest-wins fold
  (chunked merges agree with whole-chain merge).

### Skipped (out of scope)

- Live SQLite store tests — `FixtureStore` covers everything; real-store
  coverage lands with #46.
- PostToolUse hook tests — sensors issue.
- `_summary.md` body generation — P1 only.

---

## Invariants touched

- §3.0 DB-is-authority — sidecars are projections, never authoritative.
- §3.4 sidecar trio — `_index.md` (P0 here), `_summary.md` (schema only),
  `_policy.yaml` (read-only at P0).
- §4 invariant 1 harness-agnostic — pure functions in core, no harness dep.
- §4 invariant 3 CLI-is-ground-truth — `lint --fix-folders` is the verb;
  the same pure functions back future hook callers.
- §4 invariant 6 fail-closed — bad policy fails its subtree, not the vault.
- §4 invariant 7 no-unsafe — no new `unsafe` introduced.
- §4 invariant 8 no-`unwrap`-in-core — `FolderError` carries every failure.
