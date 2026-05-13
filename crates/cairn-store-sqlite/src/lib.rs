//! `SQLite` record store for Cairn.
//!
//! This crate provides the plugin manifest plus a minimal hot-memory adapter
//! backed by `SQLite` tables and vault files.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, HotMemoryInvalidationScope, HotMemoryRequest, MemoryStore,
    MemoryStoreCapabilities, MemoryStoreError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::hot_memory::{
    HotMemoryCacheInfo, HotMemoryInput, HotMemoryOutput, HotMemorySource, HotMemorySourceKind,
    HotMemorySourceSummary, HotMemoryTruncation,
};
use cairn_core::register_plugin;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Shared by the trait impl and
/// the compile-time guard below so the manifest range and the trait surface
/// derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// `SQLite`-backed memory store.
pub struct SqliteMemoryStore {
    conn: Mutex<Connection>,
    vault_path: PathBuf,
    _tempdir: Option<tempfile::TempDir>,
}

/// Seed data for inserting hot-memory records in tests and fixtures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotRecordSeed {
    record_id: String,
    kind: String,
    body: String,
    session_id: Option<String>,
    agent_id: Option<String>,
    evidence_score: f32,
    salience: f32,
    tags: Vec<String>,
    extra: Value,
}

struct RecordRow {
    record_id: String,
    kind: String,
    body: String,
    updated_at: String,
    evidence_score: f32,
    salience: f32,
    tags: Vec<String>,
    extra: Value,
}

struct CacheKeyParts {
    session_id: Option<String>,
    agent_id: Option<String>,
    budget_bytes: u32,
    config_fingerprint: String,
    source_revision: String,
}

