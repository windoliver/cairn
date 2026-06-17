# Web-clipper Connector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `cairn-connectors-webclip`, a webhook-only connector that turns HMAC-signed web-clip POSTs into pipeline `ConnectorEvent`s, scoped per domain.

**Architecture:** A new leaf crate implementing `cairn_connectors_core::Connector` + `ConnectorPlugin`. It advertises `poll=false, webhook=true, backfill=false`. `ingest_webhook` content-negotiates on `Content-Type` (`application/json` envelope vs `text/markdown`|`text/plain` body) and returns exactly one `ConnectorEvent`. The substrate verifies the HMAC and runs redaction/consent/budget/spool/emit; the adapter only parses request → event. Re-deliveries dedup via a deterministic ULID.

**Tech Stack:** Rust 2024, `cairn-connectors-core`, `serde`/`serde_json`, `url`, `sha2`/`ulid`/`hex`, `async-trait`, `thiserror`. Tests: `tokio`, `rstest`, `proptest`, `axum`+`tower` (router e2e), `tempfile`, `cairn-core` + the `fixture` feature of the substrate.

**Reference spec:** `docs/superpowers/specs/2026-05-30-issue-131-webclip-connector-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/cairn-connectors-webclip/Cargo.toml` | Crate manifest; deps are a strict subset of the GitHub adapter (no HTTP/OAuth/state). |
| `crates/cairn-connectors-webclip/connector.toml` | Bundled `ConnectorManifest`; webhook-only; per-domain scope. |
| `src/lib.rs` | Crate root: module wiring, `MANIFEST_TOML` const, `pub use`, testkit gate. |
| `src/error.rs` | `WebClipError` + `From<WebClipError> for ConnectorError`. |
| `src/event_id.rs` | Deterministic ULID minting from clip identity + payload hash. |
| `src/clip.rs` | `ClipEnvelope`, `Content-Type` negotiation, `parse_request` → `ConnectorEvent`. The core logic. |
| `src/connector.rs` | `WebClipConnector` — `Connector` + `ConnectorPlugin` impls; inert `poll`. |
| `src/testkit.rs` | Test-only signed-request builders. |
| `tests/fixtures/clip_json.json`, `clip_markdown.md` | Golden clip bodies. |
| `tests/ingest_json_clip.rs`, `ingest_markdown_clip.rs`, `content_negotiation.rs`, `idempotent_event_id.rs`, `malformed_payload.rs`, `tags_not_promoted_to_labels.rs` | Direct `ingest_webhook` integration tests. |
| `tests/registry_end_to_end.rs` | Full router → pipeline emit + disabled-route-404. |
| Root `Cargo.toml` | Add `cairn-connectors-webclip` to `[workspace.dependencies]`. |

---

### Task 1: Crate scaffold + manifest

**Files:**
- Create: `crates/cairn-connectors-webclip/Cargo.toml`
- Create: `crates/cairn-connectors-webclip/connector.toml`
- Create: `crates/cairn-connectors-webclip/src/lib.rs`
- Modify: `Cargo.toml` (root — add workspace dependency entry)

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "cairn-connectors-webclip"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
description = "Generic web-clipper connector adapter (webhook-only) for cairn-connectors-core."

[lints]
workspace = true

[features]
default = ["testkit"]
testkit = []

[dependencies]
cairn-connectors-core = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
ulid = { workspace = true }
hex = { workspace = true }
url = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
cairn-connectors-core = { workspace = true, features = ["fixture"] }
cairn-core = { workspace = true }
axum = { workspace = true }
tower = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
rstest = { workspace = true }
proptest = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Write `connector.toml`**

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
# The block is required by the manifest schema; the values are unused.
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
# is required by the manifest schema; the values are placeholders.
[poll]
cursor_kind      = "opaque-string"
min_interval     = "60s"
default_interval = "5m"

[payload]
max_bytes = "1MiB"
max_depth = 16
```

- [ ] **Step 3: Write `src/lib.rs`** (minimal root + manifest-parse test)

```rust
//! Generic web-clipper connector adapter for `cairn-connectors-core`.
//!
//! Issue #131 (slice 2), brief §19 v0.3 connector set, §9.1 source sensors.
//!
//! A **webhook-only**, stateless adapter: a browser extension HMAC-signs and
//! POSTs a captured clip to `POST /webhooks/webclip`. The substrate verifies
//! the signature, then this adapter parses the request into exactly one
//! [`cairn_connectors_core::ConnectorEvent`]. There is no upstream to poll, so
//! `capabilities = { poll: false, webhook: true, backfill: false }`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Embedded `connector.toml` bytes, parsed at `WebClipConnector::new` time.
///
/// Exposed so integration tests can derive the expected `manifest_hash` when
/// constructing a `ConsentGrant` for the registry end-to-end test.
pub const MANIFEST_TOML: &str = include_str!("../connector.toml");

#[cfg(test)]
mod manifest_tests {
    use super::MANIFEST_TOML;
    use cairn_connectors_core::ConnectorManifest;

