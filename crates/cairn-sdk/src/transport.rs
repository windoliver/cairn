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
#[derive(Debug, Clone, Copy)]
pub struct Sdk<T: Transport = InProcess> {
    _transport: T,
}

impl Sdk<InProcess> {
    /// Construct an in-process SDK client.
    ///
    /// The first `Sdk::new()` call in a process primes the
    /// process-wide incarnation snapshot, so `started_at` reflects when
    /// the SDK service started in this process rather than when something
    /// happened to call [`Sdk::status`] for the first time.
    #[must_use]
    pub fn new() -> Self {
        // Prime the snapshot so `started_at` is bound to client construction,
        // not to the first `status()` call. Subsequent constructions see the
        // already-initialized OnceLock and are no-ops.
        let _ = process_incarnation();
        Self {
            _transport: InProcess,
        }
    }
}

impl Default for Sdk<InProcess> {
    fn default() -> Self {
        Self::new()
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
        Err(unimplemented("ingest"))
    }

    /// `search` — hybrid keyword/semantic retrieval (brief §8.2).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the requested mode's capability must
    /// be advertised by [`Self::status`], otherwise the call is rejected
    /// with [`SdkError::CapabilityUnavailable`] before any dispatch.
    pub fn search(&self, args: &SearchArgs) -> Result<VerbResponse<SearchData>, SdkError> {
        validate_search(args)?;
        self.require_capability(args.mode.capability())?;
        Err(unimplemented("search"))
    }

    /// `retrieve` — by-target fetch (record/session/turn/folder/scope/profile).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn retrieve(&self, args: &RetrieveArgs) -> Result<VerbResponse<RetrieveData>, SdkError> {
        validate_retrieve(args)?;
        self.require_capability(args.capability())?;
        Err(unimplemented("retrieve"))
    }

    /// `summarize` — rolling/periodic summary build (brief §8.4).
    pub fn summarize(&self, args: &SummarizeArgs) -> Result<VerbResponse<SummarizeData>, SdkError> {
        validate_summarize(args)?;
        Err(unimplemented("summarize"))
    }

    /// `assemble_hot` — hot-memory prefix assembly (brief §8.5, §11).
    pub fn assemble_hot(
        &self,
        args: &AssembleHotArgs,
    ) -> Result<VerbResponse<AssembleHotData>, SdkError> {
        validate_assemble_hot(args)?;
        Err(unimplemented("assemble_hot"))
    }

    /// `capture_trace` — accept signed trace events (brief §8.6).
    pub fn capture_trace(
        &self,
        args: &CaptureTraceArgs,
    ) -> Result<VerbResponse<CaptureTraceData>, SdkError> {
        validate_capture_trace(args)?;
        Err(unimplemented("capture_trace"))
    }

    /// `lint` — privacy / provenance / schema / policy drift checks (brief §8.7).
    ///
    /// No pre-dispatch validation: `LintArgs` has no schema constraints
    /// beyond optional `write_report: bool`.
    pub fn lint(&self, _args: &LintArgs) -> Result<VerbResponse<LintData>, SdkError> {
        Err(unimplemented("lint"))
    }

    /// `forget` — record/session/scope tombstone + purge (brief §8.8, §5.6).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn forget(&self, args: &ForgetArgs) -> Result<VerbResponse<ForgetData>, SdkError> {
        validate_forget(args)?;
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

/// Wrap a `&'static str` from a hand-rolled validator into [`SdkError::InvalidArgs`].
fn invalid(reason: &'static str) -> SdkError {
    SdkError::InvalidArgs {
        reason: reason.to_owned(),
    }
}

/// Validate a [`Ulid`] newtype against the same rules the generated
/// `Deserialize` enforces (26 chars, Crockford base32 alphabet). Direct
/// construction in Rust skips those checks; the SDK reapplies them so
/// malformed IDs cannot cross the boundary.
fn validate_ulid(id: &cairn_core::generated::common::Ulid) -> Result<(), SdkError> {
    if id.0.len() != 26 {
        return Err(invalid("ULID: must be 26 chars"));
    }
    if !id
        .0
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'))
    {
        return Err(invalid(
            "ULID: must be Crockford base32 (uppercase, no I/L/O/U)",
        ));
    }
    Ok(())
}

/// Validate a [`Cursor`] against `Cursor::Deserialize` rules
/// (non-empty, ≤ 512 chars).
fn validate_cursor(cursor: &cairn_core::generated::common::Cursor) -> Result<(), SdkError> {
    if cursor.0.is_empty() {
        return Err(invalid("Cursor: must not be empty"));
    }
    if cursor.0.len() > 512 {
        return Err(invalid("Cursor: must be <= 512 chars"));
    }
    Ok(())
}

