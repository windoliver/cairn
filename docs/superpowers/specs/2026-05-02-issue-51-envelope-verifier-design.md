# Issue #51 — Envelope verifier design

- **Issue**: [#51 — Validate signed intent envelopes before disk writes](https://github.com/windoliver/cairn/issues/51)
- **Parent epic**: #7 (identity, signed envelope, status, handshake)
- **Phase**: v0.1 (P0)
- **Brief sources**: §4.2 Signed payload schema, §8.0.b Envelope, §5.6 WAL PREPARE
- **Date**: 2026-05-02

## 1. Goal

Replace the P0 placeholder verifier (`cairn-core/src/verifier.rs`, syntactic
checks only) with a production-grade `EnvelopeVerifier` that performs real
Ed25519 signature verification, expiry, key-version, revocation, and scope
checks before any SQLite mutation.

The verifier is the single trust boundary that mints
`VerifiedSignedIntent` proof tokens consumed by pipeline and WAL code. Every
adapter (CLI, MCP, SDK) must route through it.

Replay/nonce/sequence/handshake-challenge enforcement is **not** part of #51 —
that work lands in #52, which is explicitly blocked by this issue.

## 2. Non-goals

- Replay-ledger integration (`operation_id`, `nonce`, `sequence`,
  `server_challenge` consumption) — owned by #52.
- Network authentication for MCP transport — explicitly out of scope per the
  issue.
- Per-issuer scope grants stored in the registry — P0 uses vault-anchored
  scope policy (§4 below). Per-issuer scope grants are a future extension.
- Skew-tolerance configurability — hard-coded 60 s at P0; configurable later.
- Migrating existing ad-hoc `chrono::Utc::now()` call sites in
  `domain/identity/*` to the new `Clock` trait. New code uses the trait;
  existing call sites stay until a separate sweep.

## 3. Decisions (locked during brainstorming)

| # | Decision | Reason |
|---|----------|--------|
| Q1 | Verifier is **synchronous**; caller pre-resolves `(VerifyingKey, state)` from registry. | Keeps `cairn-core::verifier` a pure function, testable without a runtime; honours CLAUDE.md §6.3 ban on `block_on` inside async; registry I/O happens once at the verb boundary, where the caller already needs a registry handle. |
| Q2 | Scope check is **config-anchored** (`tenant`, `workspace`, allowed tiers from `.cairn/config.yaml`). | P0 single-author model — all issuers in one vault target the same `(tenant, workspace)`. Mismatched scope is misuse, not a per-issuer permission decision. Avoids extending `IdentityRegistry` schema. |
| Q3 | **Defer** replay error variants. | YAGNI — variant without a producer is dead code. #52 adds variants together with their first user. Wire-error code already exists in `generated/errors`, decoupled from `DomainError`. |
| Q4 | Reuse `domain/canonical.rs::write_canonical` to derive signed-payload bytes. | One canonicalizer, one bug surface; producer and verifier share encoding rules; avoids JCS-style string-surgery footguns. |
| Q5 | Long-lived `EnvelopeVerifier` struct, deps injected at construction. | Adapters build one at startup; lower per-call argument count; #52 adds a replay-ledger handle as a struct field, not a 5th parameter. |
| Q6 | Keep the existing sealed `SignedIntentVerifier` trait + `VerifierWitness`. | Stronger guarantee than `pub(crate) fn`. Existing scaffolding accommodates reuse; production mint path stays singular. |
| Q7 | Add a minimal `Clock` trait in `core::domain::time`. | No existing clock abstraction in `cairn-core`; identity-receipt code reaches for `chrono::Utc::now()` ad hoc, but new verifier code injects its clock. |
| Q8 | Centralized `envelope_error_for(&DomainError) -> ErrorBody` mapper in `core::error::wire`. | Wire `error.code` + `error.data` shapes are validated by the generated `Response` deserializer; need a fully-typed `ErrorBody`, not just a code. Single source of wire-error truth across CLI/MCP/SDK. |
| Q9 | Hybrid test layout (unit + DB-isolation integration + wire-error snapshot). | The issue's three verification flavours (crypto correctness, DB isolation, wire stability) map cleanly onto three test homes. |

## 4. Architecture

```
cairn-core
├── domain/
│   ├── time.rs                    [NEW]   Clock trait, SystemClock,
│   │                                       FixedClock (test-only)
│   ├── intent.rs                  [keep]  VerifiedSignedIntent + sealed
│   │                                       SignedIntentVerifier trait +
│   │                                       VerifierWitness
│   ├── canonical.rs               [extend] add canonical_bytes_signed_intent()
│   ├── error.rs                   [extend] add InvalidSignature, ExpiredIntent,
│   │                                       RevokedKey, KeyVersionMismatch,
│   │                                       ScopeDenied, Unauthorized variants
│   └── identity/                  [no change]
├── verifier/
│   ├── mod.rs                     [REWRITE] EnvelopeVerifier struct + impl
│   ├── resolved.rs                [NEW]   ResolvedIssuer
│   ├── policy.rs                  [NEW]   ScopePolicy + from_config()
│   └── resolve.rs                 [NEW]   async fn resolve_issuer(...)
└── error/
    └── wire.rs                    [NEW]   envelope_error_for(&DomainError)
                                            -> generated::common::ErrorBody

cairn-cli, cairn-mcp, cairn-sdk
└── (each: build EnvelopeVerifier at startup; verb wrappers call
     resolve_issuer + verifier.verify before WAL prep)

cairn-test-fixtures
└── (dev-dep only) signed_intent(), fixed_clock_at(), scope_policy_default()
```

### 4.1 Trust boundary

Every adapter calls `verifier.verify(intent, &resolved)` exactly once at the
boundary, before any `MemoryStore` mutation or WAL row write.
`VerifiedSignedIntent` is the typed proof token carried into pipeline and WAL
code; raw `SignedIntent` does not cross the verifier line.

### 4.2 Invariants preserved

- `cairn-core` stays I/O-free — registry is a trait, not an adapter call.
- No new workspace dep on adapter crates from `cairn-core`.
- `#![forbid(unsafe_code)]` workspace-level.
- No `unwrap()`/`expect()` in `cairn-core`.
- `scripts/check-core-boundary.sh` remains green.

## 5. Verification control flow

`EnvelopeVerifier::verify(intent, resolved) -> Result<VerifiedSignedIntent, DomainError>`

Order matters — fail-closed at the cheapest first-failing check:

1. **Wire-shape preconditions** — already enforced by IDL deserializer
   (`SignedIntent::try_from(RawSignedIntent)`); verifier asserts as
   `debug_assert!` invariant, not user-facing failure.
2. **Issuer ↔ ResolvedIssuer match** — `intent.issuer == resolved.identity`,
   else `Unauthorized` (caller-bug guard; never reached under correct caller
   code).
3. **Key-version match** — `intent.key_version == resolved.key_version`, else
   `Unauthorized` (defensive caller-bug guard; `KeyVersionMismatch` is raised
   earlier by `resolve_issuer` when the registry holds no row at the
   requested version — see §6.5).
4. **Lifecycle / revocation** — `resolved.state == Active`, else `RevokedKey`.
   Pending / Revoked / PurgePending / Purged all reject; visibility filter
   chosen by caller; verifier still checks for defence-in-depth.
5. **Expiry** — `clock.now() < expires_at` and
   `clock.now() >= issued_at - 60 s skew`, else `ExpiredIntent`.
6. **Scope policy** —
   - `intent.scope.tenant == policy.tenant`, else `ScopeDenied`
   - `intent.scope.workspace == policy.workspace`, else `ScopeDenied`
   - `intent.scope.tier ∈ policy.allowed_tiers`, else `ScopeDenied`
7. **Canonical-payload signature** —
   ```
   bytes = canonical_bytes_signed_intent(&intent)   // strips `signature`
   resolved.verifying_key.verify(bytes, intent.signature)
   ```
   else `InvalidSignature`.
8. **Mint** `VerifiedSignedIntent` via the sealed witness.

Step 7 is last so timing leaks reveal nothing exploitable: a tampered
signature on an expired intent fails at 5, never reaching the crypto step.

### 5.1 Adapter boundary flow (e.g., CLI `ingest`)

```rust
// 1. Parse Request envelope (IDL deserializer enforces wire shape).
let request: Request = serde_json::from_slice(&bytes)?;

// 2. Resolve the issuer's verifying key + lifecycle state.
let resolved = resolve_issuer(
    &registry,
    &request.signed_intent.issuer,
    KeyVersion::new(request.signed_intent.key_version)?,
).await?;

// 3. TRUST BOUNDARY: verify; mint VerifiedSignedIntent.
let verified = verifier.verify(request.signed_intent, &resolved)?;

// 4. WAL prep + record validation use `&verified`, never raw SignedIntent.
let prepared = pipeline::prepare_ingest(&verified, &request.args, ...)?;
store.append_wal(&prepared).await?;

// 5. On Err: envelope_error_for(&err) → wire ErrorBody → response.
```

## 6. Data shapes

### 6.1 `Clock` (`core/domain/time.rs`)

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> { chrono::Utc::now() }
}