    #[test]
    fn manifest_parses_and_declares_webhook_only() {
        let m = ConnectorManifest::parse_toml(MANIFEST_TOML).expect("manifest parses");
        assert_eq!(m.name(), "webclip");
        assert!(!m.capabilities.poll, "web clipper does not poll");
        assert!(m.capabilities.webhook, "web clipper accepts webhooks");
        assert!(!m.capabilities.backfill, "web clipper has no backfill");
        assert!(m.allowed_label("source:web"));
        assert!(m.allowed_label("kind:clip"));
        assert!(m.scope_matches("domain", "en.wikipedia.org"));
        assert!(m.allowed_mime("application/json"));
        assert!(m.allowed_mime("text/markdown"));
        assert!(m.allowed_mime("text/plain"));
    }
}
```

- [ ] **Step 4: Add the workspace dependency entry to root `Cargo.toml`**

Find the line:
```toml
cairn-connectors-github = { path = "crates/cairn-connectors-github", version = "0.0.1" }
```
Add immediately after it:
```toml
cairn-connectors-webclip = { path = "crates/cairn-connectors-webclip", version = "0.0.1" }
```
(`members = ["crates/*"]` already includes the new crate; no members edit needed.)

- [ ] **Step 5: Run the manifest test — verify it passes**

Run: `cargo nextest run -p cairn-connectors-webclip --locked`
Expected: PASS (`manifest_parses_and_declares_webhook_only`). If the manifest fails to parse, the error names the offending block.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-connectors-webclip/Cargo.toml \
        crates/cairn-connectors-webclip/connector.toml \
        crates/cairn-connectors-webclip/src/lib.rs \
        Cargo.toml
git commit -m "feat(connectors-webclip): crate scaffold + manifest (#131 slice 2)"
```

---

### Task 2: `WebClipError`

**Files:**
- Create: `crates/cairn-connectors-webclip/src/error.rs`
- Modify: `crates/cairn-connectors-webclip/src/lib.rs`

- [ ] **Step 1: Write the failing test** — append to `src/error.rs`:

```rust
//! `WebClipError` — adapter-local parse errors, mapped to `ConnectorError`.

use cairn_connectors_core::ConnectorError;

/// Errors produced while parsing a web-clip webhook request into a
/// [`cairn_connectors_core::ConnectorEvent`].
///
/// Every variant maps to [`ConnectorError::MalformedPayload`] so the substrate
/// webhook handler returns `400 Bad Request`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WebClipError {
    /// The request carried no `Content-Type` header.
    #[error("missing Content-Type header")]
    MissingContentType,
    /// The `Content-Type` is not an accepted clip media type.
    #[error("unsupported Content-Type: {0}")]
    UnsupportedContentType(String),
    /// A required field was absent (url, captured_at, or clip body).
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    /// The clip URL could not be parsed or had no host component.
    #[error("invalid clip url: {0}")]
    BadUrl(String),
    /// `captured_at` was not a valid Unix-seconds integer.
    #[error("invalid captured_at: {0}")]
    BadCapturedAt(String),
    /// The JSON envelope failed to deserialize.
    #[error("invalid json clip envelope: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<WebClipError> for ConnectorError {
    fn from(err: WebClipError) -> Self {
        ConnectorError::MalformedPayload(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_malformed_payload() {
        let e: ConnectorError = WebClipError::BadUrl("file:///x".into()).into();
        assert!(matches!(e, ConnectorError::MalformedPayload(_)));
        assert_eq!(e.to_string(), "malformed payload: invalid clip url: file:///x");
    }

    #[test]
    fn json_error_converts_via_from() {
        let json_err = serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
        let e: WebClipError = json_err.into();
        assert!(matches!(e, WebClipError::Json(_)));
    }
}
```

- [ ] **Step 2: Wire the module** — in `src/lib.rs`, add after the `MANIFEST_TOML` const:

```rust
mod error;

pub use error::WebClipError;
```

- [ ] **Step 3: Run the test — verify it passes**

Run: `cargo nextest run -p cairn-connectors-webclip --locked error`
Expected: PASS (`maps_to_malformed_payload`, `json_error_converts_via_from`).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-connectors-webclip/src/error.rs crates/cairn-connectors-webclip/src/lib.rs
git commit -m "feat(connectors-webclip): WebClipError -> ConnectorError mapping (#131)"
```

---

### Task 3: Deterministic event IDs

**Files:**
- Create: `crates/cairn-connectors-webclip/src/event_id.rs`
- Modify: `crates/cairn-connectors-webclip/src/lib.rs`

- [ ] **Step 1: Write `src/event_id.rs`** (implementation + tests together — this is a pure leaf module)

```rust
//! Deterministic `ConnectorEventId` minting for web clips.
//!
//! Identical re-deliveries of the same clip must collapse to one record at the
//! substrate's event-id dedup gate. We hash the clip identity tuple
//! `(url, captured_at, payload_hash)` into the 16 bytes of a ULID, so the same
//! clip always yields the same id while two distinct clips of the same URL at
//! the same second differ via `payload_hash`.

use cairn_connectors_core::ConnectorEventId;
use sha2::{Digest, Sha256};
use ulid::Ulid;

