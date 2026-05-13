# Folder Ingest Live Issue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete live GitHub issue #188 by making `cairn ingest --folder <path>` produce offline keyword extraction, cache-aware `FlushPlan` batches, deterministic operation ids, and real store-backed writes on current `origin/main`.

**Architecture:** Merge this branch onto current `origin/main` first, because `origin/main` contains the real `FlushPlan`, `MemoryStore`, SQLite store, graph, and WAL surfaces that issue #188 depends on. Folder ingest then becomes a focused CLI planner under `crates/cairn-cli/src/verbs/ingest/`: scan/cache/extract files, build one `FlushPlan` per cache-miss batch, apply each non-dry-run plan through a narrow store-backed helper, and render stable human/JSON summaries.

**Tech Stack:** Rust, clap-generated CLI from Cairn IDL, `cairn_core::domain::FlushPlan`, `cairn_core::contract::MemoryStore`, `cairn-store-sqlite`, `tokio`, `sha2`, `ulid`, `serde_json`, `insta`, `assert_cmd`, `proptest`.

---

## File Structure

Modify:

- `crates/cairn-idl/schema/verbs/ingest.json` - add folder CLI flags and `batch_size`.
- `crates/cairn-core/tests/generated_wire.rs` - generated SDK/wire tests for folder args.
- `crates/cairn-idl/tests/schema_discriminator.rs` - allowlist the intentional `mode` `oneOf` when the discriminator test reports that violation after codegen.
- `crates/cairn-cli/src/main.rs` - pass resolved vault root into `verbs::ingest::run`.
- `crates/cairn-cli/src/verbs/ingest.rs` - keep origin/main resync and existing non-folder behavior, route `--folder` into the folder runner before the generic flush stub.
- `crates/cairn-cli/src/verbs/mod.rs` - keep `with_flush_modes` as the source of `--dry-run`; do not add duplicate generated `dry-run` clap flags.
- `crates/cairn-cli/tests/cli.rs` - update source XOR/help expectations.
- `crates/cairn-cli/tests/folder_ingest.rs` - expand real CLI E2E coverage.
- `crates/cairn-cli/tests/folder_ingest_snapshot.rs` - update human summary snapshot.
- `crates/cairn-cli/tests/snapshots/folder_ingest_snapshot__folder_ingest_human.snap` - updated snapshot.

Create or restore:

- `crates/cairn-cli/src/verbs/ingest/patterns.rs` - include/exclude pattern parser and matcher.
- `crates/cairn-cli/src/verbs/ingest/scanner.rs` - deterministic folder walker and symlink policy.
- `crates/cairn-cli/src/verbs/ingest/cache.rs` - extraction cache key and atomic cache writes.
- `crates/cairn-cli/src/verbs/ingest/extract.rs` - offline keyword extraction, including Java.
- `crates/cairn-cli/src/verbs/ingest/planner.rs` - batches, deterministic operation ids, `FlushPlan` and `MemoryRecord` construction.
- `crates/cairn-cli/src/verbs/ingest/apply.rs` - real store-backed application of folder ingest plans.
- `crates/cairn-cli/src/verbs/ingest/folder.rs` - CLI normalization, runner, JSON/human output.
- `crates/cairn-cli/src/verbs/ingest/report.rs` - `FolderIngestSummary` and renderer.

---

### Task 1: Integrate Current `origin/main`

**Files:**
- Modify: repository merge state
- Preserve: `docs/superpowers/specs/2026-05-07-folder-ingest-live-issue-design.md`
- Preserve: existing folder ingest commits as history

- [ ] **Step 1: Confirm clean branch**

Run:

```bash
git status --short --branch
git rev-parse --short HEAD
git rev-list --left-right --count HEAD...origin/main
```

Expected:

- status has no modified files
- current branch is `codex/issue-188-folder-ingest-design`
- output shows this branch is behind `origin/main`

- [ ] **Step 2: Merge current main**

Run:

```bash
git fetch origin
git merge origin/main
```

Expected:

- conflicts are likely
- do not use `git reset --hard`
- resolve conflicts by taking `origin/main` for core/store/domain generated infrastructure and reapplying folder ingest only in the ingest-specific files listed above

- [ ] **Step 3: Resolve module layout**

Keep this Rust module shape:

```text
crates/cairn-cli/src/verbs/ingest.rs
crates/cairn-cli/src/verbs/ingest/cache.rs
crates/cairn-cli/src/verbs/ingest/extract.rs
crates/cairn-cli/src/verbs/ingest/folder.rs
crates/cairn-cli/src/verbs/ingest/patterns.rs
crates/cairn-cli/src/verbs/ingest/planner.rs
crates/cairn-cli/src/verbs/ingest/apply.rs
crates/cairn-cli/src/verbs/ingest/report.rs
crates/cairn-cli/src/verbs/ingest/scanner.rs
```

At the top of `crates/cairn-cli/src/verbs/ingest.rs`, after the existing imports, declare:

