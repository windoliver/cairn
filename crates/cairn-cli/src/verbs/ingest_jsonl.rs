//! `cairn ingest --jsonl <path>` handler (issue #311, brief §5.5).
//!
//! Imports harness conversation JSONL transcripts (Claude Code, generic
//! fallback) by mapping each turn through the [`cairn_core::replay`]
//! parser registry, grouping by session id, and persisting one
//! `MemoryRecord` per imported session.
//!
//! Idempotency: each (`file_path`, `session_id`) pair derives a SHA-256 marker
//! written under `.cairn/jsonl-imports/`. A second import of the same file
//! is a no-op.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::record::{Ed25519Signature, RecordId};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, EvidenceVector, Identity, MemoryClass, MemoryKind, MemoryRecord,
    MemoryVisibility, Provenance, Rfc3339Timestamp, ScopeTuple, SourceId, TargetId,
};
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::ingest::{IngestData, IngestDataJsonlSummary};
use cairn_core::replay::{ParseError, ParsedTranscriptLine, detect_parser_with};
use clap::ArgMatches;
use sha2::{Digest, Sha256};

use super::envelope::{emit_json, human_error, new_operation_id};

const CLI_AUTHOR_ID: &str = "agt:cairn-cli:p0:v1";
const CLI_SENSOR_ID: &str = "snr:local:cli:p0:v1";
const IMPORT_MARKER_DIR: &str = ".cairn/jsonl-imports";

#[derive(Debug, Default, serde::Serialize)]
struct ImportSummary {
    files_scanned: u64,
    sessions_parsed: u64,
    turns_parsed: u64,
    records_written: u64,
    sessions_skipped: u64,
    elapsed_ms: u64,
}

/// Entry point invoked by `cairn ingest --jsonl <path>`.
pub fn run(sub: &ArgMatches, json: bool, path: &Path, vault_root: &Path) -> ExitCode {
    let started = Instant::now();
    let dry_run = sub.get_flag("dry-run");
    let recursive = sub.get_flag("recursive");
    let harness = sub.get_one::<String>("harness").map(String::as_str);
    let session_id_from = sub.get_one::<String>("session_id_from").map(String::as_str);
    let limit = sub.get_one::<u32>("limit").copied();

    let files = match enumerate_files(path, recursive) {
        Ok(files) => files,
        Err(e) => return emit_jsonl_error(json, &format!("enumerate {}: {e}", path.display())),
    };
    if files.is_empty() {
        return emit_jsonl_error(
            json,
            &format!("no .jsonl files found at {}", path.display()),
        );
    }

    let mut summary = ImportSummary {
        files_scanned: files.len() as u64,
        ..ImportSummary::default()
    };

    // Stream files one at a time. Each file is parsed line-by-line
    // (constant memory in the file size — no slurp), and its proven-
    // complete sessions are persisted before the next file is read.
    // Sessions that were still receiving turns when `--limit` ran out
    // are dropped at the session level, so a multi-session file does
    // not lose earlier complete sessions just because a later session
    // was cut off.
    let mut persister = if dry_run {
        None
    } else {
        match Persister::open(vault_root) {
            Ok(p) => Some(p),
            Err(msg) => return emit_jsonl_error(json, &msg),
        }
    };
    let mut remaining = limit;
    for file in &files {
        if matches!(remaining, Some(0)) {
            break;
        }
        if let Err(code) = process_file(
            file,
            harness,
            session_id_from,
            &mut remaining,
            persister.as_mut(),
            &mut summary,
            json,
        ) {
            return code;
        }
    }

    summary.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    emit_summary(json, dry_run, &summary, &files);
    ExitCode::SUCCESS
}

#[derive(Debug, Default)]
struct SessionImport {
    role_first: String,
    blocks: Vec<serde_json::Value>,
    turns: u64,
    /// Earliest transcript timestamp seen in this session (RFC3339).
    earliest_ts: Option<String>,
    /// Latest transcript timestamp seen in this session (RFC3339).
    latest_ts: Option<String>,
}

