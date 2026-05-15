# Issue 97 Replay Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a dev-only local replay harness that loads P0 scenario fixtures, executes deterministic replay checks against a temp SQLite vault, and returns machine-readable reports.

**Architecture:** Add a `cairn_test_fixtures::replay` module plus versioned JSON fixtures under `fixtures/v0/replay/`. The harness seeds a temp vault with deterministic `MemoryRecord`s, uses `MockEmbedder` for local semantic/hybrid search, dispatches search through `cairn_core::verbs::search::run`, applies record-level tombstones through `MemoryStore`, and compares actual outcomes to fixture expectations.

**Tech Stack:** Rust 2024, `cairn-test-fixtures`, `cairn-core`, `cairn-store-sqlite`, `cairn-embeddings-local::MockEmbedder`, `serde`, `serde_json`, `tokio`, `tempfile`.

---

### Task 1: Add Replay Fixture Manifests

**Files:**
- Create: `fixtures/v0/replay/p0_stories.json`
- Create: `fixtures/v0/replay/p0_keyword_only.json`
- Modify: `fixtures/README.md`

- [ ] **Step 1: Create the fixture directory**

Run:

```bash
mkdir -p fixtures/v0/replay
```

Expected: command exits 0.

- [ ] **Step 2: Add `fixtures/v0/replay/p0_stories.json`**

Create the file with this content:

```json
{
  "id": "p0_stories",
  "description": "P0 replay scenario covering US1-US5, US7 all search modes, and US8 record-level forget.",
  "config": {
    "local_embeddings": true
  },
  "records": [
    {
      "id": "01HQZX9F5N0000000000000A0",
      "kind": "trace",
      "class": "episodic",
      "visibility": "private",
      "body": "rust memory safety session replay user question",
      "session_id": "p0-session",
      "turn_id": "1",
      "sequence": 1,
      "trace_event": "user_message"
    },
    {
      "id": "01HQZX9F5N0000000000000A1",
      "kind": "trace",
      "class": "episodic",
      "visibility": "private",
      "body": "ownership borrowing prevent memory bugs at compile time",
      "session_id": "p0-session",
      "turn_id": "1",
      "sequence": 2,
      "trace_event": "assistant_message"
    },
    {
      "id": "01HQZX9F5N0000000000000A2",
      "kind": "trace",
      "class": "episodic",
      "visibility": "private",
      "body": "cargo check tool result succeeded for memory safety example",
      "session_id": "p0-session",
      "turn_id": "1",
      "sequence": 3,
      "trace_event": "tool_call",
      "tool_name": "cargo"
    },
    {
      "id": "01HQZX9F5N0000000000000A3",
      "kind": "user",
      "class": "semantic",
      "visibility": "private",
      "body": "user prefers concise rust memory explanations"
    },
    {
      "id": "01HQZX9F5N0000000000000A4",
      "kind": "reasoning",
      "class": "semantic",
      "visibility": "private",
      "body": "rolling summary p0 session covers rust memory safety and cargo check success",
      "session_id": "p0-session",
      "turn_id": "summary",
      "sequence": 4,
      "trace_event": "turn_summary"
    },
    {
      "id": "01HQZX9F5N0000000000000A5",
      "kind": "fact",
      "class": "semantic",
      "visibility": "private",
      "body": "secret project codename aurora should be forgotten"
    },
    {
      "id": "01HQZX9F5N0000000000000A6",
      "kind": "fact",
      "class": "semantic",
      "visibility": "private",
      "body": "unrelated cooking note tomato soup"
    }
  ],
  "actions": [
    {
      "verb": "retrieve_session",
      "story": "US1_US2",
      "session_id": "p0-session",
      "expected_turn_ids": ["1"],
      "expected_trace_events": ["user_message", "assistant_message", "tool_call", "turn_summary"]
    },
    {
      "verb": "retrieve_turn",
      "story": "US5",
      "session_id": "p0-session",
      "turn_id": "1",
      "expected_trace_events": ["user_message", "assistant_message", "tool_call"]
    },
    {
      "verb": "record_present",
      "story": "US3",
      "record_id": "01HQZX9F5N0000000000000A3",
      "expected_present": true
    },
    {
      "verb": "record_present",
      "story": "US4",
      "record_id": "01HQZX9F5N0000000000000A4",
      "expected_present": true
    },
    {
      "verb": "search",
      "story": "US7",
      "mode": "keyword",
      "query": "ownership borrowing",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N0000000000000A1"]
      }
    },
    {
      "verb": "search",
      "story": "US7",
      "mode": "semantic",
      "query": "user prefers concise rust memory explanations",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N0000000000000A3"]
      }
    },
    {
      "verb": "search",
      "story": "US7",
      "mode": "hybrid",
      "query": "rust memory safety session replay user question",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N0000000000000A0"]
      }
    },
    {
      "verb": "forget_record",
      "story": "US8",
      "record_id": "01HQZX9F5N0000000000000A5",
      "followup_query": "secret project codename aurora",
      "expected_absent_from_search": true
    }
  ]
}
```