/// Mint a deterministic ULID from the clip identity components.
///
/// All components are NUL-separated in the hash input so they cannot collide
/// across boundaries (e.g. `["ab","c"]` ≠ `["abc"]`).
pub(crate) fn from_parts(kind: &str, url: &str, components: &[&str]) -> ConnectorEventId {
    let mut hasher = Sha256::new();
    hasher.update(b"cairn-connectors-webclip/v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(url.as_bytes());
    for c in components {
        hasher.update(b"\0");
        hasher.update(c.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ConnectorEventId::new(Ulid::from_bytes(bytes).to_string())
}

/// Hash arbitrary wire bytes into a short hex revision component.
pub(crate) fn payload_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(&hasher.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_connectors_core::ConnectorEventId;
    use proptest::prelude::*;

    #[test]
    fn same_inputs_yield_same_id() {
        let a = from_parts("clip", "https://e.com/a", &["100", "deadbeef"]);
        let b = from_parts("clip", "https://e.com/a", &["100", "deadbeef"]);
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn different_payload_hash_yields_different_id() {
        let a = from_parts("clip", "https://e.com/a", &["100", "aaaa"]);
        let b = from_parts("clip", "https://e.com/a", &["100", "bbbb"]);
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn output_is_valid_ulid_for_substrate_parse() {
        let id = from_parts("clip", "https://e.com/a", &["100", "rev"]);
        let parsed = ConnectorEventId::parse(id.as_str()).expect("substrate accepts ULID");
        assert_eq!(parsed.as_str(), id.as_str());
    }

    proptest! {
        #[test]
        fn from_parts_is_deterministic(url in "[a-z]{1,12}", ts in any::<i64>(), body in ".{0,40}") {
            let h = payload_hash(body.as_bytes());
            let ts_s = ts.to_string();
            let a = from_parts("clip", &url, &[&ts_s, &h]);
            let b = from_parts("clip", &url, &[&ts_s, &h]);
            prop_assert_eq!(a.as_str(), b.as_str());
        }
    }
}
```

- [ ] **Step 2: Wire the module** — in `src/lib.rs`, add after `mod error;`:

```rust
mod event_id;
```

- [ ] **Step 3: Run the tests — verify they pass**

Run: `cargo nextest run -p cairn-connectors-webclip --locked event_id`
Expected: PASS (4 tests incl. the proptest).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-connectors-webclip/src/event_id.rs crates/cairn-connectors-webclip/src/lib.rs
git commit -m "feat(connectors-webclip): deterministic clip event IDs (#131)"
```

---

### Task 4: Clip parsing + content negotiation

**Files:**
- Create: `crates/cairn-connectors-webclip/src/clip.rs`
- Modify: `crates/cairn-connectors-webclip/src/lib.rs`

- [ ] **Step 1: Write `src/clip.rs`** (implementation + unit tests)

```rust
//! Parse a verified web-clip webhook request into a `ConnectorEvent`.
//!
//! Content negotiation on `Content-Type`:
//! - `application/json` → structured [`ClipEnvelope`] → `ConnectorPayload::Json`.
//! - `text/markdown` | `text/plain` → raw body + `X-Cairn-Clip-*` headers →
//!   `ConnectorPayload::Text`.

use std::collections::BTreeSet;

use cairn_connectors_core::{
    ConnectorEvent, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef, WebhookRequest,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::WebClipError;
use crate::event_id;

/// Connector name — must match the manifest `[connector] name`.
const CONNECTOR_NAME: &str = "webclip";

/// Structured clip envelope accepted in `application/json` mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClipEnvelope {
    /// Source page URL. Required; its host becomes the consent scope.
    pub url: String,
    /// Optional page title (rides in the payload for downstream use).
    #[serde(default)]
    pub title: Option<String>,
    /// Capture time, Unix seconds. Required.
    pub captured_at: i64,
    /// Selected text/HTML, if the user clipped a selection.
    #[serde(default)]
    pub selection: Option<String>,
    /// Markdown form of the clip body.
    #[serde(default)]
    pub markdown: Option<String>,
    /// Optional free-form user note.
    #[serde(default)]
    pub note: Option<String>,
    /// Optional user tags. Preserved in the payload; NOT promoted to labels.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Accepted clip media types after `Content-Type` normalization.
enum Media {
    /// `application/json` structured envelope.
    Json,
    /// `text/markdown` or `text/plain`; the carried `String` is the concrete
    /// MIME echoed into `ConnectorPayload::Text`.
    Text(String),
}

/// Resolve and normalize the request `Content-Type` into a [`Media`].
fn media_type(req: &WebhookRequest) -> Result<Media, WebClipError> {
    let raw = req
        .header("Content-Type")
        .ok_or(WebClipError::MissingContentType)?;
    // Strip parameters (`; charset=utf-8`) and normalize case/whitespace.
    let base = raw
        .split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "application/json" => Ok(Media::Json),
        "text/markdown" | "text/plain" => Ok(Media::Text(base)),
        other => Err(WebClipError::UnsupportedContentType(other.to_owned())),
    }
}

/// Read a required header, erroring with [`WebClipError::MissingField`] if absent.
fn header_required(req: &WebhookRequest, name: &'static str) -> Result<String, WebClipError> {
    req.header(name)
        .map(str::to_owned)
        .ok_or(WebClipError::MissingField(name))
}

/// Parse a `captured_at` string as Unix seconds.
fn parse_captured_at(s: &str) -> Result<i64, WebClipError> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| WebClipError::BadCapturedAt(s.to_owned()))
}

/// Parse a verified webhook request into exactly one [`ConnectorEvent`].
///
/// The substrate has already verified the HMAC signature; `signature_id` is the
/// verified signature header value, recorded in `DeliveryMode::Webhook`.
pub(crate) fn parse_request(
    req: &WebhookRequest,
    signature_id: &str,
) -> Result<ConnectorEvent, WebClipError> {
    let (url, captured_at, payload, hash_input) = match media_type(req)? {
        Media::Json => {
            let env: ClipEnvelope = serde_json::from_slice(&req.body)?;
            if env.selection.is_none() && env.markdown.is_none() {
                return Err(WebClipError::MissingField("selection|markdown"));
            }
            let body = serde_json::to_value(&env)?;
            let hash_input = serde_json::to_vec(&body)?;
            (
                env.url.clone(),
                env.captured_at,
                ConnectorPayload::Json {
                    mime: "application/json".to_owned(),
                    body,
                },
                hash_input,
            )
        }
        Media::Text(mime) => {
            let url = header_required(req, "X-Cairn-Clip-Url")?;
            let captured_at = parse_captured_at(&header_required(req, "X-Cairn-Clip-Captured-At")?)?;
            let text = String::from_utf8_lossy(&req.body).into_owned();
            let hash_input = req.body.clone();
            (
                url,
                captured_at,
                ConnectorPayload::Text { mime, body: text },
                hash_input,
            )
        }
    };

    // Per-domain consent scope from the URL host.
    let host = Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .ok_or_else(|| WebClipError::BadUrl(url.clone()))?;

    let captured_str = captured_at.to_string();
    let hash = event_id::payload_hash(&hash_input);
    let event_id = event_id::from_parts("clip", &url, &[&captured_str, &hash]);

    let mut labels = BTreeSet::new();
    labels.insert("source:web".to_owned());
    labels.insert("kind:clip".to_owned());

    Ok(ConnectorEvent::new(
        event_id,
        CONNECTOR_NAME,
        SourceRef::new("clip", url, None),
        captured_at,
        labels,
        ConnectorScope::new("domain", host),
        payload,
        DeliveryMode::Webhook {
            signature_id: signature_id.to_owned(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(headers: Vec<(&str, &str)>, body: &[u8]) -> WebhookRequest {
        WebhookRequest {
            connector: "webclip".into(),
            body: body.to_vec(),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn json_clip_builds_scoped_event() {
        let body = br#"{"url":"https://en.wikipedia.org/wiki/Cairn","captured_at":100,"markdown":"hi"}"#;
        let r = req(vec![("Content-Type", "application/json")], body);
        let ev = parse_request(&r, "sig").expect("parses");
        assert_eq!(ev.connector, "webclip");
        assert_eq!(ev.scope.kind, "domain");
        assert_eq!(ev.scope.value, "en.wikipedia.org");
        assert_eq!(ev.source_ref.kind, "clip");
        assert!(ev.labels.contains("source:web") && ev.labels.contains("kind:clip"));
        assert!(matches!(ev.payload, ConnectorPayload::Json { .. }));
    }

    #[test]
    fn json_charset_param_is_accepted() {
        let body = br#"{"url":"https://e.com/a","captured_at":1,"selection":"x"}"#;
        let r = req(vec![("Content-Type", "application/json; charset=utf-8")], body);
        assert!(parse_request(&r, "sig").is_ok());
    }

    #[test]
    fn text_markdown_uses_headers_for_metadata() {
        let r = req(
            vec![
                ("Content-Type", "text/markdown"),
                ("X-Cairn-Clip-Url", "https://example.com/post/42"),
                ("X-Cairn-Clip-Captured-At", "1748563200"),
            ],
            b"## Heading\nbody",
        );
        let ev = parse_request(&r, "sig").expect("parses");
        assert_eq!(ev.scope.value, "example.com");
        assert_eq!(ev.occurred_at, 1_748_563_200);
        assert!(matches!(ev.payload, ConnectorPayload::Text { .. }));
    }

    #[test]
    fn missing_content_type_is_error() {
        let r = req(vec![], b"{}");
        assert!(matches!(parse_request(&r, "s"), Err(WebClipError::MissingContentType)));
    }

    #[test]
    fn unsupported_content_type_is_error() {
        let r = req(vec![("Content-Type", "text/html")], b"<p>x</p>");
        assert!(matches!(
            parse_request(&r, "s"),
            Err(WebClipError::UnsupportedContentType(_))
        ));
    }

    #[test]
    fn json_without_body_field_is_error() {
        let body = br#"{"url":"https://e.com/a","captured_at":1}"#;
        let r = req(vec![("Content-Type", "application/json")], body);
        assert!(matches!(parse_request(&r, "s"), Err(WebClipError::MissingField(_))));
    }

    #[test]
    fn hostless_url_is_error() {
        let body = br#"{"url":"file:///etc/passwd","captured_at":1,"markdown":"x"}"#;
        let r = req(vec![("Content-Type", "application/json")], body);
        assert!(matches!(parse_request(&r, "s"), Err(WebClipError::BadUrl(_))));
    }

    #[test]
    fn text_missing_url_header_is_error() {
        let r = req(
            vec![
                ("Content-Type", "text/plain"),
                ("X-Cairn-Clip-Captured-At", "1"),
            ],
            b"body",
        );
        assert!(matches!(parse_request(&r, "s"), Err(WebClipError::MissingField(_))));
    }
}
```

- [ ] **Step 2: Wire the module** — in `src/lib.rs`, add after `mod event_id;`:

```rust
mod clip;
```

- [ ] **Step 3: Run the tests — verify they pass**

Run: `cargo nextest run -p cairn-connectors-webclip --locked clip`
Expected: PASS (8 unit tests).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-connectors-webclip/src/clip.rs crates/cairn-connectors-webclip/src/lib.rs
git commit -m "feat(connectors-webclip): clip envelope + content negotiation (#131)"
```

---

### Task 5: `WebClipConnector`

**Files:**
- Create: `crates/cairn-connectors-webclip/src/connector.rs`
- Modify: `crates/cairn-connectors-webclip/src/lib.rs`

- [ ] **Step 1: Write `src/connector.rs`** (impl + unit tests)

```rust
//! `WebClipConnector` — `Connector` + `ConnectorPlugin` for web clips.
//!
//! Webhook-only and stateless: `poll` is never called (the manifest declares
//! `poll = false`) and there is no credential cache or HTTP client.

use async_trait::async_trait;
use cairn_connectors_core::{
    CONTRACT_VERSION, Connector, ConnectorCapabilities, ConnectorError, ConnectorEvent,
    ConnectorManifest, ConnectorPlugin, ContractVersion, Identity, PollContext, PollOutcome,
    VersionRange, WebhookContext, WebhookRequest,
};

use crate::MANIFEST_TOML;
use crate::clip;

/// Generic web-clipper connector. Construct once per Cairn process.
pub struct WebClipConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl WebClipConnector {
    /// Construct a new web-clipper connector from the bundled manifest.
    ///
    /// # Errors
    /// Returns [`ConnectorError::Fatal`] if the bundled manifest or sensor
    /// identity fails to parse (a compile-time invariant, covered by tests).
    pub fn new() -> Result<Self, ConnectorError> {
        let manifest = ConnectorManifest::parse_toml(MANIFEST_TOML)
            .map_err(|e| ConnectorError::fatal_msg(format!("webclip manifest: {e}")))?;
        let sensor = Identity::parse("snr:local:connector:webclip:v1")
            .map_err(|e| ConnectorError::fatal_msg(format!("webclip sensor identity: {e:?}")))?;
        Ok(Self { manifest, sensor })
    }
}

#[async_trait]
impl Connector for WebClipConnector {
    fn name(&self) -> &str {
        self.manifest.name()
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: false,
            webhook: true,
            backfill: false,
        };
        &C
    }

    fn sensor_identity(&self) -> &Identity {
        &self.sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        <Self as ConnectorPlugin>::SUPPORTED_VERSIONS
    }

    // `poll = false` in the manifest, so the registry never calls this. The
    // trait still requires a body; return an empty outcome.
    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome::default())
    }

    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        // The substrate has already verified the HMAC signature; we use the
        // header value as the surrogate signature_id (same pattern as GitHub).
        let signature_id = req
            .header("X-Cairn-Signature-256")
            .unwrap_or("unverified")
            .to_owned();
        Ok(vec![clip::parse_request(req, &signature_id)?])
    }
}

impl ConnectorPlugin for WebClipConnector {
    const NAME: &'static str = "webclip";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(CONTRACT_VERSION, ContractVersion::new(0, 2, 0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn constructs_with_expected_identity() {
        let c = WebClipConnector::new().expect("constructs");
        assert_eq!(c.name(), "webclip");
        assert_eq!(c.sensor_identity().as_str(), "snr:local:connector:webclip:v1");
    }

    #[test]
    fn capabilities_are_webhook_only() {
        let c = WebClipConnector::new().unwrap();
        let caps = c.capabilities();
        assert!(!caps.poll && caps.webhook && !caps.backfill);
    }

    #[test]
    fn is_arc_dyn_connector() {
        let c: Arc<dyn Connector> = Arc::new(WebClipConnector::new().unwrap());
        assert_eq!(c.name(), "webclip");
    }

    #[tokio::test]
    async fn poll_returns_empty_outcome() {
        // `poll` is never called in production (poll=false) but must be inert.
        let c = WebClipConnector::new().unwrap();
        let cx = PollContext::new(
            Arc::new(cairn_connectors_core::CredentialHandle::empty()),
            None,
            0,
            tokio_util::sync::CancellationToken::new(),
        );
        let out = c.poll(&cx).await.expect("poll ok");
        assert!(out.events.is_empty());
    }
}
```

> **Note for the implementer:** the `poll_returns_empty_outcome` test references `tokio_util::sync::CancellationToken`. `tokio-util` is **not** a dependency of this crate. Drop that one test (the inert `poll` is also exercised end-to-end by the registry never calling it), OR if you want to keep it, add `tokio-util = { workspace = true }` to `[dev-dependencies]`. **Recommended: delete the `poll_returns_empty_outcome` test** — the other three unit tests plus the inert manifest flag fully cover the webhook-only contract, and it avoids an extra dev-dep.

- [ ] **Step 2: Wire the module + public export** — in `src/lib.rs`, add after `mod clip;`:

```rust
mod connector;

