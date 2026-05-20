# Issue 311 JSONL Import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add full-scope `cairn ingest --jsonl <path>` transcript backfill with `claude-code` parsing, generic fallback, reasoning-preserving `TraceBlock` support, and idempotent re-import behavior.

**Architecture:** Build in two internal layers on one branch. First extend the trace domain and default query behavior so Cairn can represent structured transcript blocks, including reasoning signatures. Then add a dedicated JSONL transcript import path to `ingest`, backed by a parser registry and a shared per-session persistence path factored from the existing trace replay flow.

**Tech Stack:** Rust workspace, Clap CLI, schema-driven codegen via `cairn-codegen`, SQLite-backed store, `insta` snapshots, `tokio` async file IO where needed.

---

### Task 1: Extend Ingest Schema And Generated Wire Types

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/ingest.json`
- Modify: `crates/cairn-core/tests/generated_wire.rs`
- Regenerate: `crates/cairn-core/src/generated/verbs/ingest.rs`
- Regenerate: `crates/cairn-core/src/generated/schemas/verbs/ingest.json`
- Regenerate: `crates/cairn-mcp/src/generated/schemas/verbs/ingest.json`
- Regenerate: `crates/cairn-cli/src/generated/mod.rs`
- Regenerate: `skills/cairn/SKILL.md`
- Test: `crates/cairn-idl/tests/codegen_snapshot.rs`

- [ ] **Step 1: Write the failing schema/wire test**

Add a new generated-wire test near the existing ingest arg coverage in `crates/cairn-core/tests/generated_wire.rs`:

```rust
#[test]
fn ingest_args_accept_jsonl_source_and_reject_body_mix() {
    let ok = serde_json::json!({
        "kind": "trace",
        "jsonl": "/tmp/session.jsonl",
        "harness": "claude-code",
        "recursive": true,
        "limit": 10
    });
    let parsed: cairn_core::generated::verbs::ingest::IngestArgs =
        serde_json::from_value(ok).expect("jsonl ingest args should deserialize");
    assert_eq!(parsed.jsonl.as_deref(), Some("/tmp/session.jsonl"));
    assert_eq!(parsed.harness.as_deref(), Some("claude-code"));

    let mixed = serde_json::json!({
        "kind": "trace",
        "jsonl": "/tmp/session.jsonl",
        "body": "forbidden mix"
    });
    let err = serde_json::from_value::<cairn_core::generated::verbs::ingest::IngestArgs>(mixed)
        .expect_err("jsonl + body must fail xor validation");
    assert!(
        err.to_string().contains("exactly one of"),
        "unexpected error: {err}"
    );
}
```

- [ ] **Step 2: Run the focused test to verify it fails**

Run:

```bash
cargo test -p cairn-core ingest_args_accept_jsonl_source_and_reject_body_mix -- --exact
```

Expected: FAIL because generated `IngestArgs` does not yet expose `jsonl`, `harness`, `recursive`, or `limit`.

- [ ] **Step 3: Update the ingest IDL**

Edit `crates/cairn-idl/schema/verbs/ingest.json` so the CLI flags and schema properties include transcript import:

```json
{
  "flags": [
    { "name": "kind", "long": "kind", "value_source": "string" },
    { "name": "body", "long": "body", "value_source": "string" },
    { "name": "file", "long": "file", "value_source": "path" },
    { "name": "folder", "long": "folder", "value_source": "path" },
    { "name": "jsonl", "long": "jsonl", "value_source": "path" },
    { "name": "url", "long": "url", "value_source": "string" },
    { "name": "session_id", "long": "session", "value_source": "string" },
    { "name": "session_id_from", "long": "session-id-from", "value_source": "string" },
    { "name": "harness", "long": "harness", "value_source": "string" },
    { "name": "recursive", "long": "recursive", "value_source": "bool" },
    { "name": "limit", "long": "limit", "value_source": "integer" },
    { "name": "tags", "long": "tags", "value_source": "list<string>" },
    { "name": "no_cache", "long": "no-cache", "value_source": "bool" }
  ]
}
```

And expand the source `oneOf` to include:

```json
{ "required": ["jsonl"] }
```

- [ ] **Step 4: Regenerate codegen outputs**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

Expected: generated files under `cairn-core`, `cairn-cli`, `cairn-mcp`, and `skills/cairn/` update without manual edits.

- [ ] **Step 5: Re-run the focused test and codegen snapshot**

Run:

```bash
cargo test -p cairn-core ingest_args_accept_jsonl_source_and_reject_body_mix -- --exact
cargo test -p cairn-idl codegen_snapshot -- --nocapture
```

Expected: both PASS. If snapshots change intentionally, review later with `cargo insta review`.

- [ ] **Step 6: Commit the schema/codegen slice**

```bash
git add crates/cairn-idl/schema/verbs/ingest.json crates/cairn-core/tests/generated_wire.rs crates/cairn-core/src/generated/verbs/ingest.rs crates/cairn-core/src/generated/schemas/verbs/ingest.json crates/cairn-mcp/src/generated/schemas/verbs/ingest.json crates/cairn-cli/src/generated/mod.rs skills/cairn/SKILL.md
git commit -m "feat: extend ingest schema for transcript import"
```

### Task 2: Land TraceBlock And Reasoning Round-Trip Support

**Files:**
- Modify: `crates/cairn-core/src/domain/trace.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Modify: `crates/cairn-core/src/pipeline/capture_trace.rs`
- Modify: `crates/cairn-core/src/pipeline/turn.rs`
- Test: `crates/cairn-core/tests/capture_event.rs`
- Test: `crates/cairn-core/tests/memory_record.rs`
- Test: `crates/cairn-core/tests/pipeline_filter_e2e.rs`

