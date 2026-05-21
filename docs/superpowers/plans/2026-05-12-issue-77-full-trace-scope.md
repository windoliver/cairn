# Issue 77 Full Trace Scope Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close issue #77 in one PR by admitting every trace event variant into the existing `capture_trace` persistence path.

**Architecture:** Keep the current `MemoryKind::Trace` store schema and `capture_trace` importer. Widen the pure classifier in `cairn-core`, add a core-owned terminal trace-body helper that composes dispatch + squash, and update the CLI importer to use that helper before privacy filtering and projection.

**Tech Stack:** Rust 2024, `cairn-core`, `cairn-cli`, `cairn-store-sqlite`, Tokio tests, in-memory SQLite, `cargo test`, `cargo clippy`.

---

## File Structure

- Modify `crates/cairn-core/src/pipeline/capture_trace.rs`
  - Extend `classify`.
  - Add classifier tests for hook `ToolOutput`, terminal `ToolOutput`, proactive `AgentMessage`, and rejected ambiguous cases.
- Modify `crates/cairn-core/src/pipeline/dispatch.rs`
  - Add a public helper that returns trace-body bytes after dispatch/squash.
  - Add dispatch helper tests for interactive squash, structured bypass, and legacy context failure.
- Modify `crates/cairn-cli/src/verbs/capture_trace.rs`
  - Replace the hook-only dispatch check with the new terminal-aware trace body resolution.
- Modify `crates/cairn-cli/tests/capture_trace_verb.rs`
  - Add helpers for terminal/proactive trace events.
  - Add a full-turn integration test with all non-summary trace variants plus generated summary.
  - Add privacy/linkage assertions for terminal tool output.

## Task 1: Widen Core Trace Classification

**Files:**
- Modify: `crates/cairn-core/src/pipeline/capture_trace.rs`

- [ ] **Step 1: Write failing classifier tests**

Add these helpers and tests inside `#[cfg(test)] mod tests` in `crates/cairn-core/src/pipeline/capture_trace.rs`:

```rust
fn mk_terminal_event(tool_id: Option<&str>) -> CaptureEvent {
    let mut event = mk_hook_event("UserPromptSubmit");
    event.sensor_id =
        Identity::parse("snr:local:terminal:default:v1").expect("valid terminal sensor");
    event.actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: event.sensor_id.clone(),
        at: ts(),
    }];
    event.refs = Some(CaptureRefs {
        session_id: Some("sess".into()),
        turn_id: Some("turn".into()),
        tool_id: tool_id.map(ToOwned::to_owned),
    });
    event.payload_ref = "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into();
    event.payload = CapturePayload::Terminal {
        command: "cargo test".into(),
        exit_code: Some(0),
        context: Some(crate::domain::capture::TerminalContext::NonInteractiveOrStructured),
    };
    event.source_family = SourceFamily::Terminal;
    event
}

fn mk_proactive_event(kind: &str) -> CaptureEvent {
    let mut event = mk_hook_event("UserPromptSubmit");
    event.sensor_id =
        Identity::parse("snr:local:proactive:codex:v1").expect("valid proactive sensor");
    event.capture_mode = CaptureMode::Proactive;
    event.actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: Identity::parse("agt:codex:gpt-5:main:v1").expect("valid agent"),
        at: ts(),
    }];
    event.payload_ref = "sources/proactive/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into();
    event.payload = CapturePayload::Proactive {
        kind: kind.into(),
        rationale: "captured final agent response".into(),
    };
    event.source_family = SourceFamily::Proactive;
    event
}

#[test]
fn classifies_hook_tool_output() {
    let mut event = mk_hook_event("ToolOutput");
    event.refs.as_mut().expect("refs").tool_id = Some("toolu_1".into());
    assert_eq!(classify(&event).unwrap(), TraceEvent::ToolOutput);
}

#[test]
fn classifies_terminal_tool_output_when_tool_ref_present() {
    assert_eq!(
        classify(&mk_terminal_event(Some("toolu_1"))).unwrap(),
        TraceEvent::ToolOutput
    );
}

#[test]
fn rejects_terminal_without_tool_ref() {
    assert!(matches!(
        classify(&mk_terminal_event(None)).unwrap_err(),
        TraceProjectError::Unclassifiable
    ));
}

#[test]
fn classifies_proactive_agent_message_kinds() {
    assert_eq!(
        classify(&mk_proactive_event("agent_message")).unwrap(),
        TraceEvent::AgentMessage
    );
    assert_eq!(
        classify(&mk_proactive_event("assistant_message")).unwrap(),
        TraceEvent::AgentMessage
    );
}

#[test]
fn rejects_other_proactive_kinds() {
    assert!(matches!(
        classify(&mk_proactive_event("knowledge_gap")).unwrap_err(),
        TraceProjectError::Unclassifiable
    ));
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p cairn-core pipeline::capture_trace::tests::classifies_terminal_tool_output_when_tool_ref_present pipeline::capture_trace::tests::classifies_proactive_agent_message_kinds
```