- [ ] **Step 3: Add `fixtures/v0/replay/p0_keyword_only.json`**

Create the file with this content:

```json
{
  "id": "p0_keyword_only",
  "description": "P0 degraded keyword-only replay scenario with local embeddings disabled.",
  "config": {
    "local_embeddings": false
  },
  "records": [
    {
      "id": "01HQZX9F5N0000000000000B0",
      "kind": "fact",
      "class": "semantic",
      "visibility": "private",
      "body": "keyword only mode still finds rust memory safety"
    }
  ],
  "actions": [
    {
      "verb": "search",
      "story": "US7",
      "mode": "keyword",
      "query": "rust memory safety",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N0000000000000B0"]
      }
    },
    {
      "verb": "search",
      "story": "US7",
      "mode": "semantic",
      "query": "keyword only mode still finds rust memory safety",
      "limit": 1,
      "expected": {
        "status": "capability_unavailable",
        "capability": "cairn.mcp.v1.search.semantic"
      }
    },
    {
      "verb": "search",
      "story": "US7",
      "mode": "hybrid",
      "query": "keyword only mode still finds rust memory safety",
      "limit": 1,
      "expected": {
        "status": "capability_unavailable",
        "capability": "cairn.mcp.v1.search.hybrid"
      }
    }
  ]
}
```

- [ ] **Step 4: Document the replay fixtures**

Append this bullet to the `fixtures/v0/` tree in `fixtures/README.md`:

```markdown
    ├── replay/            ← P0 replay scenarios + golden expectations
```

Add one sentence after the existing fixture description:

```markdown
Replay fixtures are consumed by `cairn_test_fixtures::replay` to create temp vaults and emit deterministic machine-readable reports for CI gates.
```

- [ ] **Step 5: Verify fixture files are visible**

Run:

```bash
find fixtures/v0/replay -maxdepth 1 -type f | sort
```

Expected output includes both JSON files.

### Task 2: Write Failing Replay Harness Tests

**Files:**
- Create: `crates/cairn-test-fixtures/tests/replay_harness.rs`

- [ ] **Step 1: Add failing integration tests**

Create `crates/cairn-test-fixtures/tests/replay_harness.rs` with:

```rust
use cairn_test_fixtures::replay::{run_named_scenario, load_named_scenario, ReplayExpectation};

#[tokio::test(flavor = "multi_thread")]
async fn p0_stories_replay_passes_end_to_end() {
    let report = run_named_scenario("p0_stories").await.expect("run scenario");
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.scenario_id, "p0_stories");
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.story == "US7" && check.verb == "search")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keyword_only_replay_reports_capability_rejections() {
    let report = run_named_scenario("p0_keyword_only").await.expect("run scenario");
    assert!(report.passed(), "{report:#?}");
    let capabilities: Vec<_> = report
        .checks
        .iter()
        .filter(|check| check.actual["status"] == "capability_unavailable")
        .map(|check| check.actual["capability"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        capabilities,
        vec![
            "cairn.mcp.v1.search.semantic".to_owned(),
            "cairn.mcp.v1.search.hybrid".to_owned()
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failure_report_identifies_scenario_verb_query_expected_and_actual() {
    let mut scenario = load_named_scenario("p0_stories").expect("load scenario");
    let search = scenario
        .actions
        .iter_mut()
        .find_map(|action| action.as_search_mut())
        .expect("search action");
    search.expected = ReplayExpectation::Hits {
        record_ids: vec!["01HQZX9F5N0000000000000A6".to_owned()],
    };

    let report = cairn_test_fixtures::replay::run_scenario(&scenario)
        .await
        .expect("run scenario");
    assert!(!report.passed(), "{report:#?}");
    let failure = report.failures().next().expect("one failure");
    assert_eq!(failure.scenario_id, "p0_stories");
    assert_eq!(failure.verb, "search");
    assert_eq!(failure.query.as_deref(), Some("ownership borrowing"));
    assert_eq!(
        failure.expected,
        serde_json::json!({
            "status": "hits",
            "record_ids": ["01HQZX9F5N0000000000000A6"]
        })
    );
    assert_ne!(failure.expected, failure.actual);
}

#[test]
fn replay_manifests_deserialize() {
    for name in ["p0_stories", "p0_keyword_only"] {
        let scenario = load_named_scenario(name).expect("load scenario");
        assert_eq!(scenario.id, name);
        assert!(!scenario.records.is_empty());
        assert!(!scenario.actions.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo nextest run -p cairn-test-fixtures --test replay_harness
```

Expected: compile fails because `cairn_test_fixtures::replay` does not exist.

### Task 3: Implement `cairn_test_fixtures::replay`

**Files:**
- Create: `crates/cairn-test-fixtures/src/replay.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`

- [ ] **Step 1: Export the new module**

Add to `crates/cairn-test-fixtures/src/lib.rs`:

```rust
pub mod replay;
```

- [ ] **Step 2: Implement the replay module**

Create `crates/cairn-test-fixtures/src/replay.rs` with:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::config::{CairnConfig, EmbeddingModelKind};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore, TombstoneReason};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use cairn_core::domain::{RecordId, Rfc3339Timestamp, ScopeTuple, TargetId};
use cairn_core::verbs::search::{self, SearchError, SearchMode, SearchRequest};
use cairn_embeddings_local::{EmbeddingModel, MockEmbedder};
use cairn_store_sqlite::SqliteMemoryStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Machine-readable report for one replay scenario.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayReport {
    /// Scenario identifier from the manifest.
    pub scenario_id: String,
    /// One check report per manifest action.
    pub checks: Vec<ReplayCheckReport>,
}

impl ReplayReport {
    /// True when every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }

    /// Failed checks only.
    pub fn failures(&self) -> impl Iterator<Item = &ReplayCheckReport> {
        self.checks.iter().filter(|check| !check.passed)
    }
}

/// Machine-readable result for one replay action.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayCheckReport {
    /// Scenario identifier.
    pub scenario_id: String,
    /// User story label, e.g. `US7`.
    pub story: String,
    /// Verb or replay action under test.
    pub verb: String,
    /// Query text when the action is query-shaped.
    pub query: Option<String>,
    /// Expected normalized result.
    pub expected: Value,
    /// Actual normalized result.
    pub actual: Value,
    /// Whether expected and actual matched.
    pub passed: bool,
    /// Short diagnostic on failure.
    pub message: Option<String>,
}

/// Versioned replay scenario manifest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayScenario {
    /// Stable scenario id.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Scenario runtime knobs.
    #[serde(default)]
    pub config: ReplayConfig,
    /// Records to seed into the temp vault.
    pub records: Vec<ReplayRecord>,
    /// Ordered replay actions.
    pub actions: Vec<ReplayAction>,
}

/// Scenario runtime knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    /// Whether local semantic/hybrid capabilities are advertised.
    #[serde(default = "default_local_embeddings")]
    pub local_embeddings: bool,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            local_embeddings: default_local_embeddings(),
        }
    }
}

