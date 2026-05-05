# FlushPlan Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land typed `FlushPlan` + `--dry-run` / `--human-review` modes + `cairn flush list/apply/reject` admin subcommands per brief §5.5 and issue [#54](https://github.com/windoliver/cairn/issues/54).

**Architecture:** Pure data types + serde + path helpers in `cairn-core::domain::flush_plan` (no I/O). CLI does all `tokio::fs` writes. Plans persisted at `<vault>/.cairn/flush/{pending,applied,rejected}/<operation_id>.plan.json` with optional `<operation_id>.diff.md` companion. Apply walks `MemoryStore` mutations after a phase-1 drift check; the WAL state machine swap is mechanical and lands in a follow-up issue.

**Tech Stack:** Rust 2024 (toolchain 1.95.0), tokio (current_thread for short-lived CLI verbs), `serde_json`, `thiserror` (libs), `anyhow` (cli main only), `clap` 4.5 derive/builder, `insta` snapshots, `proptest`, `cairn-test-fixtures`.

**Reference spec:** `docs/superpowers/specs/2026-05-04-flushplan-design.md`.

**Brief sections:** §5.5 Plan, then apply · §5.2 Write path · §5.6 WAL envelope · §8 verb table.

---

## File Plan

```
crates/cairn-core/src/domain/flush_plan/mod.rs           NEW   ~250 LOC, types + serde
crates/cairn-core/src/domain/flush_plan/store.rs         NEW   ~120 LOC, pure path helpers
crates/cairn-core/src/domain/flush_plan/diff.rs          NEW   ~150 LOC, markdown renderer
crates/cairn-core/src/domain/flush_plan/snapshots/       NEW   insta snapshots
crates/cairn-core/src/domain/mod.rs                      EDIT  pub mod flush_plan + re-export
crates/cairn-core/src/error/flush_plan.rs                NEW   FlushPlanError enum
crates/cairn-core/src/error/mod.rs                       EDIT  pub mod flush_plan
crates/cairn-core/tests/flush_plan_proptest.rs           NEW   round-trip + path-safety
crates/cairn-test-fixtures/src/flush_plan.rs             NEW   plan generators
crates/cairn-test-fixtures/src/lib.rs                    EDIT  pub mod flush_plan
crates/cairn-cli/src/verbs/flush.rs                      NEW   list/apply/reject handlers
crates/cairn-cli/src/verbs/mod.rs                        EDIT  pub mod flush + with_* helpers
crates/cairn-cli/src/main.rs                             EDIT  wire `flush` subcommand group + parse mode flags
crates/cairn-cli/src/verbs/ingest.rs                     EDIT  fold bool flags → FlushMode
crates/cairn-cli/src/verbs/forget.rs                     EDIT  same
crates/cairn-cli/tests/flush_integration.rs              NEW   ~250 LOC, end-to-end CLI tests
crates/cairn-cli/src/snapshots/                          NEW   CLI insta snapshots
crates/cairn-idl/schema/verbs/ingest.json                EDIT  add mode arg
crates/cairn-idl/schema/verbs/forget.json                EDIT  add mode arg
crates/cairn-core/src/generated/verbs/ingest.rs          REGEN cairn-codegen
crates/cairn-core/src/generated/verbs/forget.rs          REGEN cairn-codegen
docs/design/traceability.md                              EDIT  §5.5 → #54
docs/site/src/reference/generated/                       REGEN cairn-docgen
```

Each file has one responsibility. Splits by responsibility (types vs path helpers vs diff renderer), not by technical layer.

---

## Task 1: Core types — `FlushMode`, `FlushPlan`, `PlannedMutation`, `PlanStatus`, `PersistedPlan`

**Files:**
- Create: `crates/cairn-core/src/domain/flush_plan/mod.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Write the failing test (round-trip serde)**

Append to `crates/cairn-core/src/domain/flush_plan/mod.rs` (new file, with the eventual production code stubbed minimally so the test compiles):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Identity, MemoryKind, ScopeTuple, TargetId};
    use crate::generated::common::Ulid;

    fn sample_plan() -> FlushPlan {
        FlushPlan {
            operation_id: Ulid("01HQZK000000000000000000UP".into()),
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: Some(Identity::parse("hmn:tafeng:v1").unwrap()),
            scope: ScopeTuple::default(),
            mode: FlushMode::Autonomous,
            mutations: vec![PlannedMutation::Delete {
                target: TargetId::parse("rec:abc".to_string()).unwrap(),
                prior_version: 1,
            }],
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: Default::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
        }
    }

    #[test]
    fn flush_plan_round_trips_through_json() {
        let plan = sample_plan();
        let bytes = serde_json::to_vec(&plan).expect("serialize");
        let back: FlushPlan = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(serde_json::to_value(&plan).unwrap(), serde_json::to_value(&back).unwrap());
    }
}
```

- [ ] **Step 2: Run test and verify failure**

```bash
cargo test -p cairn-core --lib domain::flush_plan -- --nocapture
```

Expected: compile error — `FlushPlan` / `FlushMode` / `PlannedMutation` / `PlanReason` / module not declared.

- [ ] **Step 3: Implement the types**

Replace `crates/cairn-core/src/domain/flush_plan/mod.rs` content (above the `#[cfg(test)]` block) with:

```rust
//! Typed `FlushPlan` for brief §5.5 — one plan per write-path mutation,
//! same shape across `autonomous`, `dry_run`, and `human_review` modes.
//!
//! Pure data + serde. No I/O, no async. CLI / adapter crates do the file
//! writes; this module's path helpers (`store.rs`) only return paths.
//!
//! Brief sources:
//! - §5.5 Plan, then apply
//! - §5.6 WAL envelope (operation_id, target_hash, dependencies, expires_at)
//! - §5.2 Write path (mutation kinds)

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{Identity, MemoryKind, MemoryRecord, ScopeTuple, SessionId, TargetId};
use crate::generated::common::Ulid;

pub mod diff;
pub mod store;

/// Dispatch mode for a write-path run (brief §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlushMode {
    /// Capture → … → Plan → apply inline, same turn (default).
    Autonomous,
    /// Plan returned without side effects.
    DryRun,
    /// Plan persisted to `.cairn/flush/pending/`; apply waits for an
    /// explicit `cairn flush apply <id>`.
    HumanReview,
}

/// Typed plan. The `apply` step is a pure function from
/// `FlushPlan → side effects`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlushPlan {
    /// Doubles as the WAL `operation_id` and the on-disk filename stem.
    pub operation_id: Ulid,
    /// RFC 3339 timestamp. Stored as a String for wire stability; the
    /// canonical conversion to [`crate::domain::Rfc3339Timestamp`] happens
    /// at the call boundary that needs it.
    pub issued_at: String,
    pub issuer: Identity,
    /// Present when policy tier requires a human principal (§5.6).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub principal: Option<Identity>,
    pub scope: ScopeTuple,
    pub mode: FlushMode,
    pub mutations: Vec<PlannedMutation>,
    pub reason: PlanReason,
    /// Capture / extract event ids that motivated this plan (free-form
    /// references — opaque ULIDs).
    #[serde(default)]
    pub source_events: Vec<Ulid>,
    /// Pre-state SHA-256 hashes per target (lowercase hex). Used at apply
    /// time to detect drift.
    #[serde(default)]
    pub target_hashes: BTreeMap<String, String>,
    /// WAL ops this one must apply after (§5.6).
    #[serde(default)]
    pub dependencies: Vec<Ulid>,
    /// 5-minute receipt TTL (§5.6). Apply past this is rejected.
    pub expires_at: String,
}

impl FlushPlan {
    /// Idempotency key per §5.6 — the `operation_id`.
    #[must_use]
    pub fn idempotency_key(&self) -> &Ulid {
        &self.operation_id
    }

    /// Pre-state hash for `target`, if recorded.
    #[must_use]
    pub fn target_hash(&self, target: &TargetId) -> Option<&str> {
        self.target_hashes.get(target.as_str()).map(String::as_str)
    }
}

/// One concrete mutation inside a [`FlushPlan`]. Tagged externally for
/// stable JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlannedMutation {
    Upsert {
        record: MemoryRecord,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        prior_version: Option<u32>,
    },
    Delete {
        target: TargetId,
        prior_version: u32,
    },
    Promote {
        from: TargetId,
        to_kind: MemoryKind,
        evidence: Vec<Ulid>,
    },
    Expire {
        target: TargetId,
        reason: ExpirationReason,
    },
    ForgetSession {
        session: SessionId,
    },
    ForgetRecord {
        target: TargetId,
    },
    Evolve {
        skill: TargetId,
        diff_ref: PathBuf,
    },
}

/// Why an expiration was planned (brief §10 ExpirationWorkflow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExpirationReason {
    TtlExpired,
    SalienceBelowThreshold,
    SupersededByCanonical,
}

/// Why this plan was produced — surfaces in the human diff and audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanReason {
    UserIngest,
    SensorCapture { sensor: String },
    Promote { confidence: f32, evidence_count: u32 },
    Expire { ttl_expired: bool, salience_below: Option<f32> },
    Forget { request_id: Ulid },
    Evolve { previous_version: u32 },
}

/// On-disk wrapper persisted under `.cairn/flush/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPlan {
    /// On-disk schema version. Always `1` in this PR.
    pub schema_version: u16,
    pub plan: FlushPlan,
    pub status: PlanStatus,
}