fn enumerate_files(path: &Path, recursive: bool) -> std::io::Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_owned()]);
    }
    if !path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("not a file or directory: {}", path.display()),
        ));
    }
    let mut out = Vec::new();
    let mut stack = vec![path.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                if recursive {
                    stack.push(p);
                }
            } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Streams `file` line-by-line and immediately persists its proven-
/// complete sessions through `persister` (skipped in dry-run mode).
/// Bytes are hashed incrementally so `import_key`/marker generation
/// reflects the exact snapshot used for the in-memory parse, even when
/// the file is too large to buffer.
#[allow(clippy::too_many_arguments, reason = "import wiring")]
fn process_file(
    file: &Path,
    harness: Option<&str>,
    session_id_from: Option<&str>,
    remaining: &mut Option<u32>,
    persister: Option<&mut Persister>,
    summary: &mut ImportSummary,
    json: bool,
) -> Result<(), ExitCode> {
    let f = match File::open(file) {
        Ok(f) => f,
        Err(e) => {
            return Err(emit_jsonl_error(
                json,
                &format!("open {}: {e}", file.display()),
            ));
        }
    };
    let mut reader = BufReader::new(HashingReader::new(f));

    let mut by_session: BTreeMap<String, SessionImport> = BTreeMap::new();
    let mut incomplete_sessions: BTreeSet<String> = BTreeSet::new();
    let mut parser_box: Option<Box<dyn cairn_core::replay::TranscriptParser>> = None;
    let mut line_no = 0_usize;
    let mut budget_exhausted = false;
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(emit_jsonl_error(
                    json,
                    &format!("read {} line {}: {e}", file.display(), line_no + 1),
                ));
            }
        };
        let _ = n;
        line_no += 1;
        let line = buf.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }

        if budget_exhausted {
            // Drain phase: still hashing every byte (handled by the
            // reader). Identify the session id on each non-empty line
            // so we can drop any session that has more turns past the
            // cutoff, not just the one being parsed at the boundary.
            if let Some(parser) = parser_box.as_ref()
                && let Ok(sid) = parser.session_id_for(line, line_no)
            {
                incomplete_sessions.insert(sid);
            }
            continue;
        }

        if parser_box.is_none() {
            match detect_parser_with(line, harness, session_id_from) {
                Ok(p) => parser_box = Some(p),
                Err(e) => return Err(emit_parse_error(json, file, &e)),
            }
        }
        let parser = parser_box.as_ref().expect("parser initialized above");
        let parsed = match parser.parse_line(line, line_no) {
            Ok(p) => p,
            Err(e) => return Err(emit_parse_error(json, file, &e)),
        };
        push_turn(&mut by_session, &parsed);
        summary.turns_parsed += 1;
        if let Some(r) = remaining.as_mut() {
            *r = r.saturating_sub(1);
            if *r == 0 {
                // The session we just touched may or may not be
                // complete — its tail could continue past this line.
                // Mark it incomplete and let the drain phase pick up
                // any other sessions that also have post-cutoff turns.
                incomplete_sessions.insert(parsed.session_id.clone());
                budget_exhausted = true;
            }
        }
    }

    let content_digest_hex = digest_hex(&reader.into_inner().finalize());

    // Drop every session proven to extend past the cutoff. Other
    // sessions in this file are complete from this file's perspective
    // and safe to persist.
    for sid in &incomplete_sessions {
        by_session.remove(sid);
    }
    summary.sessions_parsed += by_session.len() as u64;

    if let Some(p) = persister {
        p.persist_file(file, &content_digest_hex, &by_session, summary, json)?;
    }
    Ok(())
}