const fn default_local_embeddings() -> bool {
    true
}

/// Seed record manifest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRecord {
    /// Record id and default target id.
    pub id: String,
    /// Optional stable target id; defaults to `id`.
    #[serde(default)]
    pub target_id: Option<String>,
    /// Memory kind.
    pub kind: MemoryKind,
    /// Memory class.
    pub class: MemoryClass,
    /// Visibility tier.
    pub visibility: MemoryVisibility,
    /// Record body.
    pub body: String,
    /// Optional session id for trace-shaped records.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional turn id for trace-shaped records.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// Optional sequence for trace ordering.
    #[serde(default)]
    pub sequence: Option<u64>,
    /// Optional trace event label.
    #[serde(default)]
    pub trace_event: Option<String>,
    /// Optional tool name for tool-call records.
    #[serde(default)]
    pub tool_name: Option<String>,
}

/// Ordered replay action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayAction {
    /// Search query expectation.
    Search(ReplaySearchAction),
    /// Session replay expectation.
    RetrieveSession {
        /// User story label.
        story: String,
        /// Session id to inspect.
        session_id: String,
        /// Expected distinct turn ids.
        expected_turn_ids: Vec<String>,
        /// Expected trace events in sequence order.
        expected_trace_events: Vec<String>,
    },
    /// Single-turn replay expectation.
    RetrieveTurn {
        /// User story label.
        story: String,
        /// Session id to inspect.
        session_id: String,
        /// Turn id to inspect.
        turn_id: String,
        /// Expected trace events in sequence order.
        expected_trace_events: Vec<String>,
    },
    /// Direct record presence check.
    RecordPresent {
        /// User story label.
        story: String,
        /// Record id to inspect.
        record_id: String,
        /// Expected presence.
        expected_present: bool,
    },
    /// Record-level forget expectation.
    ForgetRecord {
        /// User story label.
        story: String,
        /// Record id to tombstone.
        record_id: String,
        /// Follow-up keyword query.
        followup_query: String,
        /// Whether the record must be absent from follow-up search.
        expected_absent_from_search: bool,
    },
}

impl ReplayAction {
    /// Mutable search-action view for tests that need to perturb expectations.
    pub fn as_search_mut(&mut self) -> Option<&mut ReplaySearchAction> {
        match self {
            Self::Search(action) => Some(action),
            _ => None,
        }
    }
}

/// Search replay action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaySearchAction {
    /// User story label.
    pub story: String,
    /// Search mode.
    pub mode: ReplaySearchMode,
    /// Query string.
    pub query: String,
    /// Result limit.
    pub limit: usize,
    /// Expected outcome.
    pub expected: ReplayExpectation,
}

/// Search mode in scenario manifests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaySearchMode {
    /// Keyword search.
    Keyword,
    /// Semantic search.
    Semantic,
    /// Hybrid search.
    Hybrid,
}

impl ReplaySearchMode {
    fn to_core(self) -> SearchMode {
        match self {
            Self::Keyword => SearchMode::Keyword,
            Self::Semantic => SearchMode::Semantic,
            Self::Hybrid => SearchMode::Hybrid,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Expected search outcome.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayExpectation {
    /// Expected record ids, in order.
    Hits {
        /// Expected record ids.
        record_ids: Vec<String>,
    },
    /// Expected capability rejection.
    CapabilityUnavailable {
        /// Missing capability name.
        capability: String,
    },
}

/// Errors from loading or running replay scenarios.
#[derive(Debug)]
pub enum ReplayError {
    /// Fixture file read failed.
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Source error.
        source: std::io::Error,
    },
    /// Fixture JSON failed to parse.
    Json {
        /// Path that failed.
        path: PathBuf,
        /// Source error.
        source: serde_json::Error,
    },
    /// Scenario data was malformed.
    InvalidManifest(String),
    /// Store setup or operation failed.
    Store(String),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "read {}: {source}", path.display()),
            Self::Json { path, source } => write!(f, "parse {}: {source}", path.display()),
            Self::InvalidManifest(message) => write!(f, "invalid replay manifest: {message}"),
            Self::Store(message) => write!(f, "store error: {message}"),
        }
    }
}

