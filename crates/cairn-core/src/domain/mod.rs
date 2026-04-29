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
pub mod body_hash;
pub mod canonical;
pub mod capture;
pub mod capture_attribution;
pub mod capture_manifest;
pub mod error;
pub mod evidence;
pub mod filter;
pub mod folder;
pub mod identity;
pub mod intent;
pub mod projection;
pub mod provenance;
pub mod record;
pub mod scope;
pub mod session;
pub mod target_id;
pub mod taxonomy;
pub mod timestamp;

pub use actor_chain::{ActorChainEntry, ChainRole, validate_chain};
pub use body_hash::BodyHash;
pub use canonical::CanonicalRecordHash;
pub use capture::{
    CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
    SensorLabel, SourceFamily,
};
pub use capture_attribution::attribute;
pub use capture_manifest::{P0_SENSOR_LABEL_PREFIXES, validate_label};
pub use error::DomainError;
pub use evidence::{ConfidenceBand, EvidenceVector};
pub use identity::{Identity, IdentityKind};
pub use intent::VerifiedSignedIntent;
pub use projection::{
    ConflictOutcome, MarkdownProjector, ParsedProjection, ProjectedFile, ResyncError,
};
pub use provenance::Provenance;
pub use record::{MemoryRecord, RecordId};
pub use scope::ScopeTuple;
pub use session::{
    DEFAULT_IDLE_WINDOW_SECS, LastActiveSession, SessionDecision, SessionId, SessionIdentity,
    SessionSource, resolve_session,
};
pub use target_id::TargetId;
pub use taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
pub use timestamp::Rfc3339Timestamp;