```rust
mod apply;
mod cache;
mod extract;
mod folder;
mod patterns;
mod planner;
pub mod report;
mod scanner;
```

- [ ] **Step 4: Compile enough to expose next failures**

Run:

```bash
cargo check -p cairn-cli
```

Expected:

- PASS if merge conflicts were fully resolved
- or FAIL only on missing folder ingest contract/API that subsequent tasks address

- [ ] **Step 5: Commit merge baseline**

Run:

```bash
git add .
git commit -m "chore: merge main for live folder ingest"
```

Expected: merge or conflict-resolution commit recorded.

---

### Task 2: IDL Contract For Folder And Batch Size

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/ingest.json`
- Modify: `crates/cairn-core/tests/generated_wire.rs`
- Modify: `crates/cairn-idl/tests/schema_discriminator.rs` when `schema_discriminator` reports the intentional `mode.oneOf` violation
- Generated by codegen: `crates/cairn-core/src/generated/verbs/ingest.rs`
- Generated by codegen: `crates/cairn-cli/src/generated/verbs.rs`
- Generated by codegen: `crates/cairn-mcp/src/generated/schemas/verbs/ingest*.json`
- Generated by codegen: `crates/cairn-sdk/src/generated/verbs/ingest.rs`
- Generated by codegen: `skills/cairn/verbs/ingest.md` if present after merge

- [ ] **Step 1: Write failing generated-wire tests**

Append these tests to `crates/cairn-core/tests/generated_wire.rs` near the existing ingest tests:

```rust
#[test]
fn ingest_args_accepts_folder_batch_and_mode() {
    let args: cairn_core::generated::verbs::ingest::IngestArgs =
        serde_json::from_value(serde_json::json!({
            "kind": "reference",
            "folder": "docs",
            "recursive": true,
            "include": ["*.md", "*.java"],
            "exclude": ["target"],
            "mode": "keyword",
            "dry_run": true,
            "batch_size": 2
        }))
        .expect("folder args deserialize");

    assert_eq!(args.kind, "reference");
    assert_eq!(args.folder.as_deref(), Some("docs"));
    assert_eq!(args.recursive, Some(true));
    assert_eq!(args.include.as_deref(), Some(&["*.md".to_owned(), "*.java".to_owned()][..]));
    assert_eq!(args.exclude.as_deref(), Some(&["target".to_owned()][..]));
    assert!(matches!(
        args.mode,
        Some(cairn_core::generated::verbs::ingest::IngestMode::Keyword)
    ));
    assert_eq!(args.dry_run, Some(true));
    assert_eq!(args.batch_size, Some(2));
}

#[test]
fn ingest_args_rejects_folder_combined_with_body() {
    let err = serde_json::from_value::<cairn_core::generated::verbs::ingest::IngestArgs>(
        serde_json::json!({
            "kind": "reference",
            "folder": "docs",
            "body": "hello"
        }),
    )
    .expect_err("folder/body XOR must reject");

    assert!(
        err.to_string().contains("oneOf") || err.to_string().contains("exactly one"),
        "unexpected error: {err}"
    );
}
```

- [ ] **Step 2: Run RED tests**

Run:

```bash
cargo test -p cairn-core --test generated_wire ingest_args_accepts_folder_batch_and_mode -- --exact
cargo test -p cairn-core --test generated_wire ingest_args_rejects_folder_combined_with_body -- --exact
```

Expected: FAIL because generated `IngestArgs` has no `folder`, `mode`, or `batch_size`.

- [ ] **Step 3: Update IDL schema**

In `crates/cairn-idl/schema/verbs/ingest.json`:

- add CLI flags for `folder`, `recursive`, `include`, `exclude`, `mode`, and `batch_size`
- do not add a generated CLI flag for `dry_run`, because current `crates/cairn-cli/src/verbs/mod.rs::with_flush_modes` already supplies `--dry-run`
- update positional description from `--body/--file/--url` to `--body/--file/--url/--folder`
- add schema properties:

```json
"folder": {
  "type": "string",
  "minLength": 1,
  "description": "Folder to scan for offline folder ingest."
},
"recursive": {
  "type": "boolean",
  "default": true,
  "description": "Whether folder ingest recurses into child directories. CLI default: true."
},
"include": {
  "type": "array",
  "items": { "type": "string", "minLength": 1 },
  "description": "Include glob patterns for folder ingest."
},
"exclude": {
  "type": "array",
  "items": { "type": "string", "minLength": 1 },
  "description": "Exclude glob patterns for folder ingest."
},
"mode": {
  "oneOf": [
    { "const": "keyword" },
    { "const": "semantic" },
    { "const": "full" }
  ],
  "default": "keyword",
  "description": "Folder ingest extraction mode."
},
"batch_size": {
  "type": "integer",
  "minimum": 1,
  "maximum": 65535,
  "default": 64,
  "description": "Maximum cache-miss files per FlushPlan batch."
}
```

Update `oneOf` to include:

```json
{ "required": ["folder"] }
```

- [ ] **Step 4: Run codegen**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

Expected: generated files update.

- [ ] **Step 5: Run GREEN tests**

Run:

```bash
cargo test -p cairn-core --test generated_wire ingest_args_accepts_folder_batch_and_mode -- --exact
cargo test -p cairn-core --test generated_wire ingest_args_rejects_folder_combined_with_body -- --exact
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