impl std::error::Error for ReplayError {}

struct ReplayVault {
    _dir: TempDir,
    store: SqliteMemoryStore,
    _embedder: Arc<dyn EmbeddingModel>,
}

/// Load a named scenario from `fixtures/v0/replay/{name}.json`.
pub fn load_named_scenario(name: &str) -> Result<ReplayScenario, ReplayError> {
    let path = crate::fixture_v0_dir()
        .join("replay")
        .join(format!("{name}.json"));
    load_scenario_file(&path)
}

/// Load a scenario from a specific file path.
pub fn load_scenario_file(path: &Path) -> Result<ReplayScenario, ReplayError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ReplayError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| ReplayError::Json {
        path: path.to_path_buf(),
        source,
    })
}

/// Load and run a named scenario.
pub async fn run_named_scenario(name: &str) -> Result<ReplayReport, ReplayError> {
    let scenario = load_named_scenario(name)?;
    run_scenario(&scenario).await
}

/// Run a loaded replay scenario.
pub async fn run_scenario(scenario: &ReplayScenario) -> Result<ReplayReport, ReplayError> {
    let vault = build_vault(scenario).await?;
    let mut checks = Vec::with_capacity(scenario.actions.len());
    for action in &scenario.actions {
        checks.push(run_action(&vault.store, scenario, action).await);
    }
    Ok(ReplayReport {
        scenario_id: scenario.id.clone(),
        checks,
    })
}

async fn build_vault(scenario: &ReplayScenario) -> Result<ReplayVault, ReplayError> {
    let dir = TempDir::new().map_err(|e| ReplayError::Store(format!("tempdir: {e}")))?;
    let root = dir.path().to_path_buf();
    let cairn_dir = root.join(".cairn");
    std::fs::create_dir_all(&cairn_dir)
        .map_err(|e| ReplayError::Store(format!("create {}: {e}", cairn_dir.display())))?;
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .map_err(|e| ReplayError::Store(format!("write vault.id: {e}")))?;

    let embedder: Arc<dyn EmbeddingModel> =
        Arc::new(MockEmbedder::new(EmbeddingModelKind::default()));
    let db_path = cairn_dir.join("cairn.db");
    let store = cairn_store_sqlite::open_with_embedder(&db_path, Some(Arc::clone(&embedder)))
        .await
        .map_err(|e| ReplayError::Store(format!("open {}: {e}", db_path.display())))?;

    for (index, seed) in scenario.records.iter().enumerate() {
        let record = seed_record(seed, index)?;
        store
            .upsert(&record)
            .await
            .map_err(|e| ReplayError::Store(format!("upsert {}: {e}", seed.id)))?;
    }

    if let Some(conn) = store.raw_conn_for_admin() {
        let conn = Arc::clone(conn);
        for _ in 0..1000 {
            let stats = cairn_store_sqlite::drain_once(Arc::clone(&conn), Arc::clone(&embedder))
                .await
                .map_err(|e| ReplayError::Store(format!("drain embeddings: {e}")))?;
            if stats.remaining == 0 {
                break;
            }
        }
    }

    Ok(ReplayVault {
        _dir: dir,
        store,
        _embedder: embedder,
    })
}

fn seed_record(seed: &ReplayRecord, index: usize) -> Result<MemoryRecord, ReplayError> {
    let mut record = crate::sample_record(index as u64 + 1);
    record.id = RecordId::parse(seed.id.clone()).map_err(|e| {
        ReplayError::InvalidManifest(format!("record {} has invalid id: {e}", seed.id))
    })?;
    let target = seed.target_id.as_ref().unwrap_or(&seed.id);
    record.target_id = TargetId::parse(target.clone()).map_err(|e| {
        ReplayError::InvalidManifest(format!("record {} has invalid target_id: {e}", seed.id))
    })?;
    record.kind = seed.kind;
    record.class = seed.class;
    record.visibility = seed.visibility;
    seed.body.clone_into(&mut record.body);
    record.updated_at = timestamp_for(index)?;
    record.extra_frontmatter = trace_frontmatter(seed);
    Ok(record)
}