impl HotRecordSeed {
    /// Build a hot-memory record seed.
    #[must_use]
    pub fn new(
        record_id: impl Into<String>,
        kind: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            record_id: record_id.into(),
            kind: kind.into(),
            body: body.into(),
            session_id: None,
            agent_id: None,
            evidence_score: 0.0,
            salience: 0.0,
            tags: Vec::new(),
            extra: serde_json::json!({}),
        }
    }

    /// Set the session scope.
    #[must_use]
    pub fn session(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Set the agent scope.
    #[must_use]
    pub fn agent(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// Set the evidence score.
    #[must_use]
    pub fn evidence(mut self, score: f32) -> Self {
        self.evidence_score = score;
        self
    }

    /// Set the salience score.
    #[must_use]
    pub fn salience(mut self, score: f32) -> Self {
        self.salience = score;
        self
    }

    /// Add a tag.
    #[must_use]
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set extra frontmatter JSON.
    #[must_use]
    pub fn extra(mut self, value: Value) -> Self {
        self.extra = value;
        self
    }
}

impl Default for SqliteMemoryStore {
    fn default() -> Self {
        let conn = match Connection::open_in_memory() {
            Ok(conn) => conn,
            Err(err) => panic!("failed to open default sqlite memory store: {err}"),
        };
        let store = Self {
            conn: Mutex::new(conn),
            vault_path: std::env::temp_dir(),
            _tempdir: None,
        };
        if let Err(err) = store.migrate() {
            panic!("failed to migrate default sqlite memory store: {err}");
        }
        store
    }
}

impl SqliteMemoryStore {
    /// Open a store rooted at a vault path.
    ///
    /// # Errors
    /// Returns an error when the vault directory, database, or migrations fail.
    pub fn open(vault_path: impl AsRef<Path>) -> Result<Self, MemoryStoreError> {
        let vault_path = vault_path.as_ref().to_path_buf();
        std::fs::create_dir_all(vault_path.join(".cairn"))
            .map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let conn = Connection::open(vault_path.join(".cairn/cairn.db"))
            .map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
            vault_path,
            _tempdir: None,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory store with a temporary vault path.
    ///
    /// # Errors
    /// Returns an error when the temporary directory, database, or migrations fail.
    pub fn open_memory() -> Result<Self, MemoryStoreError> {
        let tempdir =
            tempfile::tempdir().map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let conn = Connection::open_in_memory()
            .map_err(|e| MemoryStoreError::Unavailable(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
            vault_path: tempdir.path().to_path_buf(),
            _tempdir: Some(tempdir),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Write a file inside the vault root.
    ///
    /// # Errors
    /// Returns an error when the path escapes the vault or the write fails.
    pub fn write_vault_file(
        &self,
        relative_path: impl AsRef<Path>,
        body: impl AsRef<str>,
    ) -> Result<(), MemoryStoreError> {
        let path = relative_path.as_ref();
        if path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(MemoryStoreError::query("vault file path must be relative"));
        }
        let path = self.vault_path.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryStoreError::query_with_source("create vault file parent", e))?;
        }
        std::fs::write(path, body.as_ref())
            .map_err(|e| MemoryStoreError::query_with_source("write vault file", e))
    }

    /// Insert a hot-memory record row.
    ///
    /// # Errors
    /// Returns an error when serialization or insertion fails.
    pub fn insert_hot_record(&self, seed: HotRecordSeed) -> Result<(), MemoryStoreError> {
        let HotRecordSeed {
            record_id,
            kind,
            body,
            session_id,
            agent_id,
            evidence_score,
            salience,
            tags,
            extra,
        } = seed;
        let tags_json = serde_json::to_string(&tags)
            .map_err(|e| MemoryStoreError::query_with_source("serialize record tags", e))?;
        let extra_json = serde_json::to_string(&extra)
            .map_err(|e| MemoryStoreError::query_with_source("serialize record extra", e))?;
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO records (
                record_id, kind, body, session_id, agent_id, updated_at,
                evidence_score, salience, tags_json, extra_frontmatter_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP, ?6, ?7, ?8, ?9)",
            params![
                record_id,
                kind,
                body,
                session_id,
                agent_id,
                evidence_score,
                salience,
                tags_json,
                extra_json
            ],
        )
        .map_err(|e| MemoryStoreError::query_with_source("insert hot record", e))?;
        Ok(())
    }

    /// Insert an entity edge row.
    ///
    /// # Errors
    /// Returns an error when insertion fails.
    pub fn insert_entity_edge(
        &self,
        from_entity: impl AsRef<str>,
        to_entity: impl AsRef<str>,
        edge_kind: impl AsRef<str>,
        source_file: impl AsRef<str>,
        invalid_at: Option<&str>,
    ) -> Result<(), MemoryStoreError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO entity_edges (
                from_entity, to_entity, edge_kind, source_file, invalid_at
            ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                from_entity.as_ref(),
                to_entity.as_ref(),
                edge_kind.as_ref(),
                source_file.as_ref(),
                invalid_at
            ],
        )
        .map_err(|e| MemoryStoreError::query_with_source("insert entity edge", e))?;
        Ok(())
    }

    fn migrate(&self) -> Result<(), MemoryStoreError> {
        let conn = self.lock_conn()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS records (
                record_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                class TEXT NOT NULL DEFAULT 'semantic',
                visibility TEXT NOT NULL DEFAULT 'private',
                session_id TEXT,
                agent_id TEXT,
                body TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                evidence_score REAL NOT NULL DEFAULT 0.0,
                salience REAL NOT NULL DEFAULT 0.0,
                tags_json TEXT NOT NULL DEFAULT '[]',
                extra_frontmatter_json TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS entity_edges (
                edge_id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_entity TEXT NOT NULL,
                to_entity TEXT NOT NULL,
                edge_kind TEXT NOT NULL,
                source_file TEXT NOT NULL,
                invalid_at TEXT
            );

            CREATE TABLE IF NOT EXISTS hot_memory_cache (
                cache_key TEXT PRIMARY KEY,
                session_id TEXT,
                agent_id TEXT,
                budget_bytes INTEGER NOT NULL,
                config_fingerprint TEXT NOT NULL,
                source_revision TEXT NOT NULL,
                prefix TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            ",
        )
        .map_err(|e| MemoryStoreError::query_with_source("migrate sqlite store", e))
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, MemoryStoreError> {
        self.conn.lock().map_err(|_| {
            MemoryStoreError::Unavailable("sqlite connection lock poisoned".to_owned())
        })
    }

    fn read_vault_source(
        &self,
        path: &str,
        kind: HotMemorySourceKind,
    ) -> Result<Option<HotMemorySource>, MemoryStoreError> {
        let path_buf = self.vault_path.join(path);
        match std::fs::read_to_string(&path_buf) {
            Ok(body) => Ok(Some(HotMemorySource {
                kind,
                record_id: None,
                title: Some(path.to_owned()),
                body,
                salience: 1.0,
                evidence_score: 1.0,
                centrality_score: 0.0,
                updated_at: file_updated_at(&path_buf),
            })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MemoryStoreError::query_with_source(
                "read vault source",
                err,
            )),
        }
    }

    fn record_rows(&self, request: &HotMemoryRequest) -> Result<Vec<RecordRow>, MemoryStoreError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT record_id, kind, body, updated_at, evidence_score, salience,
                    tags_json, extra_frontmatter_json
                FROM records
                WHERE session_id IS NULL OR session_id = ?1
                ORDER BY updated_at DESC, record_id ASC",
            )
            .map_err(|e| MemoryStoreError::query_with_source("prepare hot records query", e))?;
        let rows = stmt
            .query_map(params![request.session_id.as_deref()], |row| {
                let tags_json: String = row.get(6)?;
                let extra_json: String = row.get(7)?;
                let tags = serde_json::from_str(&tags_json).unwrap_or_default();
                let extra =
                    serde_json::from_str(&extra_json).unwrap_or_else(|_| serde_json::json!({}));
                Ok(RecordRow {
                    record_id: row.get(0)?,
                    kind: row.get(1)?,
                    body: row.get(2)?,
                    updated_at: row.get(3)?,
                    evidence_score: row.get(4)?,
                    salience: row.get(5)?,
                    tags,
                    extra,
                })
            })
            .map_err(|e| MemoryStoreError::query_with_source("query hot records", e))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| MemoryStoreError::query_with_source("read hot records", e))
    }

    fn centrality_scores(&self) -> Result<BTreeMap<String, f32>, MemoryStoreError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT from_entity, to_entity, edge_kind, source_file
                FROM entity_edges
                WHERE invalid_at IS NULL",
            )
            .map_err(|e| MemoryStoreError::query_with_source("prepare entity edges query", e))?;
        let edges = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| MemoryStoreError::query_with_source("query entity edges", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| MemoryStoreError::query_with_source("read entity edges", e))?;

        let mut mentions: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (from, to, kind, file) in edges {
            mentions
                .entry(from)
                .or_default()
                .push((kind.clone(), file.clone()));
            mentions.entry(to).or_default().push((kind, file));
        }

        let mut degrees = BTreeMap::new();
        for (entity, entity_mentions) in mentions {
            let all_self_file_stems = entity_mentions
                .iter()
                .all(|(_, source_file)| file_stem(source_file).is_some_and(|stem| stem == entity));
            let all_structural = entity_mentions
                .iter()
                .all(|(kind, _)| matches!(kind.as_str(), "contains" | "method"));
            if !all_self_file_stems && !all_structural {
                degrees.insert(entity, f32::from(to_u16(entity_mentions.len())));
            }
        }

        let max_degree = degrees.values().copied().fold(0.0_f32, f32::max);
        if max_degree == 0.0 {
            return Ok(BTreeMap::new());
        }
        Ok(degrees
            .into_iter()
            .map(|(entity, degree)| (entity, degree / max_degree))
            .collect())
    }
}