Expected: all pass. If `schema_discriminator` fails on the `mode.oneOf`, add `verbs/ingest.json#/$defs/Args/properties/mode/oneOf` to its allowlist and rerun.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/cairn-idl/schema/verbs/ingest.json crates/cairn-core/tests/generated_wire.rs crates/cairn-idl/tests/schema_discriminator.rs crates/cairn-core/src/generated crates/cairn-cli/src/generated crates/cairn-mcp/src/generated crates/cairn-sdk/src/generated skills
git commit -m "feat(idl): add live folder ingest contract"
```

Expected: commit contains IDL, generated artifacts, and contract tests.

---

### Task 3: Port Scanner, Cache, Pattern, Report, And Java Extraction

**Files:**
- Create/modify: `crates/cairn-cli/src/verbs/ingest/patterns.rs`
- Create/modify: `crates/cairn-cli/src/verbs/ingest/scanner.rs`
- Create/modify: `crates/cairn-cli/src/verbs/ingest/cache.rs`
- Create/modify: `crates/cairn-cli/src/verbs/ingest/extract.rs`
- Create/modify: `crates/cairn-cli/src/verbs/ingest/report.rs`
- Modify: `crates/cairn-cli/src/verbs/ingest.rs`

- [ ] **Step 1: Write failing extraction test for Java**

Add to `crates/cairn-cli/src/verbs/ingest/extract.rs` tests:

```rust
#[test]
fn java_extracts_classes_interfaces_enums_and_methods() {
    let body = r#"
package demo;
public class MainService {
    public void runJob() {}
    private static String label() { return "x"; }
}
interface Worker {}
enum Mode { FAST }
"#;

    let counts = extract_keyword_counts(Path::new("src/MainService.java"), body);

    assert!(
        counts.entities_new >= 5,
        "expected package/class/interface/enum/method entities, got {counts:?}"
    );
}
```

- [ ] **Step 2: Write failing scanner regression tests**

In `crates/cairn-cli/src/verbs/ingest/scanner.rs`, keep or add:

```rust
#[cfg(unix)]
#[test]
fn broken_symlink_counts_as_warning_and_skipped() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    symlink(
        dir.path().join("does-not-exist.md"),
        dir.path().join("missing.md"),
    )
    .unwrap();
    let include = parse_pattern_list(None, &["*.md"]).unwrap();
    let exclude = parse_pattern_list(None, &[]).unwrap();

    let result = scan_folder(dir.path(), true, &include, &exclude).unwrap();

    assert_eq!(rels(&result), Vec::<String>::new());
    assert_eq!(result.warnings.broken_symlinks, 1);
    assert_eq!(result.skipped, 1);
}
```

- [ ] **Step 3: Run RED tests**

Run:

```bash
cargo test -p cairn-cli --lib java_extracts_classes_interfaces_enums_and_methods -- --exact
cargo test -p cairn-cli --lib broken_symlink_counts_as_warning_and_skipped -- --exact
```

Expected: Java test fails until `.java` extraction exists. Scanner test fails if not yet ported.

- [ ] **Step 4: Port modules from the validated old slice**

Restore the earlier implementations with these changes:

- `DEFAULT_INCLUDE` will move to `folder.rs`, but extractor supports `.rst` and `.java`
- `is_supported_keyword_file` returns true for:

```rust
Some("md" | "txt" | "rst" | "rs" | "py" | "ts" | "js" | "go" | "java")
```

- Java extraction uses conservative line regexes:

```rust
static JAVA_DECL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^\s*(?:public|private|protected|static|final|abstract|\s)*\b(class|interface|enum)\s+([A-Za-z_][A-Za-z0-9_]*)|^\s*(?:public|private|protected|static|final|synchronized|abstract|\s)+[A-Za-z_][A-Za-z0-9_<>,\[\]\s]*\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
    )
    .expect("valid Java declaration regex")
});
```

If clippy rejects the static because of `expect`, use a non-panicking `OnceLock` initializer that returns zero Java declarations on impossible regex construction.

- [ ] **Step 5: Run GREEN module tests**

Run:

```bash
cargo test -p cairn-cli --lib verbs::ingest::extract::tests
cargo test -p cairn-cli --lib verbs::ingest::scanner::tests
cargo test -p cairn-cli --lib verbs::ingest::patterns::tests
cargo test -p cairn-cli --lib verbs::ingest::cache::tests
```

Expected: all pass.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/cairn-cli/src/verbs/ingest.rs crates/cairn-cli/src/verbs/ingest/patterns.rs crates/cairn-cli/src/verbs/ingest/scanner.rs crates/cairn-cli/src/verbs/ingest/cache.rs crates/cairn-cli/src/verbs/ingest/extract.rs crates/cairn-cli/src/verbs/ingest/report.rs
git commit -m "feat(cli): restore folder scan cache extraction"
```