/// Validate a [`ScopeFilter`] against the generated `TryFrom<RawScopeFilter>`
/// rules (at least one predicate present, no empty strings or empty arrays).
fn validate_scope_filter(
    scope: &cairn_core::generated::common::ScopeFilter,
) -> Result<(), SdkError> {
    let any_present = scope.user.is_some()
        || scope.agent.is_some()
        || scope.tenant.is_some()
        || scope.workspace.is_some()
        || scope.entity.is_some()
        || scope.tier.is_some()
        || scope.session_id.is_some()
        || scope.kind.is_some()
        || scope.tags.is_some()
        || scope.record_ids.is_some();
    if !any_present {
        return Err(invalid(
            "scope: at least one of [user, agent, tenant, workspace, entity, tier, session_id, kind, tags, record_ids] is required",
        ));
    }
    let nonempty = |opt: Option<&String>, msg: &'static str| -> Result<(), SdkError> {
        if let Some(v) = opt
            && v.is_empty()
        {
            return Err(invalid(msg));
        }
        Ok(())
    };
    nonempty(scope.user.as_ref(), "user: must not be empty")?;
    nonempty(scope.agent.as_ref(), "agent: must not be empty")?;
    nonempty(scope.tenant.as_ref(), "tenant: must not be empty")?;
    nonempty(scope.workspace.as_ref(), "workspace: must not be empty")?;
    nonempty(scope.entity.as_ref(), "entity: must not be empty")?;
    nonempty(scope.session_id.as_ref(), "session_id: must not be empty")?;
    if let Some(kinds) = &scope.kind {
        if kinds.is_empty() {
            return Err(invalid("kind: must contain at least one item"));
        }
        for k in kinds {
            if k.is_empty() {
                return Err(invalid("kind: items must not be empty"));
            }
        }
    }
    if let Some(tags) = &scope.tags {
        if tags.is_empty() {
            return Err(invalid("tags: must contain at least one item"));
        }
        for t in tags {
            if t.is_empty() {
                return Err(invalid("tags: items must not be empty"));
            }
        }
    }
    if let Some(ids) = &scope.record_ids {
        if ids.is_empty() {
            return Err(invalid("record_ids: must contain at least one item"));
        }
        for id in ids {
            validate_ulid(id)?;
        }
    }
    Ok(())
}

/// `IngestArgs` has an explicit IDL `validate()` for its exactly-one-of group.
fn validate_ingest(args: &IngestArgs) -> Result<(), SdkError> {
    args.validate().map_err(invalid)
}

/// Mirrors the wire constraints from `SearchArgs`'s generated
/// `TryFrom<RawSearchArgs>` plus nested type validators (`Cursor`,
/// `ScopeFilter`) — direct Rust construction bypasses the deserializer for
/// these inner types too.
fn validate_search(args: &SearchArgs) -> Result<(), SdkError> {
    if args.query.is_empty() {
        return Err(invalid("query: must not be empty"));
    }
    if let Some(lim) = args.limit
        && !(1..=1000).contains(&lim)
    {
        return Err(invalid("limit: must be in [1, 1000]"));
    }
    if let Some(cursor) = &args.cursor {
        validate_cursor(cursor)?;
    }
    if let Some(scope) = &args.scope {
        validate_scope_filter(scope)?;
    }
    Ok(())
}