fn timestamp_for(index: usize) -> Result<Rfc3339Timestamp, ReplayError> {
    let seconds = index % 60;
    let raw = format!("2026-04-22T14:05:{seconds:02}Z");
    Rfc3339Timestamp::parse(raw.clone())
        .map_err(|e| ReplayError::InvalidManifest(format!("invalid timestamp {raw}: {e}")))
}

fn trace_frontmatter(seed: &ReplayRecord) -> BTreeMap<String, Value> {
    let mut extra = BTreeMap::new();
    if let Some(event) = &seed.trace_event {
        extra.insert("trace_event".to_owned(), Value::String(event.clone()));
    }
    if seed.session_id.is_some() || seed.turn_id.is_some() || seed.sequence.is_some() {
        let mut trace = serde_json::Map::new();
        if let Some(session_id) = &seed.session_id {
            trace.insert("session_id".to_owned(), Value::String(session_id.clone()));
        }
        if let Some(turn_id) = &seed.turn_id {
            trace.insert("turn_id".to_owned(), Value::String(turn_id.clone()));
        }
        if let Some(sequence) = seed.sequence {
            trace.insert("sequence".to_owned(), Value::Number(sequence.into()));
        }
        if let Some(tool_name) = &seed.tool_name {
            trace.insert("tool_name".to_owned(), Value::String(tool_name.clone()));
        }
        trace.insert("capture_event_id".to_owned(), Value::String(seed.id.clone()));
        extra.insert("trace".to_owned(), Value::Object(trace));
    }
    extra
}

async fn run_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    action: &ReplayAction,
) -> ReplayCheckReport {
    match action {
        ReplayAction::Search(search) => run_search_action(store, scenario, search).await,
        ReplayAction::RetrieveSession {
            story,
            session_id,
            expected_turn_ids,
            expected_trace_events,
        } => {
            let actual = trace_summary(store, Some(session_id), None)
                .await
                .unwrap_or_else(error_value);
            let expected = json!({
                "turn_ids": expected_turn_ids,
                "trace_events": expected_trace_events
            });
            report_check(&scenario.id, story, "retrieve_session", None, expected, actual)
        }
        ReplayAction::RetrieveTurn {
            story,
            session_id,
            turn_id,
            expected_trace_events,
        } => {
            let actual = trace_summary(store, Some(session_id), Some(turn_id))
                .await
                .unwrap_or_else(error_value);
            let expected = json!({
                "turn_ids": [turn_id],
                "trace_events": expected_trace_events
            });
            report_check(&scenario.id, story, "retrieve_turn", None, expected, actual)
        }
        ReplayAction::RecordPresent {
            story,
            record_id,
            expected_present,
        } => {
            let actual = record_present(store, record_id).await.unwrap_or_else(error_value);
            let expected = json!({ "present": expected_present });
            report_check(&scenario.id, story, "record_present", None, expected, actual)
        }
        ReplayAction::ForgetRecord {
            story,
            record_id,
            followup_query,
            expected_absent_from_search,
        } => {
            let actual = forget_record(store, scenario, record_id, followup_query)
                .await
                .unwrap_or_else(error_value);
            let expected = json!({
                "retrieve_found": false,
                "search_contains_record": !expected_absent_from_search
            });
            report_check(
                &scenario.id,
                story,
                "forget_record",
                Some(followup_query.clone()),
                expected,
                actual,
            )
        }
    }
}

async fn run_search_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    action: &ReplaySearchAction,
) -> ReplayCheckReport {
    let expected = expected_search_value(&action.expected);
    let actual = run_search(store, scenario, action).await;
    report_check(
        &scenario.id,
        &action.story,
        "search",
        Some(action.query.clone()),
        expected,
        actual,
    )
}