Expected: commit contains pure scan/cache/extract/report behavior only.

---

### Task 4: Build Batched FlushPlans With Deterministic Operation IDs

**Files:**
- Create: `crates/cairn-cli/src/verbs/ingest/planner.rs`
- Modify: `crates/cairn-cli/src/verbs/ingest/cache.rs`
- Test: `crates/cairn-cli/src/verbs/ingest/planner.rs`

- [ ] **Step 1: Write failing planner tests**

Create `planner.rs` with test skeletons first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::ingest::extract::ExtractionCounts;
    use std::path::PathBuf;

    fn item(path: &str, hash: &str) -> PlannedFile {
        PlannedFile {
            absolute_path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            body: format!("body for {path}"),
            body_hash: hash.to_owned(),
            cache_key: hash.to_owned(),
            counts: ExtractionCounts {
                entities_new: 1,
                edges_new: 0,
            },
            entities: vec!["Alpha".to_owned()],
            wiki_edges: vec![],
        }
    }

    #[test]
    fn batch_size_two_over_five_files_produces_three_plans() {
        let files = vec![
            item("a.md", &"a".repeat(64)),
            item("b.md", &"b".repeat(64)),
            item("c.md", &"c".repeat(64)),
            item("d.md", &"d".repeat(64)),
            item("e.md", &"e".repeat(64)),
        ];

        let plans = plan_batches(
            Path::new("/tmp/project"),
            files,
            2,
            cairn_core::domain::flush_plan::FlushMode::DryRun,
        )
        .expect("plans");

        assert_eq!(plans.len(), 3);
        assert_eq!(plans[0].plan.mutations.len(), 2);
        assert_eq!(plans[1].plan.mutations.len(), 2);
        assert_eq!(plans[2].plan.mutations.len(), 1);
    }

    #[test]
    fn deterministic_operation_id_is_stable_for_same_batch() {
        let hashes = vec!["a".repeat(64), "b".repeat(64)];
        let first = deterministic_operation_id(Path::new("/tmp/project"), 0, &hashes).unwrap();
        let second = deterministic_operation_id(Path::new("/tmp/project"), 0, &hashes).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn deterministic_operation_id_changes_with_path_body_or_batch() {
        let hashes = vec!["a".repeat(64), "b".repeat(64)];
        let base = deterministic_operation_id(Path::new("/tmp/project"), 0, &hashes).unwrap();
        let changed_path = deterministic_operation_id(Path::new("/tmp/other"), 0, &hashes).unwrap();
        let changed_batch = deterministic_operation_id(Path::new("/tmp/project"), 1, &hashes).unwrap();
        let changed_hash = deterministic_operation_id(
            Path::new("/tmp/project"),
            0,
            &["a".repeat(64), "c".repeat(64)],
        )
        .unwrap();

        assert_ne!(base, changed_path);
        assert_ne!(base, changed_batch);
        assert_ne!(base, changed_hash);
    }
}
```

- [ ] **Step 2: Run RED planner tests**

Run:

```bash
cargo test -p cairn-cli --lib verbs::ingest::planner::tests -- --nocapture
```

Expected: FAIL because planner types/functions do not exist.

- [ ] **Step 3: Implement planner types and operation ID**

Implement public-to-parent types:

```rust
pub struct PlannedFile {
    pub absolute_path: PathBuf,
    pub relative_path: PathBuf,
    pub body: String,
    pub body_hash: String,
    pub cache_key: String,
    pub counts: ExtractionCounts,
    pub entities: Vec<String>,
    pub wiki_edges: Vec<(String, String)>,
}

