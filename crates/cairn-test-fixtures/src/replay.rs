//! Local replay harness helpers for P0 evaluation fixtures.
//!
//! This module is dev-only through the `cairn-test-fixtures` crate. It loads
//! scenario manifests from `fixtures/v0/replay/`, seeds a temporary `SQLite`
//! vault, executes deterministic checks, and returns machine-readable reports
//! suitable for CI gates.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cairn_core::config::{CairnConfig, EmbeddingModelKind};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore, TombstoneReason};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use cairn_core::domain::trace::TraceEvent;
use cairn_core::domain::{RecordId, Rfc3339Timestamp, ScopeTuple, TargetId};
use cairn_core::generated::envelope::RetrieveData;
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot_with_inputs};
use cairn_core::verbs::retrieve;
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
    /// `true` when every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }

    /// Iterate over failed checks only.
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
    /// Optional tool-call id for tool lifecycle records.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Optional parent capture event id for post-tool/tool-output records.
    #[serde(default)]
    pub parent_event_id: Option<String>,
}

/// Coherence metric category assigned to a replay action.
///
/// `cairn-bench coherence` aggregates per-category pass rates. An action
/// without a `metric_category` is excluded from coherence scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricCategory {
    /// Long-horizon recall — `retrieve_session`, `retrieve_turn`,
    /// `assemble_hot`, `capture_trace`, `record_present(true)`.
    RecallPrecision,
    /// Stale-context avoidance — search actions with a non-empty
    /// `stale_record_ids` set.
    StaleAvoidance,
    /// Summary quality — `summarize` actions matching the expected record set.
    SummaryQuality,
    /// Search relevance — search actions whose top-1 hit matches expected.
    SearchUsefulness,
    /// Forget completeness — `forget_record` actions whose follow-up search
    /// excludes the tombstoned record.
    ForgetCompleteness,
}