#[cfg(any(test, feature = "test-helpers"))]
pub struct FixedClock(pub chrono::DateTime<chrono::Utc>);
#[cfg(any(test, feature = "test-helpers"))]
impl Clock for FixedClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> { self.0 }
}
```

### 6.2 `ResolvedIssuer` (`core/verifier/resolved.rs`)

```rust
pub struct ResolvedIssuer {
    pub identity: Identity,
    pub key_version: KeyVersion,
    pub verifying_key: ed25519_dalek::VerifyingKey,
    pub state: ProvisioningState,
}

impl ResolvedIssuer {
    pub(crate) fn from_registry_row(...) -> Self;  // sole non-test mint path
}

impl std::fmt::Debug for ResolvedIssuer {
    // redacts verifying_key bytes
}
```

No public constructor outside `cairn-core`. Test helpers live in
`cairn-test-fixtures` behind the `test-helpers` feature.

### 6.3 `ScopePolicy` (`core/verifier/policy.rs`)

```rust
pub struct ScopePolicy {
    pub tenant: String,
    pub workspace: String,
    pub allowed_tiers: BTreeSet<SignedIntentScopeTier>,
}

impl ScopePolicy {
    pub fn new(
        tenant: impl Into<String>,
        workspace: impl Into<String>,
        allowed_tiers: BTreeSet<SignedIntentScopeTier>,
    ) -> Result<Self, DomainError>;  // rejects empty strings
}
```

**Source of `(tenant, workspace)` at P0.** `CairnConfig` does not currently
carry vault-level tenant/workspace fields. The adapter (`cairn-cli`) builds a
`ScopePolicy` explicitly at startup from a hard-coded P0 default
(`tenant = "default"`, `workspace = vault.name`) until a follow-up issue
extends `VaultConfig` with explicit `tenant`/`workspace` fields. This
keeps #51 free of cross-issue config-schema drift.

P0 `allowed_tiers` default: all four tiers (`Project`, `Session`, `User`,
`Org` per the IDL enum). Narrow in a later PR once verbs declare per-tier
requirements.

### 6.4 `EnvelopeVerifier` (`core/verifier/mod.rs`)

```rust
pub struct EnvelopeVerifier<'a> {
    policy: &'a ScopePolicy,
    clock: &'a dyn Clock,
    skew: std::time::Duration,   // hard-coded 60 s for now
}