pub use connector::WebClipConnector;
```

- [ ] **Step 3: Run the tests — verify they pass**

Run: `cargo nextest run -p cairn-connectors-webclip --locked connector`
Expected: PASS (3 tests; the `poll_returns_empty_outcome` test deleted per the note).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-connectors-webclip/src/connector.rs crates/cairn-connectors-webclip/src/lib.rs
git commit -m "feat(connectors-webclip): WebClipConnector Connector impl (#131)"
```

---

### Task 6: Test kit

**Files:**
- Create: `crates/cairn-connectors-webclip/src/testkit.rs`
- Modify: `crates/cairn-connectors-webclip/src/lib.rs`

- [ ] **Step 1: Write `src/testkit.rs`**

```rust
//! Test-only helpers for building signed web-clip webhook requests.
//!
//! Cfg-gated; not part of the production API surface. Integration tests in
//! `tests/` use these to construct `WebhookRequest`s for direct
//! `ingest_webhook` calls.

use cairn_connectors_core::WebhookRequest;
use cairn_connectors_core::webhook::hex_hmac_sha256;

/// Build a signed `application/json` clip request from a raw JSON body.
#[must_use]
pub fn json_clip_request(secret: &[u8], json_body: &str) -> WebhookRequest {
    let sig = format!("sha256={}", hex_hmac_sha256(secret, json_body.as_bytes()));
    WebhookRequest {
        connector: "webclip".into(),
        body: json_body.as_bytes().to_vec(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Cairn-Signature-256".into(), sig),
        ],
    }
}

/// Build a signed text-mode clip request (`mime` = `text/markdown` or
/// `text/plain`) with the `X-Cairn-Clip-*` metadata headers.
#[must_use]
pub fn text_clip_request(
    secret: &[u8],
    mime: &str,
    url: &str,
    captured_at: i64,
    body: &str,
) -> WebhookRequest {
    let sig = format!("sha256={}", hex_hmac_sha256(secret, body.as_bytes()));
    WebhookRequest {
        connector: "webclip".into(),
        body: body.as_bytes().to_vec(),
        headers: vec![
            ("Content-Type".into(), mime.to_owned()),
            ("X-Cairn-Signature-256".into(), sig),
            ("X-Cairn-Clip-Url".into(), url.to_owned()),
            ("X-Cairn-Clip-Captured-At".into(), captured_at.to_string()),
        ],
    }
}
```