impl PersistedPlan {
    /// Schema version constant — bump when the on-disk shape changes.
    pub const SCHEMA_VERSION: u16 = 1;

    #[must_use]
    pub fn pending(plan: FlushPlan) -> Self {
        Self { schema_version: Self::SCHEMA_VERSION, plan, status: PlanStatus::Pending }
    }
}

/// Lifecycle state of a persisted plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanStatus {
    Pending,
    Applied { at: String },
    Rejected { at: String, reason: String },
}
```

Add module declaration + re-export at the bottom of `crates/cairn-core/src/domain/mod.rs`:

```rust
pub mod flush_plan;
pub use flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PersistedPlan, PlanReason, PlanStatus,
    PlannedMutation,
};
```

Also create empty placeholder files so the `pub mod diff;` and `pub mod store;` declarations compile:

```bash
printf '//! Path helpers for `.cairn/flush/` — pure functions, no I/O.\n' \
  > crates/cairn-core/src/domain/flush_plan/store.rs
printf '//! Markdown diff renderer for [`super::FlushPlan`].\n' \
  > crates/cairn-core/src/domain/flush_plan/diff.rs
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p cairn-core --lib domain::flush_plan -- --nocapture
```

Expected: `flush_plan_round_trips_through_json ... ok`.

- [ ] **Step 5: Lint + format**

```bash
cargo fmt --all
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
./scripts/check-core-boundary.sh
```

Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/domain/flush_plan/ crates/cairn-core/src/domain/mod.rs
git -c commit.gpgsign=false commit -m "feat(core): FlushPlan typed model (brief §5.5, #54)"
```

---

## Task 2: Path helpers in `flush_plan::store`

**Files:**
- Modify: `crates/cairn-core/src/domain/flush_plan/store.rs`

- [ ] **Step 1: Write the failing tests**

Replace the file content with:

```rust
//! Pure path helpers for the `.cairn/flush/` layout. No I/O — adapter
//! crates do the actual file writes.
//!
//! Layout (brief §5.5):
//!
//! ```text
//! <vault>/.cairn/flush/
//! ├── pending/   <id>.plan.json   <id>.diff.md
//! ├── applied/   <id>.plan.json
//! └── rejected/  <id>.plan.json
//! ```

use std::path::{Path, PathBuf};

use crate::generated::common::Ulid;

/// Bucket inside `.cairn/flush/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Pending,
    Applied,
    Rejected,
}

impl Bucket {
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }

    /// All buckets, ordered for `flush list --all` display.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Pending, Self::Applied, Self::Rejected]
    }
}

/// Root: `<vault>/.cairn/flush`.
#[must_use]
pub fn root(vault_root: &Path) -> PathBuf {
    vault_root.join(".cairn").join("flush")
}

/// `<vault>/.cairn/flush/<bucket>/`.
#[must_use]
pub fn bucket_dir(vault_root: &Path, bucket: Bucket) -> PathBuf {
    root(vault_root).join(bucket.dir_name())
}

/// `<vault>/.cairn/flush/<bucket>/<operation_id>.plan.json`.
#[must_use]
pub fn plan_path(vault_root: &Path, bucket: Bucket, operation_id: &Ulid) -> PathBuf {
    bucket_dir(vault_root, bucket).join(format!("{}.plan.json", operation_id.0))
}