pub struct FolderPlanBatch {
    pub plan: FlushPlan,
    pub files: Vec<PlannedFile>,
}
```

Implement:

```rust
pub fn deterministic_operation_id(
    folder: &Path,
    batch_index: usize,
    sorted_hashes: &[String],
) -> Result<Ulid, PlannerError>
```

Algorithm:

1. SHA-256 update with slash-normalized folder path bytes.
2. update with `b"\0"`.
3. update with `batch_index.to_be_bytes()`.
4. for each sorted hash, update with `b"\0"` then hash bytes.
5. copy first 16 digest bytes into `[u8; 16]`.
6. return `cairn_core::generated::common::Ulid(ulid::Ulid::from_bytes(bytes).to_string())`.

Implement `plan_batches(folder, files, batch_size, mode)`:

- reject `batch_size == 0`
- chunk files in deterministic input order
- sort each chunk's `body_hash` list for operation-id input
- create one `FlushPlan` per chunk
- create one `PlannedMutation::Upsert` per file using `MemoryRecord` from `build_record_for_file`

- [ ] **Step 4: Implement valid record construction**

Use current-main domain rules. The helper signature:

```rust
fn build_record_for_file(file: &PlannedFile, issued_at: &str) -> Result<MemoryRecord, PlannerError>
```

Record shape:

- `id` and `target_id`: deterministic valid ULID strings derived separately from `cache_key + ":record"` and `cache_key + ":target"`
- `kind`: `MemoryKind::Reference`
- `class`: `MemoryClass::Semantic`
- `visibility`: `MemoryVisibility::Private`
- `scope.user`: `Some("hmn:folder-ingest")`
- `provenance.source_sensor`: `snr:local:folder-ingest:v1`
- `provenance.originating_agent_id`: `hmn:folder-ingest`
- `provenance.source_hash`: `sha256:{body_hash}`
- `provenance.consent_ref`: `consent:folder-ingest`
- `provenance.llm_id_if_any`: `None`
- `actor_chain`: one author entry for `hmn:folder-ingest`
- `signature`: syntactically valid placeholder `ed25519:` plus 128 lowercase `a` characters, matching existing test fixtures until signed dispatch lands
- `extra_frontmatter`: include `folder_relative_path`, `folder_cache_key`, `folder_entities_new`, `folder_edges_new`
- call `record.validate()` before returning

- [ ] **Step 5: Run GREEN planner tests**

Run:

```bash
cargo test -p cairn-cli --lib verbs::ingest::planner::tests -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/cairn-cli/src/verbs/ingest/planner.rs crates/cairn-cli/src/verbs/ingest.rs
git commit -m "feat(cli): plan folder ingest batches"
```

Expected: commit contains planner and deterministic ID tests.

---

### Task 5: Store-Backed Plan Apply Helper

**Files:**
- Create: `crates/cairn-cli/src/verbs/ingest/apply.rs`
- Modify: `crates/cairn-cli/src/verbs/ingest/planner.rs`
- Test: `crates/cairn-cli/src/verbs/ingest/apply.rs`

- [ ] **Step 1: Write failing apply tests with `FixtureStore`**

Create tests in `apply.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::ingest::planner::{plan_batches, PlannedFile};
    use crate::verbs::ingest::extract::ExtractionCounts;
    use cairn_core::domain::flush_plan::FlushMode;
    use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
    use cairn_test_fixtures::store::FixtureStore;
    use std::path::{Path, PathBuf};

    fn file(path: &str, hash_char: char) -> PlannedFile {
        PlannedFile {
            absolute_path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            body: format!("# {path}\n[[Entity]]\n"),
            body_hash: hash_char.to_string().repeat(64),
            cache_key: hash_char.to_string().repeat(64),
            counts: ExtractionCounts {
                entities_new: 2,
                edges_new: 1,
            },
            entities: vec!["Entity".to_owned()],
            wiki_edges: vec![("file".to_owned(), "Entity".to_owned())],
        }
    }

    #[tokio::test]
    async fn apply_plan_upserts_records_and_reports_written() {
        let store = FixtureStore::new();
        let batches = plan_batches(
            Path::new("/tmp/project"),
            vec![file("a.md", 'a'), file("b.md", 'b')],
            64,
            FlushMode::Autonomous,
        )
        .unwrap();

        let stats = apply_batch(&store, &batches[0]).await.unwrap();

        assert_eq!(stats.records_written, 2);
        let page = store.list(&ListArgs::default()).await.unwrap();
        assert_eq!(page.records.len(), 2);
    }

    #[tokio::test]
    async fn apply_plan_is_idempotent_for_same_records() {
        let store = FixtureStore::new();
        let batches = plan_batches(
            Path::new("/tmp/project"),
            vec![file("a.md", 'a')],
            64,
            FlushMode::Autonomous,
        )
        .unwrap();

        let first = apply_batch(&store, &batches[0]).await.unwrap();
        let second = apply_batch(&store, &batches[0]).await.unwrap();

        assert_eq!(first.records_written, 1);
        assert_eq!(second.records_written, 0);
    }
}
```

- [ ] **Step 2: Run RED apply tests**

Run:

```bash
cargo test -p cairn-cli --lib verbs::ingest::apply::tests -- --nocapture
```

Expected: FAIL because `apply_batch` does not exist.

- [ ] **Step 3: Implement apply helper**

Implement:

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ApplyStats {
    pub records_written: u64,
    pub entities_written: u64,
    pub edges_written: u64,
}

pub async fn apply_batch(
    store: &dyn MemoryStore,
    batch: &FolderPlanBatch,
) -> Result<ApplyStats, ApplyError>
```

Rules:

- for every `PlannedMutation::Upsert`, call `store.upsert(&record).await`
- increment `records_written` only when `UpsertOutcome.content_changed` is true
- if `store.capabilities().graph_edges` is true, write graph nodes/edges from `batch.files`
- skip graph writes when graph capability is false
- return errors with file/operation context

For graph IDs, derive valid ULID strings from `cache_key + entity name` and `cache_key + edge tuple`.

- [ ] **Step 4: Run GREEN apply tests**

Run:

```bash
cargo test -p cairn-cli --lib verbs::ingest::apply::tests -- --nocapture
```

