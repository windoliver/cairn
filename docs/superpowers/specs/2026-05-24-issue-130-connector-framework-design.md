# Connector framework + OAuth/webhook payload contracts — design

- **Issue:** [#130](https://github.com/windoliver/cairn/issues/130) — [P2] Implement external connector framework and OAuth/webhook payload contracts
- **Parent:** #29 (source connectors + aggregate memory extension)
- **Siblings:** #131 (real adapter slices), #181 (Slack)
- **Brief sections:** §9 Sensors, §19 v0.3 source connectors, §4.2 SensorIdentity, §14 consent
- **Date:** 2026-05-24

## 1. Goal

Ship the substrate that the v0.3 source connector slice plugs into: one trait,
one payload envelope, one redaction stage, one consent gate, one rate-limiter.
**Not** any real adapter (GitHub / IMAP / Drive / OneDrive / Notion /
web clipper) — those land in #131.

A reviewer reading this spec should be able to answer "could a malicious or
buggy adapter emit a label the operator never granted? bypass redaction?
keep polling after `disable`?" with a clean "no" by pointing at the
framework code, not at the adapter.

## 2. Acceptance criteria (from the issue, mapped to design sections)

| Acceptance criterion | Design section |
|---|---|
| Connectors cannot emit undeclared labels or bypass consent. | §5 (registry gates), §6.5 (`undeclared_label_rejected`, `consent_gate`) |
| OAuth/webhook payloads are validated and redacted before pipeline entry. | §4 (pre-Capture pipeline), §6.5 (`payload_validation`, `redaction`) |
| Disabled connectors do not poll or ingest. | §5 (registry lifecycle), §6.5 (`disabled_no_emit`) |
| Run connector contract tests. | §6.5 `contract.rs`, `manifest_validates.rs` |
| Run payload validation fixtures. | §6.5 `payload_validation.rs`, `oauth_lifecycle.rs` |
| Run disabled/consent tests. | §6.5 `disabled_no_emit.rs`, `consent_gate.rs` |

## 3. Crate layout & dependency edges

```
crates/cairn-connectors-core/         ← NEW. L2 substrate. No network code.
├── src/
│   ├── lib.rs                        ← re-exports
│   ├── connector.rs                  ← Connector trait, ConnectorPlugin
│   ├── event.rs                      ← ConnectorEvent envelope, ConnectorEventKind
│   ├── manifest.rs                   ← ConnectorManifest (toml) + parser
│   ├── credential.rs                 ← CredentialStore trait + InMemoryCredentialStore
│   ├── credential_keychain.rs        ← KeychainCredentialStore (default impl)
│   ├── webhook.rs                    ← WebhookRouter, WebhookRequest, signature verify
│   ├── poll.rs                       ← PollScheduler (tokio task per connector + cursor mgmt)
│   ├── redact.rs                     ← payload-level redaction wrapping pipeline::filter::redact
│   ├── registry.rs                   ← ConnectorRegistry (load → validate → mount → shutdown)
│   ├── rate_limit.rs                 ← per-scope token bucket reusing brief §9.1 budgets
│   ├── error.rs                      ← ConnectorError enum (thiserror)
│   └── fixture.rs                    ← #[cfg(any(test, feature="fixture"))] FixtureConnector
├── tests/                            ← see §6.5
└── Cargo.toml
```

Dependency edges:

- `cairn-connectors-core` →
  - `cairn-core` (traits, `CaptureEvent`, `pipeline::filter::redact`, `ConsentJournal`),
  - `cairn-keychain` (default `CredentialStore`),
  - `cairn-test-fixtures` (dev-dep only).
- **No** dep on `cairn-store-sqlite`, `cairn-workflows`, `cairn-cli`, `cairn-mcp`.
- Later, `cairn-cli` imports this crate to mount the registry; `cairn-workflows`
  can wrap the framework's `emit` entrypoint with durable retry — both **out**
  of this PR.

Edits outside the new crate (the only ones):

- `cairn-core/src/domain/capture.rs` — add `SourceFamily::External` variant
  (serde tag `external`) and a `CapturePayload::External { … }` variant.
- `cairn-core/src/contract/manifest.rs` — add `ContractKind::Connector` row
  so the existing `PluginManifest` validator can parse connector manifests.
- `crates/cairn-keychain/src/lib.rs` — expose a typed `CredentialHandle` that
  the framework can hand back to connectors without leaking secret bytes.

Workspace `Cargo.toml` gains `cairn-connectors-core` as a member.

## 4. `Connector` trait surface

```rust
// crates/cairn-connectors-core/src/connector.rs

pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConnectorCapabilities {
    pub poll: bool,
    pub webhook: bool,
    pub backfill: bool,
}

#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    fn name(&self) -> &str;
    fn manifest(&self) -> &ConnectorManifest;
    fn capabilities(&self) -> &ConnectorCapabilities;
    fn sensor_identity(&self) -> &Identity;       // signed snr:remote:<name>:v1
    fn supported_contract_versions(&self) -> VersionRange;

    async fn poll(
        &self,
        cx: &PollContext,                          // CredentialStore handle, last cursor, budget
    ) -> Result<PollOutcome, ConnectorError>;

    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,                      // raw bytes + headers + verified signature
        cx: &WebhookContext,                       // CredentialStore handle, budget
    ) -> Result<Vec<ConnectorEvent>, ConnectorError>;
}

pub trait ConnectorPlugin: Connector + Sized {
    const NAME: &'static str;
    const SUPPORTED_VERSIONS: VersionRange;
}
```

Supporting types:

```rust
pub struct PollOutcome {
    pub events: Vec<ConnectorEvent>,
    pub next_cursor: Option<Cursor>,
    pub rate_limit_hint: Option<RetryAfter>,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectorError {
    #[error("auth expired for {scope}")]              AuthExpired { scope: String },
    #[error("rate limited; retry after {retry_after:?}")]
    RateLimited { retry_after: std::time::Duration },
    #[error("webhook signature mismatch")]            SignatureMismatch,
    #[error("malformed payload: {0}")]                MalformedPayload(String),
    #[error("budget exceeded for {scope}")]           BudgetExceeded { scope: String },
    #[error("undeclared label {label}")]              UndeclaredLabel { label: String },
    #[error("consent revoked for {connector}")]       ConsentRevoked { connector: String },
    #[error(transparent)]                             Transient(#[source] anyhow::Error),
    #[error(transparent)]                             Fatal(#[source] anyhow::Error),
}
```

Notes:

- `#[async_trait]` mirrors existing `SensorIngress` style; switching to native
  async-fn-in-traits is a workspace-wide change, out of scope.
- `Connector::poll` and `ingest_webhook` return `ConnectorEvent`s — **not**
  `CaptureEvent`s. The framework owns the redact → label-check →
  `CaptureEvent::try_new` transition (§5). Connectors never see `CaptureEvent`.
- Capability flag turns each method on/off; framework never calls an
  unadvertised method (mirrors brief §8.0.a fail-closed rule).
- `sensor_identity()` is required: every emitted `CaptureEvent` is attributed
  to the connector's signed `SensorIdentity` (brief §4.2 / §9). Identity
  issuance is config-time, surfaced via the manifest.

## 5. `ConnectorEvent` envelope + `ConnectorManifest`

```rust
// crates/cairn-connectors-core/src/event.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorEvent {
    pub event_id: ConnectorEventId,        // ULID minted by connector
    pub connector: String,                 // matches manifest.name
    pub source_ref: SourceRef,             // stable origin id: {kind, system_id, sub_id?}
    pub occurred_at: Rfc3339Timestamp,     // wall-clock from upstream system
    pub labels: BTreeSet<String>,          // must be subset of manifest.allowed_labels
    pub scope: ConnectorScope,             // {workspace, channel?, project?, path?}
    pub payload: ConnectorPayload,         // tagged union
    pub delivery: DeliveryMode,            // Poll { cursor } | Webhook { signature_id }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ConnectorPayload {
    Json   { mime: String, body: serde_json::Value },
    Text   { mime: String, body: String },
    Binary { mime: String, sha256: PayloadHash, bytes_ref: BytesRef },
}
```

Validation invariants enforced by `ConnectorEvent::validate(&manifest)`:

- `event.connector == manifest.name` else `ConnectorError::Fatal`.
- `event.labels ⊆ manifest.allowed_labels` else `ConnectorError::UndeclaredLabel`.
- `event.scope` matches one of `manifest.declared_scopes` patterns.
- `payload.mime` is in `manifest.allowed_mimes`.
- `Binary.bytes_ref` resolves under the vault's connector spool dir — never
  an absolute path (mirrors `CaptureEvent::payload_ref` trust boundary).

`ConnectorManifest` (TOML, shipped at `/manifests/connector.toml` inside the
adapter crate, loaded by `ConnectorRegistry::register`):

```toml
# schema = cairn.connector.v1
[connector]
name              = "fixture"
contract          = "Connector"
contract_version  = "0.1.0"
sensor_identity   = "snr:remote:fixture:v1"

[capabilities]
poll     = true
webhook  = true
backfill = true

[oauth]
required_scopes = ["read:fixture"]
token_lifetime  = "1h"
refresh         = true

[budget]
max_items_per_hour = 600
max_bytes_per_day  = "50MiB"

[labels]
allowed = ["note", "comment", "issue", "discussion"]

[scopes]
declared = [{ kind = "project", pattern = "*" }]

[webhook]
signature.algorithm = "hmac-sha256"
signature.header    = "X-Fixture-Signature"
allowed_mimes       = ["application/json"]

[poll]
cursor_kind     = "opaque-string"
min_interval    = "30s"
default_interval= "5m"

[payload]
max_bytes = "256KiB"   # per leaf string / binary in a single event
max_depth = 12         # max JSON nesting
```

- Parser reuses the `cairn-core::contract::manifest::PluginManifest`
  validator pattern. A new `ContractKind::Connector` row is added there.
- The manifest's stable hash is recorded in the consent journal at first
  enable — see §6.4.

## 6. Pre-Capture redaction, framework→pipeline handoff, registry lifecycle

### 6.1 Pipeline diagram

```
   adapter                  framework                          existing pipeline (§5.2)
   ────────                 ─────────                          ────────────────────────
   ConnectorEvent
       │
       │  (poll loop OR webhook route, in cairn-connectors-core)
       ▼
   ┌─────────────────────────────────────────────────┐
   │ 1. WebhookRouter / PollScheduler verifies        │
   │    signature, refreshes OAuth via CredentialStore│
   │ 2. ConnectorEvent::validate(&manifest)           │
   │    → label / scope / mime / source_ref checks    │
   │ 3. RateLimit::charge(scope, payload.size())      │
   │ 4. RedactionPipeline::redact(event)              │
   │    a. structural sanitize: drop JSON keys whose  │
   │       leaf size exceeds manifest.payload.max_bytes│
   │       or whose depth exceeds payload.max_depth    │
   │    b. PII redact via cairn-core::pipeline::      │
   │       filter::redact::redact(text_view)          │
   │    c. spool Binary bytes to <vault>/.cairn/      │
   │       spool/connectors/<conn>/<sha256> and       │
   │       return relative payload_ref                │
   │ 5. ConsentJournal::lookup(connector, scope)      │
   │    → ConnectorError::ConsentRevoked if absent    │
   │ 6. CaptureEventBuilder constructs a              │
   │    CaptureEvent { source_family: External,       │
   │      sensor_id: manifest.sensor_identity,        │
   │      capture_mode: Auto,                         │
   │      actor_chain: [Sensor(connector)],           │
   │      payload: CapturePayload::External {…},      │
   │      payload_ref, payload_hash, … }.try_new(…)   │
   └────────────────────────────┬────────────────────┘
                                ▼
                       pipeline::capture::ingest(event)
                                ▼
                        Extract → Filter → Classify → Store
                                ▼
                              WAL (§5.6)
```

### 6.2 Why redaction lives in the framework

- Single audit point. The acceptance criterion reads "before pipeline entry"
  — keeping it in `cairn-connectors-core` makes that literal.
- Reuses `cairn-core::pipeline::filter::redact::redact` so the redaction
  taxonomy stays single-sourced. JSON-payload walking lives in
  `connectors-core::redact` (walks JSON leaves, calls the existing function,
  collects `RedactionSpan`s into `CapturePayload::External.redacted_spans`).
- Binary bytes never travel in the envelope. They land in
  `<vault>/.cairn/spool/connectors/<connector>/<sha256>`, managed by the
  framework, cleaned by an `ExpirationWorkflow` follow-up. The
  `CaptureEvent.payload_ref` points there — identical contract to local
  sensors.
- `BudgetExceeded` and `ConsentRevoked` are typed errors, never silent drops.
  Both surface in the next `cairn lint` report.
- `tracing` boundaries:
  `#[tracing::instrument(skip(req, event), err, fields(connector, scope, event_id))]`.
  Redacted payload bodies never log above `debug` (CLAUDE.md invariant 9).
  `ConnectorEvent::Debug` mirrors `CaptureEvent::Debug` — structural
  metadata only, payload redacted.

### 6.3 New `CapturePayload::External` variant

```rust
// crates/cairn-core/src/domain/capture.rs (additive)
External {
    connector: String,
    source_ref: SourceRef,
    labels: BTreeSet<String>,
    mime: String,
    redacted_spans: Vec<RedactionSpan>,
},
```

`payload.source_family()` match arm returns `SourceFamily::External` — the
invariant the existing `CaptureEvent::validate` already checks.

### 6.4 Registry, enable/disable, consent wiring

```rust
// crates/cairn-connectors-core/src/registry.rs

pub struct ConnectorRegistry {
    entries: HashMap<String, RegistryEntry>,
    credentials: Arc<dyn CredentialStore>,
    consent: Arc<dyn ConsentJournal>,
    emit: Arc<dyn PipelineEmit>,        // trait the framework calls to hand a CaptureEvent
                                        // to cairn-core::pipeline. cairn-cli supplies the
                                        // real impl; tests supply a recording one.
    shutdown: CancellationToken,
}

struct RegistryEntry {
    connector: Arc<dyn Connector>,
    manifest: ConnectorManifest,
    state: ArcSwap<ConnectorState>,     // Disabled | Enabled { since, consent_grant_id }
    poll_task: Option<JoinHandle<()>>,
    webhook_routes: Vec<MountedRoute>,  // empty until enable()
}

impl ConnectorRegistry {
    pub fn register<P: ConnectorPlugin + 'static>(&mut self, plugin: P)
        -> Result<(), ConnectorError>;

    pub async fn enable(&self, name: &str, grant: ConsentGrant)
        -> Result<(), ConnectorError>;
    pub async fn disable(&self, name: &str)
        -> Result<(), ConnectorError>;

    pub fn router(&self) -> axum::Router;       // composed router for cairn-cli to mount
    pub async fn shutdown(self);                // graceful: cancel + join all poll tasks
}
```

Rules that make the acceptance criteria true:

- **Disabled connectors do not poll or ingest.** `register()` only stores
  the plugin; no task spawned, no route mounted. `enable()` is what brings
  them up. `disable()` cancels via `CancellationToken` and drops the routes
  from the `axum::Router`. A webhook arriving for a disabled connector hits
  a 404 mounted by the framework — never reaches the trait.
- **Cannot emit undeclared labels or bypass consent.** Two framework gates,
  neither overridable from the connector:
    1. `ConnectorEvent::validate` returns `UndeclaredLabel { label }`.
    2. `ConsentJournal::lookup(connector, scope)`; absence ⇒
       `ConsentRevoked`. The consent grant id from `enable()` is what
       `lookup` resolves against; revoking the grant
       (`forget --consent <id>`) flips subsequent ingests to
       `ConsentRevoked` without restarting the connector.
- **Validated and redacted before pipeline entry.** `PipelineEmit::emit`
  is the *only* place a `CaptureEvent` crosses out of
  `cairn-connectors-core`, and it sits after §6.1 steps 1–6. There is no
  other path.

Consent journal integration:

- `enable()` writes a `consent_grant_v1` record with
  `{connector, manifest_hash, allowed_labels, scopes, granted_at, grantor: Identity}`.
- Manifest hash drift triggers `ConsentRevoked` on next emit, forcing
  re-grant — closes brief §14's "no silent scope widening" hole.
- `cairn lint` gains a check (separate small PR, **out of scope** here)
  that warns when an enabled connector's manifest hash diverges.

### 6.5 Test plan

All tests live in `crates/cairn-connectors-core/tests/`.

| Test file | What it asserts | Drives acceptance criterion |
|---|---|---|
| `contract.rs` | `FixtureConnector` implements `ConnectorPlugin`; `register → enable → poll → emit` round-trips a `CaptureEvent` with `source_family: External` | "Run connector contract tests" |
| `manifest_validates.rs` | TOML parses, dup names rejected, invalid scope patterns rejected, snapshot of `cairn.connector.v1` schema (insta) | "Run connector contract tests" |
| `payload_validation.rs` | proptest: arbitrary `WebhookRequest` body → either rejected with typed error or emitted with all PII-redacted; no raw byte from input appears unredacted in emitted `CaptureEvent` | "Run payload validation fixtures" |
| `oauth_lifecycle.rs` | `InMemoryCredentialStore`: token-refresh on `AuthExpired`, signature-verify mismatch → `SignatureMismatch`, replay-attack (same signature, second delivery) → rejected | "Run payload validation fixtures" |
| `undeclared_label_rejected.rs` | Fixture emits label outside manifest's `allowed_labels` → `ConnectorError::UndeclaredLabel`, `PipelineEmit` never called | "Connectors cannot emit undeclared labels" |
| `consent_gate.rs` | Without grant → `ConsentRevoked`. With grant → emits. After `disable()` + revoke → `ConsentRevoked` again on next webhook | "or bypass consent" |
| `disabled_no_emit.rs` | `register` only (no `enable`): poll task absent, webhook route returns 404, `PipelineEmit` never invoked even after 3× poll interval | "Disabled connectors do not poll or ingest" |
| `rate_limit.rs` | Exceeding `manifest.budget.max_items_per_hour` → `BudgetExceeded`, recorded for `lint` | brief §9.1 budget |
| `redaction.rs` | JSON payload with email + phone leaves emitted as `redacted_spans` covering the original byte ranges | "validated and redacted before pipeline entry" |

Runner: `cargo nextest run -p cairn-connectors-core --locked`. Snapshot
review with `cargo insta`. Proptest regressions committed.

## 7. Verification checklist (run before pushing)

```bash
cargo fmt --all --check
cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run -p cairn-connectors-core --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh                   # asserts cairn-core still adapter-free
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
cargo deny check && cargo audit --deny warnings && cargo machete
```

A PR touching `cairn-core/src/domain/capture.rs` or
`cairn-core/src/contract/manifest.rs` also re-runs `cairn-codegen` and
commits the result.

## 8. Out of scope

Named so the PR description can cite them:

- Real adapter crates (GitHub, IMAP, Drive, OneDrive, Notion, web clipper)
  — #131.
- Slack connector — #181.
- `connector_enable` / `connector_disable` / `connector_backfill` admin
  verbs in `cairn.admin.v1` — wired alongside the first real adapter.
- Durable workflow wrapping the framework `emit` (would belong in
  `cairn-workflows`) — defer until webhook traffic patterns require it.
- `cairn lint` manifest-drift check — separate small PR once the journal
  record format settles.
- CLI subcommand surface (`cairn connector …`) — adapter-agnostic substrate
  doesn't need it yet.
- `evolve` WAL state-machine for connector schema migration — brief §19
  ties this to the v0.3 evolution workflow, not the substrate.

## 9. Open questions

None at design time. Implementation will surface:

- Exact shape of `CredentialHandle` (typed wrapper or opaque `[u8]`) —
  decide at first call site in `KeychainCredentialStore`.
- Whether `axum::Router` is the right router primitive or if a smaller
  `tower` handler set fits better given `cairn-cli` already uses `axum`
  for the MCP HTTP transport — defer to first integration test in
  `cairn-cli`.
