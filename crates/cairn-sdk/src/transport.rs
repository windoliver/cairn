//! In-process SDK transport.
//!
//! Each verb fn is the SDK-side mirror of the CLI handler in
//! `cairn-cli::verbs::*`. The SDK does no I/O of its own — it constructs
//! the same request envelope the CLI would and dispatches into the verb
//! layer. P0 verb handlers are stubs (#9) so each fn returns the canonical
//! `store not wired` [`SdkError::Internal`] with a fresh operation ID.
//!
//! When verb handlers move into `cairn-core::verbs::*`, replace each `Err(...)`
//! arm with the dispatch into the shared handler.

use cairn_core::generated::common::Capabilities;
use cairn_core::generated::handshake::{HandshakeResponse, HandshakeResponseChallenge};
use cairn_core::generated::status::{StatusResponse, StatusResponseServerInfo};
use cairn_core::generated::verbs::{
    assemble_hot::{AssembleHotArgs, AssembleHotData},
    capture_trace::{CaptureTraceArgs, CaptureTraceData},
    forget::{ForgetArgs, ForgetData},
    ingest::{IngestArgs, IngestData},
    lint::{LintArgs, LintData},
    retrieve::{RetrieveArgs, RetrieveData},
    search::{SearchArgs, SearchData},
    summarize::{SummarizeArgs, SummarizeData},
};

use crate::stub::{new_nonce, now_ms, now_rfc3339_seconds, store_not_wired};
use crate::{CONTRACT, SdkError, VerbResponse};

/// Marker trait for transport implementations.
///
/// P0 ships [`InProcess`] only. A `Subprocess` transport (forking the
/// `cairn` CLI and parsing its `--json` output) is tracked as a follow-up
/// once the verb handlers actually do work.
pub trait Transport: sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
}

/// In-process transport: SDK calls dispatch directly into the verb layer
/// inside the same process. Zero-copy, no IPC, no daemon. The default
/// [`Sdk`] uses this transport.
#[derive(Debug, Default, Clone, Copy)]
pub struct InProcess;

impl sealed::Sealed for InProcess {}
impl Transport for InProcess {}

/// SDK client.
///
/// Construct with [`Sdk::new`] for the default in-process transport. Every
/// verb fn returns either a typed [`VerbResponse`] or an [`SdkError`]; no
/// CLI parsing required.
///
/// `status.server_info.incarnation` and `started_at` come from a
/// **process-wide** snapshot (a [`std::sync::OnceLock`] inside the crate),
/// not from the client instance. Two `Sdk` handles in the same process
/// therefore report the same incarnation, so callers using
/// `incarnation` for cache invalidation or restart detection see real
/// process restarts only — never spurious churn from re-instantiating
/// the SDK.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sdk<T: Transport = InProcess> {
    _transport: T,
}

impl Sdk<InProcess> {
    /// Construct an in-process SDK client.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            _transport: InProcess,
        }
    }
}

/// Process-wide incarnation snapshot. Initialized lazily on first
/// [`Sdk::status`] call and stable for the lifetime of the process.
fn process_incarnation() -> &'static (cairn_core::generated::common::Ulid, String) {
    static SNAPSHOT: std::sync::OnceLock<(cairn_core::generated::common::Ulid, String)> =
        std::sync::OnceLock::new();
    SNAPSHOT.get_or_init(|| (crate::stub::new_operation_id(), now_rfc3339_seconds()))
}