- [ ] **Step 2: Wire the module** — in `src/lib.rs`, add at the end:

```rust
/// Test-only helpers exposed for integration tests. Cfg-gated; not part of the
/// production API.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p cairn-connectors-webclip --all-targets --locked`
Expected: clean build (no warnings).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-connectors-webclip/src/testkit.rs crates/cairn-connectors-webclip/src/lib.rs
git commit -m "feat(connectors-webclip): testkit signed-request builders (#131)"
```

---

### Task 7: Direct `ingest_webhook` integration tests

**Files:**
- Create: `crates/cairn-connectors-webclip/tests/fixtures/clip_json.json`
- Create: `crates/cairn-connectors-webclip/tests/fixtures/clip_markdown.md`
- Create: `crates/cairn-connectors-webclip/tests/ingest_json_clip.rs`
- Create: `crates/cairn-connectors-webclip/tests/ingest_markdown_clip.rs`
- Create: `crates/cairn-connectors-webclip/tests/content_negotiation.rs`
- Create: `crates/cairn-connectors-webclip/tests/idempotent_event_id.rs`
- Create: `crates/cairn-connectors-webclip/tests/malformed_payload.rs`
- Create: `crates/cairn-connectors-webclip/tests/tags_not_promoted_to_labels.rs`

- [ ] **Step 1: Write the fixtures**

`tests/fixtures/clip_json.json`:
```json
{
  "url": "https://en.wikipedia.org/wiki/Cairn",
  "title": "Cairn - Wikipedia",
  "captured_at": 1748563200,
  "markdown": "## Cairn\nA cairn is a human-made pile of stones.",
  "note": "trail markers",
  "tags": ["hiking", "reference"]
}
```

`tests/fixtures/clip_markdown.md`:
```markdown
## Cairn

