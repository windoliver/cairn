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
/// Construct with [`Sdk::new`] for the default in-process transport. Every
/// verb fn returns either a typed [`VerbResponse`] or an [`SdkError`]; no
/// CLI parsing required.
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

impl<T: Transport> Sdk<T> {
    /// SDK / contract version. Mirrors `status().server_info.version`.
    #[must_use]
    pub const fn version(&self) -> &'static str {
        crate::version()
    }

    /// `status` — capability discovery (brief §8.0.a).
    ///
    /// Returns the contract version, advertised capabilities, and server
    /// info. Byte-for-byte parity with `cairn status --json` (modulo
    /// `started_at` and `incarnation`, which are minted per call by design
    /// — P0 has no daemon).
    #[must_use]
    pub fn status(&self) -> StatusResponse {
        let started_at = now_rfc3339_seconds();
        StatusResponse {
            contract: CONTRACT.to_owned(),
            server_info: StatusResponseServerInfo {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                build: build_profile(),
                started_at,
                incarnation: crate::stub::new_operation_id(),
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
        validate(args.validate())?;
        Err(stub(ResponseVerb::Ingest))
    }

    /// `search` — hybrid keyword/semantic retrieval (brief §8.2).
    pub fn search(&self, _args: &SearchArgs) -> Result<VerbResponse<SearchData>, SdkError> {
        Err(stub(ResponseVerb::Search))
    }

    /// `retrieve` — by-target fetch (record/session/turn/folder/scope/profile).
    pub fn retrieve(&self, _args: &RetrieveArgs) -> Result<VerbResponse<RetrieveData>, SdkError> {
        // RetrieveArgs is a tagged enum; structural validation happens at
        // deserialization. No additional pre-dispatch validate() exists.
        Err(stub(ResponseVerb::Retrieve))
    }

    /// `summarize` — rolling/periodic summary build (brief §8.4).
    pub fn summarize(
        &self,
        _args: &SummarizeArgs,
    ) -> Result<VerbResponse<SummarizeData>, SdkError> {
        Err(stub(ResponseVerb::Summarize))
    }

    /// `assemble_hot` — hot-memory prefix assembly (brief §8.5, §11).
    pub fn assemble_hot(
        &self,
        _args: &AssembleHotArgs,
    ) -> Result<VerbResponse<AssembleHotData>, SdkError> {
        Err(stub(ResponseVerb::AssembleHot))
    }

    /// `capture_trace` — accept signed trace events (brief §8.6).
    pub fn capture_trace(
        &self,
        _args: &CaptureTraceArgs,
    ) -> Result<VerbResponse<CaptureTraceData>, SdkError> {
        Err(stub(ResponseVerb::CaptureTrace))
    }

    /// `lint` — privacy / provenance / schema / policy drift checks (brief §8.7).
    pub fn lint(&self, _args: &LintArgs) -> Result<VerbResponse<LintData>, SdkError> {
        Err(stub(ResponseVerb::Lint))
    }

    /// `forget` — record/session/scope tombstone + purge (brief §8.8, §5.6).
    pub fn forget(&self, _args: &ForgetArgs) -> Result<VerbResponse<ForgetData>, SdkError> {
        // ForgetArgs is a tagged enum. See `retrieve` rationale.
        Err(stub(ResponseVerb::Forget))
    }
}

/// Map the IDL `validate()` result into [`SdkError::InvalidArgs`].
fn validate(result: Result<(), &'static str>) -> Result<(), SdkError> {
    match result {
        Ok(()) => Ok(()),
        Err(reason) => Err(SdkError::InvalidArgs {
            reason: reason.to_owned(),
        }),
    }
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