/// Ordered replay action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayAction {
    /// Search query expectation.
    Search(ReplaySearchAction),
    /// Hot-memory assembly expectation.
    AssembleHot {
        /// User story label.
        story: String,
        /// Expected record ids included in the hot-memory debug trace.
        expected_record_ids: Vec<String>,
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
    /// Capture-trace-shaped session event expectation.
    CaptureTrace {
        /// User story label.
        story: String,
        /// Session id to inspect.
        session_id: String,
        /// Expected trace events in sequence order.
        expected_trace_events: Vec<String>,
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
    /// Summary replay expectation.
    Summarize {
        /// User story label.
        story: String,
        /// Session id to summarize.
        session_id: String,
        /// Expected summary record ids.
        expected_record_ids: Vec<String>,
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
    /// Lint replay expectation.
    Lint {
        /// User story label.
        story: String,
        /// Expected lint status.
        expected_status: String,
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
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
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
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
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
    /// Direct record presence check.
    RecordPresent {
        /// User story label.
        story: String,
        /// Record id to inspect.
        record_id: String,
        /// Expected presence.
        expected_present: bool,
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
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
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
}

impl ReplayAction {
    /// Mutable search-action view for tests that perturb expectations.
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
    /// Coherence metric category. `None` excludes this action from coherence
    /// scoring.
    #[serde(default)]
    pub metric_category: Option<MetricCategory>,
    /// Records that must NOT appear in the result. When non-empty, coherence
    /// scoring auto-classifies the action as `StaleAvoidance`.
    #[serde(default)]
    pub stale_record_ids: Vec<String>,
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
    const fn to_core(self) -> SearchMode {
        match self {
            Self::Keyword => SearchMode::Keyword,
            Self::Semantic => SearchMode::Semantic,
            Self::Hybrid => SearchMode::Hybrid,
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
///
/// # Errors
/// Returns [`ReplayError`] if the fixture cannot be read or parsed.
pub fn load_named_scenario(name: &str) -> Result<ReplayScenario, ReplayError> {
    let path = crate::fixture_v0_dir()
        .join("replay")
        .join(format!("{name}.json"));
    load_scenario_file(&path)
}

/// Load a scenario from a specific file path.
///
/// # Errors
/// Returns [`ReplayError`] if the fixture cannot be read or parsed.
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
///
/// # Errors
/// Returns [`ReplayError`] if loading, vault setup, or fixture replay fails.
pub async fn run_named_scenario(name: &str) -> Result<ReplayReport, ReplayError> {
    let scenario = load_named_scenario(name)?;
    run_scenario(&scenario).await
}

/// Run a loaded replay scenario.
///
/// # Errors
/// Returns [`ReplayError`] if the temp vault cannot be built.
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
    record.scope.session_id.clone_from(&seed.session_id);
    if let Some(event) = &seed.trace_event {
        serde_json::from_value::<TraceEvent>(Value::String(event.clone())).map_err(|e| {
            ReplayError::InvalidManifest(format!(
                "record {} has invalid trace_event {event:?}: {e}",
                seed.id
            ))
        })?;
    }
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
        if let Some(tool_call_id) = &seed.tool_call_id {
            trace.insert(
                "tool_call_id".to_owned(),
                Value::String(tool_call_id.clone()),
            );
        }
        if let Some(parent_event_id) = &seed.parent_event_id {
            trace.insert(
                "parent_event_id".to_owned(),
                Value::String(parent_event_id.clone()),
            );
        }
        trace.insert(
            "capture_event_id".to_owned(),
            Value::String(seed.id.clone()),
        );
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
        ReplayAction::AssembleHot {
            story,
            expected_record_ids,
            ..
        } => run_assemble_hot_action(store, scenario, story, expected_record_ids).await,
        ReplayAction::CaptureTrace {
            story,
            session_id,
            expected_trace_events,
            ..
        } => {
            run_capture_trace_action(store, scenario, story, session_id, expected_trace_events)
                .await
        }
        ReplayAction::Summarize {
            story,
            session_id,
            expected_record_ids,
            ..
        } => run_summarize_action(store, scenario, story, session_id, expected_record_ids).await,
        ReplayAction::Lint {
            story,
            expected_status,
            ..
        } => run_lint_action(store, scenario, story, expected_status).await,
        ReplayAction::RetrieveSession {
            story,
            session_id,
            expected_turn_ids,
            expected_trace_events,
            ..
        } => {
            run_retrieve_session_action(
                store,
                scenario,
                story,
                session_id,
                expected_turn_ids,
                expected_trace_events,
            )
            .await
        }
        ReplayAction::RetrieveTurn {
            story,
            session_id,
            turn_id,
            expected_trace_events,
            ..
        } => {
            run_retrieve_turn_action(
                store,
                scenario,
                story,
                session_id,
                turn_id,
                expected_trace_events,
            )
            .await
        }
        ReplayAction::RecordPresent {
            story,
            record_id,
            expected_present,
            ..
        } => run_record_present_action(store, scenario, story, record_id, *expected_present).await,
        ReplayAction::ForgetRecord {
            story,
            record_id,
            followup_query,
            expected_absent_from_search,
            ..
        } => {
            run_forget_record_action(
                store,
                scenario,
                story,
                record_id,
                followup_query,
                *expected_absent_from_search,
            )
            .await
        }
    }
}

async fn run_assemble_hot_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    expected_record_ids: &[String],
) -> ReplayCheckReport {
    let actual = assemble_hot_replay(store)
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({ "record_ids": expected_record_ids });
    report_check(
        &scenario.id,
        story_label,
        "assemble_hot",
        None,
        expected,
        actual,
    )
}

async fn run_capture_trace_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    session_id: &str,
    expected_trace_events: &[String],
) -> ReplayCheckReport {
    let actual = trace_events_only(store, session_id)
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({ "trace_events": expected_trace_events });
    report_check(
        &scenario.id,
        story_label,
        "capture_trace",
        None,
        expected,
        actual,
    )
}

async fn run_summarize_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    session_id: &str,
    expected_record_ids: &[String],
) -> ReplayCheckReport {
    let actual = summarize_replay(store, session_id)
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({ "record_ids": expected_record_ids });
    report_check(
        &scenario.id,
        story_label,
        "summarize",
        None,
        expected,
        actual,
    )
}

async fn run_lint_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    expected_status: &str,
) -> ReplayCheckReport {
    let actual = lint_replay(store).await.unwrap_or_else(|e| error_value(&e));
    let expected = json!({ "status": expected_status });
    report_check(&scenario.id, story_label, "lint", None, expected, actual)
}

async fn run_retrieve_session_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    session_id: &str,
    expected_turn_ids: &[String],
    expected_trace_events: &[String],
) -> ReplayCheckReport {
    let actual = trace_summary(store, Some(session_id), None)
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({
        "turn_ids": expected_turn_ids,
        "trace_events": expected_trace_events,
    });
    report_check(
        &scenario.id,
        story_label,
        "retrieve_session",
        None,
        expected,
        actual,
    )
}

