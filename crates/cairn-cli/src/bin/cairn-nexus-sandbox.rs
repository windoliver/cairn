//! Minimal bundled Nexus sandbox sidecar.
//!
//! This binary implements the narrow Cairn-facing projection endpoints used by
//! the local sandbox profile. It keeps derived projection state separate from
//! `.cairn/cairn.db`, which remains authoritative.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use cairn_cli::nexus::HttpEndpoint;
use serde::{Deserialize, Serialize};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8765";
const DEFAULT_HEALTH_PATH: &str = "/health";
const STATE_FILE: &str = "projection-state.json";

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if wants_help(&args) {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("cairn-nexus-sandbox {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args != ["sandbox", "serve"] {
        eprintln!("usage: cairn-nexus-sandbox sandbox serve");
        return ExitCode::from(64);
    }

    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cairn-nexus-sandbox: {err}");
            ExitCode::from(69)
        }
    }
}

fn wants_help(args: &[String]) -> bool {
    args.is_empty()
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h" | "help"))
}

fn print_help() {
    println!(
        "cairn-nexus-sandbox {}\n\nUSAGE:\n    cairn-nexus-sandbox sandbox serve\n\nENV:\n    CAIRN_NEXUS_ENDPOINT      endpoint to bind, default {DEFAULT_ENDPOINT}\n    CAIRN_NEXUS_HEALTH_PATH   health path, default {DEFAULT_HEALTH_PATH}\n    CAIRN_VAULT_DIR           vault root passed by cairn\n    CAIRN_NEXUS_DATA_DIR      derived projection directory passed by cairn\n    CAIRN_SQLITE_DB           authoritative SQLite DB path passed by cairn",
        env!("CARGO_PKG_VERSION")
    );
}

fn serve() -> Result<(), String> {
    let endpoint_raw =
        std::env::var("CAIRN_NEXUS_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned());
    let health_path =
        std::env::var("CAIRN_NEXUS_HEALTH_PATH").unwrap_or_else(|_| DEFAULT_HEALTH_PATH.to_owned());
    let data_dir = std::env::var_os("CAIRN_NEXUS_DATA_DIR").map(PathBuf::from);
    validate_health_path(&health_path)?;
    let endpoint = HttpEndpoint::parse(&endpoint_raw)?;
    let mut state = ProjectionState::load(data_dir.as_deref())?;
    let listener = TcpListener::bind((endpoint.host.as_str(), endpoint.port))
        .map_err(|err| format!("binding {}:{}: {err}", endpoint.host, endpoint.port))?;

    for stream in listener.incoming() {
        let stream = stream.map_err(|err| format!("accepting connection: {err}"))?;
        handle_connection(stream, &health_path, data_dir.as_deref(), &mut state);
    }
    Ok(())
}

fn validate_health_path(health_path: &str) -> Result<(), String> {
    if !health_path.starts_with('/') {
        return Err("CAIRN_NEXUS_HEALTH_PATH must start with /".to_owned());
    }
    if health_path
        .bytes()
        .any(|byte| byte == b' ' || byte.is_ascii_control())
    {
        return Err("CAIRN_NEXUS_HEALTH_PATH must not contain spaces or controls".to_owned());
    }
    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    health_path: &str,
    data_dir: Option<&Path>,
    state: &mut ProjectionState,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let raw = match read_http_request(&mut stream) {
        Ok(raw) => raw,
        Err(err) => {
            write_text(&mut stream, "400 Bad Request", &err);
            return;
        }
    };
    let Some(header_end) = header_end(&raw) else {
        write_text(&mut stream, "400 Bad Request", "missing HTTP headers");
        return;
    };
    let headers = String::from_utf8_lossy(&raw[..header_end]);
    let body = &raw[header_end + 4..];
    let expected_prefix = format!("GET {health_path} HTTP/");
    if headers.starts_with(&expected_prefix) {
        let _ =
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    } else if headers.starts_with("POST /projection/apply HTTP/") {
        let request = match serde_json::from_slice::<ApplyRequest>(body) {
            Ok(request) => request,
            Err(err) => {
                write_text(
                    &mut stream,
                    "400 Bad Request",
                    &format!("invalid apply json: {err}"),
                );
                return;
            }
        };
        let (response, changed) = apply_response(state, request);
        if changed && let Err(err) = state.persist(data_dir) {
            write_text(
                &mut stream,
                "500 Internal Server Error",
                &format!("persist projection state: {err}"),
            );
            return;
        }
        write_json(&mut stream, &response);
    } else if headers.starts_with("POST /projection/search HTTP/") {
        let request = match serde_json::from_slice::<SearchRequest>(body) {
            Ok(request) => request,
            Err(err) => {
                write_text(
                    &mut stream,
                    "400 Bad Request",
                    &format!("invalid search json: {err}"),
                );
                return;
            }
        };
        write_json(&mut stream, &search_response(state, request));
    } else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    let mut content_length = None;
    let mut body_start = None;
    loop {
        let mut buf = [0_u8; 4096];
        match stream.read(&mut buf) {
            Ok(0) => {
                if let (Some(start), Some(length)) = (body_start, content_length)
                    && raw.len() < start + length
                {
                    return Err("request body ended before Content-Length".to_owned());
                }
                return Ok(raw);
            }
            Ok(read) => raw.extend_from_slice(&buf[..read]),
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                return Err("reading request timed out".to_owned());
            }
            Err(err) => return Err(format!("read request: {err}")),
        }

        if body_start.is_none()
            && let Some(end) = header_end(&raw)
        {
            body_start = Some(end + 4);
            content_length = Some(parse_content_length(&raw[..end])?);
        }
        if let (Some(start), Some(length)) = (body_start, content_length)
            && raw.len() >= start + length
        {
            raw.truncate(start + length);
            return Ok(raw);
        }
    }
}