async fn run_search(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    action: &ReplaySearchAction,
) -> Value {
    let mut config = CairnConfig::default();
    config.search.local_embeddings = scenario.config.local_embeddings;
    let caps = config.capabilities(true);
    let result = search::run(
        store,
        &config,
        &caps,
        SearchRequest {
            query: action.query.clone(),
            mode: action.mode.to_core(),
            limit: action.limit,
            include_reasoning: true,
            visibility_allowlist: vec![],
            auth_scope: ScopeTuple::default(),
            model_label: config.search.embedding_model.as_str().to_owned(),
            filter: None,
            explain: false,
        },
    )
    .await;

    match result {
        Ok(outcome) => {
            let ids: Vec<String> = outcome
                .candidates
                .iter()
                .map(|candidate| candidate.record_id.as_str().to_owned())
                .collect();
            json!({
                "status": "hits",
                "mode": action.mode.as_str(),
                "record_ids": ids
            })
        }
        Err(SearchError::CapabilityUnavailable { capability }) => json!({
            "status": "capability_unavailable",
            "capability": capability
        }),
        Err(err) => json!({
            "status": "error",
            "message": err.to_string()
        }),
    }
}

fn expected_search_value(expected: &ReplayExpectation) -> Value {
    match expected {
        ReplayExpectation::Hits { record_ids } => json!({
            "status": "hits",
            "record_ids": record_ids
        }),
        ReplayExpectation::CapabilityUnavailable { capability } => json!({
            "status": "capability_unavailable",
            "capability": capability
        }),
    }
}