async fn run_retrieve_turn_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    session_id: &str,
    turn_id: &str,
    expected_trace_events: &[String],
) -> ReplayCheckReport {
    let actual = trace_summary(store, Some(session_id), Some(turn_id))
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({
        "turn_ids": [turn_id],
        "trace_events": expected_trace_events,
    });
    report_check(
        &scenario.id,
        story_label,
        "retrieve_turn",
        None,
        expected,
        actual,
    )
}

async fn run_record_present_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    record_id: &str,
    expected_present: bool,
) -> ReplayCheckReport {
    let actual = record_present(store, record_id)
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({ "present": expected_present });
    report_check(
        &scenario.id,
        story_label,
        "record_present",
        None,
        expected,
        actual,
    )
}

async fn run_forget_record_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    story_label: &str,
    record_id: &str,
    followup_query: &str,
    expected_absent_from_search: bool,
) -> ReplayCheckReport {
    let actual = forget_record(store, scenario, record_id, followup_query)
        .await
        .unwrap_or_else(|e| error_value(&e));
    let expected = json!({
        "retrieve_found": false,
        "search_contains_record": !expected_absent_from_search,
    });
    report_check(
        &scenario.id,
        story_label,
        "forget_record",
        Some(followup_query.to_owned()),
        expected,
        actual,
    )
}

async fn run_search_action(
    store: &SqliteMemoryStore,
    scenario: &ReplayScenario,
    action: &ReplaySearchAction,
) -> ReplayCheckReport {
    let expected = expected_search_value(&action.expected);
    let actual = run_search(store, scenario, action).await;
    // Stale-avoidance search actions use a broader `limit` so the
    // disjointness check sees lower-ranked leaks. They assert only the
    // top-K hits in `expected.record_ids` AND no stale id anywhere in
    // the returned list. A plain exact-match would force the cassette
    // author to predict every result the BM25 ranker may emit, which is
    // brittle. So when `stale_record_ids` is non-empty we use a prefix
    // comparison plus an explicit disjointness assertion.
    if !action.stale_record_ids.is_empty() {
        return stale_search_check(
            &action.query,
            &action.story,
            scenario,
            &action.expected,
            &action.stale_record_ids,
            &actual,
        );
    }
    report_check(
        &scenario.id,
        &action.story,
        "search",
        Some(action.query.clone()),
        expected,
        actual,
    )
}

fn stale_search_check(
    query: &str,
    story_label: &str,
    scenario: &ReplayScenario,
    expectation: &ReplayExpectation,
    stale_ids: &[String],
    actual: &Value,
) -> ReplayCheckReport {
    let expected_prefix = match expectation {
        ReplayExpectation::Hits { record_ids } => record_ids.clone(),
        ReplayExpectation::CapabilityUnavailable { .. } => Vec::new(),
    };
    let prefix_ok = actual_prefix_matches(actual, &expected_prefix);
    let stale_absent = !actual_contains_any(actual, stale_ids);
    let passed = matches!(actual.get("status").and_then(Value::as_str), Some("hits"))
        && prefix_ok
        && stale_absent;
    let expected_view = json!({
        "status": "hits",
        "record_ids_prefix": expected_prefix,
        "stale_record_ids_absent": stale_ids,
    });
    let actual_view = json!({
        "status": actual.get("status").cloned().unwrap_or(Value::Null),
        "record_ids": actual.get("record_ids").cloned().unwrap_or(Value::Null),
    });
    ReplayCheckReport {
        scenario_id: scenario.id.clone(),
        story: story_label.to_owned(),
        verb: "search".to_owned(),
        query: Some(query.to_owned()),
        message: if passed {
            None
        } else if !prefix_ok {
            Some("stale search: expected prefix did not match top hits".to_owned())
        } else if !stale_absent {
            Some("stale search: stale record id appeared in results".to_owned())
        } else {
            Some("stale search: response was not status=hits".to_owned())
        },
        expected: expected_view,
        actual: actual_view,
        passed,
    }
}