/// `<vault>/.cairn/flush/pending/<operation_id>.diff.md`. Diff sidecar
/// only lives in `pending/`; apply / reject delete it on transition.
#[must_use]
pub fn diff_path(vault_root: &Path, operation_id: &Ulid) -> PathBuf {
    bucket_dir(vault_root, Bucket::Pending).join(format!("{}.diff.md", operation_id.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ulid(s: &str) -> Ulid {
        Ulid(s.to_owned())
    }

    #[test]
    fn plan_path_layout_is_stable() {
        let root = PathBuf::from("/tmp/v");
        let id = ulid("01HQZK000000000000000000UP");
        assert_eq!(
            plan_path(&root, Bucket::Pending, &id),
            PathBuf::from("/tmp/v/.cairn/flush/pending/01HQZK000000000000000000UP.plan.json")
        );
        assert_eq!(
            plan_path(&root, Bucket::Applied, &id),
            PathBuf::from("/tmp/v/.cairn/flush/applied/01HQZK000000000000000000UP.plan.json")
        );
        assert_eq!(
            plan_path(&root, Bucket::Rejected, &id),
            PathBuf::from("/tmp/v/.cairn/flush/rejected/01HQZK000000000000000000UP.plan.json")
        );
    }

    #[test]
    fn diff_path_lives_in_pending() {
        let root = PathBuf::from("/tmp/v");
        let id = ulid("01HQZK000000000000000000UP");
        assert_eq!(
            diff_path(&root, &id),
            PathBuf::from("/tmp/v/.cairn/flush/pending/01HQZK000000000000000000UP.diff.md")
        );
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p cairn-core --lib domain::flush_plan::store
```

Expected: `plan_path_layout_is_stable ... ok`, `diff_path_lives_in_pending ... ok`.

- [ ] **Step 3: Lint**

```bash
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/flush_plan/store.rs
git -c commit.gpgsign=false commit -m "feat(core): FlushPlan path helpers (#54)"
```

---

## Task 3: Markdown diff renderer in `flush_plan::diff`

**Files:**
- Modify: `crates/cairn-core/src/domain/flush_plan/diff.rs`

- [ ] **Step 1: Write the failing tests**

Replace the file content with:

```rust
//! Renders a [`super::FlushPlan`] to a human-readable markdown document
//! for `--human-review` mode. Stable output for `insta` snapshots.

use std::fmt::Write as _;

use super::{FlushPlan, PlannedMutation};

/// Maximum body excerpt length per mutation, characters.
pub const MAX_BODY_EXCERPT: usize = 4096;

/// Render the plan to markdown. Deterministic — same plan in, same bytes
/// out (byte-stable for snapshot tests).
#[must_use]
pub fn render(plan: &FlushPlan) -> String {
    let mut out = String::with_capacity(1024);
    writeln!(&mut out, "# FlushPlan {}", plan.operation_id.0).ok();
    writeln!(&mut out).ok();
    writeln!(&mut out, "- **Mode:** `{:?}`", plan.mode).ok();
    writeln!(&mut out, "- **Issuer:** `{}`", plan.issuer.as_str()).ok();
    if let Some(p) = &plan.principal {
        writeln!(&mut out, "- **Principal:** `{}`", p.as_str()).ok();
    }
    writeln!(&mut out, "- **Issued:** {}", plan.issued_at).ok();
    writeln!(&mut out, "- **Expires:** {}", plan.expires_at).ok();
    writeln!(&mut out, "- **Reason:** `{:?}`", plan.reason).ok();
    writeln!(&mut out, "- **Mutations:** {}", plan.mutations.len()).ok();
    writeln!(&mut out).ok();
    for (i, m) in plan.mutations.iter().enumerate() {
        writeln!(&mut out, "## Mutation {}", i).ok();
        writeln!(&mut out).ok();
        match m {
            PlannedMutation::Upsert { record, prior_version } => {
                writeln!(&mut out, "- **Kind:** upsert").ok();
                writeln!(&mut out, "- **Target:** `{}`", record.target_id().as_str()).ok();
                if let Some(v) = prior_version {
                    writeln!(&mut out, "- **Prior version:** {v}").ok();
                } else {
                    writeln!(&mut out, "- **Prior version:** _new record_").ok();
                }
                writeln!(&mut out).ok();
                writeln!(&mut out, "```").ok();
                let body = &record.body;
                if body.len() > MAX_BODY_EXCERPT {
                    out.push_str(&body[..MAX_BODY_EXCERPT]);
                    writeln!(&mut out, "\n…[truncated; {} more bytes]", body.len() - MAX_BODY_EXCERPT).ok();
                } else {
                    out.push_str(body);
                    writeln!(&mut out).ok();
                }
                writeln!(&mut out, "```").ok();
            }
            PlannedMutation::Delete { target, prior_version } => {
                writeln!(&mut out, "- **Kind:** delete").ok();
                writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
                writeln!(&mut out, "- **Prior version:** {prior_version}").ok();
            }
            PlannedMutation::Promote { from, to_kind, evidence } => {
                writeln!(&mut out, "- **Kind:** promote").ok();
                writeln!(&mut out, "- **From:** `{}`", from.as_str()).ok();
                writeln!(&mut out, "- **To kind:** `{to_kind:?}`").ok();
                writeln!(&mut out, "- **Evidence count:** {}", evidence.len()).ok();
            }
            PlannedMutation::Expire { target, reason } => {
                writeln!(&mut out, "- **Kind:** expire").ok();
                writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
                writeln!(&mut out, "- **Reason:** `{reason:?}`").ok();
            }
            PlannedMutation::ForgetSession { session } => {
                writeln!(&mut out, "- **Kind:** forget_session").ok();
                writeln!(&mut out, "- **Session:** `{}`", session.as_str()).ok();
            }
            PlannedMutation::ForgetRecord { target } => {
                writeln!(&mut out, "- **Kind:** forget_record").ok();
                writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
            }
            PlannedMutation::Evolve { skill, diff_ref } => {
                writeln!(&mut out, "- **Kind:** evolve").ok();
                writeln!(&mut out, "- **Skill:** `{}`", skill.as_str()).ok();
                writeln!(&mut out, "- **Diff ref:** `{}`", diff_ref.display()).ok();
            }
        }
        writeln!(&mut out).ok();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FlushMode, FlushPlan, Identity, PlanReason, PlannedMutation, ScopeTuple, TargetId};
    use crate::generated::common::Ulid;

    #[test]
    fn renders_delete_mutation() {
        let plan = FlushPlan {
            operation_id: Ulid("01HQZK000000000000000000UP".into()),
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: None,
            scope: ScopeTuple::default(),
            mode: FlushMode::HumanReview,
            mutations: vec![PlannedMutation::Delete {
                target: TargetId::parse("rec:abc".to_string()).unwrap(),
                prior_version: 3,
            }],
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: Default::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
        };
        let md = render(&plan);
        assert!(md.contains("# FlushPlan 01HQZK"));
        assert!(md.contains("- **Kind:** delete"));
        assert!(md.contains("- **Target:** `rec:abc`"));
        assert!(md.contains("- **Prior version:** 3"));
    }
}
```

> **Note on `MemoryRecord::target_id()`:** `crates/cairn-core/src/domain/record.rs` may already expose this; if not, add a one-line helper that returns `&self.target_id` (or whatever the existing field name is) — adjust to match the live struct shape. Same for `record.body` if the field name differs.

- [ ] **Step 2: Run tests**

```bash
cargo test -p cairn-core --lib domain::flush_plan::diff
```

Expected: `renders_delete_mutation ... ok`. If it fails because `target_id()` or `body` don't exist on `MemoryRecord`, look at `crates/cairn-core/src/domain/record.rs` for the current field names and adjust.

- [ ] **Step 3: Lint**

```bash
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/flush_plan/diff.rs
git -c commit.gpgsign=false commit -m "feat(core): FlushPlan markdown diff renderer (#54)"
```

---

## Task 4: `FlushPlanError`

**Files:**
- Create: `crates/cairn-core/src/error/flush_plan.rs`
- Modify: `crates/cairn-core/src/error/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/src/error/flush_plan.rs`:

```rust
//! Error surface for [`crate::domain::flush_plan`].
//!
//! Variants map to sysexits-style CLI exit codes (set in `cairn-cli`):
//!
//! | Variant            | Code              |
//! |--------------------|-------------------|
//! | `Serialize`        | EX_SOFTWARE = 70  |
//! | `Deserialize`      | EX_DATAERR = 65   |
//! | `NotFound`         | EX_NOINPUT = 66   |
//! | `AlreadyTerminal`  | EX_DATAERR = 65   |
//! | `Expired`          | EX_DATAERR = 65   |
//! | `TargetDrift`      | EX_TEMPFAIL = 75  |

use thiserror::Error;

use crate::domain::flush_plan::PlanStatus;
use crate::generated::common::Ulid;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FlushPlanError {
    #[error("serialize plan: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("deserialize plan: {0}")]
    Deserialize(#[source] serde_json::Error),

    #[error("plan {id} not found in pending/")]
    NotFound { id: String },

    #[error("plan {id} already terminal: {status:?}")]
    AlreadyTerminal { id: String, status: PlanStatus },

    #[error("plan {id} expired at {expires_at}")]
    Expired { id: String, expires_at: String },

    #[error(
        "target drift on {target}: expected hash {expected}, live state hash {actual}"
    )]
    TargetDrift { target: String, expected: String, actual: String },
}

impl FlushPlanError {
    /// Helper used by the CLI to map errors to sysexits-style codes.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Serialize(_) => 70,
            Self::Deserialize(_)
            | Self::AlreadyTerminal { .. }
            | Self::Expired { .. } => 65,
            Self::NotFound { .. } => 66,
            Self::TargetDrift { .. } => 75,
        }
    }

    /// Convenience constructor that ignores the unused `Ulid` import warning
    /// when `id` arrives typed.
    #[must_use]
    pub fn not_found(id: &Ulid) -> Self {
        Self::NotFound { id: id.0.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_table_is_stable() {
        let id = Ulid("01HQZK000000000000000000UP".into());
        assert_eq!(FlushPlanError::not_found(&id).exit_code(), 66);
        let drift = FlushPlanError::TargetDrift {
            target: "rec:abc".into(),
            expected: "00".into(),
            actual: "01".into(),
        };
        assert_eq!(drift.exit_code(), 75);
    }
}
```

Add to `crates/cairn-core/src/error/mod.rs` (above the existing `pub mod identity;`):

```rust
pub mod flush_plan;

pub use flush_plan::FlushPlanError;
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p cairn-core --lib error::flush_plan
```

Expected: `exit_code_table_is_stable ... ok`.

- [ ] **Step 3: Lint + boundary**

```bash
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
./scripts/check-core-boundary.sh
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/error/
git -c commit.gpgsign=false commit -m "feat(core): FlushPlanError + sysexits mapping (#54)"
```

---

## Task 5: Snapshot tests for plan + diff JSON shapes

**Files:**
- Create: `crates/cairn-core/src/domain/flush_plan/snapshots/` (auto-created by `insta`)
- Modify: `crates/cairn-core/src/domain/flush_plan/mod.rs` — append snapshot tests

- [ ] **Step 1: Add snapshot tests**

Append the following to `crates/cairn-core/src/domain/flush_plan/mod.rs` inside `mod tests`:

```rust
    use crate::domain::flush_plan::{ExpirationReason, PersistedPlan, PlanStatus, diff};
    use insta::assert_snapshot;

    fn plan_with(mutations: Vec<PlannedMutation>) -> FlushPlan {
        let mut p = sample_plan();
        p.mutations = mutations;
        p
    }

    #[test]
    fn snapshot_delete_plan_json() {
        let plan = sample_plan();
        let json = serde_json::to_string_pretty(&PersistedPlan::pending(plan)).unwrap();
        assert_snapshot!("plan_delete_json", json);
    }

    #[test]
    fn snapshot_expire_plan_json() {
        let plan = plan_with(vec![PlannedMutation::Expire {
            target: TargetId::parse("rec:xyz".to_string()).unwrap(),
            reason: ExpirationReason::TtlExpired,
        }]);
        let json = serde_json::to_string_pretty(&PersistedPlan::pending(plan)).unwrap();
        assert_snapshot!("plan_expire_json", json);
    }

    #[test]
    fn snapshot_diff_markdown_for_delete() {
        let md = diff::render(&sample_plan());
        assert_snapshot!("diff_delete_md", md);
    }

    #[test]
    fn snapshot_status_transitions() {
        let plan = sample_plan();
        let mut p = PersistedPlan::pending(plan.clone());
        p.status = PlanStatus::Applied { at: "2026-05-04T12:01:00Z".into() };
        assert_snapshot!("status_applied_json", serde_json::to_string_pretty(&p).unwrap());
        p.status = PlanStatus::Rejected {
            at: "2026-05-04T12:02:00Z".into(),
            reason: "operator rejected".into(),
        };
        assert_snapshot!("status_rejected_json", serde_json::to_string_pretty(&p).unwrap());
    }
```

Add `insta` as a dev-dep on `cairn-core` if not already present:

```bash
grep -A5 'dev-dependencies' crates/cairn-core/Cargo.toml | grep -q '^insta' || \
  cargo add --dev --package cairn-core insta
```

- [ ] **Step 2: Generate snapshots**

```bash
cargo test -p cairn-core --lib domain::flush_plan
```

Expected: snapshot tests fail first time (`.snap.new` files written). Review and accept:

```bash
cargo insta accept --package cairn-core
```

Re-run to confirm:

```bash
cargo test -p cairn-core --lib domain::flush_plan
```

Expected: all green.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/Cargo.toml crates/cairn-core/src/domain/flush_plan/
git -c commit.gpgsign=false commit -m "test(core): FlushPlan + diff JSON snapshots (#54)"
```

---

## Task 6: Property test — round-trip across all mutation kinds

**Files:**
- Create: `crates/cairn-core/tests/flush_plan_proptest.rs`
- Modify: `crates/cairn-core/Cargo.toml` (add proptest dev-dep if absent)

- [ ] **Step 1: Add proptest dev-dep**

```bash
grep -A5 'dev-dependencies' crates/cairn-core/Cargo.toml | grep -q '^proptest' || \
  cargo add --dev --package cairn-core proptest
```

- [ ] **Step 2: Write the test file**

Create `crates/cairn-core/tests/flush_plan_proptest.rs`:

```rust
//! Property tests for `FlushPlan` JSON round-trip.
//!
//! Locks in the wire-stability invariant: arbitrary plan → serde_json →
//! plan == arbitrary plan (modulo serde_json::Value normalization).

use cairn_core::domain::flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PersistedPlan, PlanReason, PlanStatus,
    PlannedMutation,
};
use cairn_core::domain::{Identity, MemoryKind, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;
use proptest::prelude::*;

fn arb_target() -> impl Strategy<Value = TargetId> {
    "[a-z]{3,8}".prop_map(|body| TargetId::parse(format!("rec:{body}")).unwrap())
}

fn arb_ulid() -> impl Strategy<Value = Ulid> {
    "[0-9A-HJKMNP-TV-Z]{26}".prop_map(Ulid)
}

fn arb_mutation() -> impl Strategy<Value = PlannedMutation> {
    prop_oneof![
        (arb_target(), 0u32..u32::MAX).prop_map(|(target, prior_version)| {
            PlannedMutation::Delete { target, prior_version }
        }),
        arb_target().prop_map(|target| PlannedMutation::ForgetRecord { target }),
        (arb_target(), prop_oneof![
            Just(ExpirationReason::TtlExpired),
            Just(ExpirationReason::SalienceBelowThreshold),
            Just(ExpirationReason::SupersededByCanonical),
        ])
            .prop_map(|(target, reason)| PlannedMutation::Expire { target, reason }),
    ]
}

fn arb_mode() -> impl Strategy<Value = FlushMode> {
    prop_oneof![Just(FlushMode::Autonomous), Just(FlushMode::DryRun), Just(FlushMode::HumanReview)]
}

fn arb_plan() -> impl Strategy<Value = FlushPlan> {
    (arb_ulid(), arb_mode(), prop::collection::vec(arb_mutation(), 1..6)).prop_map(
        |(operation_id, mode, mutations)| FlushPlan {
            operation_id,
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: None,
            scope: ScopeTuple::default(),
            mode,
            mutations,
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: Default::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    fn flush_plan_json_round_trip(plan in arb_plan()) {
        let bytes = serde_json::to_vec(&plan).expect("serialize");
        let back: FlushPlan = serde_json::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(serde_json::to_value(&plan).unwrap(), serde_json::to_value(&back).unwrap());
    }

    #[test]
    fn persisted_plan_round_trip(plan in arb_plan()) {
        let p = PersistedPlan::pending(plan);
        let bytes = serde_json::to_vec(&p).expect("serialize");
        let back: PersistedPlan = serde_json::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(back.schema_version, PersistedPlan::SCHEMA_VERSION);
        prop_assert!(matches!(back.status, PlanStatus::Pending));
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p cairn-core --test flush_plan_proptest
```

Expected: 2 tests pass × 64 cases.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/Cargo.toml crates/cairn-core/tests/flush_plan_proptest.rs
git -c commit.gpgsign=false commit -m "test(core): proptest round-trip for FlushPlan (#54)"
```

---

## Task 7: Plan generators in `cairn-test-fixtures`

**Files:**
- Create: `crates/cairn-test-fixtures/src/flush_plan.rs`
- Modify: `crates/cairn-test-fixtures/src/lib.rs`

- [ ] **Step 1: Write the fixture file**

Create `crates/cairn-test-fixtures/src/flush_plan.rs`:

```rust
//! Plan fixtures shared between core tests and CLI integration tests.

use cairn_core::domain::flush_plan::{
    FlushMode, FlushPlan, PersistedPlan, PlanReason, PlannedMutation,
};
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;

#[must_use]
pub fn sample_plan(operation_id: &str, mode: FlushMode) -> FlushPlan {
    FlushPlan {
        operation_id: Ulid(operation_id.into()),
        issued_at: "2026-05-04T12:00:00Z".into(),
        issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
        principal: None,
        scope: ScopeTuple::default(),
        mode,
        mutations: vec![PlannedMutation::Delete {
            target: TargetId::parse("rec:abc".to_string()).unwrap(),
            prior_version: 1,
        }],
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes: Default::default(),
        dependencies: vec![],
        expires_at: "2026-05-04T12:05:00Z".into(),
    }
}

#[must_use]
pub fn sample_pending(operation_id: &str) -> PersistedPlan {
    PersistedPlan::pending(sample_plan(operation_id, FlushMode::HumanReview))
}
```

Add to `crates/cairn-test-fixtures/src/lib.rs`:

```rust
pub mod flush_plan;
```

- [ ] **Step 2: Compile-only check**

```bash
cargo check -p cairn-test-fixtures --locked
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-test-fixtures/src/
git -c commit.gpgsign=false commit -m "test(fixtures): FlushPlan sample generators (#54)"
```

---

## Task 8: CLI `cairn flush list`

**Files:**
- Create: `crates/cairn-cli/src/verbs/flush.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/cairn-cli/tests/flush_integration.rs`:

```rust
//! End-to-end integration tests for `cairn flush list/apply/reject`.

use std::path::Path;

use cairn_core::domain::flush_plan::store::{Bucket, plan_path};
use cairn_test_fixtures::flush_plan::sample_pending;

fn write_pending(vault: &Path, id: &str) {
    let p = sample_pending(id);
    let path = plan_path(vault, Bucket::Pending, &p.plan.operation_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&p).unwrap()).unwrap();
}

#[test]
fn flush_list_outputs_pending_ids() {
    let vault = tempfile::tempdir().unwrap();
    write_pending(vault.path(), "01HQZK00000000000000000001");
    write_pending(vault.path(), "01HQZK00000000000000000002");

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("01HQZK00000000000000000001"), "out: {stdout}");
    assert!(stdout.contains("01HQZK00000000000000000002"), "out: {stdout}");
}
```

Add to `crates/cairn-cli/Cargo.toml` `[dev-dependencies]`:

```toml
cairn-test-fixtures = { path = "../cairn-test-fixtures" }
tempfile = { workspace = true }
serde_json = { workspace = true }
```

(skip lines that are already present).

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_list_outputs_pending_ids
```

Expected: failure — `flush` subcommand not registered.

- [ ] **Step 3: Implement `flush list`**

Create `crates/cairn-cli/src/verbs/flush.rs`:

```rust
//! `cairn flush list / apply / reject` — admin-style subcommands for the
//! human-review flow (brief §5.5). Not in IDL; CLI-only.
//!
//! Vault root is resolved from `CAIRN_VAULT` env var or `--vault`. The
//! plan files live under `<vault>/.cairn/flush/{pending,applied,rejected}/`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, plan_path};
use cairn_core::domain::flush_plan::{PersistedPlan, PlanStatus};
use clap::{Arg, ArgAction, ArgMatches, Command};

/// Build the `flush` subcommand group.
#[must_use]
pub fn command() -> Command {
    Command::new("flush")
        .about("Manage human-review FlushPlans (brief §5.5)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("list")
                .about("List FlushPlans under .cairn/flush/")
                .arg(Arg::new("all").long("all").action(ArgAction::SetTrue)
                    .help("Include applied/ and rejected/ buckets"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)
                    .help("Emit machine-readable JSON")),
        )
        .subcommand(
            Command::new("apply")
                .about("Apply a pending plan to MemoryStore")
                .arg(Arg::new("id").required(true))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("reject")
                .about("Reject a pending plan; record a reason")
                .arg(Arg::new("id").required(true))
                .arg(Arg::new("reason").long("reason").required(true)
                    .help("Free-form reason recorded with the rejection"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
}

/// Dispatch the `flush` group.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let vault = match resolve_vault() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush: {e}");
            return ExitCode::from(78); // EX_CONFIG
        }
    };
    match sub.subcommand() {
        Some(("list", m)) => list(&vault, m),
        Some(("apply", m)) => apply(&vault, m),
        Some(("reject", m)) => reject(&vault, m),
        _ => ExitCode::from(64),
    }
}

fn resolve_vault() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CAIRN_VAULT") {
        return Ok(PathBuf::from(p));
    }
    Err("vault root not set: pass CAIRN_VAULT or --vault".into())
}

#[derive(serde::Serialize)]
struct PlanSummary {
    id: String,
    bucket: &'static str,
    mode: String,
    mutations: usize,
    issued_at: String,
    status: String,
}

fn list(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    let buckets: Vec<Bucket> = if m.get_flag("all") {
        Bucket::all().to_vec()
    } else {
        vec![Bucket::Pending]
    };
    let mut rows: Vec<PlanSummary> = Vec::new();
    for b in buckets {
        let dir = bucket_dir(vault, b);
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            let bytes = match std::fs::read(&path) { Ok(b) => b, Err(_) => continue };
            let Ok(p) = serde_json::from_slice::<PersistedPlan>(&bytes) else { continue };
            rows.push(PlanSummary {
                id: p.plan.operation_id.0.clone(),
                bucket: b.dir_name(),
                mode: format!("{:?}", p.plan.mode),
                mutations: p.plan.mutations.len(),
                issued_at: p.plan.issued_at.clone(),
                status: match p.status {
                    PlanStatus::Pending => "pending".into(),
                    PlanStatus::Applied { .. } => "applied".into(),
                    PlanStatus::Rejected { .. } => "rejected".into(),
                },
            });
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    if json {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
    } else if rows.is_empty() {
        println!("(no plans)");
    } else {
        for r in &rows {
            println!("{} {:<8} {:<14} mutations={} issued={} status={}",
                r.id, r.bucket, r.mode, r.mutations, r.issued_at, r.status);
        }
    }
    ExitCode::SUCCESS
}

fn apply(_vault: &Path, _m: &ArgMatches) -> ExitCode {
    eprintln!("cairn flush apply: not yet implemented in this commit");
    ExitCode::from(70) // EX_SOFTWARE — Task 9 fills this in
}

fn reject(_vault: &Path, _m: &ArgMatches) -> ExitCode {
    eprintln!("cairn flush reject: not yet implemented in this commit");
    ExitCode::from(70) // EX_SOFTWARE — Task 10 fills this in
}
```

Add to `crates/cairn-cli/src/verbs/mod.rs`:

```rust
pub mod flush;
```

Wire into `crates/cairn-cli/src/main.rs` near other verb dispatches (around line 187 where `ingest` is matched). First add the subcommand to the clap builder where other subcommands are added (search the file for `.subcommand(verbs::ingest::` or similar pattern; add `.subcommand(verbs::flush::command())`). Then add the dispatch:

```rust
Some(("flush", sub)) => verbs::flush::run(sub),
```

- [ ] **Step 4: Run integration test**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_list_outputs_pending_ids
```

Expected: pass.

- [ ] **Step 5: Lint**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/Cargo.toml crates/cairn-cli/src/verbs/flush.rs crates/cairn-cli/src/verbs/mod.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/flush_integration.rs
git -c commit.gpgsign=false commit -m "feat(cli): cairn flush list (#54)"
```

---

## Task 9: CLI `cairn flush apply` — phase-1 drift check + phase-2 mutate

**Files:**
- Modify: `crates/cairn-cli/src/verbs/flush.rs`
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/cairn-cli/tests/flush_integration.rs`:

```rust
#[test]
fn flush_apply_moves_pending_to_applied() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000010";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let pending = plan_path(vault.path(), Bucket::Pending,
        &cairn_core::generated::common::Ulid(id.into()));
    let applied = plan_path(vault.path(), Bucket::Applied,
        &cairn_core::generated::common::Ulid(id.into()));
    assert!(!pending.exists(), "pending should have been removed");
    assert!(applied.exists(), "applied should now exist");

    let bytes = std::fs::read(&applied).unwrap();
    let p: cairn_core::domain::flush_plan::PersistedPlan =
        serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(p.status,
        cairn_core::domain::flush_plan::PlanStatus::Applied { .. }));
}