/// Mirrors every wire constraint from `RetrieveArgs`'s generated
/// `TryFrom<RawRetrieveArgs>` — Record (no constraints), Session, Turn,
/// Folder, Scope, and Profile.
fn validate_retrieve(args: &RetrieveArgs) -> Result<(), SdkError> {
    use cairn_core::generated::verbs::retrieve::RetrieveArgs as A;
    match args {
        A::Record { id } => validate_ulid(id),
        A::Session {
            session_id,
            limit,
            include,
            cursor,
            ..
        } => {
            if let Some(c) = cursor {
                validate_cursor(c)?;
            }
            if session_id.is_empty() {
                return Err(invalid("session_id: must not be empty"));
            }
            if let Some(lim) = *limit
                && !(1..=10000).contains(&lim)
            {
                return Err(invalid("limit: must be in [1, 10000]"));
            }
            if let Some(inc) = include {
                if inc.is_empty() {
                    return Err(invalid("include: must contain at least one item"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for item in inc {
                    if !seen.insert(*item as u8) {
                        return Err(invalid("include: items must be unique"));
                    }
                }
            }
            Ok(())
        }
        A::Turn {
            session_id,
            include,
            ..
        } => {
            if session_id.is_empty() {
                return Err(invalid("session_id: must not be empty"));
            }
            if let Some(inc) = include {
                if inc.is_empty() {
                    return Err(invalid("include: must contain at least one item"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for item in inc {
                    if !seen.insert(*item as u8) {
                        return Err(invalid("include: items must be unique"));
                    }
                }
            }
            Ok(())
        }
        A::Folder { path, depth } => {
            if path.is_empty() {
                return Err(invalid("path: must not be empty"));
            }
            if let Some(d) = *depth
                && d > 16
            {
                return Err(invalid("depth: must be in [0, 16]"));
            }
            Ok(())
        }
        A::Scope { cursor, scope } => {
            // Note: A::Scope.cursor is Option<String>, not the Cursor newtype
            // (the IDL passes the raw cursor here). Mirror the inline string
            // checks the generated TryFrom enforces.
            if let Some(c) = cursor {
                if c.is_empty() {
                    return Err(invalid("cursor: must not be empty"));
                }
                if c.len() > 512 {
                    return Err(invalid("cursor: must be <= 512 chars"));
                }
            }
            validate_scope_filter(scope)
        }
        A::Profile { user, agent } => {
            if user.is_none() && agent.is_none() {
                return Err(invalid("at least one of [user, agent] is required"));
            }
            if let Some(u) = user
                && u.is_empty()
            {
                return Err(invalid("user: must not be empty"));
            }
            if let Some(a) = agent
                && a.is_empty()
            {
                return Err(invalid("agent: must not be empty"));
            }
            Ok(())
        }
        // Forward-compat: RetrieveArgs is #[non_exhaustive]; reject unknown
        // future variants rather than silently accept them.
        _ => Err(invalid("unsupported retrieve target variant")),
    }
}

/// Mirrors the JSON-schema constraints for `summarize` (the generated
/// Rust `SummarizeArgs` only enforces `deny_unknown_fields`, but the wire
/// schema requires non-empty `record_ids` and non-empty `kind` if
/// present). Each `record_id` is also validated as a real ULID.
fn validate_summarize(args: &SummarizeArgs) -> Result<(), SdkError> {
    if args.record_ids.is_empty() {
        return Err(invalid("record_ids: must contain at least one item"));
    }
    for id in &args.record_ids {
        validate_ulid(id)?;
    }
    if let Some(kind) = &args.kind
        && kind.is_empty()
    {
        return Err(invalid("kind: must not be empty when present"));
    }
    Ok(())
}

/// Mirrors the JSON-schema constraints for `assemble_hot`: `budget`
/// in `[0, 4 MiB]`, `session_id` non-empty when present.
fn validate_assemble_hot(args: &AssembleHotArgs) -> Result<(), SdkError> {
    if let Some(budget) = args.budget
        && budget > 4_194_304
    {
        return Err(invalid("budget: must be <= 4194304 (4 MiB)"));
    }
    if let Some(session_id) = &args.session_id
        && session_id.is_empty()
    {
        return Err(invalid("session_id: must not be empty when present"));
    }
    Ok(())
}

/// Mirrors the JSON-schema constraints for `capture_trace`: `from`
/// non-empty (required), `session_id` non-empty when present.
fn validate_capture_trace(args: &CaptureTraceArgs) -> Result<(), SdkError> {
    if args.from.is_empty() {
        return Err(invalid("from: must not be empty"));
    }
    if let Some(session_id) = &args.session_id
        && session_id.is_empty()
    {
        return Err(invalid("session_id: must not be empty when present"));
    }
    Ok(())
}

/// Mirrors every wire constraint in `ForgetArgs`'s generated
/// `TryFrom<RawForgetArgs>` plus nested type validators (`Ulid`, `ScopeFilter`).
fn validate_forget(args: &ForgetArgs) -> Result<(), SdkError> {
    use cairn_core::generated::verbs::forget::ForgetArgs as F;
    match args {
        F::Record { record_id } => validate_ulid(record_id),
        F::Session { session_id } => {
            if session_id.is_empty() {
                return Err(invalid("session_id: must not be empty"));
            }
            Ok(())
        }
        F::Scope { scope } => validate_scope_filter(scope),
        // Forward-compat for #[non_exhaustive].
        _ => Err(invalid("unsupported forget target variant")),
    }
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
