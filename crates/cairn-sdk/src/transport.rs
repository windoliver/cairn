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
use cairn_core::generated::envelope::ResponseVerb;

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
/// Construct with [`Sdk::new`] for the default in-process transport. The
/// client owns one `incarnation` ULID and `started_at` timestamp for its
/// entire lifetime — every [`Sdk::status`] call returns the same values
/// (brief §8.0.a wire-compat: `status` is byte-stable across an
/// incarnation), and capability gating reads from this stable snapshot
/// rather than minting a fresh one per call.
///
/// Every verb fn returns either a typed [`VerbResponse`] or an
/// [`SdkError`]; no CLI parsing required.
#[derive(Debug, Clone)]
pub struct Sdk<T: Transport = InProcess> {
    _transport: T,
    incarnation: cairn_core::generated::common::Ulid,
    started_at: String,
}

impl Default for Sdk<InProcess> {
    fn default() -> Self {
        Self::new()
    }
}

impl Sdk<InProcess> {
    /// Construct an in-process SDK client. Mints the incarnation ULID and
    /// `started_at` timestamp once; both are stable for the client's
    /// lifetime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            _transport: InProcess,
            incarnation: crate::stub::new_operation_id(),
            started_at: now_rfc3339_seconds(),
        }
    }
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
    /// info. The `incarnation` and `started_at` fields are minted once
    /// when the client is constructed and remain stable for its lifetime,
    /// so consumers can correlate operation IDs against a single
    /// incarnation snapshot.
    #[must_use]
    pub fn status(&self) -> StatusResponse {
        StatusResponse {
            contract: CONTRACT.to_owned(),
            server_info: StatusResponseServerInfo {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                build: build_profile(),
                started_at: self.started_at.clone(),
                incarnation: self.incarnation.clone(),
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
        Err(stub(ResponseVerb::Ingest))
    }

    /// `search` — hybrid keyword/semantic retrieval (brief §8.2).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the requested mode's capability must
    /// be advertised by [`Self::status`], otherwise the call is rejected
    /// with [`SdkError::CapabilityUnavailable`] before any dispatch.
    pub fn search(&self, args: &SearchArgs) -> Result<VerbResponse<SearchData>, SdkError> {
        revalidate::<SearchArgs>(args)?;
        self.require_capability(args.mode.capability())?;
        Err(stub(ResponseVerb::Search))
    }

    /// `retrieve` — by-target fetch (record/session/turn/folder/scope/profile).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn retrieve(&self, args: &RetrieveArgs) -> Result<VerbResponse<RetrieveData>, SdkError> {
        revalidate::<RetrieveArgs>(args)?;
        self.require_capability(args.capability())?;
        Err(stub(ResponseVerb::Retrieve))
    }

    /// `summarize` — rolling/periodic summary build (brief §8.4).
    pub fn summarize(&self, args: &SummarizeArgs) -> Result<VerbResponse<SummarizeData>, SdkError> {
        revalidate::<SummarizeArgs>(args)?;
        Err(stub(ResponseVerb::Summarize))
    }

    /// `assemble_hot` — hot-memory prefix assembly (brief §8.5, §11).
    pub fn assemble_hot(
        &self,
        args: &AssembleHotArgs,
    ) -> Result<VerbResponse<AssembleHotData>, SdkError> {
        revalidate::<AssembleHotArgs>(args)?;
        Err(stub(ResponseVerb::AssembleHot))
    }

    /// `capture_trace` — accept signed trace events (brief §8.6).
    pub fn capture_trace(
        &self,
        args: &CaptureTraceArgs,
    ) -> Result<VerbResponse<CaptureTraceData>, SdkError> {
        revalidate::<CaptureTraceArgs>(args)?;
        Err(stub(ResponseVerb::CaptureTrace))
    }

    /// `lint` — privacy / provenance / schema / policy drift checks (brief §8.7).
    pub fn lint(&self, args: &LintArgs) -> Result<VerbResponse<LintData>, SdkError> {
        revalidate::<LintArgs>(args)?;
        Err(stub(ResponseVerb::Lint))
    }

    /// `forget` — record/session/scope tombstone + purge (brief §8.8, §5.6).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn forget(&self, args: &ForgetArgs) -> Result<VerbResponse<ForgetData>, SdkError> {
        revalidate::<ForgetArgs>(args)?;
        self.require_capability(args.capability())?;
        Err(stub(ResponseVerb::Forget))
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

/// Canonical P0 stub: every verb returns `Internal — store not wired`.
/// Replace each call site with real dispatch when verb handlers land (#9).
fn stub(_verb: ResponseVerb) -> SdkError {
    store_not_wired()
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