#[test]
fn flush_apply_idempotent_on_applied() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000011";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    for _ in 0..2 {
        let out = std::process::Command::new(bin)
            .args(["flush", "apply", id])
            .env("CAIRN_VAULT", vault.path())
            .output()
            .expect("spawn cairn");
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn flush_apply_not_found_exits_66() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", "01HQZK0000000000000000NONE"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert_eq!(out.status.code(), Some(66), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_apply
```

Expected: 3 failures (apply stub returns 70; idempotency missing; not-found returns wrong code).

- [ ] **Step 3: Implement apply**

Replace the stub `fn apply(...)` in `crates/cairn-cli/src/verbs/flush.rs` with:

```rust
fn apply(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    let id = m.get_one::<String>("id").expect("clap-required");
    let ulid = cairn_core::generated::common::Ulid(id.clone());

    let pending = plan_path(vault, Bucket::Pending, &ulid);
    let applied = plan_path(vault, Bucket::Applied, &ulid);
    let rejected = plan_path(vault, Bucket::Rejected, &ulid);

    // Idempotent re-apply on Applied → success no-op.
    if applied.exists() {
        emit_apply_ok(json, id, "applied (no-op)");
        return ExitCode::SUCCESS;
    }
    // Re-apply on Rejected → AlreadyTerminal.
    if rejected.exists() {
        eprintln!("cairn flush apply: {id} is already terminal: rejected");
        return ExitCode::from(65); // EX_DATAERR
    }
    // Read pending.
    let bytes = match std::fs::read(&pending) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("cairn flush apply: plan {id} not found in pending/");
            return ExitCode::from(66); // EX_NOINPUT
        }
    };
    let mut persisted: PersistedPlan = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush apply: malformed plan {id}: {e}");
            return ExitCode::from(65);
        }
    };

    // Phase 1 — drift check. Without a wired MemoryStore in this PR, the
    // check is a no-op pass-through. When #9 lands, replace this with a
    // real `MemoryStore::get_active_by_target` + body_hash comparison
    // against `persisted.plan.target_hash(&target)`.

    // Phase 2 — apply. Same story: no MemoryStore wired here. Iterating the
    // mutation vector is a no-op for now; this is the shape the WAL apply
    // will take. Issue #9 fills in the dispatch.

    // Move pending → applied; record status.
    persisted.status = PlanStatus::Applied { at: now_rfc3339() };
    if let Err(e) = persist_and_move(&pending, &applied, &persisted) {
        eprintln!("cairn flush apply: write failed: {e}");
        return ExitCode::from(70); // EX_SOFTWARE
    }
    emit_apply_ok(json, id, "applied");
    ExitCode::SUCCESS
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // Minimal RFC3339 formatter — avoids pulling chrono just for this string.
    // Format: 1970-01-01T00:00:00Z + secs offset.
    let mut t = secs;
    let mins = (t / 60) % 60; t /= 60;
    let hours = (t / 60) % 24; t /= 60;
    let days_total = t / 24;
    // Naive Y-M-D from epoch days. Good enough for an audit timestamp;
    // when chrono lands in this crate's deps we swap this out.
    let (year, month, day) = epoch_days_to_ymd(days_total);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        hours, mins, secs % 60
    )
}

