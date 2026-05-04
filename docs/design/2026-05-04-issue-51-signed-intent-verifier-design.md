# Signed-intent envelope verifier (issue #51)

**Status:** implemented (2026-05-04)
**Issue:** [#51](https://github.com/windoliver/cairn/issues/51) — Validate
signed intent envelopes before disk writes
**Parent epic:** [#7](https://github.com/windoliver/cairn/issues/7) — Identity,
signed envelope, status, and handshake
**Depends on:** [#50](https://github.com/windoliver/cairn/issues/50) (closed)
— local human/agent/sensor identity provisioning
**Defers to:** [#52](https://github.com/windoliver/cairn/issues/52) (replay
ledger + sequence CAS + handshake challenge consume), [#55](https://github.com/windoliver/cairn/issues/55)
(WAL state machine + boot recovery)
**Brief sections:** §4.2 (signed payload schema, identity model, atomic replay),
§5.6 (WAL `PREPARE` coupling), §8.0.b (verb envelope wrapper)

---

## 1. Problem

Today `crates/cairn-core/src/verifier.rs::verify_signed_intent` is a
**placeholder** — it parses the issuer string, checks the `target_hash` shape,
and validates the two RFC3339 timestamps for syntax. It performs **no**
Ed25519 signature math, **no** timestamp-window check against a clock,
**no** key-version / revocation lookup, and **no** scope check.

The trust boundary that every CLI / MCP / SDK / skill call is supposed to flow
through is therefore non-functional. Any caller that constructs a
syntactically-correct envelope (no signature material at all) currently
mints a `VerifiedSignedIntent`, the sealed-trait token that the rest of core
treats as proof-of-authenticity.

Issue #51 makes the verifier real: signature, timestamp window, key version,
revocation, and scope checks, before any envelope can become a
`VerifiedSignedIntent` and therefore before it can drive a WAL `PREPARE`.

## 2. Scope

**In scope:**
- Real Ed25519 signature verification of the wire envelope using
  RFC 8785 (JCS) canonicalization of the signed payload (every envelope
  field except `signature`).
- Server-side timestamp-window enforcement: `issued_at` within ±2 min of a
  caller-supplied `now`, `expires_at − issued_at ≤ 5 min` (the brief's flat
  default; the 24 h promotion-receipt exception arrives with P2
  `ConsentReceipt` work, brief §4.2), `now ≤ expires_at`.
- Issuer pubkey + lifecycle lookup against an `IdentityRegistry`-backed
  resolver. Brief §4.2 "earlier ops remain valid" rule applies — a
  revocation with `effective_at > issued_at` does not reject prior ops.
- Scope-fit check tied to the issuer's identity kind:
  `snr:` → `tier == private`; `agt:` → `tier ∈ {private, session, project}`;
  `hmn:` → all tiers. Closes the "agent self-promotes a write to public"
  attack at the verification boundary, without requiring per-agent
  scope-policy infrastructure that does not yet exist at P0.
- A new typed error enum (`VerifyError`) covering every failure mode the
  issue lists — Malformed, ExpiredIntent, ScopeDenied, UnknownKey,
  RevokedKey, InvalidSignature, plus a wrapped `ResolverFailure` for
  adapter-side I/O.
- A new minimal contract trait `IssuerKeyResolver` (one method, async),
  plus a SQLite adapter implementation that wraps `IdentityRegistry`.
- Property tests, snapshot tests, and a "no-DB-write-on-bad-envelope"
  regression integration test that satisfies issue verification lines 1-3
  and acceptance criterion 1.

**Out of scope (deferred to other issues):**
- The replay ledger atomic transaction (`used`, `issuer_seq`,
  `outstanding_challenges`) — issue #52.
- Atomic coupling of replay-consume with WAL `PREPARE` — issues #52 + #55.
- Handshake challenge mode lifecycle (`outstanding_challenges` insert
  during `cairn handshake`, single-use TTL consume) — issue #52. The
  `server_challenge` field's *syntactic* presence is still validated here
  (exactly-one-of `sequence`/`server_challenge`).
- Per-agent scope policy (`allowed_kinds`, `max_writes_per_hour`,
  `pii_permission`, `tool_allowlist`) — future issue. P0 ships only
  identity-kind-vs-tier fit.
- Multi-actor chain verification (P2) — only the single-author P0
  baseline is verified here.
- Network authentication for the MCP transport — explicitly out of scope
  per issue #51.

## 3. Architecture

```
┌──────────────────┐
│ wire bytes       │
│ (Envelope per    │
│  §8.0.b carrying │
│  SignedIntent    │
│  per §4.2)       │
└────────┬─────────┘
         │
         ▼
┌──────────────────────────────┐         ┌────────────────────────┐
│ adapter (CLI / MCP / SDK)    │         │ cairn-store-sqlite     │
│  - parse envelope            │         │  SqliteIssuerKeyResolver│
│  - look up resolver          │ ──────▶ │   reads identity_keys  │
│  - capture SystemTime::now() │   look  │   + identities tables  │
└────────┬─────────────────────┘   ups   └────────────────────────┘
         │
         ▼
┌────────────────────────────────────────────────────────────┐
│ cairn-core::verifier::verify_signed_intent                 │
│   (extends today's placeholder; same function name)        │
│                                                            │
│   1. syntactic checks (issuer parse, target_hash shape,    │
│      RFC3339 timestamps, exactly-one-of                    │
│      sequence/server_challenge, chain_parents shape)       │
│   2. timestamp window vs `now`                             │
│   3. issuer-kind ↔ scope.tier fit                          │
│   4. resolver.lookup(issuer, key_version) → ResolvedKey    │
│   5. JCS-canonicalize envelope-minus-signature, ed25519    │
│      verify with resolved pubkey                           │
│                                                            │
│  ↳ Ok(VerifiedSignedIntent)                                │
│  ↳ Err(VerifyError::*)                                     │
└────────┬───────────────────────────────────────────────────┘
         │
         ▼
┌──────────────────────────────┐
│ verb dispatch + WAL          │
│ (#52, #55, #57 etc.)         │  ← consumes the verified token; this
│  - replay-consume txn        │    issue stops at the token boundary
│  - WAL PREPARE               │
│  - record validation         │
└──────────────────────────────┘
```

**Crate layout:**
- `cairn-core` — extends `src/verifier.rs`; new modules
  `src/intent/canonical_envelope.rs` (JCS payload builder) and
  `src/intent/verify_error.rs` (the error enum). New contract trait at
  `src/contract/issuer_key_resolver.rs`. Adds two workspace deps to
  `cairn-core`'s `Cargo.toml`: `ed25519-dalek` (direct dep — cannot
  rely on `cairn-keychain`'s, since core may not depend on workspace
  crates per invariant #1) and `serde_jcs` for RFC 8785
  canonicalization. Both pinned in `[workspace.dependencies]` so
  `cairn-keychain` and `cairn-core` use the same version. I/O-free per
  invariant #1.
- `cairn-store-sqlite` — new module `src/issuer_key_resolver.rs`
  implementing `IssuerKeyResolver` over `IdentityRegistry`. ~50 LOC.
- `cairn-test-fixtures` — new `signed_intent_builder()` returning a
  `bon`-style builder that overrides any field; replaces the inline
  `good_intent()` helper currently sitting in `verifier.rs::tests`.
- `cairn-keychain` — unchanged. Public-key reads go through the registry,
  not the keychain.

## 4. Components

### 4.1 `IssuerKeyResolver` (cairn-core, new)

```rust
#[async_trait]
pub trait IssuerKeyResolver: Send + Sync {
    /// Resolve `(issuer, key_version)` → public key + lifecycle state.
    /// Returns `Ok(None)` if no row exists for that pair.
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKey {
    pub public_key: Ed25519PublicKey, // 32-byte newtype, hand-rolled Display
    pub lifecycle: KeyLifecycle,
}

#[non_exhaustive]
pub enum KeyLifecycle {
    Active,
    Revoked { effective_at: Rfc3339Timestamp },
    Pending,
    Purged,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResolverError {
    #[error("backend failure: {0}")]
    Backend(#[from] Box<dyn std::error::Error + Send + Sync>),
}
```

The trait is intentionally tiny — `IdentityRegistry` carries 30+
methods for the full provisioning + rotation + revocation lifecycle,
none of which a verifier needs. Verification only consumes one fact:
"for `(issuer, key_version)`, what is the pubkey and is it still
trusted at this issued_at?" Async because adapter implementations are
I/O; verification is at the trust boundary, not a hot inner loop.

### 4.2 Canonical envelope (cairn-core, new)

Single function:

```rust
pub fn canonicalize_signed_payload(
    intent: &SignedIntent,
) -> Result<Vec<u8>, VerifyError>;
```

Builds an in-memory `serde_json::Value` from every `SignedIntent` field
**except** `signature`, then runs it through `serde_jcs::to_vec` (RFC 8785
key-sort, no whitespace, deterministic number serialization). The
returned bytes are the input to Ed25519 verify.

A property test in `cairn-test-fixtures` asserts:
- round-trip stability: canonicalize → JSON parse → canonicalize is
  byte-equal,
- field-coverage: any single-byte mutation of any envelope field changes
  the canonical output (no field is silently dropped),
- key independence: input map order at the Rust level does not change
  the canonical output.

### 4.3 `verify_signed_intent` (cairn-core, replaces placeholder)

```rust
pub async fn verify_signed_intent(
    intent: SignedIntent,
    resolver: &dyn IssuerKeyResolver,
    now: SystemTime,
) -> Result<VerifiedSignedIntent, VerifyError>;
```

Executes the five-step pipeline described in §3. Cheap rejections fire
first; the resolver lookup runs only after every pure check passes;
the signature check runs last because it is the most expensive (~50 µs)
and benefits from rejecting tampered envelopes upstream.

Ordering rationale: brief §4.2 hot-path lists "(1) signature, (2)
timestamp, (3) key version + revocation, (4) bloom probe" — but that
ordering is for the **production replay-ledger flow** where the bloom
probe is a fast-path miss-cache. At the per-call verification layer
without the ledger we want syntactic fails to short-circuit before
crypto, and crypto to short-circuit before resolver I/O is reached
when the input is obviously malformed. Once #52's bloom + replay land,
the adapter wraps `verify_signed_intent` so the bloom probe stays in
front of crypto exactly as the brief specifies.

### 4.4 `VerifyError` (cairn-core, new)

```rust
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VerifyError {
    #[error("malformed envelope: {field}: {reason}")]
    Malformed { field: &'static str, reason: String },

    #[error("expired intent ({kind:?}): issued_at={issued_at} expires_at={expires_at} now={now}")]
    ExpiredIntent {
        issued_at: Rfc3339Timestamp,
        expires_at: Rfc3339Timestamp,
        now: Rfc3339Timestamp,
        kind: ExpiryReason,
    },

    #[error("scope denied: issuer kind {issuer_kind:?} cannot sign tier {requested_tier:?}")]
    ScopeDenied {
        issuer_kind: IdentityKind,
        // The wire-level tier enum from cairn-idl-generated SignedIntentScope
        // (re-exported as `ScopeTier` in this module for ergonomics).
        requested_tier: SignedIntentScopeTier,
    },

    #[error("unknown key: issuer={issuer} key_version={key_version}")]
    UnknownKey { issuer: Identity, key_version: KeyVersion },

    #[error("revoked key: issuer={issuer} key_version={key_version} effective_at={effective_at}")]
    RevokedKey {
        issuer: Identity,
        key_version: KeyVersion,
        effective_at: Rfc3339Timestamp,
    },

    /// Opaque on purpose — no oracle for differential timing on
    /// signature-bytes vs canonicalization vs pubkey mismatch. The brief
    /// §4.2 "signature-first rejection" rule treats every cause as
    /// equivalent at the wire layer.
    #[error("invalid signature")]
    InvalidSignature,

    #[error("resolver failure")]
    ResolverFailure(#[source] ResolverError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExpiryReason {
    /// `|now − issued_at| > 2 min`.
    Skewed,
    /// `now > expires_at`.
    Past,
    /// `expires_at − issued_at > max_ttl`.
    TtlExceeded,
}
```

`VerifyError` is *not* folded into `DomainError`. Different layer,
different audience: `DomainError` answers "is this record well-formed",
`VerifyError` answers "is this envelope authentic". Keeping the two
enums separate also keeps the §8.0.b `policy_trace` mapping stable —
the wire layer's gate codes do not need to demux every domain
validation case.

`#[non_exhaustive]` so adding `Replay`, `OutOfOrderSequence`, and
`ChallengeMismatch` in #52 is non-breaking.

### 4.5 `SqliteIssuerKeyResolver` (cairn-store-sqlite, new)

Thin glue:

```rust
pub struct SqliteIssuerKeyResolver<R: IdentityRegistry> {
    registry: Arc<R>,
}

#[async_trait]
impl<R: IdentityRegistry> IssuerKeyResolver for SqliteIssuerKeyResolver<R> {
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError> {
        // 1. registry.list_keys(issuer) → find KeyEntry where version == key_version
        // 2. registry.get_identity(issuer, IdentityVisibility::confirmed()) → derive lifecycle
        //    (Active vs Revoked { effective_at })
        // 3. wrap into ResolvedKey or None.
    }
}
```

No SQL hand-written here — every read is a `registry` method call. The
registry's existing `RegistryError` variants map to `ResolverError::Backend`.

## 5. Data flow

(See the diagram in §3.) Failure at any verifier step short-circuits with
no side effects. Tracing instrumentation:

```rust
#[tracing::instrument(
    skip(intent, resolver),
    err,
    fields(
        verb = "verify_signed_intent",
        issuer = %intent.issuer.0,
        key_version = intent.key_version,
        operation_id = %intent.operation_id.0,
    ),
)]
```

No record bodies, no signature bytes, no canonicalized payload bytes
ever appear in spans above `debug` (privacy invariant #9).

## 6. Error handling

Every failure mode maps 1:1 to a `VerifyError` variant; mapping table:

| Step | Failure mode | Variant |
|------|--------------|---------|
| 1 | issuer not parseable | `Malformed{field:"issuer"}` |
| 1 | target_hash bad shape | `Malformed{field:"target_hash"}` |
| 1 | issued_at not RFC3339 | `Malformed{field:"issued_at"}` |
| 1 | expires_at not RFC3339 | `Malformed{field:"expires_at"}` |
| 1 | both/neither sequence + server_challenge | `Malformed{field:"sequence_or_challenge"}` |
| 1 | chain_parents element not a valid op-id | `Malformed{field:"chain_parents"}` |
| 2 | `|now − issued_at| > 2 min` | `ExpiredIntent{kind:Skewed}` |
| 2 | `now > expires_at` | `ExpiredIntent{kind:Past}` |
| 2 | `expires_at − issued_at > max_ttl` | `ExpiredIntent{kind:TtlExceeded}` |
| 3 | `snr:` issuer with tier ≠ private | `ScopeDenied` |
| 3 | `agt:` issuer with tier ∈ {team, org, public} | `ScopeDenied` |
| 4 | resolver returns None | `UnknownKey` |
| 4 | lifecycle = Pending or Purged | `UnknownKey` |
| 4 | lifecycle = Revoked w/ `effective_at ≤ issued_at` | `RevokedKey` |
| 4 | adapter I/O failure | `ResolverFailure` |
| 5 | JCS canonicalization fails | `Malformed{field:"envelope"}` |
| 5 | Ed25519 verify rejects | `InvalidSignature` |

Migration of existing callers:
- `verifier.rs::tests` is rewritten against `VerifyError` (4 existing
  tests; placeholder mapping to `DomainError::*` was incorrect).
- `MemoryRecord::validate_against_intent` keeps returning `DomainError`;
  it consumes a *verified* token and its concerns are about
  record-vs-intent containment, not envelope authenticity.

## 7. Testing

### 7.1 Unit tests (cairn-core/src/verifier.rs)

`rstest`-driven, one test per row in §6's mapping table plus the happy
paths (well-formed sequence-mode intent, well-formed challenge-mode
intent, revocation-after-issued_at boundary, exactly-5-min TTL is
accepted, `now == expires_at` is rejected as `Past`). Fake `IssuerKeyResolver` with three preloaded keys: active,
revoked-past, pending. Fake-key plumbing lives entirely in
`#[cfg(test)]` — no `for_test` constructor on the production resolver.

### 7.2 Property tests (cairn-test-fixtures)

`proptest` strategies:
- `canonical_envelope_roundtrip` — generate arbitrary `SignedIntent`,
  assert `canonicalize → parse → canonicalize` is byte-equal.
- `signature_determinism` — same `(envelope, key)` produces byte-equal
  signature bytes across N runs (catches non-determinism in JCS or
  ed25519 backend).
- `tamper_invariant` — for any envelope and any single-byte mutation,
  signature verification fails. Catches the "newly-added field
  accidentally excluded from canonicalization" bug class.

### 7.3 Integration test (cairn-store-sqlite/tests/issuer_key_resolver.rs)

In-memory SQLite seeded via `cairn-test-fixtures` with active,
revoked, pending, and purged identities at multiple key versions.
Asserts the resolver maps each lifecycle state to the right
`KeyLifecycle` and that unknown `(issuer, key_version)` pairs return
`Ok(None)` rather than an error.

### 7.4 No-DB-write regression test (cairn-store-sqlite/tests/no_db_write_on_bad_envelope.rs)

Issue verification line 2 + acceptance criterion 1 enforced as a
permanent regression test:

1. Set up a real SQLite DB at every P0 schema version.
2. Snapshot `wal_ops` row count and every record-bearing table's
   `COUNT(*)`.
3. For each invalid envelope variant — tampered signature, `Skewed`,
   `Past`, `TtlExceeded`, `UnknownKey`, `RevokedKey`, `ScopeDenied`,
   each `Malformed` field — call a minimal driver that runs
   `verify_signed_intent` and (on `Ok`, which never happens here) would
   open a write txn.
4. Re-snapshot. Assert every count is unchanged across every variant.

Lives under #51; the verb harness is not yet fully wired but the
driver only exercises the verification side and the snapshot machinery,
both of which exist today.

### 7.5 Snapshot tests (cairn-core/tests/snapshots/verify_error_*.snap)

`insta` snapshots of `Display` output for every `VerifyError` variant.
Locks the wire-stable error wording demanded by issue acceptance
criterion 3.

### 7.6 Doc tests

`verifier.rs` rustdoc gains a `# Examples` block showing the
trust-boundary call shape. Marked `rust,no_run` because it depends on
adapter wiring outside core.

## 8. Verification checklist (before push)

Beyond the §8 baseline (`cargo fmt`, `clippy -D warnings`,
`cargo nextest run --workspace`, `cargo test --doc --workspace`,
`./scripts/check-core-boundary.sh`, `cargo run -p cairn-idl --bin cairn-codegen -- --check`),
this PR specifically must:

- demonstrate `core-boundary.sh` still passes (no new workspace deps in
  `cairn-core`),
- run the new `no_db_write_on_bad_envelope` integration test under
  `cargo nextest`,
- regenerate snapshots with `cargo insta review` and commit the
  `.snap` files,
- include the `cairn-test-fixtures` builder as a dev-only dep (never
  a non-dev dep — `Cargo.toml` enforcement).

## 9. P1+ extensibility

- **Replay/sequence (#52):** adapter wraps `verify_signed_intent`, then
  opens its own SQLite txn doing `(used insert + issuer_seq CAS + WAL
  PREPARE)` in one shot. Verifier stays unchanged.
- **Per-agent scope policy:** `ResolvedKey` grows
  `policy: Option<IdentityScopePolicy>`; verifier step 3 consults it
  when present, falls back to identity-kind defaults when absent.
  Non-breaking on every call site.
- **Multi-actor chain (P2):** verifier becomes one of N hop checks; a
  chain walker calls it per actor entry. The single-author P0 path
  remains a special case (`chain.len() == 1`).
- **Handshake challenge (#52):** verifier already accepts envelopes
  carrying `server_challenge` syntactically; #52 wires the
  `outstanding_challenges` lookup + atomic delete.

## 10. Open questions / risks

- **`bon` builder vs. hand-rolled fixture builder:** `bon` is already
  workspace-mandated (CLAUDE.md §6.10). Plan: derive-builder. Risk:
  pulling `bon` into `cairn-test-fixtures` if not already there. Mitigation:
  if it isn't, hand-roll a tiny builder; the surface is < 12 fields.
- **Ed25519 crate selection:** `ed25519-dalek` 2.x is the standard
  choice and is already transitively present via `cairn-keychain`.
  Confirm a single workspace pin before merging; do not allow two
  versions of the curve crate to co-exist.
- **`serde_jcs` dependency footprint:** the crate is small but pulls
  `serde_json` (already present). Confirm `cargo deny check` accepts
  its license (Apache-2.0). No transitive surprises expected.
- **`Ed25519PublicKey` newtype location:** lives in `cairn-core`; must
  not leak `ed25519_dalek::VerifyingKey` through public API to keep
  the crypto-backend swap painless. Hand-roll a 32-byte `[u8; 32]`
  newtype and convert internally.