#[async_trait::async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: true,
            transactions: true,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }

    async fn hot_memory_input(
        &self,
        request: &HotMemoryRequest,
    ) -> Result<HotMemoryInput, MemoryStoreError> {
        let mut sources = Vec::new();
        if let Some(source) = self.read_vault_source("purpose.md", HotMemorySourceKind::Purpose)? {
            sources.push(source);
        }
        if let Some(source) =
            self.read_vault_source("index.md", HotMemorySourceKind::ProjectState)?
        {
            sources.push(source);
        }

        let centrality = self.centrality_scores()?;
        let fallback_centrality = centrality.values().next_back().copied().unwrap_or(0.0);
        for row in self.record_rows(request)? {
            if let Some(kind) = hot_source_kind(&row) {
                let score = record_centrality(&row, &centrality).unwrap_or(fallback_centrality);
                sources.push(HotMemorySource {
                    kind,
                    record_id: Some(row.record_id.clone()),
                    title: Some(row.kind.clone()),
                    body: row.body,
                    salience: row.salience,
                    evidence_score: row.evidence_score,
                    centrality_score: score,
                    updated_at: row.updated_at,
                });
            }
        }

        sources.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.record_id.cmp(&right.record_id))
        });
        let revision = source_revision(&sources);
        Ok(HotMemoryInput {
            sources,
            source_revision: revision,
        })
    }

    fn hot_memory_cache_key(
        &self,
        request: &HotMemoryRequest,
        input: &HotMemoryInput,
    ) -> Result<String, MemoryStoreError> {
        let session_id = request.session_id.as_deref().unwrap_or("");
        let agent_id = request.agent_id.as_deref().unwrap_or("");
        let budget = request.budget_bytes.to_string();
        let digest = hash_parts(&[
            session_id,
            agent_id,
            &budget,
            &request.config_fingerprint,
            &input.source_revision,
        ]);
        Ok(format!(
            "{digest}:{}:{}:{}:{}:{}",
            encode_key_part(session_id),
            encode_key_part(agent_id),
            request.budget_bytes,
            encode_key_part(&request.config_fingerprint),
            encode_key_part(&input.source_revision)
        ))
    }

    async fn load_hot_memory_cache(
        &self,
        key: &str,
    ) -> Result<Option<HotMemoryOutput>, MemoryStoreError> {
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "SELECT prefix, metadata_json FROM hot_memory_cache WHERE cache_key = ?1",
                params![key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| MemoryStoreError::cache_with_source("load hot memory cache", e))?;
        let Some((prefix, metadata_json)) = row else {
            return Ok(None);
        };
        let metadata: Value = serde_json::from_str(&metadata_json).map_err(|e| {
            MemoryStoreError::cache_with_source("parse hot memory cache metadata", e)
        })?;
        let sources = serde_json::from_value::<Vec<HotMemorySourceSummary>>(
            metadata
                .get("sources")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|e| MemoryStoreError::cache_with_source("parse hot memory cache sources", e))?;
        let truncation = serde_json::from_value::<Vec<HotMemoryTruncation>>(
            metadata
                .get("truncation")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|e| MemoryStoreError::cache_with_source("parse hot memory cache truncation", e))?;
        Ok(Some(HotMemoryOutput {
            bytes: to_u32(prefix.len()),
            prefix,
            sources,
            truncation,
            cache: HotMemoryCacheInfo::hit(key),
        }))
    }

    async fn store_hot_memory_cache(
        &self,
        key: &str,
        output: &HotMemoryOutput,
    ) -> Result<(), MemoryStoreError> {
        let parts = parse_cache_key(key)?;
        let metadata = serde_json::json!({
            "sources": output.sources,
            "truncation": output.truncation,
            "cache": output.cache,
        });
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|e| MemoryStoreError::cache_with_source("serialize hot memory cache", e))?;
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO hot_memory_cache (
                cache_key, session_id, agent_id, budget_bytes, config_fingerprint,
                source_revision, prefix, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                key,
                parts.session_id,
                parts.agent_id,
                parts.budget_bytes,
                parts.config_fingerprint,
                parts.source_revision,
                output.prefix,
                metadata_json
            ],
        )
        .map_err(|e| MemoryStoreError::cache_with_source("store hot memory cache", e))?;
        Ok(())
    }

    async fn invalidate_hot_memory_cache(
        &self,
        scope: HotMemoryInvalidationScope,
    ) -> Result<u64, MemoryStoreError> {
        let conn = self.lock_conn()?;
        let deleted = match scope {
            HotMemoryInvalidationScope::Vault => conn.execute("DELETE FROM hot_memory_cache", []),
            HotMemoryInvalidationScope::Session(session_id) => conn.execute(
                "DELETE FROM hot_memory_cache WHERE session_id = ?1",
                params![session_id],
            ),
            HotMemoryInvalidationScope::Agent(agent_id) => conn.execute(
                "DELETE FROM hot_memory_cache WHERE agent_id = ?1",
                params![agent_id],
            ),
        }
        .map_err(|e| MemoryStoreError::cache_with_source("invalidate hot memory cache", e))?;
        Ok(deleted as u64)
    }
}