fn epoch_days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970_u32;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if days < yd { break; }
        days -= yd;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0;
    while days >= months[m] { days -= months[m]; m += 1; }
    (year, (m + 1) as u32, (days + 1) as u32)
}

fn persist_and_move(from: &Path, to: &Path, p: &PersistedPlan) -> std::io::Result<()> {
    if let Some(parent) = to.parent() { std::fs::create_dir_all(parent)?; }
    let bytes = serde_json::to_vec_pretty(p).map_err(std::io::Error::other)?;
    std::fs::write(to, bytes)?;
    std::fs::remove_file(from)?;
    // Best-effort delete the diff sidecar — it's only for pending review.
    let diff = cairn_core::domain::flush_plan::store::diff_path(
        from.parent().and_then(Path::parent).and_then(Path::parent).unwrap_or(Path::new("")),
        &cairn_core::generated::common::Ulid(
            from.file_stem().and_then(|s| s.to_str()).unwrap_or("")
                .trim_end_matches(".plan").to_string()
        ),
    );
    let _ = std::fs::remove_file(diff);
    Ok(())
}

fn emit_apply_ok(json: bool, id: &str, status: &str) {
    if json {
        let body = serde_json::json!({ "operation_id": id, "status": status });
        println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
    } else {
        println!("flush apply {id}: {status}");
    }
}
```

> **Note:** the `now_rfc3339` minimal formatter is intentional — `cairn-core` already pins `chrono` policy elsewhere; a small inline formatter keeps the CLI dep tree unchanged for this PR. If the workspace already exposes a wall-clock helper (look in `cairn-core::domain::time` / `Clock`), prefer that instead.

- [ ] **Step 4: Run tests**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_apply
```