- [ ] **Step 1: Write the failing domain round-trip test**

Add a focused test in `crates/cairn-core/tests/capture_event.rs` or a new nearby trace-domain test block:

```rust
#[test]
fn trace_block_reasoning_round_trips_signature_verbatim() {
    use cairn_core::domain::trace::TraceBlock;

    let block = TraceBlock::Reasoning {
        text: "working through tool choice".into(),
        signature: Some("sig_abc123+/=".into()),
    };

    let json = serde_json::to_string(&block).expect("serialize");
    let back: TraceBlock = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, block);
}
```

- [ ] **Step 2: Run the focused test to verify it fails**

Run:

```bash
cargo test -p cairn-core trace_block_reasoning_round_trips_signature_verbatim -- --exact
```

Expected: FAIL because `TraceBlock` does not exist yet.

- [ ] **Step 3: Add the minimal `TraceBlock` type and exports**

Extend `crates/cairn-core/src/domain/trace.rs` with:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceBlock {
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Text {
        text: String,
    },
    ToolUse {
        tool: String,
        input: serde_json::Value,
        id: String,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}
```

And export it from `crates/cairn-core/src/domain/mod.rs`.

- [ ] **Step 4: Thread the new block type into trace projection**

Add the smallest possible plumbing in `crates/cairn-core/src/pipeline/capture_trace.rs` and `crates/cairn-core/src/pipeline/turn.rs` so structured blocks can be attached to projected trace records without breaking existing event replay paths. Start with optional storage fields and preserve current behavior when no blocks are present.

Use a minimal internal shape like:

```rust
#[derive(Debug, Clone, Default)]
pub struct ProjectedTraceBlocks {
    pub blocks: Vec<TraceBlock>,
}
```

Only add enough wiring to keep today’s `capture_trace` tests green while enabling transcript import to target the same representation in later tasks.

- [ ] **Step 5: Re-run the focused test plus nearby trace tests**

Run:

```bash
cargo test -p cairn-core trace_block_reasoning_round_trips_signature_verbatim -- --exact
cargo test -p cairn-core capture_event -- --nocapture
```

Expected: PASS for the new test, and existing trace/capture tests stay green.

- [ ] **Step 6: Commit the trace-block foundation**

```bash
git add crates/cairn-core/src/domain/trace.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/src/pipeline/capture_trace.rs crates/cairn-core/src/pipeline/turn.rs crates/cairn-core/tests/capture_event.rs crates/cairn-core/tests/memory_record.rs crates/cairn-core/tests/pipeline_filter_e2e.rs
git commit -m "feat: add structured trace blocks"
```

### Task 3: Hide Reasoning By Default And Add Explicit Opt-In

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/search.json`
- Modify: `crates/cairn-idl/schema/verbs/retrieve.json`
- Regenerate: `crates/cairn-core/src/generated/verbs/search.rs`
- Regenerate: `crates/cairn-core/src/generated/verbs/retrieve.rs`
- Modify: `crates/cairn-cli/src/verbs/search.rs`
- Modify: `crates/cairn-cli/src/verbs/retrieve.rs`
- Test: `crates/cairn-cli/tests/cli.rs`
- Test: `crates/cairn-cli/tests/verbs_smoke.rs`

- [ ] **Step 1: Write the failing CLI arg test for opt-in flags**

Add a test in `crates/cairn-cli/tests/cli.rs` that asserts help output or parsing includes the new flag:

```rust
#[test]
fn search_help_lists_include_reasoning_flag() {
    let out = cli()
        .args(["search", "--help"])
        .output()
        .expect("search --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("--include-reasoning"),
        "missing include-reasoning flag: {stdout}"
    );
}
```

- [ ] **Step 2: Run the focused CLI test to verify it fails**

Run:

```bash
cargo test -p cairn-cli search_help_lists_include_reasoning_flag -- --exact
```

Expected: FAIL because the flag does not exist yet.

- [ ] **Step 3: Add schema fields and regenerate**

Add `include_reasoning: boolean` to `search.json` and `retrieve.json`, wire CLI flag metadata, then regenerate:

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

The runtime behavior should treat missing flag as the default exclusion path.

- [ ] **Step 4: Implement minimal runtime gating**

In `crates/cairn-cli/src/verbs/search.rs` and `crates/cairn-cli/src/verbs/retrieve.rs`, thread the generated `include_reasoning` boolean into the request/filter layer so reasoning-marked records are excluded unless the flag is true.

Use a narrow boolean branch:

```rust
let include_reasoning = sub.get_flag("include_reasoning");
if !include_reasoning {
    request.exclude_trace_reasoning = true;
}
```

Adjust naming to the actual request types already present in those files.

- [ ] **Step 5: Re-run focused tests plus nearby verb coverage**

Run:

```bash
cargo test -p cairn-cli search_help_lists_include_reasoning_flag -- --exact
cargo test -p cairn-cli verbs_smoke -- --nocapture
```

Expected: PASS. Existing search/retrieve CLI behavior remains stable except for the new opt-in flag.

- [ ] **Step 6: Commit the visibility slice**

```bash
git add crates/cairn-idl/schema/verbs/search.json crates/cairn-idl/schema/verbs/retrieve.json crates/cairn-core/src/generated/verbs/search.rs crates/cairn-core/src/generated/verbs/retrieve.rs crates/cairn-cli/src/verbs/search.rs crates/cairn-cli/src/verbs/retrieve.rs crates/cairn-cli/tests/cli.rs crates/cairn-cli/tests/verbs_smoke.rs
git commit -m "feat: gate reasoning behind explicit opt-in"
```

### Task 4: Add Transcript Parser Registry And Parser Tests

**Files:**
- Create: `crates/cairn-core/src/replay/mod.rs`
- Create: `crates/cairn-core/src/replay/parser.rs`
- Create: `crates/cairn-core/src/replay/claude_code.rs`
- Create: `crates/cairn-core/src/replay/generic.rs`
- Modify: `crates/cairn-core/src/lib.rs`
- Test: `crates/cairn-core/tests/transcript_parser.rs`

- [ ] **Step 1: Write the failing Claude Code parser test**

Create `crates/cairn-core/tests/transcript_parser.rs` with:

```rust
#[test]
fn claude_code_parser_maps_mixed_blocks() {
    use cairn_core::replay::{detect_parser, TranscriptParser};
    use cairn_core::domain::trace::TraceBlock;

    let line = serde_json::json!({
        "sessionId": "sess-123",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "compare tools", "signature": "sig_xyz" },
                { "type": "text", "text": "I will inspect the repo." },
                { "type": "tool_use", "name": "rg", "input": { "pattern": "TraceBlock" }, "id": "toolu_1" },
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": "match", "is_error": false }
            ]
        }
    })
    .to_string();

    let parser = detect_parser(&line, None).expect("detect parser");
    let parsed = parser.parse_line(&line).expect("parse line");
    assert_eq!(parsed.session_id, "sess-123");
    assert!(matches!(parsed.blocks[0], TraceBlock::Reasoning { .. }));
    assert!(matches!(parsed.blocks[1], TraceBlock::Text { .. }));
    assert!(matches!(parsed.blocks[2], TraceBlock::ToolUse { .. }));
    assert!(matches!(parsed.blocks[3], TraceBlock::ToolResult { .. }));
}
```

- [ ] **Step 2: Run the parser test to verify it fails**

Run:

```bash
cargo test -p cairn-core claude_code_parser_maps_mixed_blocks -- --exact
```

Expected: FAIL because `cairn_core::replay` does not exist yet.

- [ ] **Step 3: Add the parser trait, normalized output, and registry**

Create the replay module with a minimal public contract:

```rust
pub struct ParsedTranscriptLine {
    pub session_id: String,
    pub role: String,
    pub blocks: Vec<TraceBlock>,
}

pub trait TranscriptParser {
    fn name(&self) -> &'static str;
    fn parse_line(&self, line: &str) -> Result<ParsedTranscriptLine, ParseError>;
    fn session_id_for(&self, line: &str) -> Result<String, ParseError>;
}
```

Add `detect_parser(first_line, forced_harness)` to choose `claude-code` or `generic`.

- [ ] **Step 4: Implement the minimal Claude Code and generic parsers**

In `claude_code.rs`, parse `message.content` block arrays into `TraceBlock` values.

In `generic.rs`, emit a single coarse `TraceBlock::Text` using the raw line:

```rust
Ok(ParsedTranscriptLine {
    session_id,
    role: "unknown".into(),
    blocks: vec![TraceBlock::Text { text: line.to_owned() }],
})
```

Use a `ParseError` that carries parser name and row context.

- [ ] **Step 5: Re-run parser tests plus malformed-line coverage**

Run:

```bash
cargo test -p cairn-core transcript_parser -- --nocapture
```

Expected: PASS for Claude Code parsing, generic fallback, and malformed-row diagnostics once those tests are added in the same file.

- [ ] **Step 6: Commit the parser layer**

```bash
git add crates/cairn-core/src/replay/mod.rs crates/cairn-core/src/replay/parser.rs crates/cairn-core/src/replay/claude_code.rs crates/cairn-core/src/replay/generic.rs crates/cairn-core/src/lib.rs crates/cairn-core/tests/transcript_parser.rs
git commit -m "feat: add transcript parser registry"
```

### Task 5: Add `ingest --jsonl` CLI Path And Shared Session Persistence

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest.rs`
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Create: `crates/cairn-cli/src/verbs/ingest_jsonl.rs`
- Test: `crates/cairn-cli/tests/cli.rs`
- Test: `crates/cairn-cli/tests/capture_trace_verb.rs`
- Test: `crates/cairn-cli/tests/ingest_jsonl.rs`

- [ ] **Step 1: Write the failing CLI integration test**

Create `crates/cairn-cli/tests/ingest_jsonl.rs` with:

```rust
#[test]
fn ingest_jsonl_help_lists_transcript_flags() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args(["ingest", "--help"])
        .output()
        .expect("ingest --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for needle in ["--jsonl", "--harness", "--session-id-from", "--recursive", "--limit"] {
        assert!(stdout.contains(needle), "missing {needle}: {stdout}");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-cli ingest_jsonl_help_lists_transcript_flags -- --exact
```

Expected: FAIL until the generated CLI picks up the new ingest flags.

- [ ] **Step 3: Add the new ingest branch**

In `crates/cairn-cli/src/verbs/ingest.rs`, branch before body/file/folder/url handling:

```rust
if let Some(path) = sub.get_one::<std::path::PathBuf>("jsonl") {
    return crate::verbs::ingest_jsonl::run(sub, json, path, vault_root);
}
```

Create `crates/cairn-cli/src/verbs/ingest_jsonl.rs` with a thin driver that:

- enumerates one file or many with `--recursive`
- reads first non-empty line for auto-detection
- uses `cairn_core::replay::detect_parser`
- groups parsed lines by session id
- emits dry-run counts or persists via shared helper

- [ ] **Step 4: Factor shared per-session apply logic from `capture_trace`**

Extract the smallest reusable helper from `crates/cairn-cli/src/verbs/capture_trace.rs` into a function that can persist one prepared session worth of projected trace material.

Target a helper shape like:

```rust
pub(crate) async fn persist_trace_session(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    session_id: &SessionId,
    turns: &[PreparedTraceTurn],
) -> anyhow::Result<()>
```

`capture_trace` should call the helper with its native prepared turns; `ingest_jsonl` should prepare analogous turns from transcript lines.

- [ ] **Step 5: Re-run focused tests**

Run:

```bash
cargo test -p cairn-cli ingest_jsonl_help_lists_transcript_flags -- --exact
cargo test -p cairn-cli capture_trace_verb -- --nocapture
```

Expected: PASS for the new help test, and existing `capture_trace` coverage stays green after the helper extraction.

- [ ] **Step 6: Commit the CLI import path**

```bash
git add crates/cairn-cli/src/verbs/ingest.rs crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-cli/src/verbs/ingest_jsonl.rs crates/cairn-cli/tests/cli.rs crates/cairn-cli/tests/capture_trace_verb.rs crates/cairn-cli/tests/ingest_jsonl.rs
git commit -m "feat: add jsonl ingest CLI path"
```

### Task 6: Add End-To-End Import, Idempotency, And Dry-Run Coverage

**Files:**
- Modify: `crates/cairn-cli/tests/ingest_jsonl.rs`
- Create: `crates/cairn-cli/tests/snapshots/ingest_jsonl__dry_run_summary.snap`
- Create: `fixtures/transcripts/claude-code-mixed.jsonl`
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`

- [ ] **Step 1: Write the failing end-to-end dry-run test**

Extend `crates/cairn-cli/tests/ingest_jsonl.rs` with:

```rust
#[test]
fn ingest_jsonl_dry_run_reports_sessions_without_writes() {
    let vault = tempfile::tempdir().expect("tempdir");
    let transcript = std::path::Path::new("fixtures/transcripts/claude-code-mixed.jsonl");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault"),
            "ingest",
            "--kind",
            "trace",
            "--jsonl",
            transcript.to_str().expect("utf-8 transcript"),
            "--harness",
            "claude-code",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("ingest --jsonl --dry-run");

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(body["status"], "committed");
    assert_eq!(body["data"]["sessions_parsed"], 1);
    assert_eq!(body["data"]["records_written"], 0);
}
```

- [ ] **Step 2: Run the dry-run test to verify it fails**

Run:

```bash
cargo test -p cairn-cli ingest_jsonl_dry_run_reports_sessions_without_writes -- --exact
```

Expected: FAIL because the transcript import response shape and logic are not complete yet.

- [ ] **Step 3: Add fixture transcript and implement summary/idempotency**

Create `fixtures/transcripts/claude-code-mixed.jsonl` with a ten-turn mixed transcript.

Implement:

- deterministic import key from file path plus session id
- skip path for already-imported sessions
- dry-run summary fields
- parser error formatting with parser name and row number

Use a stable hash helper:

```rust
fn import_key(path: &Path, session_id: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    h.update([0]);
    h.update(session_id.as_bytes());
    format!("{:x}", h.finalize())
}
```

- [ ] **Step 4: Add idempotency and malformed-row tests**

In `crates/cairn-cli/tests/ingest_jsonl.rs`, add:

```rust
#[test]
fn ingest_jsonl_second_run_is_idempotent() { /* first run writes, second run skips */ }

#[test]
fn ingest_jsonl_malformed_row_reports_parser_and_line() { /* assert parser + row in stderr/json */ }
```

Make the assertions concrete:

```rust
assert!(stderr.contains("claude-code"));
assert!(stderr.contains("line 3"));
```

- [ ] **Step 5: Re-run the end-to-end import suite**

Run:

```bash
cargo test -p cairn-cli ingest_jsonl -- --nocapture
cargo test -p cairn-cli envelope_tests -- --nocapture
```

Expected: PASS for dry-run, write-path, idempotency, and malformed-row coverage.

- [ ] **Step 6: Commit the end-to-end coverage**

```bash
git add crates/cairn-cli/tests/ingest_jsonl.rs crates/cairn-cli/tests/snapshots/ingest_jsonl__dry_run_summary.snap fixtures/transcripts/claude-code-mixed.jsonl crates/cairn-cli/tests/envelope_tests.rs
git commit -m "test: cover jsonl import end to end"
```

### Task 7: Full Verification And Snapshot Review

**Files:**
- Modify as needed from snapshot review
- Test: workspace-wide relevant suites

- [ ] **Step 1: Run targeted package verification**

Run:

```bash
cargo test -p cairn-core transcript_parser -- --nocapture
cargo test -p cairn-core generated_wire -- --nocapture
cargo test -p cairn-cli ingest_jsonl -- --nocapture
cargo test -p cairn-cli capture_trace_verb -- --nocapture
cargo test -p cairn-cli cli -- --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run codegen drift check**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: clean drift check.

- [ ] **Step 3: Review and accept snapshots if needed**

Run:

```bash
cargo insta review
```

Expected: accept only intentional snapshot diffs related to new ingest and parser behavior.

- [ ] **Step 4: Run one broader regression pass**

Run:

```bash
cargo test -p cairn-core
cargo test -p cairn-cli
```

Expected: full package suites green. If runtime cost is too high locally, at minimum record which command was skipped and why before stopping.

- [ ] **Step 5: Stage final fixes and commit**

```bash
git add -A
git commit -m "feat: implement transcript jsonl import"
```

- [ ] **Step 6: Prepare final handoff summary**

Summarize:

- new CLI surface
- reasoning visibility behavior
- parser support and limits
- idempotency behavior
- tests run