fn actual_prefix_matches(actual: &Value, expected_prefix: &[String]) -> bool {
    let Some(arr) = actual.get("record_ids").and_then(Value::as_array) else {
        return false;
    };
    if arr.len() < expected_prefix.len() {
        return false;
    }
    expected_prefix
        .iter()
        .zip(arr.iter())
        .all(|(want, got)| got.as_str() == Some(want.as_str()))
}

fn actual_contains_any(actual: &Value, candidates: &[String]) -> bool {
    let Some(arr) = actual.get("record_ids").and_then(Value::as_array) else {
        return false;
    };
    arr.iter()
        .filter_map(Value::as_str)
        .any(|id| candidates.iter().any(|c| c == id))
}

async fn assemble_hot_replay(store: &SqliteMemoryStore) -> Result<Value, ReplayError> {
    let records = all_records(store).await?;
    let pinned = records
        .iter()
        .filter(|record| matches!(record.kind, MemoryKind::User | MemoryKind::Feedback))
        .collect::<Vec<_>>();
    let projects = records
        .iter()
        .filter(|record| record.kind == MemoryKind::Project)
        .collect::<Vec<_>>();
    let playbooks = records
        .iter()
        .filter(|record| record.kind == MemoryKind::Playbook)
        .collect::<Vec<_>>();
    let summaries = records
        .iter()
        .filter(|record| record.kind == MemoryKind::Reasoning && record.scope.session_id.is_some())
        .collect::<Vec<_>>();
    let user_signals = records
        .iter()
        .filter(|record| record.kind == MemoryKind::UserSignal)
        .collect::<Vec<_>>();
    let now = Rfc3339Timestamp::parse("2026-04-22T15:00:00Z")
        .map_err(|e| ReplayError::Store(format!("parse hot-memory now: {e}")))?;
    let visibility = [MemoryVisibility::Private];
    let inputs = HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: &pinned,
        project_candidates: &projects,
        playbook_candidates: &playbooks,
        skill_graph_snapshot: None,
        rolling_summary_candidates: &summaries,
        user_signal_candidates: &user_signals,
        now,
        scope: ScopeTuple::default(),
        authorized_visibility: &visibility,
        include_debug: true,
    };
    let config = CairnConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &config.vault.hot_memory)
        .map_err(|e| ReplayError::Store(format!("assemble_hot: {e}")))?;
    let mut ids = data
        .debug
        .into_iter()
        .flat_map(|debug| debug.steps)
        .flat_map(|step| step.included)
        .map(|included| included.record_id)
        .collect::<Vec<_>>();
    ids.sort();
    Ok(json!({ "record_ids": ids }))
}

async fn trace_events_only(
    store: &SqliteMemoryStore,
    session_id: &str,
) -> Result<Value, ReplayError> {
    let summary = trace_summary(store, Some(session_id), None).await?;
    Ok(json!({
        "trace_events": summary
            .get("trace_events")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    }))
}

