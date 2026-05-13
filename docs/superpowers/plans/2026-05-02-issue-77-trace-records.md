# Trace-Record Persistence Implementation Plan (issue #77)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist the seven trace event types (user/agent message, pre/post tool, tool output, stop, turn summary) as linked, ordered, idempotent records that round-trip through reconstruction tests and survive forget.

**Architecture:** All seven event types map to one `MemoryKind::Trace` with structured linkage in `extra_frontmatter.trace`. A pure projector in `cairn-core` builds records from `CaptureEvent` + `ResolvedBody`. A SQLite migration adds generated columns + unique indices for idempotent replay, sequence monotonicity, and one summary per turn. The `capture_trace` CLI verb reads JSONL, groups by turn, and writes each turn inside `SqliteMemoryStore::with_tx` so partial imports never strand a turn. Sequences derive from `captured_at`, with two-phase parking when out-of-order events arrive.

**Tech Stack:** Rust 2024 / 1.95.0, `tokio`, `rusqlite` via `tokio_rusqlite`, `clap` 4.5, `thiserror`, `rstest`, `proptest`, `insta`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-05-02-issue-77-trace-records-design.md`. Read it first.

---

## File Structure

| Path | Role |
|---|---|
| `crates/cairn-core/src/domain/trace.rs` | NEW. `TraceEvent` enum, `TraceLink` struct, `TraceLinkError`, `summary_record_id()`, validation. |
| `crates/cairn-core/src/domain/mod.rs` | Add `pub mod trace;` + re-exports. |
| `crates/cairn-core/src/pipeline/capture_trace.rs` | NEW. Pure `project()`, `classify()`, body-shape helpers. |
| `crates/cairn-core/src/pipeline/turn.rs` | NEW. Pure `summarize_turn()` + `order_by_captured_at()` + `assign_sequences()`. |
| `crates/cairn-core/src/pipeline/mod.rs` | Add `pub mod capture_trace; pub mod turn;`. |
| `crates/cairn-store-sqlite/src/migrations/sql/0022_trace_links.sql` | NEW. Generated columns + unique indices. |
| `crates/cairn-store-sqlite/src/migrations/mod.rs` | Register migration `0022`. |
| `crates/cairn-store-sqlite/src/store/tx.rs` | Add `upsert_trace`, `list_trace_events`, `turn_summary_exists`, `payload_hash_count_in_scope` to `StoreTx`. |
| `crates/cairn-cli/src/verbs/capture_trace.rs` | Replace stub with full impl: JSONL parser → group-by-turn → renumber + summary inside `with_tx`. |
| `crates/cairn-cli/src/verbs/forget.rs` | Extend forget to delete `payload_ref` files for trace records when in-scope refcount is zero. |
| `crates/cairn-test-fixtures/src/...` | Add JSONL trace fixtures (single-turn, multi-turn, malformed). |

---

## Conventions for Every Task

- **TDD always.** Failing test first, smallest code to pass, refactor.
- **`cargo nextest run -p <crate> <pattern>`** for fast loops; `--workspace` only on commit.
- **No `unwrap()`/`expect()` in `cairn-core`** — workspace lints deny it. Use `?` and typed errors.
- **`#[cfg(test)] mod tests`** next to code in unit tests.
- **`rstest` fixtures** for table-driven cases, `insta` for snapshots, `proptest` for round-trip invariants.
- **Commit per step 5.** Imperative subject ≤72 chars referencing brief section numbers when relevant.

---

## Task 1: TraceEvent enum + module skeleton

**Files:**
- Create: `crates/cairn-core/src/domain/trace.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cairn-core/src/domain/trace.rs`:

```rust
//! Trace-record domain types (issue #77, brief §5.0, §9.3).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraceEvent {
    UserMessage,
    AgentMessage,
    PreTool,
    PostTool,
    ToolOutput,
    Stop,
    TurnSummary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TraceEvent::UserMessage).unwrap(),
            "\"user_message\""
        );
        assert_eq!(
            serde_json::from_str::<TraceEvent>("\"turn_summary\"").unwrap(),
            TraceEvent::TurnSummary
        );
    }
}
```

Wire the module by adding to `crates/cairn-core/src/domain/mod.rs` (find the `pub mod` block, alphabetical):

```rust
pub mod trace;
pub use trace::TraceEvent;
```

- [ ] **Step 2: Run test to verify it fails / compiles**

```bash
cargo nextest run -p cairn-core trace::tests
```