impl<'a> EnvelopeVerifier<'a> {
    pub fn new(policy: &'a ScopePolicy, clock: &'a dyn Clock) -> Self;

    pub fn verify(
        &self,
        intent: SignedIntent,
        resolved: &ResolvedIssuer,
    ) -> Result<VerifiedSignedIntent, DomainError>;
}

impl SignedIntentVerifier for EnvelopeVerifier<'_> {}
// Mints VerifiedSignedIntent via the sealed `__from_verified(intent, witness)`
// path after every check passes.
```

### 6.5 `resolve_issuer` (`core/verifier/resolve.rs`)

```rust
pub async fn resolve_issuer(
    registry: &dyn IdentityRegistry,
    identity: &Identity,
    key_version: KeyVersion,
) -> Result<ResolvedIssuer, DomainError> {
    // 1. registry.get_identity(identity, IdentityVisibility::IncludingPending)
    //    → None ⇒ Unauthorized
    // 2. registry.list_keys(identity)
    //    → find row at key_version
    //    → None ⇒ KeyVersionMismatch
    // 3. Decode VerifyingKey from the row's stored public-key bytes
    // 4. Build ResolvedIssuer { identity, key_version, verifying_key, state }
}
```

### 6.6 `DomainError` additions

```rust
#[error("signature: cryptographic verification failed")]
InvalidSignature,