async fn summarize_replay(
    store: &SqliteMemoryStore,
    session_id: &str,
) -> Result<Value, ReplayError> {
    let mut ids = all_records(store)
        .await?
        .into_iter()
        .filter(|record| {
            record.kind == MemoryKind::Reasoning
                && record.scope.session_id.as_deref() == Some(session_id)
                && record
                    .extra_frontmatter
                    .get("trace_event")
                    .and_then(Value::as_str)
                    == Some("turn_summary")
        })
        .map(|record| record.id.as_str().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    Ok(json!({ "record_ids": ids }))
}

async fn lint_replay(store: &SqliteMemoryStore) -> Result<Value, ReplayError> {
    let records = all_records(store).await?;
    let mut seen = BTreeSet::new();
    for record in &records {
        record
            .validate()
            .map_err(|e| ReplayError::Store(format!("lint record {}: {e}", record.id.as_str())))?;
        if !seen.insert(record.id.as_str().to_owned()) {
            return Ok(json!({ "status": "duplicate_record_id" }));
        }
        if let Some(event) = record
            .extra_frontmatter
            .get("trace_event")
            .and_then(Value::as_str)
        {
            serde_json::from_value::<TraceEvent>(Value::String(event.to_owned())).map_err(|e| {
                ReplayError::Store(format!("lint trace_event {}: {e}", record.id.as_str()))
            })?;
        }
    }
    Ok(json!({ "status": "clean" }))
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
            include_reasoning: false,
            visibility_allowlist: vec![],
            auth_scope: ScopeTuple::default(),
            rebac: cairn_core::rebac::RebacContext::default(),
            model_label: config.search.embedding_model.as_str().to_owned(),
            filter: None,
            explain: false,
        },
    )
    .await;

    match result {
        Ok(outcome) => search_success_value(&outcome),
        Err(SearchError::CapabilityUnavailable { capability }) => json!({
            "status": "capability_unavailable",
            "capability": capability,
        }),
        Err(err) => json!({
            "status": "error",
            "message": err.to_string(),
        }),
    }
}

async fn all_records(store: &SqliteMemoryStore) -> Result<Vec<MemoryRecord>, ReplayError> {
    store
        .list(&ListArgs {
            limit: 1000,
            ..ListArgs::default()
        })
        .await
        .map(|page| page.records)
        .map_err(|e| ReplayError::Store(format!("list records: {e}")))
}

fn search_success_value(outcome: &search::SearchOutcome) -> Value {
    let ids: Vec<String> = outcome
        .candidates
        .iter()
        .map(|candidate| candidate.record_id.as_str().to_owned())
        .collect();
    let mut value = json!({
        "status": "hits",
        "record_ids": ids,
    });
    if !outcome.degraded_legs.is_empty() {
        value["degraded_legs"] = Value::Array(
            outcome
                .degraded_legs
                .iter()
                .map(degraded_leg_value)
                .collect(),
        );
    }
    value
}

fn degraded_leg_value(leg: &cairn_core::search::DegradedLeg) -> Value {
    use cairn_core::search::{DegradationReason, DegradedLeg, GraphSource};

    fn reason_value(reason: DegradationReason) -> &'static str {
        match reason {
            DegradationReason::CapabilityUnavailable => "capability_unavailable",
            DegradationReason::DeadlineExceeded => "timeout",
            _ => "sql_error",
        }
    }

    fn source_value(source: GraphSource) -> &'static str {
        match source {
            GraphSource::AuthKeywordSeed => "auth_keyword_seed",
            GraphSource::AuthSemanticSeed => "auth_semantic_seed",
            _ => "all",
        }
    }

    match leg {
        DegradedLeg::Semantic { reason } => json!({
            "leg": "semantic",
            "reason": reason_value(*reason),
        }),
        DegradedLeg::Graph { reason, source } => json!({
            "leg": "graph",
            "reason": reason_value(*reason),
            "source": source_value(*source),
        }),
        _ => json!({
            "leg": "graph",
            "reason": "sql_error",
            "source": "all",
        }),
    }
}