async fn trace_summary(
    store: &SqliteMemoryStore,
    session_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<Value, ReplayError> {
    let page = store
        .list(&ListArgs {
            limit: 1000,
            ..ListArgs::default()
        })
        .await
        .map_err(|e| ReplayError::Store(format!("list trace records: {e}")))?;
    let mut rows: Vec<(u64, String, String)> = page
        .records
        .iter()
        .filter_map(|record| trace_projection(record, session_id, turn_id))
        .collect();
    rows.sort_by_key(|(sequence, _, _)| *sequence);
    let turn_ids: BTreeSet<String> = rows.iter().map(|(_, turn, _)| turn.clone()).collect();
    let trace_events: Vec<String> = rows.into_iter().map(|(_, _, event)| event).collect();
    Ok(json!({
        "turn_ids": turn_ids.into_iter().collect::<Vec<_>>(),
        "trace_events": trace_events
    }))
}

fn trace_projection(
    record: &MemoryRecord,
    session_filter: Option<&str>,
    turn_filter: Option<&str>,
) -> Option<(u64, String, String)> {
    let trace = record.extra_frontmatter.get("trace")?.as_object()?;
    let session_id = trace.get("session_id")?.as_str()?;
    if session_filter.is_some_and(|wanted| wanted != session_id) {
        return None;
    }
    let turn_id = trace.get("turn_id")?.as_str()?;
    if turn_filter.is_some_and(|wanted| wanted != turn_id) {
        return None;
    }
    let sequence = trace.get("sequence").and_then(Value::as_u64).unwrap_or(0);
    let event = record
        .extra_frontmatter
        .get("trace_event")
        .and_then(Value::as_str)?;
    Some((sequence, turn_id.to_owned(), event.to_owned()))
}

async fn record_present(store: &SqliteMemoryStore, record_id: &str) -> Result<Value, ReplayError> {
    let id = RecordId::parse(record_id.to_owned())
        .map_err(|e| ReplayError::InvalidManifest(format!("invalid record_id {record_id}: {e}")))?;
    let found = store
        .get(&id)
        .await
        .map_err(|e| ReplayError::Store(format!("get {record_id}: {e}")))?
        .is_some();
    Ok(json!({ "present": found }))
}

async fn forget_record(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    record_id: &str,
    followup_query: &str,
) -> Result<Value, ReplayError> {
    let id = RecordId::parse(record_id.to_owned())
        .map_err(|e| ReplayError::InvalidManifest(format!("invalid record_id {record_id}: {e}")))?;
    store
        .tombstone(&id, TombstoneReason::Forget)
        .await
        .map_err(|e| ReplayError::Store(format!("tombstone {record_id}: {e}")))?;
    let retrieve_found = store
        .get(&id)
        .await
        .map_err(|e| ReplayError::Store(format!("get after tombstone {record_id}: {e}")))?
        .is_some();
    let action = ReplaySearchAction {
        story: "US8".to_owned(),
        mode: ReplaySearchMode::Keyword,
        query: followup_query.to_owned(),
        limit: 10,
        expected: ReplayExpectation::Hits { record_ids: vec![] },
    };
    let search_actual = run_search(store, scenario, &action).await;
    let contains = search_actual
        .get("record_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(record_id)));
    Ok(json!({
        "retrieve_found": retrieve_found,
        "search_contains_record": contains
    }))
}

fn report_check(
    scenario_id: &str,
    story: &str,
    verb: &str,
    query: Option<String>,
    expected: Value,
    actual: Value,
) -> ReplayCheckReport {
    let passed = expected == actual;
    ReplayCheckReport {
        scenario_id: scenario_id.to_owned(),
        story: story.to_owned(),
        verb: verb.to_owned(),
        query,
        message: (!passed).then(|| "expected and actual replay outcomes differ".to_owned()),
        expected,
        actual,
        passed,
    }
}

fn error_value(error: ReplayError) -> Value {
    json!({
        "status": "error",
        "message": error.to_string()
    })
}
```

- [ ] **Step 3: Run replay tests and verify GREEN**

Run:

```bash
cargo nextest run -p cairn-test-fixtures --test replay_harness
```

Expected: all replay harness tests pass. If any search ranking expectation differs, inspect the actual report and update only the fixture expectation when the actual ranking is deterministic and defensible.

### Task 4: Tighten Fixture Schema Coverage

**Files:**
- Modify: `crates/cairn-test-fixtures/tests/schema_fixtures.rs`

- [ ] **Step 1: Add replay directory coverage to schema fixture tests**

In `crates/cairn-test-fixtures/tests/schema_fixtures.rs`, add `replay` to the required `fixtures/v0` subdirectory assertions and add a test:

```rust
#[test]
fn replay_scenarios_deserialize() {
    let replay = v0().join("replay");
    for name in ["p0_stories.json", "p0_keyword_only.json"] {
        let path = replay.join(name);
        let scenario = cairn_test_fixtures::replay::load_scenario_file(&path)
            .unwrap_or_else(|e| panic!("load {}: {e}", path.display()));
        insta::assert_json_snapshot!(
            format!("replay_{}", name.trim_end_matches(".json")),
            &scenario.id
        );
    }
}
```

- [ ] **Step 2: Run schema fixture tests**

Run:

```bash
cargo nextest run -p cairn-test-fixtures --test schema_fixtures
```

Expected: tests pass or write new accepted snapshots for the two replay scenario ids.

### Task 5: Final Verification

**Files:**
- No new files.

- [ ] **Step 1: Run replay harness verification**

Run:

```bash
cargo nextest run -p cairn-test-fixtures --test replay_harness
```

Expected: all tests pass.

- [ ] **Step 2: Run existing golden query verification**

Run:

```bash
cargo nextest run -p cairn-cli --test search_modes_golden
```

Expected: all tests pass.

- [ ] **Step 3: Run existing evaluation workflow verification**

Run:

```bash
cargo nextest run -p cairn-workflows --test evaluation
```

Expected: all tests pass.

- [ ] **Step 4: Run core boundary check**

Run:

```bash
scripts/check-core-boundary.sh
```

Expected: exits 0.

- [ ] **Step 5: Inspect final diff**

Run:

```bash
git status --short
git diff --stat
```

Expected: only replay harness, replay fixtures, fixture docs, and fixture tests changed.
