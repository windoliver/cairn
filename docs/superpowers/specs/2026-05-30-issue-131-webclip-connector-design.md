# Issue #131 — Generic web-clipper connector adapter (slice 2)

- **Issue:** [#131 — P2 Implement GitHub, email, Drive/OneDrive, Notion, and web clipper adapters](https://github.com/windoliver/cairn/issues/131)
- **Parent epic:** #29 Source connectors and aggregate memory extension
- **Substrate dependency:** #130 connector framework (closed; landed as `cairn-connectors-core`)
- **Sibling slice:** #131 slice 1 — GitHub adapter (`cairn-connectors-github`, PR #424, merged). This spec reuses that slice's structure mechanically.
- **Design brief reference:** §19 v0.3 connector set ("a generic web-clipper extension"); §9.1 source sensors; §14 privacy/consent
- **Phase:** v0.3 — Federation + evolution (P2)

---

## 1. Scope

Issue #131 enumerates **five** connector adapters: GitHub, email, Drive/OneDrive, Notion, web clipper. Per CLAUDE.md §5 ("keep the diff scoped"), each is its own L2 crate under `crates/cairn-connectors-<vendor>/` and its own PR. Slice 1 (GitHub) is merged. **This spec covers only the generic web-clipper adapter.** The remaining three (email, Drive/OneDrive, Notion) are deferred to follow-up slices.

The web clipper is the **push-only** member of the connector set: there is no upstream service to poll. A browser extension (or any HTTP client) HMAC-signs and `POST`s a captured clip to the connector's webhook endpoint. This makes it the simplest slice and a useful proof that the substrate supports a **second delivery topology** (webhook-only) distinct from GitHub's poll-shaped adapter.

### 1.1 In scope (this PR)

- New crate `cairn-connectors-webclip` implementing `cairn_connectors_core::Connector` + `ConnectorPlugin`.
- **Webhook-only delivery.** `capabilities = { poll: false, webhook: true, backfill: false }`.
- **Content-negotiated payloads** in `ingest_webhook`, branching on `Content-Type`:
  - `application/json` → structured clip envelope → `ConnectorPayload::Json`.
  - `text/markdown` | `text/plain` → raw clip body → `ConnectorPayload::Text`; clip metadata supplied via `X-Cairn-Clip-*` headers.
- **Per-domain consent scoping**: `scope = domain:<host>`, host parsed from the clip URL; manifest declares `kind = "domain", pattern = "*"`.
- **Deterministic event IDs** for idempotent re-delivery (`from_parts("clip", url, &[captured_at, payload_hash])`), mirroring GitHub's `event_id.rs`.
- Bundled `connector.toml` manifest, validated at construction time.
- Substrate-backed integration tests (real `ConnectorRegistry` + `RedactionPipeline`); no network mock needed (no upstream HTTP).

### 1.2 Out of scope (this PR, tracked separately)

- Email, Drive/OneDrive, Notion adapter crates — follow-up slices against #131.
- `cairn admin connector_enable` / `connector_disable` / `connector_backfill` CLI verbs and wiring the connector into the `cairn-cli` binary — those belong to the #29 epic / #161, not the per-adapter slice (GitHub slice 1 did not wire them either).
- **Binary payloads** (image/PDF clips). The substrate rejects `ConnectorPayload::Binary` in P0 (see `cairn-connectors-core::event` — "P0 substrate rejects Binary payloads"); the spool-verified binary path is itself part of #131's broader work but **not** this slice. Clips are text/markdown/JSON only.
- The browser-extension client itself. This PR defines and tests the **wire contract** (endpoint, headers, envelope); shipping an extension is separate.
- Full-page HTML capture / readability extraction. The extension is expected to send already-extracted markdown or a structured selection; raw `text/html` is **not** in `allowed_mimes` for this slice.

---

## 2. Load-bearing invariants this PR touches

From CLAUDE.md §4:

| Invariant | How this PR honors it |
|---|---|
| 1. Harness-agnostic | No code path references any harness. |
| 3. CLI is ground truth | Adapter exposes no verbs; substrate owns the webhook route. |
| 4. Seven contracts | Implements `Connector` only; no new contract added. |
| 5. WAL + two-phase apply | Emits `ConnectorEvent`s; substrate's `process_event` spools + emits via existing pipeline. Adapter never writes the DB. |
| 6. Fail closed on capability | Manifest `capabilities` is authoritative: `poll = false` → no poll task spawned; route only mounted when enabled + `webhook = true`. |
| 7. `#![forbid(unsafe_code)]` | Applied at crate root. |
| 8. No `unwrap()`/`expect()` in core | Adapter is **not** core; `expect("invariant: …")` tolerated in bins/tests only. |
| 9. Privacy by construction | Manifest label allow-list gates every emit; redaction runs in substrate before persist; raw clip bodies never logged above `trace`; per-domain consent scope. |
| 10. Sources immutable | Adapter only ingests; never writes back to any source. |

---

## 3. Crate topology

```
crates/cairn-connectors-webclip/
├── Cargo.toml
├── connector.toml                 # bundled at compile time via include_str!
├── src/
│   ├── lib.rs                     # pub use WebClipConnector; #![forbid(unsafe_code)]
│   ├── connector.rs               # Connector + ConnectorPlugin impls; inert poll(); ingest_webhook()
│   ├── clip.rs                    # ClipEnvelope + Content-Type negotiation; parse request -> ConnectorEvent
│   ├── event_id.rs                # deterministic ULID minting (from_parts + payload_hash)
│   ├── error.rs                   # WebClipError -> ConnectorError mapping
│   └── testkit.rs                 # cfg/feature-gated helpers: build signed WebhookRequest, sample clips
└── tests/
    ├── fixtures/
    │   ├── clip_json.json          # structured envelope
    │   └── clip_markdown.md        # raw markdown body
    ├── ingest_json_clip.rs         # JSON clip -> one ConnectorEvent (scope/labels/source_ref)
    ├── ingest_markdown_clip.rs     # markdown body + X-Cairn-Clip-* headers -> one event
    ├── content_negotiation.rs      # rstest table over Content-Type values incl. params + unknown
    ├── idempotent_event_id.rs      # same clip twice -> identical event_id
    ├── malformed_payload.rs        # bad URL / missing captured_at / unsupported Content-Type -> MalformedPayload
    ├── tags_not_promoted_to_labels.rs  # user tags stay in payload; labels stay {source:web, kind:clip}
    ├── registry_end_to_end.rs      # register -> enable (grant domain:*) -> ingest_webhook -> emit
    └── disabled_no_emit.rs         # ingest before enable / after disable -> rejected, no emit
```

### 3.1 Dependency rules

- New crate depends on `cairn-connectors-core` only — **never** on `cairn-core` directly in non-dev code. Enforced by `scripts/check-core-boundary.sh`.
- No cross-adapter imports (must not depend on `cairn-connectors-github` or any future sibling).

### 3.2 Cargo.toml deps

The web clipper is **stateless and network-free** — no HTTP client, no OAuth, no auth cache, no interior mutability. Its dependency set is therefore a strict subset of GitHub's.

| Dep | Use | Notes |
|---|---|---|
| `cairn-connectors-core` | substrate trait + re-exported core types | workspace dep |
| `async-trait` | `#[async_trait]` on the `Connector` impl | workspace |
| `serde` / `serde_json` | clip envelope + payload JSON | workspace |
| `sha2` | payload hash for deterministic event_id | workspace |
| `ulid` | ULID construction for event_id | workspace |
| `hex` | hex-encode the short payload hash | workspace |
| `url` | parse clip URL → host (scope) | workspace (already used by GitHub) |
| `tracing` | logging (metadata only; never bodies) | workspace |
| `thiserror` | `WebClipError` enum | workspace |
| **dev** `cairn-connectors-core` `features=["fixture"]` | substrate test helpers | |
| **dev** `cairn-core` | construct `ConsentGrant`, assert `CaptureEvent` | for registry end-to-end test |
| **dev** `tokio` `features=["macros","rt-multi-thread"]` | `#[tokio::test]` | |
| **dev** `rstest` | content-negotiation table tests | workspace |
| **dev** `insta` | snapshot of parsed event | workspace |
| **dev** `proptest` | event_id determinism / URL parsing round-trips | workspace |
| **dev** `tempfile` | spool dir for registry end-to-end test | workspace |

**No** `reqwest`, `wiremock`, `jsonwebtoken`, `arc-swap`, `chrono`, or `tokio-util` direct dep — none apply to a push-only, stateless adapter. (`captured_at` is carried as **Unix seconds**, avoiding a date-parsing crate; see §5.)

### 3.3 Workspace wiring

- `members = ["crates/*"]` in the root `Cargo.toml` auto-includes the new crate; no members edit needed.
- Add `cairn-connectors-webclip = { path = "crates/cairn-connectors-webclip", version = "0.0.1" }` to `[workspace.dependencies]` for symmetry with `cairn-connectors-github` and so a later CLI-registration PR can reference it.
- Adding a workspace package triggers the docgen gate (CLAUDE.md §8): re-run `cairn-docgen --write` and commit the generated reference Markdown.

---

## 4. Manifest (`connector.toml`)

Every block in `ConnectorManifest` is **mandatory** (`cairn-connectors-core::manifest`, `#[serde(deny_unknown_fields)]`). A webhook-only connector therefore still ships inert `[oauth]` and `[poll]` blocks.

```toml
[connector]
name              = "webclip"
contract          = "Connector"
contract_version  = "0.1.0"
sensor_identity   = "snr:local:connector:webclip:v1"

[capabilities]
poll     = false
webhook  = true
backfill = false

# Inert: the web clipper authenticates via the per-connector HMAC webhook
# secret (CredentialStore key "connector/webclip/webhook_secret"), not OAuth.
# The block is required by the manifest schema; values are unused.
[oauth]
required_scopes = []
token_lifetime  = "0s"
refresh         = false

[budget]
max_items_per_hour = 600
max_bytes_per_day  = "100MiB"

[labels]
allowed = ["source:web", "kind:clip"]

[[scopes.declared]]
kind    = "domain"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header"    = "X-Cairn-Signature-256"
"signature.prefix"    = "sha256="
allowed_mimes         = ["application/json", "text/markdown", "text/plain"]
delivery_id_header    = "X-Cairn-Delivery"

# Inert: poll = false means the registry never spawns a poll task. The block
# is required by the manifest schema; values are placeholders.
[poll]
cursor_kind      = "opaque-string"
min_interval     = "60s"
default_interval = "5m"

[payload]
max_bytes = "1MiB"
max_depth = 16
```

Bundled via `include_str!("../connector.toml")` and parsed by `ConnectorManifest::parse_toml` inside `WebClipConnector::new`. The substrate's `register()` checks that the runtime `sensor_identity()` equals the manifest declaration and that the wire form is `snr:local:connector:webclip:v…`.

`delivery_id_header` is adapter-facing metadata only — the substrate keys its replay guard on the HMAC of the body, never on this header (see `manifest::WebhookBlock` docs). Identical-body dedup beyond that is handled by our deterministic `event_id`.

---

## 5. Wire contract

### 5.1 Endpoint

`POST /webhooks/webclip` — mounted by `ConnectorRegistry::webhook_router()` once the connector is registered **and** enabled. Before calling `ingest_webhook`, the substrate:

1. Bounds the body by `payload.max_bytes` (`1MiB`) → `413` if exceeded.
2. Looks up the secret at `CredentialStore::get("connector/webclip/webhook_secret")` → `401` if absent.
3. Verifies HMAC-SHA256 over the raw body using header `X-Cairn-Signature-256` with prefix `sha256=` → `401` on mismatch.

The adapter therefore **never** verifies the signature itself.

### 5.2 JSON mode (`Content-Type: application/json`)

Body is a structured envelope:

```jsonc
{
  "url": "https://en.wikipedia.org/wiki/Cairn",   // required
  "title": "Cairn - Wikipedia",                    // optional
  "captured_at": 1748563200,                       // required, Unix seconds (i64)
  "selection": "A cairn is a human-made pile…",    // clip body; one of selection|markdown required
  "markdown": "## Cairn\nA cairn is…",             // clip body (markdown form)
  "note": "for the trail-marker article",          // optional user note
  "tags": ["hiking", "reference"]                   // optional; NOT promoted to labels (see §6)
}
```

Parsed into a `ClipEnvelope` (`#[serde(deny_unknown_fields)]`). At least one of `selection` / `markdown` must be present. The resulting `ConnectorPayload::Json { mime: "application/json", body }` carries the re-serialized, validated envelope so downstream extract/classify sees full structure.

### 5.3 Text mode (`Content-Type: text/markdown` or `text/plain`)

Body is the raw clip text (opaque to the adapter). Metadata rides in headers:

| Header | Required | Meaning |
|---|---|---|
| `X-Cairn-Clip-Url` | yes | source URL (→ scope + `source_ref`) |
| `X-Cairn-Clip-Captured-At` | yes | Unix seconds (decimal string) |
| `X-Cairn-Clip-Title` | no | clip title |

Produces `ConnectorPayload::Text { mime: <content-type>, body: <raw text> }`. `note`/`tags` are JSON-mode only (text mode has no structured envelope).

### 5.4 Content-Type matching

`Content-Type` is read case-insensitively; parameters are stripped (`application/json; charset=utf-8` → `application/json`) and the result trimmed before matching. Any value not in `{application/json, text/markdown, text/plain}` → `WebClipError::UnsupportedContentType` → `ConnectorError::MalformedPayload`.

`captured_at` is **required** in both modes. Carrying it explicitly (rather than stamping a server clock) keeps the adapter free of wall-clock reads, makes `occurred_at` and `event_id` deterministic, and makes every test reproducible. Missing/unparseable → `MalformedPayload`.

---

## 6. Event construction

For each accepted request, `ingest_webhook` returns exactly **one** `ConnectorEvent`:

| Field | Value |
|---|---|
| `event_id` | `event_id::from_parts("clip", &url, &[&captured_at.to_string(), &payload_hash])` — deterministic ULID. |
| `connector` | `"webclip"`. |
| `source_ref` | `{ kind: "clip", system_id: <url>, sub_id: None }`. |
| `occurred_at` | `captured_at` (Unix seconds, `i64`). |
| `labels` | fixed `{ "source:web", "kind:clip" }` (BTreeSet) — both declared in the manifest. |
| `scope` | `ConnectorScope::new("domain", <host>)` where `<host>` = `Url::parse(url).host_str()`. Missing host → `MalformedPayload`. |
| `payload` | `Json` (JSON mode) or `Text` (text mode) per §5. |
| `delivery` | `DeliveryMode::Webhook { signature_id }` using the `X-Cairn-Signature-256` header value as the surrogate id (same pattern as GitHub). |

`payload_hash` = `hex(sha256(<wire bytes>))[..16 hex chars]` — over the raw body bytes (text) or the canonical re-serialized JSON (json). This guarantees two distinct clips of the same URL at the same second still get distinct event IDs, while identical re-deliveries collapse to one.

**Tags do not become labels.** Promoting user-supplied `tags` to `ConnectorEvent::labels` would trip the substrate's undeclared-label gate (`process_event` rejects any label outside the manifest allow-list and the grant). Tags are preserved inside the JSON payload as data, surfaced downstream by extract/classify — not as taxonomy labels.

The substrate's `process_event` then runs: ULID path-safety check → name integrity → enabled check → manifest-hash/consent → grant + manifest label checks → grant scope-pattern + manifest scope match → MIME/size validation → budget reserve → **redaction** → spool (two-phase) → `build_capture_event` → re-check enabled → emit. The adapter contributes none of these; it only produces a well-formed event.

---

## 7. `Connector` implementation sketch

```rust
// connector.rs
pub struct WebClipConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl WebClipConnector {
    pub fn new() -> Result<Self, ConnectorError> {
        let manifest = ConnectorManifest::parse_toml(MANIFEST_TOML)?;
        let sensor = Identity::parse("snr:local:connector:webclip:v1")?;
        Ok(Self { manifest, sensor })
    }
}

#[async_trait]
impl Connector for WebClipConnector {
    fn name(&self) -> &str { self.manifest.name() }
    fn manifest(&self) -> &ConnectorManifest { &self.manifest }
    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities =
            ConnectorCapabilities { poll: false, webhook: true, backfill: false };
        &C
    }
    fn sensor_identity(&self) -> &Identity { &self.sensor }
    fn supported_contract_versions(&self) -> VersionRange {
        <Self as ConnectorPlugin>::SUPPORTED_VERSIONS
    }

    // poll = false in the manifest, so the registry never calls this. The trait
    // still requires a body; return an empty outcome.
    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome::default())
    }

    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        let signature_id = req.header("X-Cairn-Signature-256").unwrap_or("unverified").to_owned();
        Ok(vec![clip::parse_request(req, &signature_id)?])
    }
}

impl ConnectorPlugin for WebClipConnector {
    const NAME: &'static str = "webclip";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(CONTRACT_VERSION, ContractVersion::new(0, 2, 0));
}
```

`clip::parse_request` owns all the fallible parsing (Content-Type negotiation, URL/host, `captured_at`, envelope validation) and returns a `WebClipError` that converts (`#[from]`) into `ConnectorError::MalformedPayload`.

---

## 8. Error handling

`WebClipError` (thiserror) → `ConnectorError`:

| `WebClipError` | Cause | Maps to |
|---|---|---|
| `MissingContentType` | no `Content-Type` header | `MalformedPayload` |
| `UnsupportedContentType(String)` | type not in allow-list | `MalformedPayload` |
| `MissingField(&'static str)` | required url / captured_at / clip body absent | `MalformedPayload` |
| `BadUrl(String)` | URL unparseable or has no host | `MalformedPayload` |
| `BadCapturedAt(String)` | non-integer Unix seconds | `MalformedPayload` |
| `Json(serde_json::Error)` | envelope parse failed (`#[from]`) | `MalformedPayload` |

All adapter-side failures are `MalformedPayload`, which the webhook handler maps to a `4xx`. Signature (`401`), consent (`ConsentRevoked`), budget (`BudgetExceeded`), rate-limit, and spool errors are the substrate's responsibility. No `unwrap`/`expect` outside tests; no `panic!`.

---

## 9. Test strategy

### 9.1 Integration tests (`tests/`) — real substrate, no network mock

| Test file | What it proves |
|---|---|
| `ingest_json_clip.rs` | JSON envelope → one event: `scope = domain:en.wikipedia.org`, labels `{source:web,kind:clip}`, `source_ref.kind = "clip"`, `payload = Json`. |
| `ingest_markdown_clip.rs` | `text/markdown` body + `X-Cairn-Clip-*` headers → one event with `payload = Text`. |
| `content_negotiation.rs` | `rstest` table: `application/json`, `text/markdown`, `text/plain`, json-with-charset-param all accepted; `text/html` and `application/xml` rejected. |
| `idempotent_event_id.rs` | Same clip parsed twice → identical `event_id`; same URL + same second but different body → different `event_id`. |
| `malformed_payload.rs` | Missing URL, missing `captured_at`, hostless URL (`file:///x`), missing Content-Type, unsupported type → `ConnectorError::MalformedPayload`. |
| `tags_not_promoted_to_labels.rs` | Envelope with `tags` → emitted labels are exactly `{source:web,kind:clip}`; tags present in payload JSON. |
| `registry_end_to_end.rs` | `register` → `enable` with a `ConsentGrant` whose `scope_patterns = ["domain:*"]` and `allowed_labels = {source:web,kind:clip}` (and matching `manifest_hash`) → drive `ingest_webhook` via the connector → assert the spooled `CaptureEvent` emitted. Mirrors GitHub's `registry_end_to_end.rs`. |
| `disabled_no_emit.rs` | `process_event` before enable / after disable rejects with no emit (substrate gate; asserts capability fail-closed). |

Tests construct signed `WebhookRequest`s with `cairn_connectors_core::webhook::hex_hmac_sha256` (+ `sha256=` prefix) via a `testkit` helper. The registry end-to-end test uses a `tempfile::tempdir()` spool root and an in-memory consent journal / emit recorder from `cairn-connectors-core`'s fixture feature.

### 9.2 Unit tests (inline `#[cfg(test)] mod tests`)

- `clip.rs` — Content-Type normalization (param stripping, case); URL→host extraction; `captured_at` parsing; envelope `deny_unknown_fields`; "at least one of selection/markdown" rule.
- `event_id.rs` — determinism (same inputs → same ULID), output is a valid ULID accepted by `ConnectorEventId::parse`, content-change → different id; `proptest` over arbitrary url/captured_at/body.
- `error.rs` — `WebClipError` → `ConnectorError::MalformedPayload` mapping.
- `connector.rs` — `new()` constructs; `name()=="webclip"`; capabilities `{false,true,false}`; sensor identity string; `poll()` returns empty.

### 9.3 No mocking the substrate

Per CLAUDE.md §6.4: integration tests use the **real** `ConnectorRegistry`, **real** consent gate, **real** `RedactionPipeline`. There is no upstream network to mock for a push-only connector.

---

## 10. Acceptance-criteria mapping (and the push-only deviation)

Issue #131 acceptance criteria were written for the connector set as a whole and assume a **poll-shaped** source. The web clipper has no upstream to poll, so the cursor/backfill criteria are reinterpreted, not dropped:

| Criterion | Web-clipper treatment |
|---|---|
| "Each connector can backfill and incrementally sync fixture data." | **Reinterpreted.** A push-only source has no cursor/backfill. The clipper's equivalent of incremental sync is **idempotent webhook ingestion**: deterministic `event_id` (§6) + the substrate's body-HMAC replay guard ensure re-delivered clips collapse to one record. `capabilities.poll = false`, `backfill = false` are declared honestly. Covered by `idempotent_event_id.rs`. |
| "Rate limits and auth failures are reported without data corruption." | Per-scope item budget + daily byte budget are enforced by the substrate from the manifest (`max_items_per_hour`, `max_bytes_per_day`); over-budget events return `BudgetExceeded` with no partial spool (substrate two-phase write). "Auth failure" = missing/invalid webhook secret → `401` before `ingest_webhook`. Adapter-side malformed input → `MalformedPayload` (`4xx`), nothing persisted. Covered by `malformed_payload.rs` + substrate budget tests. |
| "Imported records remain searchable, scoped, and forgettable." | Events flow through the unchanged substrate pipeline → existing `cairn-core` search/forget paths. Scope = `domain:<host>` makes per-site `forget` and scoped search work. Covered by `registry_end_to_end.rs` asserting a scoped `CaptureEvent`. |

This deviation is intentional and documented so review does not read `poll = false` as a missing requirement. **Verification checklist** items map: "adapter fixture tests" → §9.1 ingest tests; "cursor/backfill tests" → N/A for push-only, replaced by `idempotent_event_id.rs` (documented above); "rate-limit/auth failure tests" → `malformed_payload.rs` + substrate budget/secret gates.

---

## 11. CI verification (run before opening the PR)

Per CLAUDE.md §8:

```bash
cargo fmt --all --check
cargo clippy -p cairn-connectors-webclip --all-targets --locked -- -D warnings
cargo check -p cairn-connectors-webclip --all-targets --locked
cargo nextest run -p cairn-connectors-webclip --locked --no-fail-fast
cargo test --doc -p cairn-connectors-webclip --locked
./scripts/check-core-boundary.sh
cargo deny check
cargo audit --deny warnings
cargo machete
# workspace package membership changed → regenerate docs:
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write   # commit generated Markdown
```

Then a full-workspace `cargo nextest run --workspace --locked --no-fail-fast` to confirm nothing else regressed.

---

## 12. Risks and open items

| Risk | Mitigation |
|---|---|
| Acceptance criteria assume polling | §10 documents the push-only reinterpretation explicitly; reviewers sign off on it rather than discovering it. |
| Single connector instance per process | Substrate registry keys by `name()`; manifest fixes name to `"webclip"`. One clipper per process is sufficient (one local endpoint). Matches GitHub slice's documented limitation. |
| `text/html` / binary clips wanted later | Out of scope here (§1.2); `allowed_mimes` and the absence of `Binary` make the boundary explicit. Adding HTML is a one-line manifest change + a parse branch in a follow-up. |
| Client must send `captured_at` | Documented in the wire contract (§5); the extension is ours to define. Avoids a server clock and keeps tests deterministic. |
| Webhook secret provisioning | Out of scope for the slice (operator provisions `connector/webclip/webhook_secret` via the same path GitHub uses); the registry returns `401` until provisioned — fail-closed. |

---

## 13. Follow-ups (not in this PR)

1. Email adapter (`cairn-connectors-email`) — IMAP poll + inbound webhook.
2. Drive / OneDrive adapter (`cairn-connectors-drive`) — OAuth + change-feed cursors; needs the spool-verified Binary path.
3. Notion adapter (`cairn-connectors-notion`) — Notion API poll + webhook.
4. Wire `webclip` (and siblings) into `cairn-cli` connector registration + `cairn admin connector_*` verbs (#29 / #161).
5. Ship the browser extension that produces the wire contract defined in §5.

Each follow-up gets its own spec under `docs/superpowers/specs/` and reuses this slice's structure.