fn hot_source_kind(row: &RecordRow) -> Option<HotMemorySourceKind> {
    let is_pinned = row.tags.iter().any(|tag| tag == "pinned");
    match row.kind.as_str() {
        "user" | "feedback" if is_pinned => Some(HotMemorySourceKind::Pinned),
        "project" | "reference" => Some(HotMemorySourceKind::ProjectState),
        "playbook" => Some(HotMemorySourceKind::Playbook),
        "user_signal" => Some(HotMemorySourceKind::RecentUserSignal),
        "trace" | "reasoning"
            if row
                .extra
                .get("rolling_summary")
                .and_then(Value::as_bool)
                .unwrap_or(false) =>
        {
            Some(HotMemorySourceKind::RollingSummary)
        }
        _ if row.salience >= 0.7 => Some(HotMemorySourceKind::HighSalience),
        _ => None,
    }
}

fn record_centrality(row: &RecordRow, centrality: &BTreeMap<String, f32>) -> Option<f32> {
    let haystack = format!("{} {}", row.record_id, row.body).to_lowercase();
    centrality
        .iter()
        .filter_map(|(entity, score)| {
            if haystack.contains(&entity.to_lowercase()) {
                Some(*score)
            } else {
                None
            }
        })
        .max_by(f32::total_cmp)
}