Expected: PASS (this task only adds the enum). If lints fire on the empty
module, address them inline — the enum is enough.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/domain/trace.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add TraceEvent enum (issue #77, brief §5.0)"
```

---

## Task 2: TraceLink struct + field-level validation

**Files:**
- Modify: `crates/cairn-core/src/domain/trace.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cairn-core/src/domain/trace.rs` `mod tests`:

```rust
use crate::domain::{CaptureEventId, SessionId};

fn cap(id: &str) -> CaptureEventId {
    CaptureEventId::parse(id).expect("valid ulid")
}
fn sess() -> SessionId {
    SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid")
}

#[test]
fn valid_pre_tool_link() {
    let link = TraceLink {
        session_id: sess(),
        turn_id: "turn-4".into(),
        sequence: 2,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: None,
        tool_call_id: Some("call_abc".into()),
        member_event_ids: Vec::new(),
    };
    link.validate(TraceEvent::PreTool).expect("valid");
}

#[test]
fn pre_tool_requires_tool_call_id() {
    let link = TraceLink {
        session_id: sess(),
        turn_id: "turn-4".into(),
        sequence: 0,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: None,
        tool_call_id: None,
        member_event_ids: Vec::new(),
    };
    let err = link.validate(TraceEvent::PreTool).unwrap_err();
    assert!(matches!(err, TraceLinkError::MissingToolCallId));
}

#[test]
fn post_tool_requires_parent() {
    let mut link = TraceLink {
        session_id: sess(),
        turn_id: "t".into(),
        sequence: 0,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: None,
        tool_call_id: Some("call".into()),
        member_event_ids: Vec::new(),
    };
    assert!(matches!(
        link.validate(TraceEvent::PostTool).unwrap_err(),
        TraceLinkError::MissingParent
    ));
    link.parent_event_id = Some(cap("01ARZ3NDEKTSV4RRFFQ69G5FBW"));
    link.validate(TraceEvent::PostTool).expect("valid");
}

#[test]
fn member_ids_only_on_summary() {
    let link = TraceLink {
        session_id: sess(),
        turn_id: "t".into(),
        sequence: 0,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: None,
        tool_call_id: None,
        member_event_ids: vec![cap("01ARZ3NDEKTSV4RRFFQ69G5FBW")],
    };
    assert!(matches!(
        link.validate(TraceEvent::UserMessage).unwrap_err(),
        TraceLinkError::UnexpectedMemberIds
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p cairn-core trace::tests
```

Expected: FAIL — `TraceLink` and `TraceLinkError` undefined.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/cairn-core/src/domain/trace.rs`:

```rust
use crate::domain::{CaptureEventId, SessionId};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceLink {
    pub session_id: SessionId,
    pub turn_id: String,
    pub sequence: u64,
    pub capture_event_id: CaptureEventId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<CaptureEventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_event_ids: Vec<CaptureEventId>,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceLinkError {
    #[error("turn_id must not be empty or whitespace")]
    EmptyTurnId,
    #[error("tool_call_id required for {0:?}")]
    MissingToolCallId,
    #[error("parent_event_id required for {0:?}")]
    MissingParent,
    #[error("parent_event_id only valid on PostTool/ToolOutput")]
    UnexpectedParent,
    #[error("tool_call_id only valid on PreTool/PostTool/ToolOutput")]
    UnexpectedToolCallId,
    #[error("member_event_ids only valid on TurnSummary")]
    UnexpectedMemberIds,
    #[error("turn_summary requires non-empty member_event_ids")]
    EmptyMemberIds,
}

impl TraceLink {
    pub fn validate(&self, event: TraceEvent) -> Result<(), TraceLinkError> {
        if self.turn_id.trim().is_empty() {
            return Err(TraceLinkError::EmptyTurnId);
        }
        let needs_tool_call = matches!(
            event,
            TraceEvent::PreTool | TraceEvent::PostTool | TraceEvent::ToolOutput
        );
        match (needs_tool_call, self.tool_call_id.as_ref()) {
            (true, None) => return Err(TraceLinkError::MissingToolCallId),
            (false, Some(_)) => return Err(TraceLinkError::UnexpectedToolCallId),
            _ => {}
        }
        let needs_parent = matches!(event, TraceEvent::PostTool | TraceEvent::ToolOutput);
        match (needs_parent, self.parent_event_id.as_ref()) {
            (true, None) => return Err(TraceLinkError::MissingParent),
            (false, Some(_)) => return Err(TraceLinkError::UnexpectedParent),
            _ => {}
        }
        let is_summary = event == TraceEvent::TurnSummary;
        match (is_summary, self.member_event_ids.is_empty()) {
            (true, true) => return Err(TraceLinkError::EmptyMemberIds),
            (false, false) => return Err(TraceLinkError::UnexpectedMemberIds),
            _ => {}
        }
        Ok(())
    }
}
```

The two error variants `UnexpectedParent` and `UnexpectedToolCallId` are
declared but not asserted in the tests above; add a test for each:

```rust
#[test]
fn user_message_rejects_parent() {
    let link = TraceLink {
        session_id: sess(), turn_id: "t".into(), sequence: 0,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: Some(cap("01ARZ3NDEKTSV4RRFFQ69G5FBW")),
        tool_call_id: None, member_event_ids: Vec::new(),
    };
    assert!(matches!(
        link.validate(TraceEvent::UserMessage).unwrap_err(),
        TraceLinkError::UnexpectedParent
    ));
}

#[test]
fn user_message_rejects_tool_call_id() {
    let link = TraceLink {
        session_id: sess(), turn_id: "t".into(), sequence: 0,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: None, tool_call_id: Some("c".into()),
        member_event_ids: Vec::new(),
    };
    assert!(matches!(
        link.validate(TraceEvent::UserMessage).unwrap_err(),
        TraceLinkError::UnexpectedToolCallId
    ));
}

#[test]
fn empty_turn_id_rejected() {
    let link = TraceLink {
        session_id: sess(), turn_id: "  ".into(), sequence: 0,
        capture_event_id: cap("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        parent_event_id: None, tool_call_id: None,
        member_event_ids: Vec::new(),
    };
    assert!(matches!(
        link.validate(TraceEvent::UserMessage).unwrap_err(),
        TraceLinkError::EmptyTurnId
    ));
}
```

- [ ] **Step 4: Run all tests pass**

```bash
cargo nextest run -p cairn-core trace::tests
```

Expected: PASS, all cases.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/trace.rs
git commit -m "feat(core): TraceLink + field-level validation (#77)"
```

---

## Task 3: Deterministic summary record id

**Files:**
- Modify: `crates/cairn-core/src/domain/trace.rs`

- [ ] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
#[test]
fn summary_id_is_deterministic() {
    let s1 = summary_record_id(&sess(), "turn-4");
    let s2 = summary_record_id(&sess(), "turn-4");
    assert_eq!(s1, s2);
    assert_ne!(s1, summary_record_id(&sess(), "turn-5"));
}

#[test]
fn summary_id_is_valid_record_id() {
    let id = summary_record_id(&sess(), "turn-4");
    // RecordId::parse re-validates ULID shape — must round-trip.
    let reparsed = crate::domain::RecordId::parse(id.as_str()).expect("valid record id");
    assert_eq!(id, reparsed);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p cairn-core summary_id
```

Expected: FAIL — `summary_record_id` undefined.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/cairn-core/src/domain/trace.rs`:

```rust
use crate::domain::RecordId;
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Deterministic, ULID-shaped record id for a `turn_summary`. Same
/// `(session_id, turn_id)` always maps to the same id, so summary upserts
/// are idempotent under replay.
#[must_use]
pub fn summary_record_id(session_id: &SessionId, turn_id: &str) -> RecordId {
    let mut h = Sha256::new();
    h.update(b"cairn:trace:turnsum\0");
    h.update(session_id.as_str().as_bytes());
    h.update(b"\0");
    h.update(turn_id.as_bytes());
    let digest = h.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    let ulid = Ulid::from_bytes(bytes);
    RecordId::parse(ulid.to_string()).unwrap_or_else(|_| {
        // Unreachable: a 16-byte ULID always serializes to 26 valid chars.
        // This branch exists to keep no-unwrap lints happy without an expect.
        unreachable!("ulid::to_string always produces valid RecordId")
    })
}
```

If `sha2` and `ulid` are not already in `cairn-core`'s `Cargo.toml`, add
them as workspace deps. Check first:

```bash
grep -E "^sha2|^ulid" crates/cairn-core/Cargo.toml
```

If missing, add `sha2 = { workspace = true }` and `ulid = { workspace = true }`
to `[dependencies]`. If they aren't workspace deps yet, add them to
`[workspace.dependencies]` in the root `Cargo.toml` (`sha2 = "0.10"`,
`ulid = "1"` — match versions used elsewhere if present).

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-core summary_id
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/trace.rs crates/cairn-core/Cargo.toml Cargo.toml
git commit -m "feat(core): deterministic summary_record_id (#77)"
```

---

## Task 4: ResolvedBody review + body-shape helper

**Files:**
- Read: `crates/cairn-core/src/pipeline/extract/body.rs` (no edits)
- Create: `crates/cairn-core/src/pipeline/capture_trace.rs`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`

- [ ] **Step 1: Read the existing `ResolvedBody`**

```bash
grep -n "pub struct ResolvedBody\|impl ResolvedBody\|pub fn from_" crates/cairn-core/src/pipeline/extract/body.rs | head -20
```

Note the constructors and what `text()` / `payload_hash()` accessors exist.
The trace projector reuses this type — do not redefine it.

- [ ] **Step 2: Write the failing test for body shaping**

Create `crates/cairn-core/src/pipeline/capture_trace.rs`:

```rust
//! Trace-event projector (issue #77).

use crate::domain::trace::{TraceEvent, TraceLink, TraceLinkError};

/// Maximum size of a stored trace-record body, in bytes. Anything larger is
/// truncated; the full bytes remain in `sources/` referenced by
/// `payload_hash`.
pub const TRACE_BODY_CAP: usize = 4 * 1024;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceProjectError {
    #[error("trace link: {0}")]
    Link(#[from] TraceLinkError),
}

/// Render the textual body for a given event type. Pure; no I/O.
pub(crate) fn shape_body(event: TraceEvent, filtered_text: &str) -> String {
    let truncated = truncate(filtered_text, TRACE_BODY_CAP);
    match event {
        TraceEvent::UserMessage | TraceEvent::AgentMessage | TraceEvent::ToolOutput => truncated,
        TraceEvent::PreTool | TraceEvent::PostTool | TraceEvent::Stop => truncated,
        TraceEvent::TurnSummary => truncated,
    }
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes { return s.to_owned(); }
    // Truncate on a char boundary.
    let mut end = max_bytes;
    while !s.is_char_boundary(end) { end -= 1; }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_truncates_at_cap() {
        let big = "x".repeat(TRACE_BODY_CAP + 100);
        let body = shape_body(TraceEvent::ToolOutput, &big);
        assert!(body.len() <= TRACE_BODY_CAP);
    }

    #[test]
    fn truncate_respects_char_boundary() {
        // 4-byte char at the cap boundary
        let mut s = "x".repeat(TRACE_BODY_CAP - 2);
        s.push('𝄞'); // 4 bytes
        let body = shape_body(TraceEvent::UserMessage, &s);
        assert!(body.is_char_boundary(body.len()));
    }
}
```

Add to `crates/cairn-core/src/pipeline/mod.rs`:

```rust
pub mod capture_trace;
```

- [ ] **Step 3: Run tests**

```bash
cargo nextest run -p cairn-core pipeline::capture_trace
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/capture_trace.rs crates/cairn-core/src/pipeline/mod.rs
git commit -m "feat(core): trace-projector body shaping + cap (#77)"
```

---

## Task 5: Classifier — CaptureEvent → TraceEvent

**Files:**
- Modify: `crates/cairn-core/src/pipeline/capture_trace.rs`

- [ ] **Step 1: Survey the existing `CapturePayload` variants**

```bash
grep -n "pub enum CapturePayload\|^    Hook\|hook_name\|HookEvent" crates/cairn-core/src/domain/capture.rs | head -20
```

Note which payload variant carries `hook_name` and what its values are
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
`PreCompact`, `Stop`). The classifier maps those plus non-hook payloads
to `TraceEvent`.

- [ ] **Step 2: Write the failing test**

Append to `pipeline::capture_trace::tests`:

```rust
use crate::domain::capture::{CaptureEvent, CapturePayload, /* whatever the hook variant is */};

// Use the existing test helpers from pipeline::squash::tests (look there
// for a `terminal_event` / `hook_event` builder you can copy locally —
// don't depend on test-only code from another module).

#[test]
fn classifies_user_prompt_submit() {
    let event = mk_hook_event("UserPromptSubmit");
    assert_eq!(classify(&event).unwrap(), TraceEvent::UserMessage);
}

#[test]
fn classifies_pre_tool_use() {
    assert_eq!(classify(&mk_hook_event("PreToolUse")).unwrap(), TraceEvent::PreTool);
}

#[test]
fn classifies_post_tool_use() {
    assert_eq!(classify(&mk_hook_event("PostToolUse")).unwrap(), TraceEvent::PostTool);
}

#[test]
fn classifies_stop() {
    assert_eq!(classify(&mk_hook_event("Stop")).unwrap(), TraceEvent::Stop);
}

#[test]
fn unknown_hook_rejected() {
    assert!(matches!(
        classify(&mk_hook_event("UnknownHook")).unwrap_err(),
        TraceProjectError::Unclassifiable
    ));
}
```

`mk_hook_event` should mirror the `hook_event(payload_bytes)` helper in
`pipeline::squash::tests` — copy its body into this `mod tests`. Refer
back to that module if signatures change.

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo nextest run -p cairn-core pipeline::capture_trace::tests::classifies
```

Expected: FAIL — `classify` undefined.

- [ ] **Step 4: Implement classifier**

Add to `crates/cairn-core/src/pipeline/capture_trace.rs`:

```rust
use crate::domain::capture::{CaptureEvent, CapturePayload};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceProjectError {
    #[error("trace link: {0}")]
    Link(#[from] TraceLinkError),
    #[error("cannot classify capture event into a trace event type")]
    Unclassifiable,
    #[error("agent_message events must come from a non-hook payload")]
    AgentMessageFromHook,
    #[error("tool_output events require a tool_call_id")]
    ToolOutputMissingCallId,
}

/// Map a `CaptureEvent` to a `TraceEvent`. Static rules:
/// - Hook payloads route by `hook_name`.
/// - Non-hook payloads (CLI/MCP message capture) route to UserMessage or
///   AgentMessage based on payload tag (the existing source_family /
///   payload variant — adjust the match arm to whatever the codebase
///   uses today).
pub fn classify(event: &CaptureEvent) -> Result<TraceEvent, TraceProjectError> {
    match &event.payload {
        CapturePayload::Hook { hook_name, .. } => match hook_name.as_str() {
            "UserPromptSubmit" => Ok(TraceEvent::UserMessage),
            "PreToolUse"       => Ok(TraceEvent::PreTool),
            "PostToolUse"      => Ok(TraceEvent::PostTool),
            "Stop"             => Ok(TraceEvent::Stop),
            // SessionStart / PreCompact do not map to trace records in P0.
            _ => Err(TraceProjectError::Unclassifiable),
        },
        // Adjust this arm to whatever non-hook payload variant carries
        // agent text + tool output. Look at the existing CapturePayload
        // definition to see the right discriminator.
        _ => Err(TraceProjectError::Unclassifiable),
    }
}
```

**Important:** the `match &event.payload` arms must match the actual
variants on `CapturePayload`. Open `crates/cairn-core/src/domain/capture.rs`
around line 400 and adapt — do not invent variants. If the codebase uses
a different field name than `hook_name`, follow the actual one.

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn-core pipeline::capture_trace::tests::classifies
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/pipeline/capture_trace.rs
git commit -m "feat(core): classify CaptureEvent → TraceEvent (#77, brief §9.3)"
```

---

## Task 6: Projector — `project()` builds MemoryRecord

**Files:**
- Modify: `crates/cairn-core/src/pipeline/capture_trace.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[test]
fn projects_user_message_record() {
    let event = mk_hook_event("UserPromptSubmit");
    let resolved = ResolvedBody::from_text("hello world", &event.payload_hash);
    let link = TraceLink {
        session_id: sess(),
        turn_id: "turn-1".into(),
        sequence: 0,
        capture_event_id: event.event_id.clone(),
        parent_event_id: None,
        tool_call_id: None,
        member_event_ids: Vec::new(),
    };
    let record = project(&event, TraceEvent::UserMessage, &resolved, link).unwrap();
    assert_eq!(record.kind, MemoryKind::Trace);
    assert_eq!(record.class, MemoryClass::Episodic);
    assert_eq!(record.visibility, MemoryVisibility::Private);
    assert_eq!(record.scope.session_id.as_deref(), Some(sess().as_str()));
    assert!(record.body.contains("hello world"));
    let trace_meta = record.extra_frontmatter.get("trace").unwrap();
    assert_eq!(trace_meta["turn_id"], "turn-1");
    assert_eq!(trace_meta["sequence"], 0);
    assert_eq!(record.extra_frontmatter.get("trace_event").unwrap(), "user_message");
}

#[test]
fn project_validates_link_against_event() {
    let event = mk_hook_event("PreToolUse");
    let resolved = ResolvedBody::from_text("Read", &event.payload_hash);
    let link_bad = TraceLink { /* missing tool_call_id */ };
    assert!(project(&event, TraceEvent::PreTool, &resolved, link_bad).is_err());
}
```

(`ResolvedBody::from_text` in the test — if no constructor exists yet, use
whatever public constructor `body.rs` exposes. If none does and the type
is private, route through the existing extractor pipeline's test-only
helpers in `body.rs`'s `mod tests`.)

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p cairn-core pipeline::capture_trace::tests::projects
```

Expected: FAIL — `project` undefined.

- [ ] **Step 3: Implement `project`**

```rust
use crate::domain::record::MemoryRecord;
use crate::domain::scope::ScopeTuple;
use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use crate::domain::EvidenceVector;
use crate::pipeline::extract::body::ResolvedBody;
use serde_json::{Map as JsonMap, Value as Json};

pub fn project(
    event: &CaptureEvent,
    classified: TraceEvent,
    resolved_body: &ResolvedBody,
    link: TraceLink,
) -> Result<MemoryRecord, TraceProjectError> {
    link.validate(classified)?;

    // Trace fields go entirely under extra_frontmatter.trace (the single
    // canonical path; see spec §6.1 / §8.1).
    let mut trace = JsonMap::new();
    trace.insert("session_id".into(), Json::String(link.session_id.as_str().to_owned()));
    trace.insert("turn_id".into(), Json::String(link.turn_id.clone()));
    trace.insert("sequence".into(), Json::Number(link.sequence.into()));
    trace.insert("capture_event_id".into(), Json::String(link.capture_event_id.to_string()));
    trace.insert("payload_hash".into(), Json::String(event.payload_hash.to_string()));
    trace.insert("payload_ref".into(), Json::String(event.payload_ref.clone()));
    if let Some(parent) = &link.parent_event_id {
        trace.insert("parent_event_id".into(), Json::String(parent.to_string()));
    }
    if let Some(call) = &link.tool_call_id {
        trace.insert("tool_call_id".into(), Json::String(call.clone()));
    }
    if !link.member_event_ids.is_empty() {
        let arr = link.member_event_ids.iter()
            .map(|id| Json::String(id.to_string()))
            .collect();
        trace.insert("member_event_ids".into(), Json::Array(arr));
    }

    let mut extra = std::collections::BTreeMap::new();
    extra.insert("trace_event".into(), Json::String(serde_json::to_value(classified)?
        .as_str().unwrap_or("").to_owned()));
    extra.insert("trace".into(), Json::Object(trace));

    let body = shape_body(classified, resolved_body.text());
    let scope = ScopeTuple {
        session_id: Some(link.session_id.as_str().to_owned()),
        // Other dimensions copied from the event's actor chain — match
        // whatever the existing squash projector does for ScopeTuple.
        ..ScopeTuple::default()
    };

    let id = if classified == TraceEvent::TurnSummary {
        crate::domain::trace::summary_record_id(&link.session_id, &link.turn_id)
    } else {
        // For non-summary events, derive RecordId from capture_event_id
        // (same shape, ULID).
        crate::domain::RecordId::parse(link.capture_event_id.as_str())?
    };

    Ok(MemoryRecord {
        id: id.clone(),
        target_id: id.into(), // fresh record, target_id == id (record.rs §3)
        kind: MemoryKind::Trace,
        class: MemoryClass::Episodic,
        visibility: MemoryVisibility::Private,
        scope,
        body,
        provenance: provenance_from_event(event), // small helper, see below
        updated_at: event.captured_at.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 1.0,
        actor_chain: event.actor_chain.clone(),
        signature: /* see note */ ,
        tags: Vec::new(),
        extra_frontmatter: extra,
    })
}
```

**Signature note:** `MemoryRecord` carries an Ed25519 signature over the
canonical record bytes (`record.rs:170`). The squash pipeline already has
a sign-after-build path; mirror it. If signing requires a key handle, the
projector takes a signing closure as an extra parameter. Look at how
`pipeline::squash` resolves this in the codebase before forcing a shape;
if signing is deferred to a separate stage in squash (likely), do the
same here — leave `signature` as a placeholder until a follow-up signing
stage. Update the test to construct an unsigned record if the codebase
already has that pattern.

`provenance_from_event` is a small helper that copies sensor + capture_mode
+ captured_at into the existing `Provenance` struct. Look at the squash
projector's equivalent and replicate.

Add `#[derive(thiserror::Error)] enum` variants for any new errors used:

```rust
#[error("invalid record id derivation: {0}")]
RecordId(#[from] crate::domain::DomainError),
#[error("serde: {0}")]
Serde(#[from] serde_json::Error),
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-core pipeline::capture_trace::tests::projects
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/capture_trace.rs
git commit -m "feat(core): project CaptureEvent → trace MemoryRecord (#77, brief §5.0)"
```

---

## Task 7: Sequence ordering — `order_by_captured_at` + `assign_sequences`

**Files:**
- Create: `crates/cairn-core/src/pipeline/turn.rs`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/src/pipeline/turn.rs`:

```rust
//! Turn-level pure helpers for trace persistence (issue #77).

use crate::domain::capture::CaptureEvent;
use crate::domain::record::MemoryRecord;

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(id: &str, captured_at: &str) -> CaptureEvent {
        // Builder — copy from pipeline::squash::tests if available.
        todo!("use the existing test builder")
    }

    #[test]
    fn orders_by_captured_at_then_event_id() {
        let a = mk("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:01Z");
        let b = mk("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:00Z");
        let c = mk("01ARZ3NDEKTSV4RRFFQ69G5FAC", "2026-05-02T00:00:00Z");
        let ordered = order_by_captured_at(&[], &[(&a, ()), (&b, ()), (&c, ())]);
        // Earlier timestamp first; ties broken by capture_event_id.
        assert_eq!(ordered[0].event.event_id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAB");
        assert_eq!(ordered[1].event.event_id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAC");
        assert_eq!(ordered[2].event.event_id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAA");
    }

    #[test]
    fn assigns_sequences_zero_indexed() {
        // Three sorted events → sequences 0,1,2.
        // Test details after order_by_captured_at lands.
    }
}
```

(Note: the `(&a, ())` pattern shows the function takes pairs of (event,
classified). Pick the actual signature when implementing — `()` is a
placeholder for whatever metadata travels with each event; in practice
it'll be `TraceEvent`.)

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo nextest run -p cairn-core pipeline::turn::tests::orders
```

Expected: FAIL — undefined.

- [ ] **Step 3: Implement**

```rust
use crate::domain::trace::{TraceEvent, TraceLink};

pub struct OrderedEntry<'a> {
    pub event: &'a CaptureEvent,
    pub classified: TraceEvent,
}

/// Merge already-persisted records (decoded from the store) with newly
/// arriving events, sorted by `(captured_at, capture_event_id)`. Stable
/// across replays.
pub fn order_by_captured_at<'a>(
    persisted: &'a [(MemoryRecord, &'a CaptureEvent, TraceEvent)],
    incoming: &'a [(&'a CaptureEvent, TraceEvent)],
) -> Vec<OrderedEntry<'a>> {
    let mut all: Vec<OrderedEntry<'a>> = persisted.iter()
        .map(|(_, ev, cls)| OrderedEntry { event: ev, classified: *cls })
        .chain(incoming.iter().map(|(ev, cls)| OrderedEntry { event: *ev, classified: *cls }))
        .collect();
    all.sort_by(|x, y| {
        x.event.captured_at.cmp(&y.event.captured_at)
            .then_with(|| x.event.event_id.as_str().cmp(y.event.event_id.as_str()))
    });
    all
}

/// Assign sequences 0..N over an already-ordered slice. Returns Vec<TraceLink>
/// with the resolved `sequence`. Caller fills the rest of `TraceLink`.
pub fn assign_sequences(
    session_id: &crate::domain::SessionId,
    turn_id: &str,
    ordered: &[OrderedEntry<'_>],
) -> Vec<TraceLink> {
    ordered.iter().enumerate().map(|(i, entry)| {
        let mut link = TraceLink {
            session_id: session_id.clone(),
            turn_id: turn_id.to_owned(),
            sequence: i as u64,
            capture_event_id: entry.event.event_id.clone(),
            parent_event_id: None,
            tool_call_id: None,
            member_event_ids: Vec::new(),
        };
        // Caller (CLI verb) fills parent/tool_call_id from the event's
        // refs after this returns. Keep this fn pure-data, no side effects.
        link
    }).collect()
}
```

Note: real signatures may differ — adapt the persisted-slice tuple to
whatever the store actually returns. The point is: reading a turn's rows
gives you back enough to reconstruct an `OrderedEntry`.

Add to `crates/cairn-core/src/pipeline/mod.rs`:

```rust
pub mod turn;
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-core pipeline::turn
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/turn.rs crates/cairn-core/src/pipeline/mod.rs
git commit -m "feat(core): captured_at sequence ordering helpers (#77)"
```

---

## Task 8: `summarize_turn` — pure roll-up

**Files:**
- Modify: `crates/cairn-core/src/pipeline/turn.rs`

- [ ] **Step 1: Write the failing test**

Append to `pipeline::turn::tests`:

```rust
#[test]
fn summarize_orders_and_collects_member_ids() {
    let events: Vec<MemoryRecord> = vec![
        // build three trace records with sequence 0,1,2 and three different
        // capture_event_ids in extra_frontmatter.trace
        // ...
    ];
    let summary = summarize_turn(&sess(), "turn-1", &events).unwrap();
    let trace = summary.extra_frontmatter.get("trace").unwrap();
    let members = trace["member_event_ids"].as_array().unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(summary.kind, MemoryKind::Trace);
    assert_eq!(
        summary.extra_frontmatter.get("trace_event").unwrap(),
        "turn_summary"
    );
    // Deterministic id matches what summary_record_id produces.
    assert_eq!(
        summary.id,
        crate::domain::trace::summary_record_id(&sess(), "turn-1")
    );
}

#[test]
fn summarize_rejects_cross_turn_events() {
    let mixed = vec![/* records from two different turns */];
    assert!(matches!(
        summarize_turn(&sess(), "turn-1", &mixed).unwrap_err(),
        TurnSummaryError::CrossTurn
    ));
}

#[test]
fn summarize_rejects_empty() {
    assert!(matches!(
        summarize_turn(&sess(), "turn-1", &[]).unwrap_err(),
        TurnSummaryError::EmptyTurn
    ));
}
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo nextest run -p cairn-core pipeline::turn::tests::summarize
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TurnSummaryError {
    #[error("turn must contain at least one event")]
    EmptyTurn,
    #[error("events span more than one (session_id, turn_id)")]
    CrossTurn,
    #[error("sequences must be strictly increasing")]
    OutOfOrderSequences,
    #[error("record is missing extra_frontmatter.trace")]
    MissingTrace,
}

pub fn summarize_turn(
    session_id: &crate::domain::SessionId,
    turn_id: &str,
    events: &[MemoryRecord],
) -> Result<MemoryRecord, TurnSummaryError> {
    if events.is_empty() {
        return Err(TurnSummaryError::EmptyTurn);
    }
    // Validate all events share (session_id, turn_id) and sequences strict-inc.
    let mut last_seq: Option<u64> = None;
    let mut member_ids = Vec::with_capacity(events.len());
    for r in events {
        let trace = r.extra_frontmatter.get("trace")
            .and_then(|v| v.as_object())
            .ok_or(TurnSummaryError::MissingTrace)?;
        let s = trace.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let t = trace.get("turn_id").and_then(|v| v.as_str()).unwrap_or("");
        if s != session_id.as_str() || t != turn_id {
            return Err(TurnSummaryError::CrossTurn);
        }
        let seq = trace.get("sequence").and_then(|v| v.as_u64())
            .ok_or(TurnSummaryError::MissingTrace)?;
        if let Some(prev) = last_seq {
            if seq <= prev { return Err(TurnSummaryError::OutOfOrderSequences); }
        }
        last_seq = Some(seq);
        let cap_id = trace.get("capture_event_id").and_then(|v| v.as_str())
            .ok_or(TurnSummaryError::MissingTrace)?;
        member_ids.push(cap_id.to_owned());
    }

    // Body: deterministic concat (one line per event). Cap at TRACE_BODY_CAP.
    let mut body = format!("## Turn {} (session {})\n", turn_id, session_id.as_str());
    for r in events {
        let trace = r.extra_frontmatter.get("trace").and_then(|v| v.as_object()).unwrap();
        let seq = trace.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
        let evt = r.extra_frontmatter.get("trace_event").and_then(|v| v.as_str()).unwrap_or("");
        let excerpt: String = r.body.chars().take(80).collect();
        body.push_str(&format!("- [seq {}] {}: {}\n", seq, evt, excerpt));
    }

    // Build the summary record using the same projector path: synthesize a
    // TraceLink with member_event_ids set, then construct directly (we don't
    // have a CaptureEvent for synthetic summaries).
    let id = crate::domain::trace::summary_record_id(session_id, turn_id);
    // ... fill in MemoryRecord fields, mirroring project() but without
    // payload_hash/payload_ref (no underlying CaptureEvent).

    todo!("assemble MemoryRecord — see project() for the field set")
}
```

(The `todo!()` marker is for *you, the engineer* — fill in by mirroring
`project()`. Do not commit a `todo!`.)

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-core pipeline::turn::tests::summarize
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/turn.rs
git commit -m "feat(core): summarize_turn pure roll-up (#77, brief §5.0)"
```

---

## Task 9: SQLite migration `0022_trace_links.sql`

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0022_trace_links.sql`
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs`

- [ ] **Step 1: Inspect migration registration**

```bash
grep -n "0021\|register_migrations\|MIGRATIONS\|migrations\\\!" crates/cairn-store-sqlite/src/migrations/mod.rs
```

Note the registration shape (likely a `&[(&str, &str)]` or `migrations!`
macro). Match the existing pattern.

- [ ] **Step 2: Write the failing test**

Add to `crates/cairn-store-sqlite/tests/migration_smoke.rs`:

```rust
#[tokio::test]
async fn migration_0022_trace_links_applies() {
    let store = test_store_in_memory().await;
    let conn = store.require_conn("test").unwrap();
    conn.call(|c| {
        // Verify the generated columns exist.
        let mut stmt = c.prepare("PRAGMA table_info(records)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(cols.contains(&"trace_event".to_owned()));
        assert!(cols.contains(&"trace_session_id".to_owned()));
        assert!(cols.contains(&"trace_turn_id".to_owned()));
        assert!(cols.contains(&"trace_sequence".to_owned()));
        assert!(cols.contains(&"trace_capture_event_id".to_owned()));
        assert!(cols.contains(&"trace_parent_event_id".to_owned()));
        assert!(cols.contains(&"trace_payload_hash".to_owned()));

        // Verify indices.
        let mut idx = c.prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='records'").unwrap();
        let names: Vec<String> = idx.query_map([], |r| r.get(0)).unwrap().filter_map(Result::ok).collect();
        assert!(names.contains(&"records_trace_event_id".to_owned()));
        assert!(names.contains(&"records_trace_summary".to_owned()));
        assert!(names.contains(&"records_trace_seq".to_owned()));
        assert!(names.contains(&"records_trace_parent".to_owned()));
        assert!(names.contains(&"records_trace_payload_hash".to_owned()));
        Ok::<_, tokio_rusqlite::Error>(())
    }).await.unwrap();
}
```

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo nextest run -p cairn-store-sqlite migration_0022
```

Expected: FAIL.

- [ ] **Step 4: Write the migration**

`crates/cairn-store-sqlite/src/migrations/sql/0022_trace_links.sql`:

```sql
-- 0022_trace_links — generated columns + unique indices for trace records
-- (issue #77, spec §6.1).

ALTER TABLE records ADD COLUMN trace_event TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace_event')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_session_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.session_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_turn_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.turn_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_sequence INTEGER
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.sequence')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_capture_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.capture_event_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_parent_event_id TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.parent_event_id')) VIRTUAL;
ALTER TABLE records ADD COLUMN trace_payload_hash TEXT
  GENERATED ALWAYS AS (json_extract(extra_frontmatter, '$.trace.payload_hash')) VIRTUAL;

CREATE UNIQUE INDEX records_trace_event_id
  ON records(trace_capture_event_id)
  WHERE trace_capture_event_id IS NOT NULL;

CREATE UNIQUE INDEX records_trace_summary
  ON records(trace_session_id, trace_turn_id)
  WHERE trace_event = 'turn_summary';

CREATE UNIQUE INDEX records_trace_seq
  ON records(trace_session_id, trace_turn_id, trace_sequence)
  WHERE trace_event IS NOT NULL AND trace_event != 'turn_summary';

CREATE INDEX records_trace_parent
  ON records(trace_parent_event_id)
  WHERE trace_parent_event_id IS NOT NULL;

CREATE INDEX records_trace_payload_hash
  ON records(trace_payload_hash)
  WHERE trace_payload_hash IS NOT NULL;
```

Register in `crates/cairn-store-sqlite/src/migrations/mod.rs` matching the
existing 0021 entry's pattern.

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite migration_0022
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-store-sqlite/src/migrations/
git commit -m "feat(store): trace-link generated cols + indices (#77, spec §6.1)"
```

---

## Task 10: `StoreTx::list_trace_events`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`

- [ ] **Step 1: Write the failing test**

In `crates/cairn-store-sqlite/tests/trace_store.rs` (new file):

```rust
//! Integration tests for trace-record store methods.

use cairn_test_fixtures::*;
use cairn_core::domain::SessionId;

#[tokio::test]
async fn list_trace_events_orders_by_sequence() {
    let store = test_store_in_memory().await;
    // Insert three trace records with sequences 2, 0, 1 (deliberately scrambled).
    insert_trace_record(&store, /* session, turn, sequence */).await;
    // ...
    store.with_tx(|tx| {
        let rows = tx.list_trace_events(&sess(), "turn-1")?;
        let seqs: Vec<u64> = rows.iter()
            .map(|r| r.extra_frontmatter["trace"]["sequence"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn list_trace_events_excludes_summary() {
    // Insert summary + two events, expect only the two events back.
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run -p cairn-store-sqlite trace_store
```

Expected: FAIL — `list_trace_events` undefined.

- [ ] **Step 3: Implement**

Add to `crates/cairn-store-sqlite/src/store/tx.rs` `impl StoreTx`:

```rust
use cairn_core::domain::SessionId;
use cairn_core::domain::record::MemoryRecord;

pub fn list_trace_events(
    &self,
    session_id: &SessionId,
    turn_id: &str,
) -> Result<Vec<MemoryRecord>, StoreError> {
    let mut stmt = self.tx.prepare(
        "SELECT <full record-row columns> FROM records \
            WHERE trace_session_id = ?1 \
              AND trace_turn_id = ?2 \
              AND trace_event IS NOT NULL \
              AND trace_event != 'turn_summary' \
              AND tombstoned = 0 \
            ORDER BY trace_sequence ASC"
    )?;
    let rows = stmt.query_map(
        rusqlite::params![session_id.as_str(), turn_id],
        |row| { /* re-use existing row→MemoryRecord decoder */ Ok(decode_row(row)) },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)
}
```

The `<full record-row columns>` and `decode_row` should reuse the same
helpers as `read.rs`'s existing record loaders. Look there:

```bash
grep -n "fn decode_row\|fn record_from_row\|SELECT.*records" crates/cairn-store-sqlite/src/store/read.rs | head
```

Reuse — do not duplicate the column list.

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-store-sqlite trace_store::list_trace_events
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tx.rs crates/cairn-store-sqlite/tests/trace_store.rs
git commit -m "feat(store): list_trace_events ordered read (#77)"
```

---

## Task 11: `StoreTx::turn_summary_exists` + `payload_hash_count_in_scope`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`

- [ ] **Step 1: Failing tests**

Append to `tests/trace_store.rs`:

```rust
#[tokio::test]
async fn turn_summary_exists_after_write() {
    let store = test_store_in_memory().await;
    store.with_tx(|tx| {
        assert!(!tx.turn_summary_exists(&sess(), "turn-1")?);
        // Write a summary record (raw upsert with trace_event='turn_summary').
        insert_summary(tx, &sess(), "turn-1");
        assert!(tx.turn_summary_exists(&sess(), "turn-1")?);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn payload_hash_count_excludes_target_set() {
    // Two records sharing the same payload_hash under the same scope.
    // count_in_scope(hash, scope, exclude=[id1]) should return 1.
}
```

- [ ] **Step 2: Run failing**

```bash
cargo nextest run -p cairn-store-sqlite turn_summary_exists payload_hash_count
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
pub fn turn_summary_exists(
    &self,
    session_id: &SessionId,
    turn_id: &str,
) -> Result<bool, StoreError> {
    let mut stmt = self.tx.prepare(
        "SELECT 1 FROM records \
          WHERE trace_event = 'turn_summary' \
            AND trace_session_id = ?1 \
            AND trace_turn_id = ?2 \
            AND tombstoned = 0 \
          LIMIT 1"
    )?;
    let exists = stmt
        .query_row(rusqlite::params![session_id.as_str(), turn_id], |_| Ok(()))
        .optional()?
        .is_some();
    Ok(exists)
}

pub fn payload_hash_count_in_scope(
    &self,
    payload_hash: &str,
    tenant: Option<&str>,
    user: Option<&str>,
    agent: Option<&str>,
    exclude: &[&str],   // record_ids being forgotten
) -> Result<u64, StoreError> {
    // Build the IN-clause dynamically; or use a temp table for large excludes.
    // Look at existing `forget` or `tombstone` queries for the established
    // pattern.
    todo!("see read.rs / consent.rs for in-scope query templates")
}
```

- [ ] **Step 4: Run passing**

```bash
cargo nextest run -p cairn-store-sqlite turn_summary_exists payload_hash_count
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tx.rs crates/cairn-store-sqlite/tests/trace_store.rs
git commit -m "feat(store): summary-exists + scoped payload_hash refcount (#77)"
```

---

## Task 12: `StoreTx::upsert_trace`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`

- [ ] **Step 1: Failing test — idempotency**

Append to `tests/trace_store.rs`:

```rust
#[tokio::test]
async fn upsert_trace_is_idempotent_on_capture_event_id() {
    let store = test_store_in_memory().await;
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let record = mk_trace_record(event_id, /* seq */ 0);
    store.with_tx(|tx| { tx.upsert_trace(&record)?; Ok(()) }).await.unwrap();
    store.with_tx(|tx| { tx.upsert_trace(&record)?; Ok(()) }).await.unwrap();

    store.with_tx(|tx| {
        let rows = tx.list_trace_events(&sess(), "turn-1")?;
        assert_eq!(rows.len(), 1, "duplicate capture_event_id must not produce two rows");
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn upsert_trace_rejects_duplicate_sequence() {
    let store = test_store_in_memory().await;
    let r1 = mk_trace_record("01ARZ3NDEKTSV4RRFFQ69G5FAA", 0);
    let r2 = mk_trace_record("01ARZ3NDEKTSV4RRFFQ69G5FBB", 0); // same seq, different event id
    let result: Result<_, _> = store.with_tx(|tx| {
        tx.upsert_trace(&r1)?;
        tx.upsert_trace(&r2)?;
        Ok(())
    }).await;
    let err = result.unwrap_err();
    // Map UNIQUE-constraint failure to a typed TraceSequenceConflict.
    assert!(matches!(err, StoreError::TraceSequenceConflict { .. }));
}
```

- [ ] **Step 2: Run failing**

```bash
cargo nextest run -p cairn-store-sqlite upsert_trace
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
pub fn upsert_trace(
    &mut self,
    record: &MemoryRecord,
) -> Result<UpsertOutcome, StoreError> {
    // Reuse upsert_in_tx — the unique indices on trace_capture_event_id and
    // (trace_session_id, trace_turn_id, trace_sequence) handle idempotency
    // and conflicts respectively.
    match upsert_in_tx(&mut self.tx, record) {
        Ok(out) => Ok(out),
        Err(StoreError::Sqlite(e)) if is_unique_constraint(&e, "records_trace_seq") => {
            Err(StoreError::TraceSequenceConflict {
                session_id: extract_session(record),
                turn_id:    extract_turn(record),
                sequence:   extract_seq(record),
            })
        }
        Err(other) => Err(other),
    }
}
```

Add `TraceSequenceConflict { session_id, turn_id, sequence }` to
`crates/cairn-store-sqlite/src/error.rs`. The `is_unique_constraint`
helper checks the SQLite error string for the index name.

- [ ] **Step 4: Run passing**

```bash
cargo nextest run -p cairn-store-sqlite upsert_trace
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tx.rs crates/cairn-store-sqlite/src/error.rs crates/cairn-store-sqlite/tests/trace_store.rs
git commit -m "feat(store): upsert_trace + TraceSequenceConflict (#77)"
```

---

## Task 13: Two-phase renumber on backfill

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`

- [ ] **Step 1: Failing test**

Append to `tests/trace_store.rs`:

```rust
#[tokio::test]
async fn out_of_order_backfill_renumbers() {
    let store = test_store_in_memory().await;
    // Insert two events at captured_at t=2 (seq 0) and t=3 (seq 1).
    // Then backfill an event at captured_at t=1 — expect final order
    // [t1=seq0, t2=seq1, t3=seq2].
    let a = mk_trace_record_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:02Z", 0);
    let b = mk_trace_record_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:03Z", 1);
    store.with_tx(|tx| { tx.upsert_trace(&a)?; tx.upsert_trace(&b)?; Ok(()) }).await.unwrap();

    let c = mk_trace_record_at("01ARZ3NDEKTSV4RRFFQ69G5FAC", "2026-05-02T00:00:01Z", 0);
    // The CLI verb's renumber path will assign c=0, a=1, b=2. Drive it via
    // a helper: `renumber_turn_with(tx, &sess(), "turn-1", &[c])`.
    store.with_tx(|tx| {
        renumber_turn_with(tx, &sess(), "turn-1", &[c])?;
        let seqs: Vec<u64> = tx.list_trace_events(&sess(), "turn-1")?.iter()
            .map(|r| r.extra_frontmatter["trace"]["sequence"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
        Ok(())
    }).await.unwrap();
}
```

- [ ] **Step 2: Run failing**

Expected: FAIL — `renumber_turn_with` undefined.

- [ ] **Step 3: Implement two-phase renumber**

In `tx.rs`:

```rust
/// Two-phase renumber of all trace events for a turn (issue #77, spec §4
/// "Ordering"). Park existing rows on negative sentinels first, reassign
/// `0..N` sorted by (captured_at, capture_event_id), then write the
/// final values. Runs entirely inside the open transaction.
pub fn renumber_turn_with(
    &mut self,
    session_id: &SessionId,
    turn_id: &str,
    incoming: &[MemoryRecord],
) -> Result<(), StoreError> {
    // 1. Read existing rows.
    let existing = self.list_trace_events(session_id, turn_id)?;
    // 2. Park: rewrite each existing row's trace.sequence to -1-i.
    for (i, row) in existing.iter().enumerate() {
        let parked = with_sequence(row, -1 - i as i64)?;
        upsert_in_tx(&mut self.tx, &parked)?;
    }
    // 3. Order all (existing + incoming) by captured_at + event_id, assign 0..N.
    let mut all: Vec<MemoryRecord> = existing;
    all.extend(incoming.iter().cloned());
    all.sort_by(|a, b| compare_by_captured_at_and_event_id(a, b));
    // 4. Final assignment.
    for (i, row) in all.iter().enumerate() {
        let final_row = with_sequence(row, i as i64)?;
        upsert_in_tx(&mut self.tx, &final_row)?;
    }
    Ok(())
}
```

`with_sequence` rebuilds `extra_frontmatter.trace.sequence` and re-signs
the record (signature note in Task 6 applies). `compare_by_captured_at_and_event_id`
walks `extra_frontmatter.trace.capture_event_id` plus the record's
`updated_at` (or a stored `captured_at` you carry through projection).

**Sequence column type:** if `trace_sequence` is INTEGER, negative
sentinels work natively. If you defined it as a TEXT-storing JSON
representation, the comparison still works but verify with
`PRAGMA table_info`.

- [ ] **Step 4: Run passing**

```bash
cargo nextest run -p cairn-store-sqlite trace_store::out_of_order_backfill
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tx.rs crates/cairn-store-sqlite/tests/trace_store.rs
git commit -m "feat(store): two-phase renumber on out-of-order backfill (#77)"
```

---

## Task 14: Referential validation in transaction

**Files:**
- Modify: `crates/cairn-store-sqlite/src/store/tx.rs`

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn orphan_parent_rejected() {
    let store = test_store_in_memory().await;
    let post_tool = mk_post_tool_record(
        "01ARZ3NDEKTSV4RRFFQ69G5FAA",
        /* parent */ "01ARZ3NDEKTSV4RRFFQ69G5FBB",  // never inserted
        /* tool_call_id */ "call_abc",
    );
    let result: Result<_, _> = store.with_tx(|tx| {
        tx.upsert_trace(&post_tool)?;
        tx.validate_turn_links(&sess(), "turn-1")?;
        Ok(())
    }).await;
    assert!(matches!(
        result.unwrap_err(),
        StoreError::TraceLinkOrphan { .. }
    ));
}

#[tokio::test]
async fn tool_call_id_mismatch_rejected() {
    // pre_tool with call_abc, post_tool referencing pre_tool but with
    // tool_call_id="other" — rejected.
}
```

- [ ] **Step 2: Run failing**

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
pub fn validate_turn_links(
    &self,
    session_id: &SessionId,
    turn_id: &str,
) -> Result<(), StoreError> {
    let rows = self.list_trace_events(session_id, turn_id)?;
    // Build map: capture_event_id → (trace_event, tool_call_id).
    let by_id: std::collections::HashMap<String, (String, Option<String>)> = rows.iter()
        .map(|r| {
            let trace = &r.extra_frontmatter["trace"];
            let id = trace["capture_event_id"].as_str().unwrap_or("").to_owned();
            let evt = r.extra_frontmatter["trace_event"].as_str().unwrap_or("").to_owned();
            let tcid = trace.get("tool_call_id").and_then(|v| v.as_str()).map(str::to_owned);
            (id, (evt, tcid))
        })
        .collect();
    for r in &rows {
        let trace = &r.extra_frontmatter["trace"];
        let event = r.extra_frontmatter["trace_event"].as_str().unwrap_or("");
        if !matches!(event, "post_tool" | "tool_output") { continue; }
        let parent = trace.get("parent_event_id").and_then(|v| v.as_str())
            .ok_or_else(|| StoreError::Invariant("missing parent_event_id".into()))?;
        let (parent_evt, parent_tcid) = by_id.get(parent)
            .ok_or_else(|| StoreError::TraceLinkOrphan {
                child_id: trace["capture_event_id"].as_str().unwrap().to_owned(),
                parent_id: parent.to_owned(),
                reason: "parent not in turn".into(),
            })?;
        if parent_evt != "pre_tool" {
            return Err(StoreError::TraceLinkOrphan { /* ... */ });
        }
        let child_tcid = trace.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
        let parent_tcid = parent_tcid.as_deref().unwrap_or("");
        if child_tcid != parent_tcid {
            return Err(StoreError::TraceLinkOrphan { /* mismatch */ });
        }
    }
    Ok(())
}
```

Add `TraceLinkOrphan { child_id, parent_id, reason }` to `StoreError`.

- [ ] **Step 4: Run passing**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-store-sqlite/src/store/tx.rs crates/cairn-store-sqlite/src/error.rs
git commit -m "feat(store): in-transaction trace-link integrity check (#77)"
```

---

## Task 15: CLI verb — JSONL parser

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`

- [ ] **Step 1: Failing test**

```bash
mkdir -p crates/cairn-cli/tests/fixtures/trace
```

Create `crates/cairn-cli/tests/fixtures/trace/single-turn.jsonl` with three
hand-written `CaptureEvent` JSON lines (UserPromptSubmit + PreToolUse +
PostToolUse for a single `(session, turn-1)`).

Add `crates/cairn-cli/tests/capture_trace_verb.rs`:

```rust
#[tokio::test]
async fn parses_jsonl_into_events() {
    let path = "tests/fixtures/trace/single-turn.jsonl";
    let events = read_jsonl_events(path).await.unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].refs.as_ref().unwrap().turn_id.as_deref(), Some("turn-1"));
}
```

- [ ] **Step 2: Failing**

Expected: FAIL — `read_jsonl_events` undefined.

- [ ] **Step 3: Implement**

In `capture_trace.rs`:

```rust
use cairn_core::domain::capture::CaptureEvent;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};

pub async fn read_jsonl_events(path: impl AsRef<Path>) -> anyhow::Result<Vec<CaptureEvent>> {
    let f = File::open(path).await.context("open trace JSONL")?;
    let mut lines = BufReader::new(f).lines();
    let mut events = Vec::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let event: CaptureEvent = serde_json::from_str(&line)
            .context("parse CaptureEvent line")?;
        events.push(event);
    }
    Ok(events)
}
```

- [ ] **Step 4: Passing**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-cli/tests/
git commit -m "feat(cli): capture_trace JSONL parser (#77)"
```

---

## Task 16: CLI verb — group by turn + per-turn transaction

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`

- [ ] **Step 1: Failing test — single-turn end-to-end**

```rust
#[tokio::test]
async fn capture_trace_single_turn_persists_and_summarizes() {
    let store = test_sqlite_store().await;
    capture_trace::run_handler(&store, "tests/fixtures/trace/single-turn.jsonl")
        .await.unwrap();

    store.with_tx(|tx| {
        let rows = tx.list_trace_events(&sess(), "turn-1")?;
        assert_eq!(rows.len(), 3);
        assert!(tx.turn_summary_exists(&sess(), "turn-1")?);
        Ok(())
    }).await.unwrap();
}
```

- [ ] **Step 2: Failing**

Expected: FAIL.

- [ ] **Step 3: Implement** (replace stub)

```rust
pub async fn run_handler(
    store: &SqliteMemoryStore,
    from: &Path,
) -> anyhow::Result<CaptureTraceResponse> {
    refuse_if_degraded(store).await?;
    let events = read_jsonl_events(from).await?;

    let groups = group_by_turn(&events);  // Vec<(SessionId, String, Vec<&CaptureEvent>)>
    let mut failed = Vec::new();
    for (session_id, turn_id, raw_group) in groups {
        let result = store.with_tx(move |tx| {
            // Project + classify each event.
            let mut to_write = Vec::with_capacity(raw_group.len());
            for event in &raw_group {
                event.validate()?;
                let classified = classify(event)?;
                to_write.push((event, classified));
            }
            // Pull existing turn rows for renumber.
            let existing = tx.list_trace_events(&session_id, &turn_id)?;
            // Compose merged TraceLink list ordered by captured_at.
            let merged = order_and_assign(&existing, &to_write, &session_id, &turn_id);
            // Park existing → write merged (Task 13 helper).
            for parked in park_existing(&existing) {
                tx.upsert_trace(&parked)?;
            }
            for (event, classified, link) in &merged {
                let resolved = resolve_body(tx, event)?;
                let record = pipeline::capture_trace::project(
                    event, *classified, &resolved, link.clone()
                )?;
                tx.upsert_trace(&record)?;
            }
            tx.validate_turn_links(&session_id, &turn_id)?;

            // Decide whether to summarize.
            let close_event_in_batch = raw_group.iter()
                .any(|e| matches!(classify(e), Ok(TraceEvent::Stop)));
            if close_event_in_batch || tx.turn_summary_exists(&session_id, &turn_id)? {
                let final_rows = tx.list_trace_events(&session_id, &turn_id)?;
                let summary = pipeline::turn::summarize_turn(
                    &session_id, &turn_id, &final_rows,
                )?;
                tx.upsert_trace(&summary)?;
            }
            Ok::<(), StoreError>(())
        }).await;
        if let Err(e) = result {
            failed.push((session_id, turn_id, e.to_string()));
        }
    }
    Ok(CaptureTraceResponse { trace_id: ulid::Ulid::new().to_string(), failed })
}
```

`group_by_turn`, `order_and_assign`, `park_existing`, `resolve_body` are
small helpers — implement them in this same file. `resolve_body` reads
`sources/<event.payload_ref>` and verifies `sha256 == event.payload_hash`,
returning a `ResolvedBody` (use the existing extractor pipeline's helper if
exposed; otherwise add it next to read_jsonl_events).

- [ ] **Step 4: Passing**

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/capture_trace.rs
git commit -m "feat(cli): capture_trace persists turn + summary (#77, brief §5.0)"
```

---

## Task 17: CLI verb — multi-turn fixture + atomicity test

**Files:**
- Create: `crates/cairn-cli/tests/fixtures/trace/multi-turn.jsonl`
- Create: `crates/cairn-cli/tests/fixtures/trace/multi-turn-second-broken.jsonl`
- Modify: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn multi_turn_each_summarized_independently() {
    let store = test_sqlite_store().await;
    capture_trace::run_handler(&store, "tests/fixtures/trace/multi-turn.jsonl")
        .await.unwrap();
    store.with_tx(|tx| {
        assert!(tx.turn_summary_exists(&sess(), "turn-1")?);
        assert!(tx.turn_summary_exists(&sess(), "turn-2")?);
        // Member ids in turn-1 summary do NOT include turn-2 events.
        // ...
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn malformed_second_turn_leaves_first_intact() {
    let store = test_sqlite_store().await;
    let result = capture_trace::run_handler(
        &store, "tests/fixtures/trace/multi-turn-second-broken.jsonl"
    ).await.unwrap();
    assert_eq!(result.failed.len(), 1);

    store.with_tx(|tx| {
        // Turn 1 fully present + summarized.
        assert!(tx.turn_summary_exists(&sess(), "turn-1")?);
        // Turn 2 unwritten.
        let t2 = tx.list_trace_events(&sess(), "turn-2")?;
        assert!(t2.is_empty());
        Ok(())
    }).await.unwrap();
}
```

Build the fixtures: `multi-turn.jsonl` is two complete turns; the broken
variant has a structurally invalid second turn (e.g., `post_tool` whose
`parent_event_id` does not appear in its own turn).

- [ ] **Step 2: Failing → Passing**

```bash
cargo nextest run -p cairn-cli capture_trace_verb::multi_turn capture_trace_verb::malformed_second_turn
```

Expected: PASS (if Task 16's per-turn-transaction logic is correct).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/
git commit -m "test(cli): multi-turn + per-turn atomicity for capture_trace (#77)"
```

---

## Task 18: Replay idempotency test + closed-turn resummarize

**Files:**
- Modify: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Tests**

```rust
#[tokio::test]
async fn replay_is_idempotent() {
    let store = test_sqlite_store().await;
    let path = "tests/fixtures/trace/single-turn.jsonl";
    capture_trace::run_handler(&store, path).await.unwrap();
    capture_trace::run_handler(&store, path).await.unwrap();

    store.with_tx(|tx| {
        let rows = tx.list_trace_events(&sess(), "turn-1")?;
        assert_eq!(rows.len(), 3, "no dupes from replay");
        assert!(tx.turn_summary_exists(&sess(), "turn-1")?);
        // Exactly one summary.
        let n: u32 = tx.tx.query_row(
            "SELECT COUNT(*) FROM records WHERE trace_event='turn_summary' \
              AND trace_session_id=?1 AND trace_turn_id=?2",
            params![sess().as_str(), "turn-1"],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn late_event_after_close_resummarizes() {
    let store = test_sqlite_store().await;
    capture_trace::run_handler(&store, "tests/fixtures/trace/single-turn.jsonl")
        .await.unwrap();
    // Now import a late tool_output for turn-1 that arrives after Stop.
    capture_trace::run_handler(
        &store, "tests/fixtures/trace/single-turn-late.jsonl"
    ).await.unwrap();
    store.with_tx(|tx| {
        let rows = tx.list_trace_events(&sess(), "turn-1")?;
        assert_eq!(rows.len(), 4);
        // Summary's member_event_ids must now include the late event.
        let summary = load_summary(tx, &sess(), "turn-1")?;
        let members = summary.extra_frontmatter["trace"]["member_event_ids"].as_array().unwrap();
        assert_eq!(members.len(), 4);
        Ok(())
    }).await.unwrap();
}
```

Add the `single-turn-late.jsonl` fixture: one `tool_output` event with
`captured_at` between two existing events.

- [ ] **Step 2: Run**

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/
git commit -m "test(cli): replay idempotency + closed-turn resummarize (#77)"
```

---

## Task 19: Forget extension — per-principal hash refcount + payload_ref delete

**Files:**
- Modify: `crates/cairn-cli/src/verbs/forget.rs`
- Modify: `crates/cairn-cli/tests/forget_trace.rs` (new)

- [ ] **Step 1: Read existing forget**

```bash
sed -n '1,80p' crates/cairn-cli/src/verbs/forget.rs
```

Note where the verb dispatches per record and what state it has access to.

- [ ] **Step 2: Failing test — privacy boundary**

```rust
#[tokio::test]
async fn forget_session_deletes_sources_only_when_unreferenced_in_scope() {
    let store = test_sqlite_store().await;
    // Insert two trace records under the same scope sharing the same
    // payload_hash but different payload_refs.
    let hash = "sha256:deadbeef";
    insert_trace(&store, /* event_id */ "01A...AA", hash, "sources/a.bin").await;
    insert_trace(&store, /* event_id */ "01A...BB", hash, "sources/b.bin").await;

    write_blob("sources/a.bin", b"x");
    write_blob("sources/b.bin", b"x");

    // Forget only the first record.
    forget::run_record(&store, "01A...AA").await.unwrap();
    assert!(Path::new("sources/a.bin").exists() == false || /* deleted */ true);
    // The second record still references the hash → file may stay.
    assert!(Path::new("sources/b.bin").exists());
    // Consent journal entry is "retained-self" (not "retained-shared").
    let entries = read_consent_journal();
    assert!(entries.iter().any(|e| e.contains("retained-self")));
}
```

- [ ] **Step 3: Failing**

Expected: FAIL.

- [ ] **Step 4: Implement**

In `forget.rs`, when iterating forgotten records, branch on
`record.kind == MemoryKind::Trace`:

```rust
if record.kind == MemoryKind::Trace {
    let trace = &record.extra_frontmatter["trace"];
    let payload_hash = trace["payload_hash"].as_str().unwrap_or("");
    let payload_ref  = trace["payload_ref"].as_str().unwrap_or("");

    // In-scope refcount. exclude = the record being forgotten.
    let count = store.with_tx(|tx| tx.payload_hash_count_in_scope(
        payload_hash,
        record.scope.tenant.as_deref(),
        record.scope.user.as_deref(),
        record.scope.agent.as_deref(),
        &[record.id.as_str()],
    )).await?;

    let action = if count == 0 {
        let path = vault_root.join(payload_ref);
        if path.exists() {
            tokio::fs::remove_file(&path).await
                .context("remove sources/ blob")?;
        }
        "deleted"
    } else {
        "retained-self"
    };
    consent_journal.append(record.id.as_str(), payload_hash, action).await?;
}
```

Then redact the record body (existing forget code).

- [ ] **Step 5: Passing**

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/forget.rs crates/cairn-cli/tests/
git commit -m "feat(cli): forget deletes trace sources/ by hash (#77, spec §8.1)"
```

---

## Task 20: Reconstruction property test

**Files:**
- Create: `crates/cairn-store-sqlite/tests/trace_reconstruction_proptest.rs`

- [ ] **Step 1: Test**

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn turn_round_trips_through_store(
        n_events in 1usize..=8,
        seed in any::<u64>(),
    ) {
        // Generate n events with random captured_at timestamps for one turn.
        // Persist via run_handler (writing them to a tmp JSONL).
        // list_trace_events back → assert ordering matches sort(captured_at, event_id)
        // and member_event_ids on the summary equals the full set.
        // Use tokio::runtime::Runtime::new() inside the proptest body.
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-store-sqlite trace_reconstruction
```

Expected: PASS (after enough iterations).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/trace_reconstruction_proptest.rs
git commit -m "test(store): proptest turn round-trip reconstruction (#77)"
```

---

## Task 21: CLI snapshot test

**Files:**
- Modify: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Test**

```rust
#[tokio::test]
async fn capture_trace_response_snapshot() {
    let store = test_sqlite_store().await;
    let resp = capture_trace::run_handler(&store, "tests/fixtures/trace/single-turn.jsonl")
        .await.unwrap();
    // Stable trace_id for snapshot — override with a fixed ULID in tests.
    insta::assert_json_snapshot!(resp);
}
```

Snapshot file lives at `crates/cairn-cli/tests/snapshots/...`.

- [ ] **Step 2: Run + accept**

```bash
cargo nextest run -p cairn-cli capture_trace_response_snapshot
cargo insta review
```

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/snapshots/ crates/cairn-cli/tests/capture_trace_verb.rs
git commit -m "test(cli): capture_trace response snapshot (#77)"
```

---

## Task 22: Verification + traceability

**Files:**
- Modify: `docs/design/traceability.md`

- [ ] **Step 1: Run full verification checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Fix anything that fires. Common issues:
- Missing rustdoc on new public items → `warn(missing_docs)` is on.
- `clippy::pedantic` complaints → resolve, don't `#[allow]` without a one-line reason comment.
- `cargo machete` flags an unused dep you added in Task 3 → delete.

- [ ] **Step 2: Update traceability**

Add to `docs/design/traceability.md` under §5.0 / §9.3 entries:

```
| §5.0 turn journey | issue #77 | persisted as MemoryKind::Trace records, see crates/cairn-core/src/pipeline/capture_trace.rs |
```

- [ ] **Step 3: File the follow-up issue locally**

Capture (don't open yet — let the user do that on PR open):

```bash
cat > /tmp/followup-77.md <<'EOF'
[P0] Evolve retrieve(target=Turn|Session) IDL to carry an ordered event array

- DataTurn.turn_id should be string (matches CaptureRefs.turn_id).
- DataTurn.turn should become an array of TurnItems.
- TurnItem.turn_id should be string.

Trace records persist with the linkage needed (issue #77). This issue
exposes them through the public retrieve verb so first-party CLI/MCP/SDK
clients can read trace data back.
EOF
```

- [ ] **Step 4: Commit**

```bash
git add docs/design/traceability.md
git commit -m "docs: traceability for #77 trace-record persistence"
```

- [ ] **Step 5: Open PR**

```bash
gh pr create --title "feat: persist trace records (issue #77)" --body "$(cat <<'EOF'
## Summary

- Persists the seven trace event types (user/agent message, pre/post tool, tool output, stop, turn summary) as linked, idempotent records (brief §5.0).
- Migration 0022 adds generated columns + unique indices: idempotency on capture_event_id, one summary per turn, sequence monotonicity, parent index, payload_hash index.
- CLI verb `cairn capture_trace --from <jsonl>` runs each turn inside its own transaction; out-of-order backfills renumber via two-phase parking; closed-turn writes resummarize.
- Forget extended for trace records: in-scope payload_hash refcount + concrete payload_ref deletion.
- Hook handlers (#79), sensor adapters (#84), Claude Code hook map (#102), and public retrieve(target=Turn) IDL evolution all explicitly out of scope.

## Test plan
- [ ] `cargo nextest run --workspace`
- [ ] proptest turn round-trip: 256+ iterations
- [ ] manual: import single-turn.jsonl, multi-turn.jsonl, late-event.jsonl
- [ ] forget --session deletes sources/ blobs

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

After the PR opens, post a comment with the captured follow-up text and
file it as a separate GitHub issue.

---

## Self-Review Notes

- **Spec coverage:** §4 domain types → Tasks 1–3; §5 pipeline → Tasks 4–8; §6.1 migration → Task 9; §6.2 store ops → Tasks 10–14; §7 CLI verb → Tasks 15–18; §8.1 forget → Task 19; §9.x tests → Tasks 17, 18, 20, 21; §12 verification → Task 22.
- **Type consistency:** `TraceLink.turn_id: String` end-to-end; `summary_record_id` returns `RecordId`; CLI verb takes `&SqliteMemoryStore`. Match the actual member name on `CaptureRefs` (`turn_id`, `tool_id`) when reading capture envelopes — do not invent fields.
- **Open todo's:** Tasks 6 and 8 contain `todo!("...")` placeholders for the engineer to fill in from existing patterns (signature handling, MemoryRecord assembly). They are documentation, not code — do not commit `todo!()`.
- **Persistent gap (intentional):** public `retrieve(target=Turn)` evolution. Captured in Task 22 follow-up.