Expected: failures because `classify` still rejects terminal/proactive payloads.

- [ ] **Step 3: Implement minimal classifier changes**

Change `classify` to:

```rust
pub fn classify(event: &CaptureEvent) -> Result<TraceEvent, TraceProjectError> {
    match &event.payload {
        CapturePayload::Hook { hook_name, .. } => match hook_name.as_str() {
            "UserPromptSubmit" => Ok(TraceEvent::UserMessage),
            "PreToolUse" => Ok(TraceEvent::PreTool),
            "PostToolUse" => Ok(TraceEvent::PostTool),
            "ToolOutput" => Ok(TraceEvent::ToolOutput),
            "Stop" => Ok(TraceEvent::Stop),
            _ => Err(TraceProjectError::Unclassifiable),
        },
        CapturePayload::Terminal { .. }
            if event
                .refs
                .as_ref()
                .and_then(|refs| refs.tool_id.as_ref())
                .is_some() =>
        {
            Ok(TraceEvent::ToolOutput)
        }
        CapturePayload::Proactive { kind, .. }
            if matches!(kind.as_str(), "agent_message" | "assistant_message") =>
        {
            Ok(TraceEvent::AgentMessage)
        }
        _ => Err(TraceProjectError::Unclassifiable),
    }
}
```

Update the function doc comment to describe all admitted shapes.

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p cairn-core pipeline::capture_trace::tests
```

Expected: all `pipeline::capture_trace` tests pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/cairn-core/src/pipeline/capture_trace.rs
git commit -m "feat(core): classify full trace event set"
```

## Task 2: Add Terminal Trace Body Routing Helper

**Files:**
- Modify: `crates/cairn-core/src/pipeline/dispatch.rs`

- [ ] **Step 1: Write failing dispatch helper tests**

Add tests near the existing `dispatch` tests in `crates/cairn-core/src/pipeline/dispatch.rs`:

```rust
#[test]
fn trace_body_squashes_interactive_terminal_output() {
    let raw = b"same\nsame\nsame\n";
    let event = terminal_event_with_context(raw, Some(TerminalContext::InteractiveTty));

    let body = trace_body_bytes(&event, raw, &DefaultRegistry)
        .expect("interactive terminal should route through squash");

    let text = String::from_utf8(body).expect("squash output is utf-8 for fixture");
    assert!(text.contains("[repeated 3x]") || text.len() < raw.len());
}

#[test]
fn trace_body_bypasses_structured_terminal_output() {
    let raw = br#"{"ok":true}"#;
    let event =
        terminal_event_with_context(raw, Some(TerminalContext::NonInteractiveOrStructured));

    let body = trace_body_bytes(&event, raw, &DefaultRegistry)
        .expect("structured terminal output should bypass squash");

    assert_eq!(body, raw);
}

#[test]
fn trace_body_rejects_legacy_terminal_context() {
    let raw = b"legacy output";
    let event = terminal_event_with_context(raw, None);

    let err = trace_body_bytes(&event, raw, &DefaultRegistry).unwrap_err();

    assert!(err.to_string().contains("legacy"));
}
```