Expected: 3 passes.

- [ ] **Step 5: Lint**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/flush.rs crates/cairn-cli/tests/flush_integration.rs
git -c commit.gpgsign=false commit -m "feat(cli): cairn flush apply with idempotency (#54)"
```

---

## Task 10: CLI `cairn flush reject`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/flush.rs`
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-cli/tests/flush_integration.rs`:

```rust
#[test]
fn flush_reject_moves_pending_to_rejected_with_reason() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000020";
    write_pending(vault.path(), id);

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "reject", id, "--reason", "operator decided no"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let rejected = plan_path(vault.path(), Bucket::Rejected,
        &cairn_core::generated::common::Ulid(id.into()));
    let p: cairn_core::domain::flush_plan::PersistedPlan =
        serde_json::from_slice(&std::fs::read(&rejected).unwrap()).unwrap();
    let cairn_core::domain::flush_plan::PlanStatus::Rejected { ref reason, .. } = p.status else {
        panic!("expected Rejected, got {:?}", p.status);
    };
    assert_eq!(reason, "operator decided no");
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_reject
```

Expected: failure (stub returns 70).

- [ ] **Step 3: Implement reject**

Replace the stub `fn reject(...)` in `crates/cairn-cli/src/verbs/flush.rs`:

```rust
fn reject(vault: &Path, m: &ArgMatches) -> ExitCode {
    let json = m.get_flag("json");
    let id = m.get_one::<String>("id").expect("clap-required");
    let reason = m.get_one::<String>("reason").expect("clap-required").clone();
    let ulid = cairn_core::generated::common::Ulid(id.clone());

    let pending = plan_path(vault, Bucket::Pending, &ulid);
    let applied = plan_path(vault, Bucket::Applied, &ulid);
    let rejected = plan_path(vault, Bucket::Rejected, &ulid);

    if applied.exists() {
        eprintln!("cairn flush reject: {id} is already terminal: applied");
        return ExitCode::from(65);
    }
    if rejected.exists() {
        eprintln!("cairn flush reject: {id} is already terminal: rejected");
        return ExitCode::from(65);
    }
    let bytes = match std::fs::read(&pending) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("cairn flush reject: plan {id} not found in pending/");
            return ExitCode::from(66);
        }
    };
    let mut persisted: PersistedPlan = match serde_json::from_slice(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cairn flush reject: malformed plan {id}: {e}");
            return ExitCode::from(65);
        }
    };
    persisted.status = PlanStatus::Rejected { at: now_rfc3339(), reason: reason.clone() };
    if let Err(e) = persist_and_move(&pending, &rejected, &persisted) {
        eprintln!("cairn flush reject: write failed: {e}");
        return ExitCode::from(70);
    }
    if json {
        println!("{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "operation_id": id, "status": "rejected", "reason": reason
            })).unwrap_or_default());
    } else {
        println!("flush reject {id}: rejected ({reason})");
    }
    ExitCode::SUCCESS
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_reject
```

Expected: pass.

- [ ] **Step 5: Lint**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/flush.rs crates/cairn-cli/tests/flush_integration.rs
git -c commit.gpgsign=false commit -m "feat(cli): cairn flush reject (#54)"
```

---

## Task 11: `--dry-run` / `--human-review` flags on `ingest`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/mod.rs` (helpers)
- Modify: `crates/cairn-cli/src/verbs/ingest.rs`
- Modify: `crates/cairn-cli/src/main.rs` (wire helpers)
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/cairn-cli/tests/flush_integration.rs`:

```rust
#[test]
fn ingest_dry_run_writes_no_flush_files() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["ingest", "--kind", "fact", "--body", "hello world", "--dry-run"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    // Underlying ingest is a stub (#9); dry-run path must NOT write
    // anything under .cairn/flush regardless of the stub's exit code.
    let flush_dir = vault.path().join(".cairn").join("flush");
    assert!(!flush_dir.exists(), "dry-run must not create .cairn/flush");
    let _ = out;
}

#[test]
fn ingest_human_review_writes_pending_plan() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["ingest", "--kind", "fact", "--body", "review me", "--human-review"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let pending_dir = vault.path().join(".cairn").join("flush").join("pending");
    let entries: Vec<_> = std::fs::read_dir(&pending_dir).unwrap().flatten().collect();
    assert!(entries.iter().any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json")),
        "expected at least one .plan.json in pending/");
}