fn expected_search_value(expected: &ReplayExpectation) -> Value {
    match expected {
        ReplayExpectation::Hits { record_ids } => json!({
            "status": "hits",
            "record_ids": record_ids,
        }),
        ReplayExpectation::CapabilityUnavailable { capability } => json!({
            "status": "capability_unavailable",
            "capability": capability,
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
            scope: session_id.map(|session_id| ScopeTuple {
                session_id: Some(session_id.to_owned()),
                ..ScopeTuple::default()
            }),
            ..ListArgs::default()
        })
        .await
        .map_err(|e| ReplayError::Store(format!("list trace records: {e}")))?;
    let mut rows: Vec<(u64, MemoryRecord)> = page
        .records
        .into_iter()
        .filter_map(|record| {
            trace_projection(&record, session_id, turn_id)
                .map(|(sequence, _, _)| (sequence, record))
        })
        .collect();
    rows.sort_by_key(|(sequence, _)| *sequence);
    let records = rows
        .into_iter()
        .map(|(_, record)| record)
        .collect::<Vec<_>>();

    match (session_id, turn_id) {
        (Some(session_id), Some(turn_id)) => Ok(summary_from_retrieve_data(
            retrieve::turn_data_with_options(
                session_id.to_owned(),
                turn_id.to_owned(),
                &records,
                false,
                false,
            ),
        )),
        (Some(session_id), None) => Ok(summary_from_retrieve_data(
            retrieve::session_data_with_options(
                session_id.to_owned(),
                &records,
                None,
                false,
                false,
            ),
        )),
        (None, _) => Ok(summary_from_records(&records)),
    }
}

fn trace_projection(
    record: &MemoryRecord,
    session_filter: Option<&str>,
    turn_filter: Option<&str>,
) -> Option<(u64, String, String)> {
    let trace = record.extra_frontmatter.get("trace")?.as_object()?;
    let scope_session_id = record.scope.session_id.as_deref()?;
    let trace_session_id = trace.get("session_id")?.as_str()?;
    if trace_session_id != scope_session_id {
        return None;
    }
    if session_filter.is_some_and(|wanted| wanted != scope_session_id) {
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

fn summary_from_retrieve_data(data: RetrieveData) -> Value {
    match data {
        RetrieveData::Session(session) => summary_from_turn_items(session.items),
        RetrieveData::Turn(turn) => summary_from_turn_items(turn.turn),
        _ => json!({
            "status": "error",
            "message": "unexpected retrieve data shape",
        }),
    }
}

fn summary_from_turn_items(
    items: impl IntoIterator<Item = cairn_core::generated::verbs::retrieve::TurnItem>,
) -> Value {
    let mut turn_ids = BTreeSet::new();
    let mut trace_events = Vec::new();
    for item in items {
        let Some(linkage) = item.linkage else {
            continue;
        };
        let Some(event) = linkage.trace_event else {
            continue;
        };
        if event != "turn_summary" {
            turn_ids.insert(item.turn_id);
        }
        trace_events.push(event);
    }
    json!({
        "turn_ids": turn_ids.into_iter().collect::<Vec<_>>(),
        "trace_events": trace_events,
    })
}

fn summary_from_records(records: &[MemoryRecord]) -> Value {
    let mut rows = records
        .iter()
        .filter_map(|record| trace_projection(record, None, None))
        .collect::<Vec<_>>();
    rows.sort_by_key(|(sequence, _, _)| *sequence);
    let turn_ids: BTreeSet<String> = rows
        .iter()
        .filter(|(_, _, event)| event != "turn_summary")
        .map(|(_, turn, _)| turn.clone())
        .collect();
    let trace_events: Vec<String> = rows.into_iter().map(|(_, _, event)| event).collect();
    json!({
        "turn_ids": turn_ids.into_iter().collect::<Vec<_>>(),
        "trace_events": trace_events,
    })
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
        metric_category: None,
        stale_record_ids: vec![],
    };
    let search_actual = run_search(store, scenario, &action).await;
    if search_actual.get("status").and_then(Value::as_str) != Some("hits") {
        return Ok(json!({
            "status": search_actual
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("error"),
            "retrieve_found": retrieve_found,
            "search_actual": search_actual,
        }));
    }
    let Some(ids) = search_actual.get("record_ids").and_then(Value::as_array) else {
        return Ok(json!({
            "status": "error",
            "retrieve_found": retrieve_found,
            "message": "follow-up search returned hits without record_ids",
            "search_actual": search_actual,
        }));
    };
    let contains = ids.iter().any(|value| value.as_str() == Some(record_id));
    Ok(json!({
        "retrieve_found": retrieve_found,
        "search_contains_record": contains,
    }))
}

fn report_check(
    scenario_id: &str,
    story_label: &str,
    verb: &str,
    query: Option<String>,
    expected: Value,
    actual: Value,
) -> ReplayCheckReport {
    let passed = expected == actual;
    ReplayCheckReport {
        scenario_id: scenario_id.to_owned(),
        story: story_label.to_owned(),
        verb: verb.to_owned(),
        query,
        message: (!passed).then(|| "expected and actual replay outcomes differ".to_owned()),
        expected,
        actual,
        passed,
    }
}

fn error_value(error: &ReplayError) -> Value {
    json!({
        "status": "error",
        "message": error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_category_round_trips_through_serde() {
        let raw = serde_json::json!({
            "verb": "summarize",
            "story": "RESEARCH_SUMMARY_GOLDEN",
            "session_id": "research-literature",
            "expected_record_ids": ["01HQZX9F5N00000000000000R4"],
            "metric_category": "summary_quality"
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        match action {
            ReplayAction::Summarize {
                metric_category, ..
            } => {
                assert_eq!(metric_category, Some(MetricCategory::SummaryQuality));
            }
            other => panic!("expected Summarize, got {other:?}"),
        }
    }

    #[test]
    fn metric_category_absent_parses_as_none() {
        let raw = serde_json::json!({
            "verb": "summarize",
            "story": "RESEARCH_SUMMARY_GOLDEN",
            "session_id": "research-literature",
            "expected_record_ids": ["01HQZX9F5N00000000000000R4"]
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        match action {
            ReplayAction::Summarize {
                metric_category, ..
            } => {
                assert_eq!(metric_category, None);
            }
            other => panic!("expected Summarize, got {other:?}"),
        }
    }

    fn trace_seed() -> ReplayRecord {
        ReplayRecord {
            id: "01HQZX9F5N00000000000000C0".to_owned(),
            target_id: None,
            kind: MemoryKind::Trace,
            class: MemoryClass::Episodic,
            visibility: MemoryVisibility::Private,
            body: "trace body".to_owned(),
            session_id: Some("session-a".to_owned()),
            turn_id: Some("turn-a".to_owned()),
            sequence: Some(1),
            trace_event: Some("user_message".to_owned()),
            tool_name: None,
            tool_call_id: None,
            parent_event_id: None,
        }
    }

    #[test]
    fn seed_record_sets_scope_session_id_for_trace_records() {
        let record = seed_record(&trace_seed(), 0).expect("seed record");

        assert_eq!(record.scope.session_id.as_deref(), Some("session-a"));
    }

    #[test]
    fn trace_projection_rejects_trace_session_without_record_scope() {
        let mut record = crate::sample_record(1);
        record.extra_frontmatter = trace_frontmatter(&trace_seed());

        assert_eq!(trace_projection(&record, Some("session-a"), None), None);
    }

    #[test]
    fn stale_record_ids_round_trip() {
        let raw = serde_json::json!({
            "verb": "search",
            "story": "RESEARCH_STALE_AVOIDANCE",
            "mode": "keyword",
            "query": "lattice biology",
            "limit": 5,
            "expected": { "status": "hits", "record_ids": ["01HQZX9F5N00000000000000R5"] },
            "stale_record_ids": ["01HQZX9F5N00000000000000R7"]
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        let ReplayAction::Search(search) = action else {
            panic!("expected Search variant");
        };
        assert_eq!(
            search.stale_record_ids,
            vec!["01HQZX9F5N00000000000000R7".to_owned()]
        );
    }

    #[test]
    fn stale_record_ids_absent_defaults_to_empty() {
        let raw = serde_json::json!({
            "verb": "search",
            "story": "RESEARCH_SEARCH_RELEVANCE",
            "mode": "keyword",
            "query": "lattice biology drift marker",
            "limit": 1,
            "expected": { "status": "hits", "record_ids": ["01HQZX9F5N00000000000000R5"] }
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        let ReplayAction::Search(search) = action else {
            panic!("expected Search variant");
        };
        assert!(search.stale_record_ids.is_empty());
    }

    #[test]
    fn search_success_value_surfaces_degraded_hybrid_legs() {
        let actual = search_success_value(&search::SearchOutcome {
            candidates: vec![],
            explain: None,
            policy_trace: vec![],
            excluded: None,
            degraded_legs: vec![cairn_core::search::DegradedLeg::Graph {
                reason: cairn_core::search::DegradationReason::CapabilityUnavailable,
                source: cairn_core::search::GraphSource::All,
            }],
            semantic_degraded: false,
        });

        assert_eq!(
            actual,
            json!({
                "status": "hits",
                "record_ids": [],
                "degraded_legs": [{
                    "leg": "graph",
                    "reason": "capability_unavailable",
                    "source": "all"
                }]
            })
        );
    }
}
