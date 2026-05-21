//! Cairn domain types — the typed `MemoryRecord` model and its supporting
//! taxonomy, identity, scope, provenance, evidence, and actor-chain types.
//!
//! Brief sources:
//! - §4.2 Identity — agents, sensors, actor chains
//! - §6 Taxonomy — kind × class × visibility × scope
//! - §6.4 `ConfidenceBand` + Evidence Vector
//! - §6.5 Provenance (mandatory on every record)
//!
//! These types are pure data with `serde` derive. They have no I/O, no
//! adapter dependency, and no async; the [`MemoryRecord::validate`] entry
//! point is what every adapter calls before any persistence side effect.
//!
//! Serialization is stable across three call sites:
//! - **API envelopes** → `serde_json` (the wire format).
//! - **`SQLite` row JSON columns** → same `serde_json` representation.
//! - **Markdown frontmatter** → YAML; field names and shapes match the
//!   JSON form so a YAML projector reuses the same `serde` derive.

pub mod actor_chain;
pub mod admission;
pub mod backup;
pub mod body_hash;
pub mod canonical;
pub mod capture;
pub mod capture_attribution;
pub mod capture_manifest;
pub mod consent;
pub mod consent_timeline;
pub mod error;
pub mod evidence;
pub mod filter;
pub mod flush_plan;
pub mod folder;
pub mod graph;
pub mod hot_prefix;
pub mod identity;
pub mod intent;
pub mod metrics;
pub mod projection;
pub mod provenance;
pub mod record;
pub mod scope;
pub mod sensor_policy;
pub mod session;
pub mod session_tree;
pub mod sharing;
pub mod source_id;
pub mod source_ref;
pub mod sre;
pub mod target_id;
pub mod taxonomy;
pub mod time;
pub mod timestamp;
pub mod trace;
pub mod tree_read_window;
pub mod zero_capture;

pub use actor_chain::{ActorChainEntry, ChainRole, validate_chain};
pub use admission::{AdmissionError, SignedAdmission, WalActionKind};
pub use backup::{BackupRegistryEntry, RewritePlan, ShreddedBackupEntry};
pub use body_hash::BodyHash;
pub use canonical::CanonicalRecordHash;
pub use capture::{
    CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
    SensorLabel, SourceFamily, TerminalContext,
};
pub use capture_attribution::attribute;
pub use capture_manifest::{P0_SENSOR_LABEL_PREFIXES, validate_label};
pub use consent::{ConsentEvent, ConsentEventError, ConsentKind, ConsentPayload};
pub use consent_timeline::{
    ConsentModel, ConsentTimelineEvent, ConsentTimelineEventKind, CoveringGrant,
};
pub use error::DomainError;
pub use evidence::{ConfidenceBand, EvidenceVector};
pub use flush_plan::{
    ApplyKind, ExpirationReason, FlushMode, FlushPlan, PersistedPlan, PlanReason, PlanStatus,
    PlannedMutation,
};
pub use identity::{Identity, IdentityKind};
pub use intent::VerifiedSignedIntent;
pub use projection::{
    ConflictOutcome, MarkdownProjector, ParsedProjection, ParserProjectionKind, ProjectedFile,
    ProjectionCursor, ProjectionItemState, ProjectionLedgerRow, ProjectionSummary,
    ProjectionTarget, ResyncError,
};
pub use provenance::Provenance;
pub use record::{Ed25519Signature, MemoryRecord, RecordId};
pub use scope::ScopeTuple;
pub use sensor_policy::{BudgetObservation, LocalSensorName, SensorGateReason};
pub use session::{
    DEFAULT_IDLE_WINDOW_SECS, LastActiveSession, Session, SessionDecision, SessionId,
    SessionIdentity, SessionSource, resolve_session,
};
pub use session_tree::{
    BranchKind, MergeStrategy, SessionMerge, SessionParent, SessionTree, SessionTreeError,
    SessionTreeNode,
};
pub use sharing::{
    PromotionConsentPayload, PromotionConsentReceipt, PromotionGateInput, ShareLinkGateInput,
    ShareLinkJournalDecision, ShareLinkPayload, SharingDecisionKind, SharingGateRejection,
    SharingPolicyAction, SharingPolicySubject, SharingRevocationState, SignedShareLink,
    verify_promotion_gate, verify_share_link_grant,
};
pub use source_id::SourceId;
pub use source_ref::{SourceRef, validate_source_refs};
pub use sre::{
    SreDetail, SreGateResult, SreGateSummary, SreMeasurement, SrePrivacySummary,
    SreProjectionSummary, SreProjectionTargetSummary, SreRehydrationSummary, SreReport,
    SreSearchModeSummary, SreSearchSummary, SreStatus, SreVaultSummary, SreWorkflowKindSummary,
    SreWorkflowSummary, classify_count_status, classify_threshold, scrub_detail,
};
pub use target_id::TargetId;
pub use taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
pub use time::{Clock, SystemClock};
pub use timestamp::Rfc3339Timestamp;
pub use trace::{TraceBlock, TraceEvent, TraceLink, TraceLinkError};
pub use tree_read_window::{
    TreeBudgetReport, TreeReadRecord, TreeReadSegmentKind, TreeReadSelection, TreeReadWindow,
    TreeReadWindowError, TreeReadWindowInput, plan_tree_read_window,
};
pub use zero_capture::{
    ZeroCaptureAuditInput, ZeroCaptureDecision, ZeroCaptureDecisionCode, ZeroCaptureNudge,
    ZeroCaptureReport, ZeroCaptureReportSummary, ZeroCaptureSuppression, ZeroCaptureTrigger,
    decide_zero_capture_nudge, render_zero_capture_report,
};