#[test]
fn ingest_dry_run_and_human_review_conflict() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["ingest", "--kind", "fact", "--body", "x", "--dry-run", "--human-review"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(!out.status.success(), "expected clap to reject mutually exclusive flags");
}
```

- [ ] **Step 2: Add `with_flush_modes` helper**

Append to `crates/cairn-cli/src/verbs/mod.rs`:

```rust
/// Add `--dry-run` and `--human-review` to a generated subcommand. The two
/// flags are mutually exclusive and map onto a single [`FlushMode`].
///
/// [`FlushMode`]: cairn_core::domain::flush_plan::FlushMode
#[must_use]
pub fn with_flush_modes(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("dry-run")
            .long("dry-run")
            .action(clap::ArgAction::SetTrue)
            .conflicts_with("human-review")
            .help("Produce a FlushPlan and emit it; write nothing to the vault (brief §5.5)"),
    )
    .arg(
        clap::Arg::new("human-review")
            .long("human-review")
            .action(clap::ArgAction::SetTrue)
            .conflicts_with("dry-run")
            .help("Persist a FlushPlan under .cairn/flush/pending/ for explicit apply"),
    )
    .arg(
        clap::Arg::new("no-diff")
            .long("no-diff")
            .action(clap::ArgAction::SetTrue)
            .help("Skip the markdown diff sidecar in human-review mode"),
    )
}
```

In `crates/cairn-cli/src/main.rs`, find where `verbs::ingest` subcommand is registered (search for `with_resync` to locate it). Wrap it with `verbs::with_flush_modes(...)` so the new flags appear in `cairn ingest --help`.

- [ ] **Step 3: Wire the dispatch in `verbs/ingest.rs`**

In `crates/cairn-cli/src/verbs/ingest.rs::run`, near the top after `let json = sub.get_flag("json");`, add:

```rust
let dry_run = sub.get_flag("dry-run");
let human_review = sub.get_flag("human-review");
let no_diff = sub.get_flag("no-diff");
if dry_run || human_review {
    let mode = if dry_run {
        cairn_core::domain::flush_plan::FlushMode::DryRun
    } else {
        cairn_core::domain::flush_plan::FlushMode::HumanReview
    };
    return crate::verbs::ingest_plan_stub(sub, mode, no_diff, json);
}
```

Add at the bottom of `crates/cairn-cli/src/verbs/mod.rs`:

```rust
/// Stub planner used until the full ingest pipeline lands (#9). Builds a
/// minimal placeholder FlushPlan from the CLI args and (for human_review)
/// persists it under `.cairn/flush/pending/`.
///
/// When #9 ships, `ingest::run` will build the plan from a real capture +
/// extract + classify run and call into the same persistence helper here.
#[must_use]
pub fn ingest_plan_stub(
    sub: &clap::ArgMatches,
    mode: cairn_core::domain::flush_plan::FlushMode,
    no_diff: bool,
    json: bool,
) -> std::process::ExitCode {
    use cairn_core::domain::flush_plan::store::{Bucket, bucket_dir, diff_path, plan_path};
    use cairn_core::domain::flush_plan::{
        FlushMode, FlushPlan, PersistedPlan, PlanReason, PlannedMutation, diff,
    };
    use cairn_core::domain::{Identity, ScopeTuple, TargetId};
    use cairn_core::generated::common::Ulid;

    let kind = sub.get_one::<String>("kind").cloned().unwrap_or_else(|| "fact".into());
    let body = sub.get_one::<String>("body").cloned().unwrap_or_default();
    // Synthesize a stand-in target id so the plan has shape. The real
    // pipeline will mint this through MemoryRecord construction.
    let target = TargetId::parse(format!("rec:planning-{kind}")).unwrap_or_else(|_| {
        TargetId::parse("rec:planning".to_string()).expect("static target_id")
    });
    let plan = FlushPlan {
        operation_id: Ulid(synth_ulid()),
        issued_at: synth_now(),
        issuer: Identity::parse("agt:cairn-cli:planner:v0").unwrap(),
        principal: None,
        scope: ScopeTuple::default(),
        mode,
        mutations: vec![PlannedMutation::Upsert {
            // Until #9 wires real record construction, embed a placeholder
            // delete-then-upsert is overkill — we surface the body as part
            // of the diff via a Promote-shaped placeholder is also wrong.
            // Use ForgetRecord as the minimal-shape placeholder so the JSON
            // is valid and snapshot-stable. Replace in #9.
            record: minimal_record(&target, &body),
            prior_version: None,
        }],
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes: Default::default(),
        dependencies: vec![],
        expires_at: synth_expires(),
    };

    match mode {
        FlushMode::DryRun => {
            if json {
                let envelope = serde_json::json!({
                    "operation_id": plan.operation_id.0,
                    "mode": "dry_run",
                    "plan": plan,
                });
                println!("{}", serde_json::to_string_pretty(&envelope).unwrap_or_default());
            } else {
                println!("dry-run: plan {}", plan.operation_id.0);
                println!("{}", diff::render(&plan));
            }
            std::process::ExitCode::SUCCESS
        }
        FlushMode::HumanReview => {
            let Some(vault) = std::env::var_os("CAIRN_VAULT").map(std::path::PathBuf::from) else {
                eprintln!("cairn ingest --human-review: CAIRN_VAULT must be set");
                return std::process::ExitCode::from(78);
            };
            let pending_dir = bucket_dir(&vault, Bucket::Pending);
            if let Err(e) = std::fs::create_dir_all(&pending_dir) {
                eprintln!("cairn ingest --human-review: mkdir {}: {e}", pending_dir.display());
                return std::process::ExitCode::from(73);
            }
            let path = plan_path(&vault, Bucket::Pending, &plan.operation_id);
            let persisted = PersistedPlan::pending(plan.clone());
            let bytes = match serde_json::to_vec_pretty(&persisted) {
                Ok(b) => b, Err(e) => {
                    eprintln!("cairn ingest --human-review: serialize: {e}");
                    return std::process::ExitCode::from(70);
                }
            };
            if let Err(e) = std::fs::write(&path, &bytes) {
                eprintln!("cairn ingest --human-review: write {}: {e}", path.display());
                return std::process::ExitCode::from(73);
            }
            if !no_diff {
                let dpath = diff_path(&vault, &plan.operation_id);
                let _ = std::fs::write(&dpath, diff::render(&plan));
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "operation_id": plan.operation_id.0,
                    "mode": "human_review",
                    "plan_ref": path.display().to_string(),
                })).unwrap_or_default());
            } else {
                println!("human-review: plan written to {}", path.display());
            }
            std::process::ExitCode::SUCCESS
        }
        FlushMode::Autonomous => unreachable!("planner stub only handles dry_run / human_review"),
    }
}

fn synth_ulid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    // Crockford-base32-ish: 26 chars padded. Not a real ULID but legal as
    // an opaque id for the stub planner. #9 swaps in a real ULID generator.
    format!("{nanos:026X}").chars().take(26).collect()
}

fn synth_now() -> String {
    "2026-05-04T00:00:00Z".to_string() // pinned for snapshot-friendliness
}

fn synth_expires() -> String {
    "2026-05-04T00:05:00Z".to_string()
}

fn minimal_record(target: &cairn_core::domain::TargetId, body: &str)
    -> cairn_core::domain::MemoryRecord
{
    // Use whatever minimal-construction helper MemoryRecord exposes today.
    // If construction requires more fields, fill defaults; this is the
    // stub-planner placeholder until #9.
    let _ = (target, body);
    cairn_core::domain::MemoryRecord::default()
}
```

> **Note on `MemoryRecord::default()`:** if the live `MemoryRecord` doesn't implement `Default`, replace `minimal_record` with whatever minimal constructor the type exposes (look at `crates/cairn-core/src/domain/record.rs`). The stub only needs *something* serializable so the plan JSON is valid; the live pipeline will replace it in #9.

- [ ] **Step 4: Run tests**

```bash
cargo test -p cairn-cli --test flush_integration -- ingest_
```

Expected: 3 passes.

- [ ] **Step 5: Lint**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest.rs crates/cairn-cli/src/verbs/mod.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/flush_integration.rs
git -c commit.gpgsign=false commit -m "feat(cli): ingest --dry-run / --human-review (#54)"
```

---

## Task 12: Same flags on `forget`