Use an existing test helper if one is present in the module. If there is no accessible helper, add a local helper that constructs a valid terminal `CaptureEvent` with matching `payload_hash`, sensor `snr:local:terminal:default:v1`, `CaptureMode::Auto`, and `SourceFamily::Terminal`.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test -p cairn-core pipeline::dispatch::tests::trace_body
```

Expected: compile failure because `trace_body_bytes` does not exist.

- [ ] **Step 3: Implement helper**

Add imports:

```rust
use crate::pipeline::squash::{self, SquashConfig, UnstructuredBindError};
```

Add the error enum and helper:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceBodyRouteError {
    #[error("terminal trace squash bind: {0}")]
    SquashBind(#[from] UnstructuredBindError),
    #[error("terminal trace legacy context requires migration before trace import")]
    LegacyTerminalContext,
}

pub fn trace_body_bytes<R: ToolSchemaLookup + ?Sized>(
    event: &CaptureEvent,
    raw: &[u8],
    registry: &R,
) -> Result<Vec<u8>, TraceBodyRouteError> {
    match dispatch(event, registry) {
        DispatchDecision::Squash(admission) => {
            let wrapped = squash::UnstructuredTextBytes::try_from_terminal_event(
                event, raw, admission,
            )?;
            Ok(squash::squash(wrapped, &SquashConfig::default()).compacted_bytes)
        }
        DispatchDecision::Bypass(BypassReason::TerminalLegacyMissingContext) => {
            Err(TraceBodyRouteError::LegacyTerminalContext)
        }
        DispatchDecision::Bypass(_) => Ok(raw.to_vec()),
    }
}
```

- [ ] **Step 4: Run tests and verify GREEN**

Run:

```bash
cargo test -p cairn-core pipeline::dispatch::tests::trace_body
```