#[error("intent expired at {expires_at}; now is {now}")]
ExpiredIntent { expires_at: String, now: String },

#[error("issuer key revoked: {id}")]
RevokedKey { id: Identity },

#[error("key version mismatch: intent={intent}, registry has {current:?}")]
KeyVersionMismatch {
    intent: KeyVersion,
    /// Highest active key version the registry holds for this issuer; `None`
    /// when the issuer is unknown to the registry. The verifier itself does
    /// not raise this variant — it is produced by `resolve_issuer` and
    /// surfaces back through the adapter boundary.
    current: Option<KeyVersion>,
},

#[error("scope denied: {message}")]
ScopeDenied { message: String },

#[error("unauthorized: {message}")]
Unauthorized { message: String },
```

All variants `#[non_exhaustive]` per existing pattern.

### 6.7 Wire mapper (`core/error/wire.rs`)

```rust
pub fn envelope_error_for(err: &DomainError) -> ErrorBody {
    match err {
        DomainError::InvalidSignature
        | DomainError::MissingSignature { .. }    => ErrorBody::missing_signature(...),
        DomainError::ExpiredIntent { .. }         => ErrorBody::expired_intent(...),
        DomainError::RevokedKey { .. }
        | DomainError::KeyVersionMismatch { .. }  => ErrorBody::revoked_key(...),
        DomainError::ScopeDenied { .. }
        | DomainError::Unauthorized { .. }        => ErrorBody::unauthorized(...),
        DomainError::InvalidIdentity { .. }
        | DomainError::InvalidTimestamp { .. }    => ErrorBody::invalid_args(...),
        // Catch-all tier:
        _                                          => ErrorBody::invalid_args(...),
    }
}
```

`ErrorBody` is shaped by the generated `Response.error` struct.
`KeyVersionMismatch → RevokedKey` is the chosen wire mapping at P0
(rationale: callers must rotate or re-fetch their effective key state on
either signal). Revisit if MCP transport needs finer-grained codes.

### 6.8 `canonical_bytes_signed_intent`

Extend `domain/canonical.rs`:

```rust
pub fn canonical_bytes_signed_intent(
    intent: &SignedIntent,
) -> Result<Vec<u8>, DomainError> {
    let mut value = serde_json::to_value(intent).map_err(|e| {
        DomainError::InvalidIdentity {
            message: format!("canonical serialize failed: {e}"),
        }
    })?;
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("signature");
    } else {
        return Err(DomainError::InvalidIdentity {
            message: "SignedIntent did not serialize to a JSON object".into(),
        });
    }
    let mut buf = String::new();
    write_canonical(&value, &mut buf);
    Ok(buf.into_bytes())
}
```

Test that `canonical_bytes_signed_intent` is invariant under
`signature`-field mutation, and changes under any other field mutation.

## 7. Testing strategy

### 7.1 Layer 1 — `cairn-core` unit tests

File: `crates/cairn-core/src/verifier/mod.rs` `#[cfg(test)] mod tests`

Use `rstest` parameterized cases. Each case builds a `SigningKey` (test
seed), a real-signed `SignedIntent`, a `ResolvedIssuer` at version 1 / state
Active, a `ScopePolicy { tenant: "t", workspace: "w", allowed_tiers: all }`,
and a `FixedClock` inside the issued/expires window.

| name | mutation | expected |
|---|---|---|
| `accepts_valid` | none | `Ok(VerifiedSignedIntent)` |
| `rejects_tampered_signature` | flip a byte in `intent.signature` | `InvalidSignature` |
| `rejects_tampered_payload` | mutate `intent.scope.entity` after signing | `InvalidSignature` |
| `rejects_expired` | clock past `expires_at` | `ExpiredIntent` |
| `rejects_pre_issued` | clock before `issued_at − 60 s` | `ExpiredIntent` |
| `rejects_wrong_key_version` | resolved.key_version=2 (caller-bug guard) | `Unauthorized` |
| `rejects_revoked` | resolved.state=Revoked | `RevokedKey` |
| `rejects_pending` | resolved.state=Pending | `RevokedKey` |
| `rejects_scope_tenant` | policy.tenant="x" | `ScopeDenied` |
| `rejects_scope_workspace` | policy.workspace="x" | `ScopeDenied` |
| `rejects_scope_tier` | tier ∉ allowed | `ScopeDenied` |
| `rejects_issuer_mismatch` | resolved.identity ≠ intent.issuer | `Unauthorized` |