Expected: all pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/cairn-cli/src/verbs/ingest/apply.rs crates/cairn-cli/src/verbs/ingest.rs
git commit -m "feat(cli): apply folder ingest plans to store"
```

Expected: commit contains store-backed apply helper and tests.

---

### Task 6: Folder Runner And CLI Routing

**Files:**
- Create/modify: `crates/cairn-cli/src/verbs/ingest/folder.rs`
- Modify: `crates/cairn-cli/src/verbs/ingest.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Modify: `crates/cairn-cli/tests/folder_ingest.rs`
- Modify: `crates/cairn-cli/tests/cli.rs`

- [ ] **Step 1: Write failing CLI tests**

Create or update `crates/cairn-cli/tests/folder_ingest.rs` with these tests:

```rust
use assert_cmd::prelude::*;
use serde_json::Value;
use std::process::Command;

fn bin() -> Command {
    Command::cargo_bin("cairn").expect("cairn binary")
}

#[test]
fn folder_help_exposes_live_flags() {
    let output = bin().args(["ingest", "--help"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--folder"));
    assert!(stdout.contains("--batch-size"));
    assert!(stdout.contains("--mode"));
    assert!(stdout.contains("--dry-run"));
}

#[test]
fn batch_size_zero_exits_usage() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# Alpha").unwrap();

    let output = bin()
        .current_dir(dir.path())
        .args(["ingest", "--folder", ".", "--batch-size", "0", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(64));
}

#[test]
fn dry_run_materializes_plans_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# Alpha\n[[Beta]]").unwrap();
    std::fs::write(dir.path().join("b.java"), "class Main { void run() {} }").unwrap();

    let output = bin()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--folder",
            ".",
            "--mode",
            "keyword",
            "--batch-size",
            "1",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr={}", String::from_utf8_lossy(&output.stderr));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["processed"], 2);
    assert_eq!(json["plans"], 2);
    assert_eq!(json["records_written"], 0);
    assert!(!dir.path().join(".cairn/cache").exists());
    assert!(!dir.path().join(".cairn/cairn.db").exists());
}

#[test]
fn non_dry_run_writes_records_and_second_run_uses_cache() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# Alpha\n[[Beta]]").unwrap();
    std::fs::write(dir.path().join("b.java"), "class Main { void run() {} }").unwrap();

    let first = bin()
        .current_dir(dir.path())
        .args(["ingest", "--folder", ".", "--mode", "keyword", "--batch-size", "1", "--json"])
        .output()
        .unwrap();
    assert!(first.status.success(), "stderr={}", String::from_utf8_lossy(&first.stderr));
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["processed"], 2);
    assert_eq!(first_json["plans"], 2);
    assert_eq!(first_json["records_written"], 2);
    assert!(dir.path().join(".cairn/cache").exists());
    assert!(dir.path().join(".cairn/cairn.db").exists());

    let second = bin()
        .current_dir(dir.path())
        .args(["ingest", "--folder", ".", "--mode", "keyword", "--batch-size", "1", "--json"])
        .output()
        .unwrap();
    assert!(second.status.success(), "stderr={}", String::from_utf8_lossy(&second.stderr));
    let second_json: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["cached"], 2);
    assert_eq!(second_json["processed"], 0);
    assert_eq!(second_json["records_written"], 0);
}
```

- [ ] **Step 2: Run RED CLI tests**

Run:

```bash
cargo test -p cairn-cli --test folder_ingest folder_help_exposes_live_flags -- --exact
cargo test -p cairn-cli --test folder_ingest dry_run_materializes_plans_without_writes -- --exact
cargo test -p cairn-cli --test folder_ingest non_dry_run_writes_records_and_second_run_uses_cache -- --exact
```

Expected: fail until routing/runner exists on current-main.

- [ ] **Step 3: Change `main.rs` ingest dispatch**

In `crates/cairn-cli/src/main.rs`, replace:

```rust
Some(("ingest", sub)) => verbs::ingest::run(sub),
```

with:

```rust
Some(("ingest", sub)) => match resolve_vault_or_cwd(explicit_vault.as_deref()) {
    Ok((vault_root, _source)) => verbs::ingest::run(sub, vault_root),
    Err(e) => {
        eprintln!("cairn ingest: vault resolution error - {e:#}");
        ExitCode::from(78)
    }
},
```

Update `crates/cairn-cli/src/verbs/ingest.rs` signature:

```rust
pub fn run(sub: &ArgMatches, vault_root: std::path::PathBuf) -> ExitCode
```

Keep current-main `--resync` behavior and non-folder stub behavior intact.

- [ ] **Step 4: Route folder before generic flush stub**

In `ingest.rs`, compute `has_folder` before the current dry-run/human-review branch:

```rust
let has_folder = sub.get_one::<std::path::PathBuf>("folder").is_some();
if has_folder {
    return folder::run(sub, vault_root);
}
```

Then update source XOR in the non-folder path to include folder in the error message, even though folder already returned:

```rust
let source_count =
    u8::from(has_source) + u8::from(has_body) + u8::from(has_file) + u8::from(has_url) + u8::from(has_folder);
```

- [ ] **Step 5: Implement folder runner**

`folder.rs` responsibilities:

- normalize `--folder`, `--include`, `--exclude`, `--mode`, `--batch-size`
- use wrapper flag `sub.get_flag("dry-run")`
- fail semantic/full with `CapabilityUnavailable` exit `78`
- scan folder
- for each scan entry:
  - skip unsupported extension with warning/skipped
  - read UTF-8 body or warn/skip invalid UTF-8
  - compute body-for-cache and cache key
  - read cache hit and count cached
  - extract keyword counts/entities/edges
  - add `PlannedFile` for cache miss
- build `FlushPlan` batches
- dry-run: do not create `.cairn`, cache, DB, or plan files
- non-dry-run:
  - create `vault_root/.cairn`
  - open `cairn_store_sqlite::open(&vault_root.join(".cairn/cairn.db")).await`
  - apply each batch with `apply_batch`
  - write cache entries after the corresponding batch applies successfully
- render JSON/human summary

Use a current-thread runtime in `folder::run`:

```rust
let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build();
```

- [ ] **Step 6: Run GREEN CLI tests**

Run:

```bash
cargo test -p cairn-cli --test folder_ingest
cargo test -p cairn-cli --test cli
```

Expected: all pass.

- [ ] **Step 7: Commit**

Run:

```bash
git add crates/cairn-cli/src/main.rs crates/cairn-cli/src/verbs/ingest.rs crates/cairn-cli/src/verbs/ingest/folder.rs crates/cairn-cli/tests/folder_ingest.rs crates/cairn-cli/tests/cli.rs
git commit -m "feat(cli): wire live folder ingest runner"
```

Expected: CLI routing and runner commit.

---

### Task 7: Crash/Retry And Batch Idempotency Coverage

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/apply.rs`
- Modify: `crates/cairn-cli/tests/folder_ingest.rs`
- Test: `crates/cairn-cli/src/verbs/ingest/planner.rs`

- [ ] **Step 1: Write failing retry test**

Add to `crates/cairn-cli/tests/folder_ingest.rs`:

```rust
#[test]
fn changed_second_batch_resumes_without_rewriting_first_batch() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.md", "b.md", "c.md"] {
        std::fs::write(dir.path().join(name), format!("# {name}\n[[Entity]]")).unwrap();
    }

    let first = bin()
        .current_dir(dir.path())
        .args(["ingest", "--folder", ".", "--mode", "keyword", "--batch-size", "2", "--json"])
        .output()
        .unwrap();
    assert!(first.status.success(), "stderr={}", String::from_utf8_lossy(&first.stderr));
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["plans"], 2);
    let first_ops = first_json["operation_ids"].as_array().unwrap().clone();

    std::fs::write(dir.path().join("c.md"), "# c changed\n[[Entity]]\n[[Other]]").unwrap();

    let second = bin()
        .current_dir(dir.path())
        .args(["ingest", "--folder", ".", "--mode", "keyword", "--batch-size", "2", "--json"])
        .output()
        .unwrap();
    assert!(second.status.success(), "stderr={}", String::from_utf8_lossy(&second.stderr));
    let second_json: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["cached"], 2);
    assert_eq!(second_json["processed"], 1);
    assert_eq!(second_json["plans"], 1);
    assert_ne!(second_json["operation_ids"][0], first_ops[1]);
}
```

- [ ] **Step 2: Run RED retry test**

Run:

```bash
cargo test -p cairn-cli --test folder_ingest changed_second_batch_resumes_without_rewriting_first_batch -- --exact
```

Expected: FAIL until cache filtering and deterministic operation IDs interact correctly.

- [ ] **Step 3: Keep retry behavior cache-first and upsert-idempotent**

Rely on cache filtering and `MemoryStore::upsert` idempotency for this task. Already cached files never enter a retry plan. Files that do re-enter a plan because their content changed produce a new deterministic operation id. Do not report duplicate writes when `MemoryStore::upsert` returns `content_changed == false`.

- [ ] **Step 4: Run GREEN retry test**

Run:

```bash
cargo test -p cairn-cli --test folder_ingest changed_second_batch_resumes_without_rewriting_first_batch -- --exact
```

Expected: pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/cairn-cli/src/verbs/ingest/apply.rs crates/cairn-cli/tests/folder_ingest.rs
git commit -m "test(cli): cover folder ingest retry behavior"
```

Expected: retry/idempotency test commit.

---

### Task 8: Snapshot, Docs Integration, And Real CLI Matrix

**Files:**
- Modify: `crates/cairn-cli/tests/folder_ingest_snapshot.rs`
- Modify: `crates/cairn-cli/tests/snapshots/folder_ingest_snapshot__folder_ingest_human.snap`
- Modify: `crates/cairn-cli/tests/folder_ingest.rs`

- [ ] **Step 1: Write/update human snapshot test**

Use `FolderIngestSummary` with plan fields:

```rust
#[test]
fn folder_ingest_human() {
    let summary = FolderIngestSummary {
        scanned: 5,
        cached: 1,
        processed: 4,
        skipped: 0,
        warnings: 0,
        entities_new: 12,
        entities_merged: 0,
        edges_new: 3,
        contradictions_resolved: 0,
        records_written: 4,
        plans: 2,
        batch_size: 2,
        operation_ids: vec![
            "01HQZX9F5N0000000000000000".to_owned(),
            "01HQZX9F5N0000000000000001".to_owned(),
        ],
        elapsed_ms: 1200,
        dry_run: false,
        mode: "keyword".to_owned(),
    };

    insta::assert_snapshot!("folder_ingest_human", render_human("./docs", &summary));
}
```

- [ ] **Step 2: Run RED/UPDATE snapshot**

Run:

```bash
cargo test -p cairn-cli --test folder_ingest_snapshot
```

Expected: snapshot mismatch until accepted.

Review the new snapshot, then accept:

```bash
INSTA_UPDATE=always cargo test -p cairn-cli --test folder_ingest_snapshot
```

- [ ] **Step 3: Ensure docs integration remains real CLI**

Add or keep test:

```rust
#[test]
fn docs_folder_keyword_dry_run_extracts_entities_and_plans() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();

    let output = bin()
        .current_dir(&repo)
        .args([
            "ingest",
            "--folder",
            "docs",
            "--mode",
            "keyword",
            "--batch-size",
            "64",
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr={}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["entities_new"].as_u64().unwrap() > 0);
    assert!(json["plans"].as_u64().unwrap() > 0);
    assert_eq!(json["records_written"], 0);
}
```

- [ ] **Step 4: Run GREEN snapshot/docs tests**

Run:

```bash
cargo test -p cairn-cli --test folder_ingest_snapshot
cargo test -p cairn-cli --test folder_ingest docs_folder_keyword_dry_run_extracts_entities_and_plans -- --exact
```

Expected: pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/cairn-cli/tests/folder_ingest_snapshot.rs crates/cairn-cli/tests/snapshots/folder_ingest_snapshot__folder_ingest_human.snap crates/cairn-cli/tests/folder_ingest.rs
git commit -m "test(cli): verify folder ingest output"
```

Expected: snapshot and docs integration commit.

---

### Task 9: Full Verification And Real CLI E2E

**Files:**
- No intended source changes unless verification reveals a defect.

- [ ] **Step 1: Run codegen check**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

Expected: clean.

- [ ] **Step 2: Run workspace checks**

Run:

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/check-core-boundary.sh
git diff --check
```

Expected: all pass.

- [ ] **Step 3: Run manual real CLI matrix**

Run:

```bash
cargo build -p cairn-cli
target/debug/cairn ingest --folder docs --mode keyword --dry-run --json
```

Expected:

- exit `0`
- `entities_new > 0`
- `plans > 0`
- `records_written == 0`

Then:

```bash
tmp=$(mktemp -d)
mkdir -p "$tmp/project/src"
printf '# Alpha\n[[Beta]]\nTODO: fix\n' > "$tmp/project/notes.md"
printf 'class Main { void runJob() {} }\n' > "$tmp/project/src/Main.java"
printf 'not supported\n' > "$tmp/project/image.png"
cd "$tmp"
/Users/tafeng/.codex/worktrees/cb34/cairn/target/debug/cairn ingest --folder project --mode keyword --batch-size 1 --include '*.md,*.java,*.png' --json
/Users/tafeng/.codex/worktrees/cb34/cairn/target/debug/cairn ingest --folder project --mode keyword --batch-size 1 --include '*.md,*.java,*.png' --json
```

Expected first run:

- exit `0`
- `processed == 2`
- `skipped == 1`
- `warnings == 1`
- `plans == 2`
- `records_written == 2`
- `.cairn/cairn.db` exists
- `.cairn/cache` has 2 files

Expected second run:

- exit `0`
- `cached == 2`
- `processed == 0`
- `records_written == 0`

- [ ] **Step 4: Handle verification fixes**

When verification reveals a source defect, return to the task that owns that file, add a focused failing test for the defect, implement the fix, rerun the relevant focused test, and commit with the task's affected file list. When verification reveals no source defect, do not create an empty commit.

---

## Self-Review Checklist

- Spec coverage: tasks cover rebase/main integration, IDL, batch size, Java extraction, deterministic operation ids, dry-run materialized plans, non-dry-run store writes, cache second run, symlink/hidden/unsupported behavior, docs integration, and real CLI E2E.
- TDD coverage: each behavior task begins with failing tests before implementation.
- No generated files are hand-edited; codegen owns SDK/MCP/CLI generated surfaces.
- `--dry-run` is not duplicated in generated clap flags; current `with_flush_modes` remains the CLI source of that flag.
- `records_written` counts store upserts only, never cache writes.
- `cairn flush apply` metadata-only placeholder is not used for folder ingest non-dry-run.