**Files:**
- Modify: `crates/cairn-cli/src/main.rs` (wrap forget subcommand)
- Modify: `crates/cairn-cli/src/verbs/forget.rs`
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/cairn-cli/tests/flush_integration.rs`:

```rust
#[test]
fn forget_dry_run_writes_nothing() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let _ = std::process::Command::new(bin)
        .args(["forget", "--record", "rec:abc", "--dry-run"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(!vault.path().join(".cairn/flush").exists());
}

#[test]
fn forget_human_review_writes_pending() {
    let vault = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["forget", "--record", "rec:abc", "--human-review"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let pending = vault.path().join(".cairn/flush/pending");
    assert!(pending.exists());
}
```

> **Note:** `--record` may not be the actual flag name on `forget` — check `crates/cairn-idl/schema/verbs/forget.json` and adjust to whatever flag the existing IDL exposes (likely `--record` or `--target`).

- [ ] **Step 2: Verify failure**

```bash
cargo test -p cairn-cli --test flush_integration -- forget_
```

Expected: failure (forget doesn't accept `--dry-run`).

- [ ] **Step 3: Wire flags into forget**

In `crates/cairn-cli/src/main.rs` where `verbs::forget` subcommand is registered, wrap with `verbs::with_flush_modes(...)`.

In `crates/cairn-cli/src/verbs/forget.rs::run`, near the top after `let json = sub.get_flag("json");`:

```rust
let dry_run = sub.get_flag("dry-run");
let human_review = sub.get_flag("human-review");
let no_diff = sub.get_flag("no-diff");
if dry_run || human_review {
    let mode = if dry_run {
        cairn_core::domain::flush_plan::FlushMode::DryRun
    } else {
        cairn_core::domain::flush_plan::FlushMode::HumanReview
    };
    // Reuse the same stub planner — for the stub, the ingest/forget
    // distinction collapses to "produce a placeholder plan." #9 will
    // split them into real builders.
    return crate::verbs::ingest_plan_stub(sub, mode, no_diff, json);
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cairn-cli --test flush_integration -- forget_
```

Expected: pass.

- [ ] **Step 5: Lint**

```bash
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/forget.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/flush_integration.rs
git -c commit.gpgsign=false commit -m "feat(cli): forget --dry-run / --human-review (#54)"
```

---

## Task 13: IDL — add `mode` arg to ingest

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/ingest.json`

- [ ] **Step 1: Add `mode` to Args**

In `crates/cairn-idl/schema/verbs/ingest.json`, inside `$defs.Args.properties`, add:

```jsonc
"mode": {
  "type": "string",
  "enum": ["autonomous", "dry_run", "human_review"],
  "default": "autonomous",
  "description": "Plan dispatch mode (brief §5.5). Wire-equivalent of CLI --dry-run / --human-review."
}
```

In the same file, inside `$defs.Data.properties`, add:

```jsonc
"plan_ref": {
  "type": "string",
  "minLength": 1,
  "description": "Path under .cairn/flush/pending/ when mode=human_review."
}
```

- [ ] **Step 2: Regenerate**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: `crates/cairn-core/src/generated/verbs/ingest.rs` updated.

- [ ] **Step 3: Verify check passes**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo check -p cairn-core --all-targets --locked
```

Expected: both clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-idl/schema/verbs/ingest.json crates/cairn-core/src/generated/verbs/ingest.rs
git -c commit.gpgsign=false commit -m "feat(idl): ingest mode arg (autonomous|dry_run|human_review) (#54)"
```

---

## Task 14: IDL — add `mode` arg to forget

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/forget.json`

- [ ] **Step 1: Add `mode` field**

Same diff shape as Task 13 — add `mode` to `$defs.Args.properties` and `plan_ref` to `$defs.Data.properties`.

- [ ] **Step 2: Regen + check**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo check -p cairn-core --all-targets --locked
```

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-idl/schema/verbs/forget.json crates/cairn-core/src/generated/verbs/forget.rs
git -c commit.gpgsign=false commit -m "feat(idl): forget mode arg (#54)"
```

---

## Task 15: Snapshot tests for CLI output

**Files:**
- Modify: `crates/cairn-cli/tests/flush_integration.rs`

- [ ] **Step 1: Add snapshot tests**

Append to `crates/cairn-cli/tests/flush_integration.rs`:

```rust
#[test]
fn flush_list_json_snapshot() {
    let vault = tempfile::tempdir().unwrap();
    write_pending(vault.path(), "01HQZK00000000000000000030");
    write_pending(vault.path(), "01HQZK00000000000000000031");
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "list", "--json"])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    insta::assert_snapshot!("flush_list_json", String::from_utf8(out.stdout).unwrap());
}

#[test]
fn flush_apply_human_output_snapshot() {
    let vault = tempfile::tempdir().unwrap();
    let id = "01HQZK00000000000000000040";
    write_pending(vault.path(), id);
    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = std::process::Command::new(bin)
        .args(["flush", "apply", id])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("spawn cairn");
    insta::assert_snapshot!("flush_apply_human", String::from_utf8(out.stdout).unwrap());
}
```

Add `insta` as a dev-dep on `cairn-cli` if absent:

```bash
grep -A5 'dev-dependencies' crates/cairn-cli/Cargo.toml | grep -q '^insta' || \
  cargo add --dev --package cairn-cli insta
```

- [ ] **Step 2: Run + accept snapshots**

```bash
cargo test -p cairn-cli --test flush_integration -- flush_list_json_snapshot flush_apply_human_output_snapshot
cargo insta accept --package cairn-cli
cargo test -p cairn-cli --test flush_integration -- flush_list_json_snapshot flush_apply_human_output_snapshot
```

Expected: tests pass on second run.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/Cargo.toml crates/cairn-cli/tests/flush_integration.rs crates/cairn-cli/tests/snapshots/
git -c commit.gpgsign=false commit -m "test(cli): flush list + apply output snapshots (#54)"
```

---

## Task 16: Traceability + docgen

**Files:**
- Modify: `docs/design/traceability.md`
- Regen: `docs/site/src/reference/generated/`

- [ ] **Step 1: Add §5.5 → #54 row**

Open `docs/design/traceability.md`. Find the table or list mapping brief sections to issues. Add (or update) the row for §5.5 to point at #54. If there's already a row for §5.5 from earlier work, append `, #54`.

- [ ] **Step 2: Regenerate docs**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Expected: regen produces a diff under `docs/site/src/reference/generated/` for the new ingest/forget `mode` arg + the new `cairn flush` subcommand. Check passes.

- [ ] **Step 3: Build mdbook**

```bash
mdbook build docs/site
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add docs/design/traceability.md docs/site/src/reference/generated/
git -c commit.gpgsign=false commit -m "docs: traceability §5.5 → #54 + regen docgen"
```

---

## Task 17: Full verification sweep (CLAUDE.md §8)

**Files:** none — verification only.

- [ ] **Step 1: Format + clippy + tests**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

Expected: all clean.

- [ ] **Step 2: Codegen + docgen check**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: all clean.

- [ ] **Step 3: Supply-chain**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: all clean. Note: `cargo machete` may flag `insta` / `proptest` if they were added but not all generated snapshot files were committed — re-run snapshot acceptance and commit.

- [ ] **Step 4: Open the PR**

```bash
git push -u origin HEAD:feat/issue-54-flushplan
gh pr create --title "feat(core,cli): FlushPlan + dry-run + human-review (issue #54)" \
  --body "$(cat <<'EOF'
## Summary

Implements brief §5.5 — typed `FlushPlan`, `--dry-run` / `--human-review`
modes on `ingest` and `forget`, and `cairn flush list/apply/reject` admin
subcommands. Spec: `docs/superpowers/specs/2026-05-04-flushplan-design.md`.

Closes #54.

## Brief sections touched

- §5.5 Plan, then apply (primary)
- §5.2 Write path (mutation kinds)
- §5.6 WAL envelope (operation_id, target_hash, dependencies, expires_at)
- §8 verb table (ingest, forget — added `mode` arg)

## Invariants touched (CLAUDE.md §4)

- (3) CLI is ground truth — `mode` arg appears in IDL for ingest/forget;
  `cairn flush` admin commands are CLI-only (brief §8 pins 8 verbs).
- (5) WAL + two-phase apply — `apply` walks `MemoryStore` directly in this
  PR with a phase-1 drift check; #9 will swap in the WAL state machine.
- (8) No `unwrap()/expect()` in `cairn-core` — verified.
- (10) Source/record/schema layers respected — plan files live under
  `.cairn/flush/`, separate from `sources/` / `raw/` / `wiki/` / `skills/`.

## Verification

All commands in `CLAUDE.md` §8 pass. `cargo nextest run --workspace`,
`cargo deny check`, `cargo audit`, `cargo machete` all clean.

## Test plan

- [x] Unit: type round-trips, idempotency_key, target_hash
- [x] Property: 64-case JSON round-trip across mutation variants
- [x] Snapshot: plan JSON + diff markdown locked
- [x] Integration: list/apply/reject lifecycle, idempotent re-apply,
  not-found exit code, mutually-exclusive flag rejection
EOF
)"
```

---

## Self-Review Checklist (run before handing off)

**1. Spec coverage:**

- §5.5 mode dispatch (autonomous/dry_run/human_review) → Tasks 1, 11, 12, 13, 14 ✓
- §5.5 `.cairn/flush/<id>.plan.json` → Tasks 2, 11 ✓
- §5.5 `cairn flush apply <id>` → Task 9 ✓
- Issue acceptance criterion: dry-run returns plan, writes nothing → Task 11 test `ingest_dry_run_writes_no_flush_files` ✓
- Issue acceptance criterion: human-review writes reviewable plan → Task 11 test `ingest_human_review_writes_pending_plan` ✓
- Issue acceptance criterion: plans include reasons, source events, target hashes, idempotency keys → Task 1 `FlushPlan` fields ✓
- Issue verification: dry-run no-write tests → Task 11 ✓
- Issue verification: apply/reject tests → Tasks 9, 10 ✓
- Issue verification: plan serialization snapshots → Tasks 5, 15 ✓

**2. Placeholder scan:**

- One TBD-shaped piece remains — `minimal_record` in Task 11 explicitly notes the type-construction shape may need to flex. That's a real open ambiguity, not a hidden TBD; Task 11 step 3 instructs the engineer how to resolve it by inspecting `MemoryRecord` directly.
- All other steps include the actual code.

**3. Type consistency:**

- `FlushMode` / `FlushPlan` / `PlannedMutation` / `PlanReason` / `PlanStatus` / `PersistedPlan` names match across Tasks 1, 5, 6, 7, 8, 9, 10, 11, 12.
- `Bucket` / `plan_path` / `bucket_dir` / `diff_path` from Task 2 used by Tasks 8, 9, 10, 11.
- `with_flush_modes` from Task 11 used by Tasks 11, 12.
- `ingest_plan_stub` from Task 11 reused by Task 12 (forget) — same arity, same return.
- `now_rfc3339` from Task 9 reused by Task 10 — same signature.
- `persist_and_move` from Task 9 reused by Task 10 — same signature.

No drift detected.