Plus a separate `resolve_issuer` integration test in
`crates/cairn-store-sqlite/tests/resolve_issuer.rs` covering the cases the
verifier itself does *not* raise:

| name | scenario | expected |
|---|---|---|
| `unknown_issuer` | identity not in registry | `Unauthorized` |
| `unknown_key_version` | identity exists; row at version 2; intent asks for 3 | `KeyVersionMismatch { intent: 3, current: Some(2) }` |
| `revoked_issuer_returns_state` | identity Revoked; resolve still succeeds; verifier rejects on state | `Ok(ResolvedIssuer { state: Revoked, … })` |

Plus a **proptest** in `crates/cairn-core/tests/verifier_proptests.rs`: any
one-byte mutation in canonical-payload bytes implies `InvalidSignature` (or a
wire-shape parse failure at the deserializer, which is treated as out of
scope of the verifier itself).

### 7.2 Layer 2 — DB-isolation integration test

File: `crates/cairn-store-sqlite/tests/envelope_blocks_wal.rs`

Real `sqlite::memory:` + real `SqliteIdentityRegistry`. Pre-provision one
identity. Stage `ingest` calls with each of:
- tampered signature → `InvalidSignature`
- expired clock → `ExpiredIntent`
- revoked key → `RevokedKey`
- denied scope → `ScopeDenied`

After each, assert:
1. Verb returns the expected `DomainError`.
2. `SELECT count(*) FROM wal_*` rows = 0 (no PREPARE row written).
3. `SELECT count(*) FROM records` = 0 (no record row written).
4. (Once #52 lands) `SELECT count(*) FROM replay_ledger` = 0 — gated `cfg`.

### 7.3 Layer 3 — wire-error snapshot tests

File: `crates/cairn-core/tests/envelope_errors.rs`

Each `DomainError` variant the verifier can return →
`envelope_error_for(&err)` → `insta::assert_json_snapshot!`.

Locks the wire shape across CLI/MCP/SDK. Any future verifier change altering
wire output triggers `cargo insta review`.

### 7.4 Layer 4 — adapter smoke tests

- `crates/cairn-cli/tests/envelope_e2e.rs`
- `crates/cairn-mcp/tests/envelope_e2e.rs`

One test per surface: feed a tampered envelope through the public entry point
and assert the wire `error.code` matches the snapshot from Layer 3.

### 7.5 Test fixture helpers (`cairn-test-fixtures`)

```rust
// fn signs `intent` with `key`, replacing intent.signature
pub fn sign_intent(key: &SigningKey, intent: SignedIntent) -> SignedIntent;

pub fn fixed_clock_at(iso: &str) -> FixedClock;

pub fn scope_policy_default() -> ScopePolicy;
```

`cairn-test-fixtures` stays a `dev-dependencies` of every consumer per
CLAUDE.md §7 (never a non-dev dep).

## 8. Acceptance criteria mapping (to issue #51)

| Issue criterion | Where covered |
|---|---|
| Unsigned or invalid envelopes never reach WAL preparation. | §5 step 7 + §7.2 Layer 2 DB-isolation integration test |
| Valid envelopes produce a verified identity context consumed by pipeline and WAL code. | `VerifiedSignedIntent` continues to be the only token accepted downstream; §5.1 adapter flow |
| Errors are stable across CLI/MCP/SDK surfaces through the common response envelope. | §6.7 `envelope_error_for` + §7.3 snapshot tests + §7.4 surface smoke tests |

## 9. Out of scope (deferred)

- Replay ledger integration (`operation_id`, `nonce`, issuer `sequence`,
  `server_challenge`) — #52.
- `handshake` verb implementation — #52.
- Per-issuer scope-grant fields on `IdentityKeyEntry` — future extension.
- Migrating identity-receipt code's `chrono::Utc::now()` to the new `Clock`
  trait — separate sweep.
- Configurable skew tolerance — fixed 60 s at P0.

## 10. Verification commands

Per CLAUDE.md §8:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

No IDL changes in this PR (envelope schema unchanged) — codegen `--check`
should pass without re-running it.