/// Wraps a reader and feeds every byte read through a SHA-256 hasher.
/// Used to snapshot a transcript's content digest in a single pass over
/// the file, without buffering the whole file in memory.
struct HashingReader<R: Read> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn finalize(self) -> Vec<u8> {
        self.hasher.finalize().to_vec()
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

fn digest_hex(digest: &[u8]) -> String {
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn push_turn(by_session: &mut BTreeMap<String, SessionImport>, parsed: &ParsedTranscriptLine) {
    let entry = by_session.entry(parsed.session_id.clone()).or_default();
    if entry.role_first.is_empty() {
        entry.role_first.clone_from(&parsed.role);
    }
    entry.turns += 1;
    entry.blocks.push(serde_json::json!({
        "role": parsed.role,
        "blocks": parsed.blocks,
    }));
    if let Some(ts) = parsed.timestamp.as_deref() {
        match entry.earliest_ts.as_deref() {
            Some(cur) if cur <= ts => {}
            _ => entry.earliest_ts = Some(ts.to_owned()),
        }
        match entry.latest_ts.as_deref() {
            Some(cur) if cur >= ts => {}
            _ => entry.latest_ts = Some(ts.to_owned()),
        }
    }
}

/// Lazy holder for the tokio runtime, opened store, and marker
/// directory. Built once per invocation so each file's sessions can be
/// persisted as they finish parsing, instead of materializing the whole
/// import in memory and writing at the end.
struct Persister {
    rt: tokio::runtime::Runtime,
    store: Box<dyn MemoryStore>,
    marker_dir: PathBuf,
}

impl Persister {
    fn open(vault_root: &Path) -> Result<Self, String> {
        let marker_dir = vault_root.join(IMPORT_MARKER_DIR);
        fs::create_dir_all(&marker_dir)
            .map_err(|e| format!("create marker dir {}: {e}", marker_dir.display()))?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime build: {e}"))?;
        let db_path = vault_root.join(".cairn").join("cairn.db");
        let store = rt
            .block_on(async { cairn_store_sqlite::open(&db_path).await })
            .map_err(|e| format!("open store: {e}"))?;
        Ok(Self {
            rt,
            store: Box::new(store),
            marker_dir,
        })
    }

    fn persist_file(
        &mut self,
        file: &Path,
        content_digest_hex: &str,
        by_session: &BTreeMap<String, SessionImport>,
        summary: &mut ImportSummary,
        json: bool,
    ) -> Result<(), ExitCode> {
        for (session_id, session) in by_session {
            let key = import_key(file, session_id, content_digest_hex);
            let marker = self.marker_dir.join(format!("{key}.marker"));
            if marker.exists() {
                summary.sessions_skipped += 1;
                continue;
            }
            let records = match build_session_records(file, session_id, session, content_digest_hex)
            {
                Ok(rs) => rs,
                Err(e) => {
                    return Err(emit_jsonl_error(
                        json,
                        &format!("build record for {session_id}: {e}"),
                    ));
                }
            };
            for record in &records {
                if let Err(e) = self.rt.block_on(self.store.upsert(record)) {
                    return Err(emit_jsonl_error(json, &format!("upsert {session_id}: {e}")));
                }
            }
            if let Err(e) = write_marker(&marker, &key, file, session_id) {
                return Err(emit_jsonl_error(
                    json,
                    &format!("write marker {}: {e}", marker.display()),
                ));
            }
            summary.records_written += records.len() as u64;
        }
        Ok(())
    }
}

/// Stable per-version key. Mixes in `content_digest_hex` (captured once
/// at parse time, not re-read here) so an in-place edit re-keys the
/// import (skip-marker invalidates) without racing the bytes used for
/// the in-memory parse.
fn import_key(file: &Path, session_id: &str, content_digest_hex: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cairn:jsonl_import:version:v1\0");
    h.update(file.to_string_lossy().as_bytes());
    h.update([0]);
    h.update(session_id.as_bytes());
    h.update([0]);
    h.update(content_digest_hex.as_bytes());
    digest_hex(&h.finalize())
}

/// Stable logical-session key (path + `session_id`, no content). Used to
/// derive `target_id` so successive imports of the same logical session
/// supersede each other instead of forking parallel active records.
fn target_key(file: &Path, session_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cairn:jsonl_import:target:v1\0");
    h.update(file.to_string_lossy().as_bytes());
    h.update([0]);
    h.update(session_id.as_bytes());
    let digest = h.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Map a 64-char hex key onto a 26-char Crockford-base32 ULID string.
/// Same key → same id, so re-import on unchanged inputs is a no-op via
/// store upsert; changed keys mint distinct ids.
fn ulid_from_key(domain: &[u8], key: &str) -> String {
    let mut h = Sha256::new();
    h.update(domain);
    h.update(key.as_bytes());
    let digest = h.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ulid::Ulid::from_bytes(bytes).to_string()
}

/// Build the indexable `body` for an imported session. Concatenates non-
/// reasoning `text`/`tool_result` content so FTS hits the actual transcript.
/// Reasoning blocks are intentionally omitted from the body and live only
/// in `extra_frontmatter.trace_blocks`, gated by the search reasoning
/// filter.
fn build_searchable_body(header: &str, blocks: &[serde_json::Value]) -> String {
    let mut out = String::from(header);
    for block in blocks {
        let kind = block
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if kind.eq_ignore_ascii_case("reasoning") {
            continue;
        }
        let text = match kind {
            "text" => block.get("text").and_then(|v| v.as_str()),
            "tool_result" => block.get("content").and_then(|v| v.as_str()),
            "tool_use" => block.get("tool").and_then(|v| v.as_str()),
            _ => None,
        };
        if let Some(t) = text
            && !t.is_empty()
        {
            out.push_str("\n\n");
            out.push_str(t);
        }
    }
    out
}

/// Build the indexable `body` for the reasoning sibling record so FTS
/// can match reasoning-only text when `--include-reasoning` is set.
/// The default-search reasoning filter hides this row based on
/// `extra_frontmatter.trace_blocks` kinds, not on the body, so opting
/// in does not change what is indexed — only what is returned.
fn build_reasoning_body(header: &str, blocks: &[serde_json::Value]) -> String {
    let mut out = String::from(header);
    for block in blocks {
        if let Some(text) = block.get("text").and_then(|v| v.as_str())
            && !text.is_empty()
        {
            out.push_str("\n\n");
            out.push_str(text);
        }
    }
    out
}

/// Flatten the per-turn `{role, blocks: [...]}` envelope into a single
/// list of trace blocks tagged with their role. Output shape matches
/// `extra_frontmatter.trace_blocks` consumed by
/// `cairn_core::verbs::search` so reasoning hiding works on imports.
fn flatten_blocks(turns: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for turn in turns {
        let role = turn.get("role").cloned().unwrap_or(serde_json::Value::Null);
        if let Some(blocks) = turn.get("blocks").and_then(|v| v.as_array()) {
            for block in blocks {
                let mut entry = block.clone();
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("role".to_owned(), role.clone());
                }
                out.push(entry);
            }
        }
    }
    out
}

fn write_marker(marker: &Path, key: &str, file: &Path, session_id: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(marker)?;
    let body = serde_json::json!({
        "import_key": key,
        "file": file.to_string_lossy(),
        "session_id": session_id,
    });
    f.write_all(body.to_string().as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Build the MemoryRecord(s) persisted for one imported session.
///
/// Always returns a "session" record carrying scrubbed (non-reasoning)
/// blocks. When the source contained reasoning, also returns a sibling
/// "reasoning" record carrying ONLY the reasoning blocks under
/// `trace_blocks`, so the search-side reasoning filter
/// (`candidate_has_reasoning`) hides it by default and surfaces it only
/// when `--include-reasoning` is set.
fn build_session_records(
    file: &Path,
    session_id: &str,
    session: &SessionImport,
    content_digest_hex: &str,
) -> anyhow::Result<Vec<MemoryRecord>> {
    let key = import_key(file, session_id, content_digest_hex);
    let target_id_text = ulid_from_key(
        b"cairn:jsonl_import:target:v1\0",
        &target_key(file, session_id),
    );
    let reasoning_target_text = ulid_from_key(
        b"cairn:jsonl_import:reasoning_target:v1\0",
        &target_key(file, session_id),
    );
    let session_id_text = ulid_from_key(b"cairn:jsonl_import:version:v1\0", &key);
    let reasoning_id_text = ulid_from_key(b"cairn:jsonl_import:reasoning:v1\0", &key);
    let target_id = TargetId::parse(target_id_text).map_err(anyhow::Error::msg)?;
    let reasoning_target_id = TargetId::parse(reasoning_target_text).map_err(anyhow::Error::msg)?;

    let author = Identity::parse(CLI_AUTHOR_ID).map_err(anyhow::Error::msg)?;
    let import_ts = now_timestamp()?;
    let created_at = session
        .earliest_ts
        .as_deref()
        .and_then(parse_rfc3339)
        .unwrap_or_else(|| import_ts.clone());
    let updated_at = session
        .latest_ts
        .as_deref()
        .and_then(parse_rfc3339)
        .unwrap_or_else(|| import_ts.clone());

    let blocks = flatten_blocks(&session.blocks);
    let (reasoning_blocks, non_reasoning_blocks): (Vec<_>, Vec<_>) =
        blocks.into_iter().partition(|b| {
            b.get("kind")
                .and_then(|v| v.as_str())
                .is_some_and(|k| k.eq_ignore_ascii_case("reasoning"))
        });

    let header = format!(
        "Imported {turns} turn(s) from {path} (session {sid})",
        turns = session.turns,
        path = file.display(),
        sid = session_id
    );
    let body = build_searchable_body(&header, &non_reasoning_blocks);

    let session_record = build_record(
        RecordId::parse(session_id_text).map_err(anyhow::Error::msg)?,
        target_id,
        session_id,
        &author,
        &created_at,
        &updated_at,
        &body,
        non_reasoning_blocks,
        &key,
        file,
        false,
    )?;

    if reasoning_blocks.is_empty() {
        return Ok(vec![session_record]);
    }

    // Sibling reasoning record. `trace_blocks` carries the reasoning
    // entries verbatim, AND `body` contains the concatenated reasoning
    // text so FTS can match callers who opt in with
    // `--include-reasoning`. The search-side filter
    // (`candidate_has_reasoning`) still hides this row from default
    // results based on the `trace_blocks` kind, not the body content,
    // so privacy by default is preserved. Distinct `target_id` keeps
    // both rows independently active (sharing one would supersede the
    // visible session record).
    let reasoning_header = format!(
        "Reasoning for imported session {sid} ({n} block(s))",
        sid = session_id,
        n = reasoning_blocks.len()
    );
    let reasoning_body = build_reasoning_body(&reasoning_header, &reasoning_blocks);
    let reasoning_record = build_record(
        RecordId::parse(reasoning_id_text).map_err(anyhow::Error::msg)?,
        reasoning_target_id,
        session_id,
        &author,
        &created_at,
        &updated_at,
        &reasoning_body,
        reasoning_blocks,
        &key,
        file,
        true,
    )?;

    Ok(vec![session_record, reasoning_record])
}

#[allow(clippy::too_many_arguments, reason = "internal record builder")]
fn build_record(
    id: RecordId,
    target_id: TargetId,
    session_id: &str,
    author: &Identity,
    created_at: &Rfc3339Timestamp,
    updated_at: &Rfc3339Timestamp,
    body: &str,
    trace_blocks: Vec<serde_json::Value>,
    import_key: &str,
    file: &Path,
    is_reasoning_addendum: bool,
) -> anyhow::Result<MemoryRecord> {
    let source_hash = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
    let mut extra = std::collections::BTreeMap::new();
    extra.insert(
        "import_key".to_owned(),
        serde_json::Value::String(import_key.to_owned()),
    );
    extra.insert(
        "import_source".to_owned(),
        serde_json::Value::String("jsonl_import".to_owned()),
    );
    extra.insert(
        "original_session_id".to_owned(),
        serde_json::Value::String(session_id.to_owned()),
    );
    extra.insert(
        "source_file".to_owned(),
        serde_json::Value::String(file.display().to_string()),
    );
    extra.insert(
        "trace_blocks".to_owned(),
        serde_json::Value::Array(trace_blocks),
    );
    if is_reasoning_addendum {
        extra.insert(
            "import_role".to_owned(),
            serde_json::Value::String("reasoning_addendum".to_owned()),
        );
    }

    let scope = ScopeTuple {
        session_id: Some(session_id.to_owned()),
        agent: Some(CLI_AUTHOR_ID.to_owned()),
        ..ScopeTuple::default()
    };

    let source_id = SourceId::parse(id.as_str().to_owned()).map_err(anyhow::Error::msg)?;
    let record = MemoryRecord {
        id,
        target_id,
        kind: MemoryKind::Trace,
        class: MemoryClass::Episodic,
        visibility: MemoryVisibility::Private,
        scope,
        body: body.to_owned(),
        source_ids: vec![source_id.clone()],
        provenance: Provenance {
            source_sensor: Identity::parse(CLI_SENSOR_ID).map_err(anyhow::Error::msg)?,
            created_at: created_at.clone(),
            originating_agent_id: author.clone(),
            source_ids: vec![source_id],
            source_hash,
            consent_ref: "consent:cli:p0".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: updated_at.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: author.clone(),
            at: updated_at.clone(),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))
            .map_err(anyhow::Error::msg)?,
        tags: Vec::new(),
        extra_frontmatter: extra,
        consent_model: None,
    };
    record.validate().map_err(anyhow::Error::msg)?;
    Ok(record)
}

fn now_timestamp() -> anyhow::Result<Rfc3339Timestamp> {
    let raw = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Rfc3339Timestamp::parse(raw).map_err(anyhow::Error::msg)
}

fn parse_rfc3339(s: &str) -> Option<Rfc3339Timestamp> {
    // Normalize to seconds precision so the domain validator accepts it.
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let normalized = dt
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Rfc3339Timestamp::parse(normalized).ok()
}

fn emit_summary(json: bool, dry_run: bool, summary: &ImportSummary, files: &[PathBuf]) {
    if json {
        let op = new_operation_id();
        let resp = Response {
            contract: "cairn.mcp.v1".to_owned(),
            data: Some(ResponseData::Ingest(IngestData {
                cache_hits: None,
                cache_misses: None,
                cache_writes: None,
                files_processed: Some(summary.files_scanned),
                plan_ref: None,
                // No single new RecordId on a multi-session import; reuse the
                // operation id so the field carries a real ULID and the
                // contract stays valid for downstream JSON consumers.
                record_id: op.clone(),
                session_id: "jsonl_import".to_owned(),
                jsonl_summary: Some(IngestDataJsonlSummary {
                    files_scanned: Some(summary.files_scanned),
                    sessions_parsed: Some(summary.sessions_parsed),
                    turns_parsed: Some(summary.turns_parsed),
                    records_written: Some(summary.records_written),
                    sessions_skipped: Some(summary.sessions_skipped),
                    elapsed_ms: Some(summary.elapsed_ms),
                }),
            })),
            error: None,
            operation_id: op,
            policy_trace: Vec::<ResponsePolicyTrace>::new(),
            status: ResponseStatus::Committed,
            target: None,
            verb: ResponseVerb::Ingest,
        };
        emit_json(&resp);
    } else {
        println!("Scanning {} transcript files...", files.len());
        println!(
            "  Parsed   {} sessions, {} turns",
            summary.sessions_parsed, summary.turns_parsed
        );
        if dry_run {
            println!(
                "  Dry-run: {} would import, {} already imported",
                summary
                    .sessions_parsed
                    .saturating_sub(summary.sessions_skipped),
                summary.sessions_skipped
            );
        } else {
            println!(
                "  Imported {} sessions, {} records",
                summary.records_written, summary.records_written
            );
            println!("  Skipped  {} (idempotency)", summary.sessions_skipped);
        }
        println!("Elapsed: {}ms", summary.elapsed_ms);
    }
}

fn emit_jsonl_error(json: bool, message: &str) -> ExitCode {
    if json {
        let resp = Response {
            contract: "cairn.mcp.v1".to_owned(),
            data: None,
            error: Some(serde_json::json!({
                "code": "Internal",
                "message": message,
            })),
            operation_id: new_operation_id(),
            policy_trace: Vec::<ResponsePolicyTrace>::new(),
            status: ResponseStatus::Aborted,
            target: None,
            verb: ResponseVerb::Ingest,
        };
        emit_json(&resp);
    } else {
        let op = new_operation_id();
        human_error("ingest", "Internal", message, &op);
    }
    ExitCode::FAILURE
}

fn emit_parse_error(json: bool, file: &Path, err: &ParseError) -> ExitCode {
    let msg = format!("{}: {err}", file.display());
    eprintln!("cairn ingest --jsonl: {msg}");
    emit_jsonl_error(json, &msg)
}