fn header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &[u8]) -> Result<usize, String> {
    let headers =
        std::str::from_utf8(headers).map_err(|err| format!("headers are not utf-8: {err}"))?;
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|err| format!("invalid Content-Length: {err}"));
        }
    }
    Ok(0)
}

fn write_json<T: Serialize>(stream: &mut TcpStream, value: &T) {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{\"items\":[]}".to_vec());
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
}

fn write_text(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProjectionState {
    bm25s: BTreeMap<String, IndexedDocument>,
    parser: BTreeMap<String, ParserProjection>,
}

impl ProjectionState {
    fn load(data_dir: Option<&Path>) -> Result<Self, String> {
        let Some(data_dir) = data_dir else {
            return Ok(Self::default());
        };
        let path = data_dir.join(STATE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
        serde_json::from_str(&raw).map_err(|err| format!("parse {}: {err}", path.display()))
    }

    fn persist(&self, data_dir: Option<&Path>) -> Result<(), String> {
        let Some(data_dir) = data_dir else {
            return Ok(());
        };
        fs::create_dir_all(data_dir)
            .map_err(|err| format!("create {}: {err}", data_dir.display()))?;
        let path = data_dir.join(STATE_FILE);
        let raw = serde_json::to_vec_pretty(self)
            .map_err(|err| format!("serialize projection state: {err}"))?;
        fs::write(&path, raw).map_err(|err| format!("write {}: {err}", path.display()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexedDocument {
    record_hash: String,
    wal_sequence: u64,
    token_count: u32,
    terms: BTreeMap<String, u32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParserProjection {
    target: String,
    record_hash: String,
    source_hash: String,
    source_path: String,
    wal_sequence: u64,
    extracted_text_terms: BTreeMap<String, u32>,
}

#[derive(Deserialize)]
struct ApplyRequest {
    target: String,
    items: Vec<ApplyRequestItem>,
}

#[derive(Deserialize)]
struct ApplyRequestItem {
    record_id: String,
    wal_sequence: u64,
    record_hash: String,
    #[serde(default)]
    source_hash: Option<String>,
    #[serde(default)]
    source_path: Option<String>,
    body: String,
}

#[derive(Serialize)]
struct ApplyResponse {
    items: Vec<ApplyResponseItem>,
}

#[derive(Serialize)]
struct ApplyResponseItem {
    record_id: String,
    record_hash: String,
    source_hash: Option<String>,
    state: String,
    reason: Option<String>,
}

fn apply_response(state: &mut ProjectionState, request: ApplyRequest) -> (ApplyResponse, bool) {
    let mut changed = false;
    let mut items = Vec::new();
    reset_target(state, &request);
    for item in request.items {
        let record_id = item.record_id.clone();
        let record_hash = item.record_hash.clone();
        let source_hash = item.source_hash.clone();
        let result = apply_item(state, &request.target, item);
        if result.changed {
            changed = true;
        }
        items.push(ApplyResponseItem {
            record_id,
            record_hash,
            source_hash,
            state: result.state,
            reason: result.reason,
        });
    }
    (ApplyResponse { items }, changed)
}

fn reset_target(state: &mut ProjectionState, request: &ApplyRequest) {
    if request.target == "bm25s_lexical" {
        state.bm25s.clear();
        return;
    }
    if is_parser_target(&request.target) {
        state
            .parser
            .retain(|_, projection| projection.target != request.target);
    }
}

struct ApplyItemResult {
    state: String,
    reason: Option<String>,
    changed: bool,
}

fn apply_item(
    state: &mut ProjectionState,
    target: &str,
    item: ApplyRequestItem,
) -> ApplyItemResult {
    if target == "bm25s_lexical" {
        let terms = term_counts(&item.body);
        let token_count = terms.values().copied().sum();
        state.bm25s.insert(
            item.record_id,
            IndexedDocument {
                record_hash: item.record_hash,
                wal_sequence: item.wal_sequence,
                token_count,
                terms,
            },
        );
        return current_result(true);
    }

    if !is_parser_target(target) {
        return failed_result(format!("unknown projection target {target}"), false);
    }
    let Some(source_path) = item.source_path else {
        return failed_result("parser projection missing source_path".to_owned(), false);
    };
    let Some(source_hash) = item.source_hash else {
        return failed_result("parser projection missing source_hash".to_owned(), false);
    };
    if parser_target_for_source(&source_path).as_deref() != Some(target) {
        return failed_result(
            format!("source path {source_path} does not match projection target {target}"),
            false,
        );
    }
    if source_path.to_ascii_lowercase().contains("corrupt") {
        return failed_result("parser rejected malformed PDF header".to_owned(), false);
    }

    let key = format!("{}:{}:{}", target, item.record_id, source_hash);
    state.parser.insert(
        key,
        ParserProjection {
            target: target.to_owned(),
            record_hash: item.record_hash,
            source_hash,
            source_path,
            wal_sequence: item.wal_sequence,
            extracted_text_terms: term_counts(&item.body),
        },
    );
    current_result(true)
}

fn current_result(changed: bool) -> ApplyItemResult {
    ApplyItemResult {
        state: "current".to_owned(),
        reason: None,
        changed,
    }
}

fn failed_result(reason: String, changed: bool) -> ApplyItemResult {
    ApplyItemResult {
        state: "failed".to_owned(),
        reason: Some(reason),
        changed,
    }
}

fn is_parser_target(target: &str) -> bool {
    matches!(
        target,
        "parser_pdf_text"
            | "parser_docx_text"
            | "parser_video_frame_text"
            | "parser_vision_caption"
    )
}

fn parser_target_for_source(path: &str) -> Option<String> {
    let extension = Path::new(path).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("pdf") {
        return Some("parser_pdf_text".to_owned());
    }
    if extension.eq_ignore_ascii_case("docx") {
        return Some("parser_docx_text".to_owned());
    }
    if extension.eq_ignore_ascii_case("json") && path.to_ascii_lowercase().contains("frame") {
        return Some("parser_video_frame_text".to_owned());
    }
    if ["png", "jpg", "jpeg", "webp"]
        .iter()
        .any(|image_ext| extension.eq_ignore_ascii_case(image_ext))
    {
        return Some("parser_vision_caption".to_owned());
    }
    None
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    candidates: Vec<SearchCandidate>,
    limit: u32,
}

#[derive(Deserialize)]
struct SearchCandidate {
    record_id: String,
    record_hash: String,
}

#[derive(Serialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
}

#[derive(Serialize)]
struct SearchHit {
    record_id: String,
    record_hash: String,
    score: f64,
}

fn search_response(state: &ProjectionState, request: SearchRequest) -> SearchResponse {
    let query_terms = term_counts(&request.query);
    if query_terms.is_empty() || state.bm25s.is_empty() {
        return SearchResponse { hits: vec![] };
    }
    let mut hits = Vec::new();
    for candidate in request.candidates {
        let Some(document) = state.bm25s.get(&candidate.record_id) else {
            continue;
        };
        if document.record_hash != candidate.record_hash {
            continue;
        }
        let score = bm25_score(state, document, &query_terms);
        if score > 0.0 {
            hits.push(SearchHit {
                record_id: candidate.record_id,
                record_hash: candidate.record_hash,
                score,
            });
        }
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.record_id.cmp(&right.record_id))
    });
    let limit = usize::try_from(request.limit).unwrap_or(usize::MAX);
    hits.truncate(limit);
    SearchResponse { hits }
}

fn bm25_score(
    state: &ProjectionState,
    document: &IndexedDocument,
    query_terms: &BTreeMap<String, u32>,
) -> f64 {
    let doc_count = u32::try_from(state.bm25s.len()).unwrap_or(u32::MAX);
    let total_tokens = state.bm25s.values().fold(0_u32, |total, document| {
        total.saturating_add(document.token_count)
    });
    let average_len = if doc_count == 0 {
        1.0
    } else {
        f64::from(total_tokens.max(1)) / f64::from(doc_count)
    };
    let doc_len = f64::from(document.token_count.max(1));
    let k1 = 1.2_f64;
    let b = 0.75_f64;
    let mut score = 0.0_f64;
    for (term, query_count) in query_terms {
        let Some(term_count) = document.terms.get(term).copied() else {
            continue;
        };
        let df = document_frequency(state, term);
        let idf = ((f64::from(doc_count) - f64::from(df) + 0.5) / (f64::from(df) + 0.5) + 1.0).ln();
        let tf = f64::from(term_count);
        let numerator = tf * (k1 + 1.0);
        let denominator = tf + k1 * (1.0 - b + b * (doc_len / average_len));
        score += f64::from(*query_count) * idf * (numerator / denominator);
    }
    score
}

fn document_frequency(state: &ProjectionState, term: &str) -> u32 {
    let count = state
        .bm25s
        .values()
        .filter(|document| document.terms.contains_key(term))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn term_counts(text: &str) -> BTreeMap<String, u32> {
    let mut terms = BTreeMap::new();
    for term in tokenize(text) {
        let entry = terms.entry(term).or_insert(0_u32);
        *entry = entry.saturating_add(1);
    }
    terms
}

fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
}