Expected: dispatch helper tests pass.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/cairn-core/src/pipeline/dispatch.rs
git commit -m "feat(core): route terminal trace bodies through dispatch"
```

## Task 3: Wire Importer And Full-Turn Integration Test

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Modify: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Write failing CLI integration test**

In `crates/cairn-cli/tests/capture_trace_verb.rs`, add imports:

```rust
use cairn_core::domain::TerminalContext;
```

Add a generic source writer:

```rust
fn write_source_for_family(vault: &Path, family: &str, filename: &str, content: &str) -> String {
    let dir = vault.join("sources").join(family);
    std::fs::create_dir_all(&dir).expect("create source family dir");
    let abs = dir.join(filename);
    std::fs::write(&abs, content).expect("write source file");
    format!("sources/{family}/{filename}")
}
```

Add event builders:

```rust
#[allow(clippy::too_many_arguments)]
fn make_terminal_event(
    event_id: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    tool_id: &str,
    payload_ref: &str,
    payload_hash_hex: &str,
    context: TerminalContext,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:terminal:default:v1").expect("valid terminal sensor");
    let hash_str = format!("sha256:{payload_hash_hex}");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("valid timestamp"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: Some(tool_id.to_owned()),
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("valid hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("valid timestamp"),
        payload: CapturePayload::Terminal {
            command: "cargo test".to_owned(),
            exit_code: Some(0),
            context: Some(context),
        },
        source_family: SourceFamily::Terminal,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_agent_event(
    event_id: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    payload_ref: &str,
    payload_hash_hex: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:proactive:codex:v1").expect("valid proactive sensor");
    let hash_str = format!("sha256:{payload_hash_hex}");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("valid ULID"),
        sensor_id: sensor,
        capture_mode: CaptureMode::Proactive,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("agt:codex:gpt-5:main:v1").expect("valid agent"),
            at: Rfc3339Timestamp::parse(timestamp).expect("valid timestamp"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("valid hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("valid timestamp"),
        payload: CapturePayload::Proactive {
            kind: "agent_message".to_owned(),
            rationale: "final response".to_owned(),
        },
        source_family: SourceFamily::Proactive,
    }
}
```

Add the test:

```rust
#[tokio::test]
async fn full_scope_turn_persists_all_trace_event_variants() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("full_trace.jsonl");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-full";
    let tool_id = "toolu_full_01";

    let user = "please run the test suite";
    let pre = r#"{"tool":"bash","input":{"command":"cargo test"}}"#;
    let post = r#"{"tool":"bash","status":"ok"}"#;
    let output = "TOKEN=secret\nsame\nsame\nsame\n";
    let agent = "I ran the test suite and it passed.";
    let stop = "stop: turn complete";

    let id_user = "01ARZ3NDEKTSV4RRFFQ69G5FAN";
    let id_pre = "01ARZ3NDEKTSV4RRFFQ69G5FAP";
    let id_post = "01ARZ3NDEKTSV4RRFFQ69G5FAQ";
    let id_output = "01ARZ3NDEKTSV4RRFFQ69G5FAR";
    let id_agent = "01ARZ3NDEKTSV4RRFFQ69G5FAS";
    let id_stop = "01ARZ3NDEKTSV4RRFFQ69G5FAT";

    let user_ref = write_source(vault.path(), &format!("{id_user}.txt"), user);
    let pre_ref = write_source(vault.path(), &format!("{id_pre}.txt"), pre);
    let post_ref = write_source(vault.path(), &format!("{id_post}.txt"), post);
    let output_ref =
        write_source_for_family(vault.path(), "terminal", &format!("{id_output}.txt"), output);
    let agent_ref =
        write_source_for_family(vault.path(), "proactive", &format!("{id_agent}.txt"), agent);
    let stop_ref = write_source(vault.path(), &format!("{id_stop}.txt"), stop);

    let events = vec![
        make_event(id_user, "UserPromptSubmit", session, turn, "2026-05-12T00:00:01Z", None, &user_ref, &sha256_hex(user)),
        make_event(id_pre, "PreToolUse", session, turn, "2026-05-12T00:00:02Z", Some(tool_id.to_owned()), &pre_ref, &sha256_hex(pre)),
        make_event(id_post, "PostToolUse", session, turn, "2026-05-12T00:00:03Z", Some(tool_id.to_owned()), &post_ref, &sha256_hex(post)),
        make_terminal_event(id_output, session, turn, "2026-05-12T00:00:04Z", tool_id, &output_ref, &sha256_hex(output), TerminalContext::InteractiveTty),
        make_agent_event(id_agent, session, turn, "2026-05-12T00:00:05Z", &agent_ref, &sha256_hex(agent)),
        make_event(id_stop, "Stop", session, turn, "2026-05-12T00:00:06Z", None, &stop_ref, &sha256_hex(stop)),
    ];

    let mut f = std::fs::File::create(&jsonl_path).expect("create JSONL file");
    for ev in &events {
        writeln!(f, "{}", serde_json::to_string(ev).expect("serialize")).expect("write JSONL");
    }

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should succeed");
    assert!(resp.failed_turns.is_empty(), "{:?}", resp.failed_turns);

    let session_id = SessionId::parse(session).expect("valid session_id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, turn)?;
            let events: Vec<&str> = rows
                .iter()
                .map(|row| row.extra_frontmatter["trace_event"].as_str().expect("trace_event"))
                .collect();
            assert_eq!(
                events,
                vec![
                    "user_message",
                    "pre_tool",
                    "post_tool",
                    "tool_output",
                    "agent_message",
                    "stop",
                ]
            );
            assert!(tx.turn_summary_exists(&session_id, turn)?);
            let tool_output = rows
                .iter()
                .find(|row| row.extra_frontmatter["trace_event"] == "tool_output")
                .expect("tool_output row");
            assert_eq!(
                tool_output.extra_frontmatter["trace"]["parent_event_id"],
                id_pre
            );
            assert!(!tool_output.body.contains("secret"), "terminal body must be redacted");
            assert!(tool_output.body.contains("[REDACTED]"));
            Ok(())
        })
        .await
        .expect("store query should succeed");
}
```

- [ ] **Step 2: Run test and verify RED**

Run:

```bash
cargo test -p cairn-cli --test capture_trace_verb full_scope_turn_persists_all_trace_event_variants
```

Expected: failure because terminal/proactive events are rejected by classifier/importer dispatch.

- [ ] **Step 3: Wire the importer to `trace_body_bytes`**

In `crates/cairn-cli/src/verbs/capture_trace.rs`:

1. Change dispatch imports from:

```rust
use cairn_core::pipeline::dispatch::{BypassReason, DefaultRegistry, DispatchDecision, dispatch};
```

to:

```rust
use cairn_core::pipeline::dispatch::{DefaultRegistry, trace_body_bytes};
```

2. Replace the explicit `match dispatch(event, &DefaultRegistry)` block with a call after `resolve_body_text` is replaced by raw bytes:

```rust
let raw_bytes = match resolve_body_bytes(vault_root, event).await {
    Ok(bytes) => bytes,
    Err(e) => {
        failed_turns.push((
            session_str.clone(),
            turn_str.clone(),
            format!("resolve_body: {e}"),
        ));
        group_failed = true;
        break;
    }
};
let routed = match trace_body_bytes(event, &raw_bytes, &DefaultRegistry) {
    Ok(bytes) => bytes,
    Err(e) => {
        failed_turns.push((
            session_str.clone(),
            turn_str.clone(),
            format!("trace_body: {e}"),
        ));
        group_failed = true;
        break;
    }
};
let raw_text = match String::from_utf8(routed) {
    Ok(text) => text,
    Err(e) => {
        failed_turns.push((
            session_str.clone(),
            turn_str.clone(),
            format!("trace_body utf8: {e}"),
        ));
        group_failed = true;
        break;
    }
};
```

3. Split the existing `resolve_body_text` into a byte-returning helper:

```rust
async fn resolve_body_bytes(vault_root: &Path, event: &CaptureEvent) -> anyhow::Result<Vec<u8>> {
    let rel = Path::new(&event.payload_ref);
    let full = vault_root.join(rel);
    let canon_root = vault_root
        .canonicalize()
        .with_context(|| format!("canonicalize vault root {}", vault_root.display()))?;
    let canon_full = full
        .canonicalize()
        .with_context(|| format!("canonicalize payload_ref {}", event.payload_ref))?;
    if !canon_full.starts_with(&canon_root) {
        anyhow::bail!(
            "payload_ref {} resolves outside vault root {}",
            event.payload_ref,
            vault_root.display()
        );
    }
    let bytes = tokio::fs::read(&canon_full)
        .await
        .with_context(|| format!("read payload_ref {}", event.payload_ref))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let got = format!("sha256:{:x}", h.finalize());
    if got != event.payload_hash.as_str() {
        anyhow::bail!(
            "payload_hash mismatch for {}: expected {}, got {}",
            event.payload_ref,
            event.payload_hash.as_str(),
            got
        );
    }
    Ok(bytes)
}

async fn resolve_body_text(vault_root: &Path, event: &CaptureEvent) -> anyhow::Result<String> {
    let bytes = resolve_body_bytes(vault_root, event).await?;
    String::from_utf8(bytes).context("payload bytes are not valid UTF-8")
}
```

If `resolve_body_text` is no longer used after the edit, delete it instead of retaining dead code.

- [ ] **Step 4: Run test and verify GREEN**

Run:

```bash
cargo test -p cairn-cli --test capture_trace_verb full_scope_turn_persists_all_trace_event_variants
```

Expected: the full-scope test passes.

- [ ] **Step 5: Run existing trace importer tests**

Run:

```bash
cargo test -p cairn-cli --test capture_trace_verb
```

Expected: all capture trace integration tests pass.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-cli/tests/capture_trace_verb.rs
git commit -m "feat(cli): import full-scope trace turns"
```

## Task 4: Verification And Cleanup

**Files:**
- Modify only files changed by earlier tasks if formatting or clippy requires it.

- [ ] **Step 1: Run focused core and CLI tests**

Run:

```bash
cargo test -p cairn-core pipeline::capture_trace
cargo test -p cairn-core pipeline::dispatch
cargo test -p cairn-cli --test capture_trace_verb
cargo test -p cairn-store-sqlite trace
cargo test -p cairn-cli --test forget_record
```

Expected: all commands exit `0`.

- [ ] **Step 2: Run formatting**

Run:

```bash
cargo fmt --all
```

Expected: command exits `0`.

- [ ] **Step 3: Run clippy for touched crates**

Run:

```bash
cargo clippy -p cairn-core -p cairn-cli -p cairn-store-sqlite --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 4: Run repository boundary check**

Run:

```bash
./scripts/check-core-boundary.sh
```

Expected: command exits `0`.

- [ ] **Step 5: Commit cleanup if needed**

If formatting or clippy changed files, run:

```bash
git add crates/cairn-core/src/pipeline/capture_trace.rs crates/cairn-core/src/pipeline/dispatch.rs crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-cli/tests/capture_trace_verb.rs
git commit -m "chore: verify full trace scope"
```

If there are no file changes, do not create an empty commit.

## Self-Review

- Spec coverage: Tasks cover classifier widening, terminal dispatch/squash routing, importer integration, full-turn reconstruction, parent links, privacy redaction, and focused search/forget-adjacent verification through existing tests.
- Placeholder scan: no placeholder markers or unspecified “add tests” steps remain.
- Type consistency: `TraceEvent`, `CapturePayload`, `TerminalContext`, `DefaultRegistry`, and `trace_body_bytes` names match the planned Rust modules and imports.