impl<T: Transport> Sdk<T> {
    /// SDK / contract version. Mirrors `status().server_info.version`.
    #[must_use]
    pub const fn version(&self) -> &'static str {
        crate::version()
    }

    /// `status` — capability discovery (brief §8.0.a).
    ///
    /// Returns the contract version, advertised capabilities, and server
    /// info. `incarnation` and `started_at` come from a process-wide
    /// snapshot — every `Sdk` instance in the same process reports
    /// identical values, so the field correctly identifies the embedded
    /// service lifecycle rather than the client object.
    #[must_use]
    pub fn status(&self) -> StatusResponse {
        let (incarnation, started_at) = process_incarnation();
        StatusResponse {
            contract: CONTRACT.to_owned(),
            server_info: StatusResponseServerInfo {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                build: build_profile(),
                started_at: started_at.clone(),
                incarnation: incarnation.clone(),
            },
            capabilities: p0_capabilities(),
            extensions: vec![],
        }
    }

    /// `handshake` — challenge mint (brief §8.0.a point d).
    ///
    /// P0 caveat: the issued nonce is ephemeral and is not persisted. Same
    /// caveat as `cairn handshake` — challenge-mode signed intents will be
    /// rejected until the store lands.
    #[must_use]
    pub fn handshake(&self) -> HandshakeResponse {
        const CHALLENGE_TTL_MS: u64 = 60_000;
        HandshakeResponse {
            contract: CONTRACT.to_owned(),
            challenge: HandshakeResponseChallenge {
                nonce: new_nonce(),
                expires_at: now_ms() + CHALLENGE_TTL_MS,
            },
        }
    }

    /// `ingest` — accept new memory (brief §8.1).
    pub fn ingest(&self, args: &IngestArgs) -> Result<VerbResponse<IngestData>, SdkError> {
        validate_ingest(args)?;
        revalidate::<IngestArgs>(args)?;
        Err(unimplemented("ingest"))
    }

    /// `search` — hybrid keyword/semantic retrieval (brief §8.2).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the requested mode's capability must
    /// be advertised by [`Self::status`], otherwise the call is rejected
    /// with [`SdkError::CapabilityUnavailable`] before any dispatch.
    pub fn search(&self, args: &SearchArgs) -> Result<VerbResponse<SearchData>, SdkError> {
        revalidate::<SearchArgs>(args)?;
        self.require_capability(args.mode.capability())?;
        Err(unimplemented("search"))
    }

    /// `retrieve` — by-target fetch (record/session/turn/folder/scope/profile).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn retrieve(&self, args: &RetrieveArgs) -> Result<VerbResponse<RetrieveData>, SdkError> {
        revalidate::<RetrieveArgs>(args)?;
        self.require_capability(args.capability())?;
        Err(unimplemented("retrieve"))
    }

    /// `summarize` — rolling/periodic summary build (brief §8.4).
    pub fn summarize(&self, args: &SummarizeArgs) -> Result<VerbResponse<SummarizeData>, SdkError> {
        revalidate::<SummarizeArgs>(args)?;
        Err(unimplemented("summarize"))
    }

    /// `assemble_hot` — hot-memory prefix assembly (brief §8.5, §11).
    pub fn assemble_hot(
        &self,
        args: &AssembleHotArgs,
    ) -> Result<VerbResponse<AssembleHotData>, SdkError> {
        revalidate::<AssembleHotArgs>(args)?;
        Err(unimplemented("assemble_hot"))
    }

    /// `capture_trace` — accept signed trace events (brief §8.6).
    pub fn capture_trace(
        &self,
        args: &CaptureTraceArgs,
    ) -> Result<VerbResponse<CaptureTraceData>, SdkError> {
        revalidate::<CaptureTraceArgs>(args)?;
        Err(unimplemented("capture_trace"))
    }

    /// `lint` — privacy / provenance / schema / policy drift checks (brief §8.7).
    pub fn lint(&self, args: &LintArgs) -> Result<VerbResponse<LintData>, SdkError> {
        revalidate::<LintArgs>(args)?;
        Err(unimplemented("lint"))
    }

    /// `forget` — record/session/scope tombstone + purge (brief §8.8, §5.6).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn forget(&self, args: &ForgetArgs) -> Result<VerbResponse<ForgetData>, SdkError> {
        revalidate::<ForgetArgs>(args)?;
        self.require_capability(args.capability())?;
        Err(unimplemented("forget"))
    }

    /// Reject with [`SdkError::CapabilityUnavailable`] when `required` is
    /// not advertised by `status()`. Verbs whose IDL declares no
    /// capability (`None`) are unconditionally allowed.
    fn require_capability(&self, required: Option<&'static str>) -> Result<(), SdkError> {
        let Some(cap) = required else {
            return Ok(());
        };
        let advertised = self.status().capabilities;
        let is_advertised = advertised.iter().any(|c| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .as_deref()
                == Some(cap)
        });
        if is_advertised {
            Ok(())
        } else {
            Err(SdkError::CapabilityUnavailable {
                capability: cap.to_owned(),
                reason: "not advertised by `status` in this incarnation".to_owned(),
                operation_id: crate::stub::new_operation_id(),
            })
        }
    }
}

/// `IngestArgs` has an explicit IDL `validate()` for its exactly-one-of group.
fn validate_ingest(args: &IngestArgs) -> Result<(), SdkError> {
    args.validate().map_err(|reason| SdkError::InvalidArgs {
        reason: reason.to_owned(),
    })
}

/// Run the same constraint checks the wire format runs (non-empty strings,
/// numeric ranges, oneOf groups, etc.) by serializing the typed arg and
/// re-deserializing it through the generated `TryFrom<Raw...>` path.
///
/// Direct construction in Rust skips those checks; this brings SDK callers
/// back onto the same validation as the CLI/MCP surfaces, surfaced as
/// [`SdkError::InvalidArgs`].
fn revalidate<T>(args: &T) -> Result<(), SdkError>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_value(args).map_err(|err| SdkError::InvalidArgs {
        reason: format!("non-serializable args: {err}"),
    })?;
    serde_json::from_value::<T>(json)
        .map(|_| ())
        .map_err(|err| SdkError::InvalidArgs {
            reason: err.to_string(),
        })
}

/// Canonical P0 stub: every verb returns [`SdkError::Unimplemented`] until
/// verb dispatch lands (#9). Distinct from a generic `Internal` so callers
/// can fail fast instead of retrying.
fn unimplemented(verb: &'static str) -> SdkError {
    store_not_wired(verb)
}

fn p0_capabilities() -> Vec<Capabilities> {
    // P0 advertises no capabilities — the store adapter is not wired yet.
    // Mirrors `cairn-cli::verbs::status::p0_capabilities`.
    vec![]
}

fn build_profile() -> String {
    if cfg!(debug_assertions) {
        "debug".to_owned()
    } else {
        "release".to_owned()
    }
}