A cairn is a human-made pile (or stack) of stones raised for a purpose.
```

- [ ] **Step 2: Write `tests/ingest_json_clip.rs`**

```rust
//! JSON clip → one per-domain-scoped ConnectorEvent.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorPayload, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn json_clip_produces_one_scoped_event() {
    let connector = WebClipConnector::new().expect("construct");
    let body = include_str!("fixtures/clip_json.json");
    let req = testkit::json_clip_request(b"secret", body);

    let events = connector.ingest_webhook(&req, &ctx()).await.expect("ingest");
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.connector, "webclip");
    assert_eq!(e.source_ref.kind, "clip");
    assert_eq!(e.scope.kind, "domain");
    assert_eq!(e.scope.value, "en.wikipedia.org");
    assert_eq!(e.occurred_at, 1_748_563_200);
    assert!(e.labels.contains("source:web"));
    assert!(e.labels.contains("kind:clip"));
    assert!(matches!(e.payload, ConnectorPayload::Json { .. }));
}
```

- [ ] **Step 3: Write `tests/ingest_markdown_clip.rs`**

```rust
//! text/markdown clip body + X-Cairn-Clip-* headers → Text-payload event.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorPayload, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn markdown_clip_produces_text_payload_event() {
    let connector = WebClipConnector::new().expect("construct");
    let body = include_str!("fixtures/clip_markdown.md");
    let req = testkit::text_clip_request(
        b"secret",
        "text/markdown",
        "https://example.com/post/42",
        1_748_563_200,
        body,
    );

    let events = connector.ingest_webhook(&req, &ctx()).await.expect("ingest");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].scope.value, "example.com");
    assert!(matches!(events[0].payload, ConnectorPayload::Text { .. }));
}
```

- [ ] **Step 4: Write `tests/content_negotiation.rs`**

```rust
//! Content-Type acceptance / rejection table.

use std::sync::Arc;

use cairn_connectors_core::{
    Connector, ConnectorError, CredentialHandle, WebhookContext, WebhookRequest,
};
use cairn_connectors_webclip::WebClipConnector;
use rstest::rstest;

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

fn raw_request(content_type: &str, body: &[u8]) -> WebhookRequest {
    WebhookRequest {
        connector: "webclip".into(),
        body: body.to_vec(),
        headers: vec![("Content-Type".into(), content_type.into())],
    }
}

#[rstest]
#[case("text/html")]
#[case("application/xml")]
#[case("image/png")]
#[tokio::test]
async fn rejects_unsupported_content_type(#[case] content_type: &str) {
    let connector = WebClipConnector::new().unwrap();
    // media_type() errors before the body is read, so any body is fine.
    let req = raw_request(content_type, b"whatever");
    let result = connector.ingest_webhook(&req, &ctx()).await;
    assert!(
        matches!(result, Err(ConnectorError::MalformedPayload(_))),
        "expected MalformedPayload for {content_type}, got {result:?}"
    );
}

#[tokio::test]
async fn accepts_json_with_charset_parameter() {
    let connector = WebClipConnector::new().unwrap();
    let body = br#"{"url":"https://e.com/a","captured_at":1,"selection":"x"}"#;
    let req = WebhookRequest {
        connector: "webclip".into(),
        body: body.to_vec(),
        headers: vec![("Content-Type".into(), "application/json; charset=utf-8".into())],
    };
    assert!(connector.ingest_webhook(&req, &ctx()).await.is_ok());
}
```

- [ ] **Step 5: Write `tests/idempotent_event_id.rs`**

```rust
//! Re-delivery idempotency + content-sensitivity of the event id.

use std::sync::Arc;

