# Issue #131 — GitHub connector adapter (first slice)

- **Issue:** [#131 — P2 Implement GitHub, email, Drive/OneDrive, Notion, and web clipper adapters](https://github.com/windoliver/cairn/issues/131)
- **Parent epic:** #29 Source connectors and aggregate memory extension
- **Substrate dependency:** #130 connector framework (closed; landed as `cairn-connectors-core`)
- **Design brief reference:** §19 v0.3 connector set; §9.1 source sensors
- **Phase:** v0.3 — Federation + evolution (P2)

---

## 1. Scope

Issue #131 enumerates **five** connector adapters: GitHub, email, Drive/OneDrive, Notion, web clipper. Per CLAUDE.md §5 ("keep the diff scoped"), they decompose into five sibling PRs, each its own L2 crate under `crates/cairn-connectors-<vendor>/`. **This spec covers only the GitHub adapter** — the first slice. The four remaining adapters are deferred to follow-up PRs that reuse this design's structure mechanically.

### 1.1 In scope (this PR)

- New crate `cairn-connectors-github` implementing `cairn_connectors_core::Connector` for three GitHub resources: **issues**, **pull requests**, **commits**.
- Both auth modes: **Personal Access Token (PAT)** and **GitHub App** (installation tokens minted from an RSA-signed JWT).
- **Poll + webhook** delivery in one PR.
  - Poll: REST `since`-based incremental sync for issues/PRs; sha-walking for commits; full backfill when cursor is unset.
  - Webhook: HMAC-SHA256 verification (already implemented in substrate); dispatch on `X-GitHub-Event`; dedup via `X-GitHub-Delivery`.
- Rate-limit handling honoring `X-RateLimit-Remaining` / `X-RateLimit-Reset` and `429` responses.
- Bundled `connector.toml` manifest validated at construction time.
- Wiremock-based integration tests against recorded fixture JSON.

### 1.2 Out of scope (this PR, tracked separately)

- Email (IMAP + webhook), Drive (Google + OneDrive), Notion, generic web-clipper adapter crates — follow-up PRs against #131.
- `cairn admin connector_enable` / `connector_disable` / `connector_backfill` CLI verbs — those belong to #29 epic, not the per-adapter slice.
- GitLab adapter — not part of #131 (the brief lists GitLab under #29's broader scope but #131 names only GitHub).
- Aggregate-memory rollups (`cairn.aggregate.v1`) — issue #132.
- Enterprise SaaS connector catalog UI (explicit non-goal in issue body).

---

## 2. Load-bearing invariants this PR touches

From CLAUDE.md §4:

| Invariant | How this PR honors it |
|---|---|
| 1. Harness-agnostic | No code path references Claude Code, Codex, etc. |
| 3. CLI is ground truth | Adapter exposes no verbs; substrate already exposes `connector_enable` shape behind §3 epic. |
| 4. Seven contracts | Implements `Connector` only; no new contract added. |
| 5. WAL + two-phase apply | Emits `ConnectorEvent`s; substrate persists via existing WAL hook. Adapter never writes the DB. |
| 6. Fail closed on capability | Manifest's `capabilities` block is authoritative; if `webhook=false`, the webhook route is not mounted. |
| 7. `#![forbid(unsafe_code)]` | Applied at crate root. |
| 8. No `unwrap()` / `expect()` in core | Adapter is **not** core; `expect("invariant: …")` is tolerated. None of the new code is in `cairn-core`. |
| 9. Privacy by construction | Manifest labels gate every emit; raw HTTP bodies never logged above `trace`; access tokens wrapped in `SecretString`. |
| 10. Sources immutable | Adapter only reads from GitHub; never writes back. |

---

## 3. Crate topology

```
crates/cairn-connectors-github/
├── Cargo.toml
├── connector.toml                 # bundled at compile time via include_str!
├── src/
│   ├── lib.rs                     # pub use GitHubConnector; #![forbid(unsafe_code)]
│   ├── connector.rs               # Connector + ConnectorPlugin impls; orchestrates resources
│   ├── auth.rs                    # GitHubAuth { Pat, App }; installation-token cache
│   ├── client.rs                  # GhClient (reqwest wrapper); base_url injectable; RateState
│   ├── cursor.rs                  # CursorState (JSON serde); ResourceCursor variants
│   ├── error.rs                   # GhError -> ConnectorError mapping
│   ├── webhook.rs                 # X-GitHub-Event dispatch; X-GitHub-Delivery dedup key
│   └── resources/
│       ├── mod.rs                 # trait GhResource; ResourcePoll struct
│       ├── issues.rs              # IssuesResource: REST since-poll + issues/issue_comment webhook
│       ├── prs.rs                 # PrsResource: REST since-poll + pull_request/* webhook
│       └── commits.rs             # CommitsResource: sha-walk poll + push webhook
└── tests/
    ├── fixtures/
    │   ├── issues_page_1.json
    │   ├── issues_page_2.json
    │   ├── prs_page_1.json
    │   ├── commits_page_1.json
    │   ├── webhook_issues_opened.json
    │   ├── webhook_pull_request_opened.json
    │   ├── webhook_push.json
    │   ├── rate_limit_429.json
    │   └── installation_token.json
    ├── poll_issues_fixture.rs
    ├── poll_prs_fixture.rs
    ├── poll_commits_fixture.rs
    ├── backfill_cursor_rewind.rs
    ├── webhook_issues_opened.rs
    ├── webhook_pull_request.rs
    ├── webhook_push.rs
    ├── rate_limit_429.rs
    ├── auth_pat_bearer_header.rs
    ├── auth_app_jwt_then_installation_token.rs
    ├── auth_app_token_refresh.rs
    ├── disabled_connector_no_poll.rs
    └── consent_revoked_drops_events.rs
```

### 3.1 Dependency rules

- New crate depends on `cairn-connectors-core` only — **never** on `cairn-core` directly. Enforced by `scripts/check-core-boundary.sh`.
- No cross-adapter imports (`cairn-connectors-github` does not depend on a future `cairn-connectors-email` and vice versa).

### 3.2 Cargo.toml deps

| Dep | Use | Notes |
|---|---|---|
| `cairn-connectors-core` | substrate | workspace dep, no features needed |
| `cairn-core` | type re-exports only (`Identity`, `ContractVersion`) | via `cairn-connectors-core` re-exports preferred; direct dep only if substrate doesn't re-export needed types |
| `reqwest` | HTTP | `default-features = false`, `features = ["rustls-tls", "json", "gzip"]` |
| `serde` / `serde_json` | wire types + cursor | workspace |
| `jsonwebtoken` | App JWT (RS256) | **new dep** — justify in PR. Footprint ~150 KB; alternatives (`josekit`, hand-rolled RSA-SHA256) are heavier or unsafe. |
| `secrecy` | token wrapping | already in workspace via `cairn-keychain` |
| `arc-swap` | installation-token cache | workspace |
| `tokio` | runtime | workspace |
| `async-trait` | internal `GhResource` trait | workspace |
| `tracing` | logging | workspace |
| `thiserror` | `GhError` | workspace |
| `time` or `chrono` | `since` timestamp + JWT exp | match what the workspace already uses; do not introduce a second date crate |
| `bon` | builders for `Repo`, `GhClient` | workspace |
| **dev-dep** `wiremock` | HTTP mock server | **new dev-dep**. Used in `tests/` only. |
| **dev-dep** `tokio` features `["macros", "rt-multi-thread"]` | `#[tokio::test]` | already pattern-used elsewhere |
| **dev-dep** `insta` | snapshot tests on parsed events | workspace |

---

## 4. Manifest (`connector.toml`)

```toml
[connector]
name              = "github"
contract          = "Connector"
contract_version  = "0.1.0"
sensor_identity   = "snr:local:connector:github:v1"

[capabilities]
poll     = true
webhook  = true
backfill = true

[oauth]
required_scopes = ["repo", "read:org"]
token_lifetime  = "1h"      # tightest case (App installation token); PAT mode ignores
refresh         = true

[budget]
max_items_per_hour = 1000
max_bytes_per_day  = "50MiB"

[labels]
allowed = ["source:github", "kind:issue", "kind:pr", "kind:commit", "kind:comment"]

[[scopes.declared]]
pattern = "github://*/*"

[webhook]
signing_header = "X-Hub-Signature-256"
# Secret rotation is supported by the substrate; manifest only declares the header.

[poll]
cadence_seconds  = 300
backoff_seconds  = 60

[payload]
max_bytes = "512KiB"
max_depth = 32
```

Bundled at compile time via `include_str!("../connector.toml")` and validated through `cairn_connectors_core::manifest::ConnectorManifest::parse_toml` inside `GitHubConnector::new`. Any drift (label not declared, scope not matched) is caught by substrate-side gates at emit time.

---

## 5. Auth model

```rust
// auth.rs
pub enum GitHubAuth {
    Pat(SecretString),
    App {
        app_id: u64,
        private_key_pem: SecretString,
        installation_id: u64,
        cached: arc_swap::ArcSwap<Option<InstallationToken>>,
    },
}

#[derive(Clone)]
pub struct InstallationToken {
    pub token: SecretString,
    pub expires_at: OffsetDateTime,
}

impl GitHubAuth {
    /// Returns a `Bearer` token suitable for the `Authorization` header.
    pub async fn bearer(&self, http: &reqwest::Client) -> Result<SecretString, GhError>;
}
```

### 5.1 PAT path

- `bearer()` returns the wrapped PAT verbatim.
- No refresh; no JWT.

### 5.2 GitHub App path

- Mint JWT (RS256) — claims: `iss = app_id`, `iat = now - 60s`, `exp = now + 540s` (GitHub max is 10 min; 9 min stays safely under).
- POST `/app/installations/{installation_id}/access_tokens` with `Authorization: Bearer <jwt>`.
- Parse response into `InstallationToken { token, expires_at }`.
- Cache via `ArcSwap<Option<InstallationToken>>`. Subsequent calls reuse the cached token until `expires_at - 90s`, then refresh.
- Refresh is single-flight (acquired through `tokio::sync::Mutex` held only over the cache-update window, not over the HTTP call) to prevent thundering-herd JWT minting.

### 5.3 Credential resolution from substrate

`GitHubConnector::new` accepts `Arc<CredentialHandle>` from the substrate's `CredentialStore`. A small `auth::from_handle(handle)` resolver inspects the handle's discriminant and constructs the matching `GitHubAuth` variant. The discriminant is **not** a public enum field on the adapter; the substrate's `CredentialHandle` already encodes credential shape.

Open detail (resolve in implementation): the substrate's current `CredentialHandle` shape (see `crates/cairn-connectors-core/src/credential.rs`) may or may not cleanly carry an App's `(app_id, installation_id, private_key_pem)` triple. If it does not, the resolver reads the App fields from connector-scoped config (`.cairn/config.yaml`) and the handle holds only the private-key reference. **Decision deferred to implementation review** — both paths satisfy the spec.

---

## 6. HTTP client (`client.rs`)

```rust
pub struct GhClient {
    http: reqwest::Client,
    base_url: Url,                   // default https://api.github.com, overridable for tests
    auth: GitHubAuth,
    rate_state: arc_swap::ArcSwap<RateState>,
}

#[derive(Clone, Default)]
pub struct RateState {
    pub remaining: Option<u32>,
    pub reset_at: Option<OffsetDateTime>,
}
```

- `GhClient::get_json(path, query) -> Result<T, GhError>` — single entry point for all REST calls.
- Records `X-RateLimit-Remaining` / `X-RateLimit-Reset` from every response into `rate_state` (atomic swap).
- Maps 401/403 → `ConnectorError::AuthFailure`, 429 → `ConnectorError::RateLimited { retry_after }`, 5xx → `ConnectorError::Transient`, malformed JSON → `ConnectorError::MalformedPayload`.
- User-Agent header: `cairn-connectors-github/<crate-version>` (per GitHub's API requirements).

---

## 7. Resource trait + dispatch

```rust
// resources/mod.rs
#[async_trait::async_trait]
pub(crate) trait GhResource: Send + Sync {
    fn kind(&self) -> &'static str;          // "issue" | "pr" | "commit"

    async fn poll(
        &self,
        client: &GhClient,
        repo: &Repo,
        sub_cursor: Option<&ResourceCursor>,
        budget: u32,
    ) -> Result<ResourcePoll, GhError>;

    fn parse_webhook(
        &self,
        event_type: &str,
        delivery_id: &str,
        body: &[u8],
    ) -> Result<Vec<ConnectorEvent>, GhError>;
}

pub(crate) struct ResourcePoll {
    pub events: Vec<ConnectorEvent>,
    pub next_cursor: ResourceCursor,
    pub rate_limit_hint: Option<Duration>,
}
```

`GitHubConnector::poll(cx)`:

1. Deserialize `cx.last_cursor` into `CursorState` (or default if `None`).
2. Compute per-resource budget split — equal share by default; remainder goes to issues.
3. For each enabled resource: call `resource.poll(&client, &repo, sub_cursor, budget_share).await`.
4. Merge `ResourcePoll`s: concat events, rebuild `CursorState`, take the **max** `rate_limit_hint` across resources.
5. Serialize cursor back to JSON string; return `PollOutcome`.

`GitHubConnector::ingest_webhook(req, cx)`:

1. Read `X-GitHub-Event` and `X-GitHub-Delivery` (case-insensitive lookup is provided by substrate's `WebhookRequest::header`).
2. Dispatch by event type to the matching `GhResource::parse_webhook`:
   - `issues`, `issue_comment` → `IssuesResource`
   - `pull_request`, `pull_request_review`, `pull_request_review_comment` → `PrsResource`
   - `push` → `CommitsResource`
   - other → empty vec, `tracing::debug!` log (no error)
3. Set `ConnectorEvent::delivery_mode = DeliveryMode::Webhook { signature_id, delivery_id }` so substrate's replay guard dedups on `X-GitHub-Delivery`.

---

## 8. Cursor format

JSON object, opaque to the substrate, serialized to the `PollContext::last_cursor` string:

```json
{
  "v": 1,
  "issues":  {"since": "2026-05-25T12:00:00Z", "page": 3},
  "prs":     {"since": "2026-05-25T12:00:00Z", "page": 1},
  "commits": {"last_sha": "abc123def", "branch": "main"}
}
```

- `v: 1` is a schema version for forward compatibility; deserializer accepts any `v >= 1` and ignores unknown keys for forward compat.
- Missing per-resource entries default to "from epoch" (full backfill).
- Empty cursor = first run = full backfill for every enabled resource.

**Backfill control**: setting `last_cursor = None` (e.g., when an operator runs `connector_backfill` from #29 epic) re-pulls from epoch. Until that CLI verb lands, backfill is exercised by the test `backfill_cursor_rewind.rs`.

---

## 9. Rate limit + error handling

| Upstream signal | Adapter behavior |
|---|---|
| `X-RateLimit-Remaining < 50` | Set `rate_limit_hint = reset_at - now`; stop the current resource's pagination cleanly. |
| `429 Too Many Requests` with `Retry-After` | Set `rate_limit_hint = Retry-After`; bail current resource, keep accumulated events. |
| `429` without `Retry-After` | Set `rate_limit_hint = max(60s, reset_at - now)`. |
| `401` / `403 Bad credentials` | `ConnectorError::AuthFailure { source }`. **No data lost** — events accumulated so far in this poll are still returned via the `PollOutcome` returned in `Ok`; the auth error short-circuits **subsequent** resources only. Acceptance criterion: "rate limits and auth failures are reported without data corruption." |
| `5xx` | `ConnectorError::Transient`. Substrate's retry policy applies. |
| `404 on resource` | Treat as empty — log `tracing::warn!` and return empty `ResourcePoll`. |

`PollOutcome.next_cursor` is committed **only on success**. On `Err`, the substrate keeps the prior cursor. This is the data-corruption guarantee: a failed poll never advances the cursor past an unread page.

---

## 10. Webhook delivery

- Substrate verifies `X-Hub-Signature-256` HMAC **before** `ingest_webhook` is called (see `cairn-connectors-core::webhook::verify_hmac_sha256`). Adapter does **not** re-verify.
- `X-GitHub-Delivery` is the dedup key. Adapter places it in `ConnectorEvent::delivery_mode = Webhook { signature_id, delivery_id }`; substrate's replay guard rejects repeats.
- Event-type dispatch table:

| `X-GitHub-Event` | Resource | Emitted `SourceRef.kind` |
|---|---|---|
| `issues` | issues | `issue` |
| `issue_comment` | issues | `comment` |
| `pull_request` | prs | `pr` |
| `pull_request_review` | prs | `pr_review` |
| `pull_request_review_comment` | prs | `pr_review_comment` |
| `push` | commits | `commit` |
| `ping` | (none) | `Ok(vec![])` |
| (other) | (none) | `Ok(vec![])` + `tracing::debug!` |

`ping` deliveries return empty so GitHub's installation handshake succeeds without polluting the event stream.

---

## 11. Test strategy

### 11.1 Integration tests (`tests/`)

Every integration test:
1. Spins up a `wiremock::MockServer`.
2. Loads fixture JSON via `include_str!` from `tests/fixtures/`.
3. Constructs `GhClient` with `base_url = mock_server.uri()`.
4. Calls `GitHubConnector::poll` or `ingest_webhook` against the real substrate registry.
5. Asserts on the returned `PollOutcome` / `Vec<ConnectorEvent>`.

Per-test summary:

| Test file | What it proves |
|---|---|
| `poll_issues_fixture.rs` | Two-page issues poll; events carry `kind:issue`; cursor advances `since` + `page`. |
| `poll_prs_fixture.rs` | Same for PRs. |
| `poll_commits_fixture.rs` | Sha-walk advances `last_sha`. |
| `backfill_cursor_rewind.rs` | `last_cursor = None` walks from epoch. |
| `webhook_issues_opened.rs` | Real HMAC + substrate router → ConnectorEvent with `kind:issue`. |
| `webhook_pull_request.rs` | Same for PR. |
| `webhook_push.rs` | Push → one ConnectorEvent per commit. |
| `rate_limit_429.rs` | 429 with `X-RateLimit-Reset` → `rate_limit_hint` set, cursor **not** advanced. |
| `auth_pat_bearer_header.rs` | Authorization header == `Bearer <pat>`. |
| `auth_app_jwt_then_installation_token.rs` | First request mints JWT, second uses installation token, third reuses cached token. |
| `auth_app_token_refresh.rs` | Mock token with `expires_at = now+30s` triggers refresh on next call. |
| `disabled_connector_no_poll.rs` | Substrate `disable` → no further `poll` calls land. |
| `consent_revoked_drops_events.rs` | Mutated manifest hash → substrate rejects emit. |

### 11.2 Unit tests (inline `#[cfg(test)] mod tests`)

- `cursor.rs` — serde round-trip + `proptest!` over arbitrary `ResourceCursor` JSON; unknown-key tolerance.
- `auth.rs` — JWT claims (iss, iat, exp window); expiry-90s refresh trigger.
- `resources/issues.rs`, `prs.rs`, `commits.rs` — `parse_webhook` payload-parse cases per event type, including malformed bodies → `ConnectorError::MalformedPayload`.
- `webhook.rs` — event-type dispatch table; `ping` returns empty; unknown event returns empty.

### 11.3 No mocking the substrate

Per CLAUDE.md §6.4: integration tests use the **real** `ConnectorRegistry`, **real** `WebhookRouter`, **real** `RedactionPipeline`. Only the network boundary is mocked (`wiremock`). This is the equivalent of "no mocking the DB."

---

## 12. CI verification

Commands run before opening the PR (per CLAUDE.md §8):

```bash
cargo fmt --all --check
cargo clippy -p cairn-connectors-github --all-targets --locked -- -D warnings
cargo check -p cairn-connectors-github --all-targets --locked
cargo nextest run -p cairn-connectors-github --locked --no-fail-fast
cargo test --doc -p cairn-connectors-github --locked
./scripts/check-core-boundary.sh
cargo deny check
cargo audit --deny warnings
cargo machete
```

Full workspace `cargo nextest run --workspace` to ensure nothing else regressed.

---

## 13. Acceptance criteria mapping

Issue #131 acceptance:

| Criterion | Where addressed |
|---|---|
| Each connector can backfill and incrementally sync fixture data | `poll_*_fixture.rs` (incremental) + `backfill_cursor_rewind.rs` (backfill). |
| Rate limits and auth failures are reported without data corruption | `rate_limit_429.rs` + auth tests; §9 above states cursor-not-advanced-on-error guarantee. |
| Imported records remain searchable, scoped, and forgettable | Events flow through the unchanged substrate pipeline → existing `cairn-core` search / forget paths; covered by substrate's own tests. |

Issue #131 verification checklist items 1–3 map to §11.1 + §11.2 above.

---

## 14. Risks and open items

| Risk | Mitigation |
|---|---|
| `jsonwebtoken` is a new workspace dep | Justified in §3.2; alternative is RustCrypto's `rsa` + `sha2` hand-rolled, which we should avoid. PR description will call this out. |
| Substrate `CredentialHandle` shape may not carry App triple | §5.3 lists the fallback path (read from config); pick the cleaner option during implementation, document the choice in the PR. |
| GitHub API changes / new event types | `parse_webhook` returns empty on unknown event types; no panic. Spec for new events lands as a follow-up PR. |
| Wiremock as a new dev-dep | Dev-only; not in the release binary. Already mainstream in Rust HTTP testing. |
| Per-resource budget split fairness | Spec says equal split with remainder to issues. If empirical traffic shows starvation, revisit — but only when measured, not preemptively (CLAUDE.md "don't design for hypothetical future requirements"). |

---

## 15. Follow-ups (not in this PR)

1. Email adapter (`cairn-connectors-email`) — IMAP + webhook, follows the same structure with `MessageResource` etc.
2. Drive / OneDrive adapter (`cairn-connectors-drive`) — Google Drive + Microsoft Graph, OAuth 2.0 user flow.
3. Notion adapter (`cairn-connectors-notion`) — Notion API; webhooks landed Q1 2026.
4. Generic web-clipper (`cairn-connectors-webclip`) — accepts POST from browser extension; minimal auth (shared secret).
5. `cairn admin connector_*` CLI verbs — part of #29 epic, separate PR.

Each follow-up gets its own spec under `docs/superpowers/specs/` and reuses this design's resource-trait + cursor-state pattern.
