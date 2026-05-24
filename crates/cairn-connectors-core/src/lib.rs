//! Cairn connector framework — substrate that lets adapter crates
//! emit `CaptureEvent`s through the cairn-core pipeline behind a
//! validated, redacted, consent-gated boundary.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.
//!
//! # Status
//! Scaffold with T5–T8 modules landed. Remaining modules (T9–T16) are added
//! task-by-task; their re-exports are uncommented as each task lands.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod connector;
pub mod credential;
pub mod error;
pub mod event;
pub mod manifest;
pub mod rate_limit;
pub mod redact;

pub use connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
    CONTRACT_VERSION,
};
pub use credential::{CredentialHandle, CredentialStore, InMemoryCredentialStore};
pub use error::ConnectorError;
pub use event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
pub use manifest::ConnectorManifest;
pub use rate_limit::RateLimit;
pub use redact::{Redacted, RedactionPipeline};