use cairn_connectors_core::{Connector, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn same_clip_twice_yields_same_event_id() {
    let c = WebClipConnector::new().unwrap();
    let body = include_str!("fixtures/clip_json.json");
    let e1 = c.ingest_webhook(&testkit::json_clip_request(b"s", body), &ctx()).await.unwrap();
    let e2 = c.ingest_webhook(&testkit::json_clip_request(b"s", body), &ctx()).await.unwrap();
    assert_eq!(e1[0].event_id.as_str(), e2[0].event_id.as_str());
}

#[tokio::test]
async fn same_url_same_second_different_body_differs() {
    let c = WebClipConnector::new().unwrap();
    let b1 = r#"{"url":"https://e.com/a","captured_at":100,"markdown":"one"}"#;
    let b2 = r#"{"url":"https://e.com/a","captured_at":100,"markdown":"two"}"#;
    let e1 = c.ingest_webhook(&testkit::json_clip_request(b"s", b1), &ctx()).await.unwrap();
    let e2 = c.ingest_webhook(&testkit::json_clip_request(b"s", b2), &ctx()).await.unwrap();
    assert_ne!(e1[0].event_id.as_str(), e2[0].event_id.as_str());
}
```

- [ ] **Step 6: Write `tests/malformed_payload.rs`**

```rust
//! Adapter-side rejections all surface as ConnectorError::MalformedPayload.

use std::sync::Arc;

use cairn_connectors_core::{
    Connector, ConnectorError, CredentialHandle, WebhookContext, WebhookRequest,
};
use cairn_connectors_webclip::WebClipConnector;

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

async fn assert_malformed(req: WebhookRequest) {
    let connector = WebClipConnector::new().unwrap();
    let result = connector.ingest_webhook(&req, &ctx()).await;
    assert!(
        matches!(result, Err(ConnectorError::MalformedPayload(_))),
        "expected MalformedPayload, got {result:?}"
    );
}

fn json_req(body: &[u8]) -> WebhookRequest {
    WebhookRequest {
        connector: "webclip".into(),
        body: body.to_vec(),
        headers: vec![("Content-Type".into(), "application/json".into())],
    }
}

#[tokio::test]
async fn json_missing_url_is_malformed() {
    assert_malformed(json_req(br#"{"captured_at":1,"markdown":"x"}"#)).await;
}

#[tokio::test]
async fn json_missing_captured_at_is_malformed() {
    assert_malformed(json_req(br#"{"url":"https://e.com/a","markdown":"x"}"#)).await;
}

#[tokio::test]
async fn json_no_body_field_is_malformed() {
    assert_malformed(json_req(br#"{"url":"https://e.com/a","captured_at":1}"#)).await;
}

#[tokio::test]
async fn hostless_url_is_malformed() {
    assert_malformed(json_req(br#"{"url":"file:///x","captured_at":1,"markdown":"x"}"#)).await;
}

#[tokio::test]
async fn missing_content_type_is_malformed() {
    assert_malformed(WebhookRequest {
        connector: "webclip".into(),
        body: b"{}".to_vec(),
        headers: vec![],
    })
    .await;
}

#[tokio::test]
async fn text_mode_missing_url_header_is_malformed() {
    assert_malformed(WebhookRequest {
        connector: "webclip".into(),
        body: b"body".to_vec(),
        headers: vec![
            ("Content-Type".into(), "text/plain".into()),
            ("X-Cairn-Clip-Captured-At".into(), "1".into()),
        ],
    })
    .await;
}
```

- [ ] **Step 7: Write `tests/tags_not_promoted_to_labels.rs`**

```rust
//! User tags stay inside the payload; emitted labels are fixed.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorPayload, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn tags_stay_in_payload_not_labels() {
    let c = WebClipConnector::new().unwrap();
    let body = include_str!("fixtures/clip_json.json"); // contains "tags"
    let events = c.ingest_webhook(&testkit::json_clip_request(b"s", body), &ctx()).await.unwrap();
    let e = &events[0];

    // Exactly the two manifest-declared labels — tags are NOT promoted.
    assert_eq!(e.labels.len(), 2);
    assert!(e.labels.contains("source:web") && e.labels.contains("kind:clip"));

    // Tags survive inside the JSON payload as data.
    match &e.payload {
        ConnectorPayload::Json { body, .. } => {
            assert!(body.get("tags").is_some(), "tags must be preserved in payload");
        }
        other => panic!("expected Json payload, got {other:?}"),
    }
}
```

- [ ] **Step 8: Run all integration tests — verify they pass**

Run: `cargo nextest run -p cairn-connectors-webclip --locked`
Expected: PASS (all unit + integration tests).

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-connectors-webclip/tests/
git commit -m "test(connectors-webclip): ingest, content-neg, idempotency, malformed (#131)"
```

---

### Task 8: Registry end-to-end + verification

**Files:**
- Create: `crates/cairn-connectors-webclip/tests/registry_end_to_end.rs`

- [ ] **Step 1: Write `tests/registry_end_to_end.rs`**

```rust
//! End-to-end: signed clip → real ConnectorRegistry webhook router → PipelineEmit.
//!
//! Proves the adapter's event passes the substrate's full gate (HMAC verify →
//! consent → label → scope → redaction → spool → emit) with the real per-domain
//! scope, and that a registered-but-not-enabled connector has no route.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::hex_hmac_sha256;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, CredentialStore, InMemoryCredentialStore, PipelineEmit,
};
use cairn_connectors_webclip::{MANIFEST_TOML, WebClipConnector};
use cairn_core::contract::connector_consent::ConsentGrant;
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

const SECRET: &[u8] = b"clip-secret";

#[derive(Default)]
struct Capturer(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Capturer {
    async fn emit(&self, ev: CaptureEvent) -> Result<(), ConnectorError> {
        self.0.lock().expect("mutex unpoisoned").push(ev);
        Ok(())
    }
}

/// Build a grant whose manifest_hash matches the compiled-in connector.toml.
fn webclip_grant() -> ConsentGrant {
    let manifest_hash = ConnectorManifest::parse_toml(MANIFEST_TOML)
        .expect("webclip manifest valid")
        .hash();
    ConsentGrant::new(
        "webclip",
        manifest_hash,
        BTreeSet::from(["source:web".to_string(), "kind:clip".to_string()]),
        vec!["domain:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("valid identity"),
    )
}

#[tokio::test]
async fn signed_clip_reaches_pipeline_emit() {
    let capturer = Arc::new(Capturer::default());
    let creds = Arc::new(InMemoryCredentialStore::default());
    creds
        .put("connector/webclip/webhook_secret", SECRET.to_vec())
        .await
        .expect("secret put succeeds");

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::clone(&creds) as Arc<dyn CredentialStore>)
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(WebClipConnector::new().expect("construct"))
        .expect("register succeeds");
    reg.enable("webclip", webclip_grant())
        .await
        .expect("enable succeeds");

    let router = reg.webhook_router();

    let body = include_str!("fixtures/clip_json.json");
    let sig = format!("sha256={}", hex_hmac_sha256(SECRET, body.as_bytes()));
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/webclip")
        .header("content-type", "application/json")
        .header("X-Cairn-Signature-256", &sig)
        .body(Body::from(body))
        .expect("request builds");

    let resp = router.oneshot(req).await.expect("router responds");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "valid clip → 204");

    {
        let events = capturer.0.lock().expect("mutex unpoisoned");
        assert_eq!(events.len(), 1, "exactly one CaptureEvent emitted");
        assert_eq!(
            events[0].source_family,
            cairn_core::domain::capture::SourceFamily::External,
            "clip event must be source_family External",
        );
    }

    reg.shutdown().await;
}

#[tokio::test]
async fn not_enabled_connector_has_no_route() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(Capturer::default()) as Arc<dyn PipelineEmit>)
        .build();

    // Register but do NOT enable.
    reg.register(WebClipConnector::new().unwrap())
        .expect("register succeeds");

    let router = reg.webhook_router();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/webclip")
        .body(Body::from(b"{}".as_slice()))
        .expect("request builds");

    let resp = router.oneshot(req).await.expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "disabled connector must not have a webhook route (fail-closed)"
    );

    reg.shutdown().await;
}
```

- [ ] **Step 2: Run the end-to-end tests — verify they pass**

Run: `cargo nextest run -p cairn-connectors-webclip --locked registry_end_to_end`
Expected: PASS (`signed_clip_reaches_pipeline_emit`, `not_enabled_connector_has_no_route`).

> If `signed_clip_reaches_pipeline_emit` returns a non-204 status, print the response: a `400` means the manifest `allowed_mimes` or the grant `scope_patterns`/`allowed_labels` don't match what the adapter emits; a `401` means the secret key path is wrong (`connector/webclip/webhook_secret`).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-connectors-webclip/tests/registry_end_to_end.rs
git commit -m "test(connectors-webclip): registry router end-to-end emit (#131)"
```

- [ ] **Step 4: Run the full local verification suite (CLAUDE.md §8)**

```bash
cargo fmt --all --check
cargo clippy -p cairn-connectors-webclip --all-targets --locked -- -D warnings
cargo check -p cairn-connectors-webclip --all-targets --locked
cargo nextest run -p cairn-connectors-webclip --locked --no-fail-fast
cargo test --doc -p cairn-connectors-webclip --locked
./scripts/check-core-boundary.sh
cargo deny check
cargo machete
```
Expected: all green. Fix any clippy pedantic findings inline (e.g. add `#[must_use]`, doc comments). `cargo machete` must report no unused deps — if it flags one, remove it from `Cargo.toml`.

- [ ] **Step 5: Regenerate docs (workspace membership changed)**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
```
If this produces changes under `docs/site/src/reference/generated/`, stage them. If there is no diff, skip.

- [ ] **Step 6: Full-workspace regression check**

```bash
cargo nextest run --workspace --locked --no-fail-fast
```
Expected: no regressions in other crates.

- [ ] **Step 7: Commit any docgen output**

```bash
git add docs/site/src/reference/generated/ 2>/dev/null || true
git commit -m "docs: regenerate reference after adding cairn-connectors-webclip (#131)" || echo "no docgen diff"
```

---

## Self-Review

**1. Spec coverage:**
- §1 in-scope webhook-only adapter → Task 5. ✓
- §2 invariants (fail-closed, forbid unsafe, no core dep) → `#![forbid(unsafe_code)]` (Task 1/3), capabilities + `not_enabled_connector_has_no_route` (Task 8), cairn-core dev-only (Task 1 Cargo.toml). ✓
- §3 crate topology → Tasks 1–6 create exactly the listed files. ✓
- §4 manifest → Task 1 `connector.toml` + parse test. ✓
- §5 wire contract (endpoint, JSON/text negotiation, headers, captured_at) → Task 4 `clip.rs` + tests. ✓
- §6 event construction (event_id, scope, labels, tags-not-labels, delivery) → Tasks 3, 4, 7. ✓
- §7 Connector impl → Task 5. ✓
- §8 error mapping → Task 2. ✓
- §9 tests → Tasks 7, 8. ✓
- §10 acceptance/deviation: idempotency (`idempotent_event_id.rs`), auth/malformed (`malformed_payload.rs`, e2e 401/404), searchable/scoped (`signed_clip_reaches_pipeline_emit`). ✓
- §11 CI verification → Task 8 Steps 4–6. ✓
- §3.3 workspace wiring + docgen → Task 1 Step 4, Task 8 Step 5. ✓

**2. Placeholder scan:** No `TBD`/`TODO`/"handle errors appropriately". Every code step shows full code. The one conditional (the `poll_returns_empty_outcome` test needing `tokio-util`) is resolved explicitly with a recommended action (delete it). ✓

**3. Type consistency:** `WebClipConnector::new() -> Result<Self, ConnectorError>` used identically in Tasks 5, 7, 8. `clip::parse_request(req, signature_id)` signature matches its definition and its caller in `connector.rs`. `event_id::from_parts(kind, url, components)` / `payload_hash(bytes)` match between Task 3 and Task 4. `testkit::json_clip_request` / `text_clip_request` signatures match between Task 6 and Tasks 7/8. Manifest name `"webclip"`, scope `domain:*`, labels `{source:web, kind:clip}`, secret key `connector/webclip/webhook_secret`, signature header `X-Cairn-Signature-256` are consistent across manifest, adapter, testkit, and the e2e grant. ✓

---

## Execution Handoff

(Filled in by the brainstorming/writing-plans operator after saving.)