fn source_revision(sources: &[HotMemorySource]) -> String {
    let mut parts = Vec::with_capacity(sources.len() * 4);
    for source in sources {
        parts.push(kind_name(source.kind).to_owned());
        parts.push(source.record_id.clone().unwrap_or_default());
        parts.push(source.updated_at.clone());
        parts.push(source.body.clone());
    }
    let refs = parts.iter().map(String::as_str).collect::<Vec<_>>();
    hash_parts(&refs)
}

fn hash_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn parse_cache_key(key: &str) -> Result<CacheKeyParts, MemoryStoreError> {
    let mut parts = key.splitn(6, ':');
    let digest = parts.next().unwrap_or_default();
    if digest.is_empty() {
        return Err(MemoryStoreError::cache("invalid hot memory cache key"));
    }
    let session = parts.next();
    let agent = parts.next();
    let budget = parts.next();
    let config = parts.next();
    let revision = parts.next();
    let (Some(session), Some(agent), Some(budget), Some(config), Some(revision)) =
        (session, agent, budget, config, revision)
    else {
        return Err(MemoryStoreError::cache("invalid hot memory cache key"));
    };
    Ok(CacheKeyParts {
        session_id: non_empty(decode_key_part(session)?),
        agent_id: non_empty(decode_key_part(agent)?),
        budget_bytes: budget
            .parse()
            .map_err(|e| MemoryStoreError::cache_with_source("parse hot memory cache budget", e))?,
        config_fingerprint: decode_key_part(config)?,
        source_revision: decode_key_part(revision)?,
    })
}

fn encode_key_part(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_key_part(value: &str) -> Result<String, MemoryStoreError> {
    if !value.len().is_multiple_of(2) {
        return Err(MemoryStoreError::cache(
            "invalid hot memory cache key encoding",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks(2) {
        let hex = std::str::from_utf8(chunk)
            .map_err(|e| MemoryStoreError::cache_with_source("decode hot memory cache key", e))?;
        bytes.push(
            u8::from_str_radix(hex, 16).map_err(|e| {
                MemoryStoreError::cache_with_source("decode hot memory cache key", e)
            })?,
        );
    }
    String::from_utf8(bytes)
        .map_err(|e| MemoryStoreError::cache_with_source("decode hot memory cache key", e))
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn file_stem(path: &str) -> Option<String> {
    Path::new(path)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .map(ToOwned::to_owned)
}

fn file_updated_at(path: &Path) -> String {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(
            || "1970-01-01T00:00:00Z".to_owned(),
            |duration| format!("{:020}", duration.as_secs()),
        )
}

fn kind_name(kind: HotMemorySourceKind) -> &'static str {
    match kind {
        HotMemorySourceKind::Purpose => "purpose",
        HotMemorySourceKind::Profile => "profile",
        HotMemorySourceKind::Pinned => "pinned",
        HotMemorySourceKind::HighSalience => "high_salience",
        HotMemorySourceKind::ProjectState => "project_state",
        HotMemorySourceKind::RollingSummary => "rolling_summary",
        HotMemorySourceKind::Playbook => "playbook",
        HotMemorySourceKind::RecentUserSignal => "recent_user_signal",
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn to_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping the
// range, the const evaluation here panics at build.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
