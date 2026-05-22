# Issue #123 — Federation hub protocol + propagation workflow

**Status:** Draft for review
**Brief sections:** §12.a Distribution Model, §10 Continuous Learning, §19 v0.3
**Parent epic:** #26
**Depends on:** #122 (closed — signed share links + consent receipts), #121 (closed — ReBAC enforcement)
**Out of scope:** public SaaS hosting; CLI subcommands; real HTTP transport adapter; hub-side Nexus projection; operator dashboard for the outbox; cross-boundary `forget` fan-out beyond `revoke_share` (separate issue).

---

## 1. Goal

Land the `cairn.federation.v1` extension protocol and the `PropagationWorkflow` that moves consented records from one Cairn vault to another over a pluggable `FederationTransport`. Acceptance criteria (from the issue):

- Federation only sends records allowed by consent and ReBAC.
- Propagation is retryable, auditable, and idempotent.
- Inbound shared records preserve provenance and trust status.

The work flips `cairn.federation.v1` from "capability registered but unwired" to "advertised when `wiring::federation_extension_ready() == true`".

## 2. Constraints (from CLAUDE.md + brief)

- `cairn-core` has zero dependencies on other workspace crates.
- Every mutation goes through the existing §5.6 WAL state machine. No new state machine; new payload variants on existing operations.
- `#![forbid(unsafe_code)]`, no `unwrap`/`expect` in `cairn-core`.
- Capability advertisement decisions live in `cairn-core::status::advertise`. Adding a capability is a row in that table; flipping it on is a `wiring::*_WIRED` constant change.
- Body-free policy traces and error variants.

## 3. Architecture

```
                                ┌──────────────────────────────┐
   issuer (human)               │  cairn-core (pure)           │
        │                       │                              │
        ▼                       │  domain::federation          │
  propose_share ───────────────▶│   - FederationEnvelope       │
   verb                         │   - PeerEndpoint             │
        │                       │   (piggybacks on             │
        │  (sign + ReBAC + WAL  │    domain::sharing #122)     │
        │   consent_journal     │                              │
        │   Grant entry)        │  contract::                  │
        │                       │    federation_transport      │
        │  enqueue              │   trait                      │
        ▼                       └──────────────┬───────────────┘
  scheduler job                                │
   (existing cairn-workflows                   │
    scheduler::worker)                         │
        │                                      │
        ▼                                      ▼
  PropagationHandler ───────▶  FederationTransport (trait obj)
   - load + sign envelope        │  in-memory LoopbackTransport
   - send                        │  real HTTP transport = later
   - on Transient: retry         │
   - on Permanent: dead          ▼
   - on Ack: done       peer's verbs::accept_share
                          - verify sig/expiry/scope/hashes
                          - ReBAC check on receiver
                          - dedup by (issuer_key, link_id, nonce)
                          - upsert via existing §5.6 WAL
                          - consent_journal Accept entry
```

Revoke uses the same path with an `OutboundRevoke` job → peer's `accept_share` revoke action → existing `forget --record` Phase A+B state machine on the receiver's projection.

## 4. Components

| New module | Lives in | Purpose |
|---|---|---|
| `contract::federation_transport` | `cairn-core/src/contract/` | `FederationTransport` trait. `send(envelope, peer) -> SendOutcome` (`Ack` / `Transient(Error)` / `Permanent(Error)`). Pure trait, no I/O. |
| `domain::federation` | `cairn-core/src/domain/` | `FederationEnvelope { kind: Propose \| Accept \| Revoke, signed_payload, manifest? }`; `PeerEndpoint`; `PropagationOutbound { operation_id, peer, envelope, attempts, next_run_at }`. Reuses `domain::sharing::SignedShareLink`, `PromotionConsentReceipt`, `SharingDecisionKind`. |
| `verbs::propose_share`, `accept_share`, `revoke_share` | `cairn-core/src/verbs/` | Three new verb functions following existing pattern (validate → plan → FlushPlan). |
| `error::federation` | `cairn-core/src/error/` | `FederationError` thiserror enum. Maps to existing `SharingDecisionKind` for policy-trace reuse. |
| `status::wiring::FEDERATION_*_WIRED` + `federation_extension_ready()` | `cairn-core/src/status/wiring.rs` | Capability gate, same pattern as `coord_extension_ready()`. Initially all `false`; flipped in the wiring commit. |
| `propagation/` (`handler.rs`, `payload.rs`, `trigger.rs`, `mod.rs`) | `cairn-workflows/src/` | `PropagationHandler` implements scheduler `JobHandler`. Mirrors `consolidation/`, `dream/` layout. |
| MCP tool registrations | `cairn-mcp/src/` | Three tools under `cairn.federation.v1` namespace, capability-gated. Schema via `schemars`. |
| `LoopbackTransport` | `cairn-test-fixtures/src/` | In-process `FederationTransport` impl. Configurable to inject Ack/Transient/Permanent for tests. |

## 5. Data flow

### Outbound propose (issuer side)

1. `propose_share { record_ids, grantee, scope, grant_tier, expires_at }` enters via MCP.
2. Verb canonicalizes records, builds `ShareLinkPayload`, calls `domain::sharing` to sign with the issuer's human key. Validates ReBAC: issuer is permitted to grant each record at `grant_tier` (existing `RebacAction::Share` predicate on the issuer/record/tier triple). Both gates produce `PolicyTraceEntry` rows.
3. **Atomic WAL transaction:** `ConsentEvent` Grant entry written to `consent_journal` + `OutboundShare` scheduler job row, single SQLite tx. No partial state.
4. Verb returns to caller with the signed link.

### Outbound delivery (background)

5. Scheduler picks up `OutboundShare`. `PropagationHandler` builds `FederationEnvelope::Propose { link, manifest }` where manifest holds the record bodies allowed at `grant_tier`.
6. `FederationTransport::send(envelope, peer)`:
   - `Ack` → mark job done; audit entry.
   - `Transient` → bump `attempts`, exponential backoff `min(2^attempts, 3600)` seconds, requeue. After `attempts == 10` → Dead.
   - `Permanent` → Dead immediately. `cairn lint` surfaces dead jobs via new check kind `federation_dead_propagation`.

### Inbound accept (peer side)

7. Peer's MCP receives envelope. For `LoopbackTransport`, this is a direct in-process call to `verbs::accept_share`.
8. Verb verifies signature, `expires_at`, scope/target-hash/tier match, then ReBAC: receiver can write at `grant_tier`. Each failure → typed `SharingDecisionKind` rejection.
9. **Idempotency:** lookup `(issuer_key_id, link_id, nonce)` in `consent_journal`. Hit → return original outcome without re-applying. Miss → proceed.
10. **Atomic WAL transaction:** existing `upsert` state machine writes the records with `provenance.source = ShareLink { link_id, issuer }`, visibility tier capped at `grant_tier`, trust status `inbound_shared`. `ConsentEvent` Accept entry to `consent_journal` in the same tx.
11. Return Ack.

### Revoke

12. Issuer calls `revoke_share { link_id }`. Verb marks local link revoked (uses existing `ShareLinkJournalDecision::Revoke` in `domain::sharing`), writes the corresponding `ConsentEvent` Revoke entry to `consent_journal`, enqueues `OutboundRevoke` job — same atomic tx.
13. Handler delivers `FederationEnvelope::Revoke { signed_revocation }` to peer.
14. Peer's `accept_share` revoke path: verify sig, find projected records by `(link_id, target_id_hashes)`, run them through the existing `forget --record` Phase A+B state machine. Trust status flips to `revoked`. Audit.

Subsequent `accept_share` calls for the revoked link reject with `SharingDecisionKind::Revoked` at the verifier — issuer-side revocation takes precedence over in-flight propose envelopes via the consent_journal lookup at step 9.

## 6. Error handling

### Verb layer — `FederationError` enum

`thiserror`, `#[non_exhaustive]`. All variants emit a body-free `PolicyTraceEntry` via existing `SharingPolicySubject` / `SharingPolicyAction` enums.

| Variant | Cause | Maps to `SharingDecisionKind` |
|---|---|---|
| `Expired` | receipt/link past `expires_at` | `Expired` |
| `TargetMismatch` | record hash or id-hash set doesn't match payload | `TargetMismatch` |
| `ScopeMismatch` | requested op outside grant scope | `ScopeMismatch` |
| `TierMismatch` | record visibility > `grant_tier` | `TierMismatch` |
| `BadSignature` | Ed25519 verify failed | `BadSignature` |
| `Revoked` | link previously revoked | `Revoked` |
| `NotHuman` | issuer/signer not a human identity | `NotHuman` |
| `NoRebacRelation` | issuer can't read at tier, or receiver can't write at tier | `NoRebacRelation` |
| `InvalidShape` | shape validation before sig check | `InvalidShape` |
| `DuplicateNonce` | dedup hit on `(issuer_key, link_id, nonce)` | n/a — succeeds with original outcome, not an error |
| `UnknownLink` | revoke target not found | n/a — new |
| `CapabilityDisabled` | `wiring::federation_extension_ready() == false` | wraps `CapabilityUnavailable` with remediation |

### Transport layer — `TransportError`

- `Transient(reason)` — network blip, 5xx, rate-limit. Retry with exponential backoff `min(2^attempts, 3600)` seconds. Dead after 10 attempts.
- `Permanent(reason)` — 4xx, signature rejected at peer, peer ReBAC denied. No retry. Dead immediately.

### Dead jobs

- Stay in scheduler table with `state = dead` and last error.
- Surfaced by `cairn lint` (new check kind `federation_dead_propagation`).
- Surfaced in `cairn.admin.v1.snapshot` for ops visibility.

### Capability discipline

Every verb checks `wiring::federation_extension_ready()` at entry. False → `CapabilityUnavailable` with remediation from `status::REMEDIATION` (new row: `"enable federation: set federation.enabled = true and configure a peer endpoint"`). The constant stays `false` until the wiring commit lands the dispatch path end-to-end.

### WAL discipline

Every state change (job enqueue, `ConsentEvent` append, record upsert/tombstone) goes through the existing §5.6 WAL state machines. No new state machine; new payload variants on the existing `upsert` / `tombstone` operations, and new `ConsentEvent` kinds for the federation grant/accept/revoke triples.

## 7. Testing

### Unit (`cairn-core`)

- Verb-level happy path for each of `propose_share` / `accept_share` / `revoke_share`.
- One test per `FederationError` variant → asserts typed error + correct `PolicyTraceEntry` shape (body-free).
- Capability gate: with `federation_extension_ready() == false`, every verb returns `CapabilityUnavailable` with remediation hint.

### Property tests (`proptest`) — load-bearing correctness

- **Idempotency:** arbitrary multiset of `accept_share` invocations for the same `(issuer_key, link_id, nonce)` converge to identical store state and identical Ack outcome.
- **Retry safety:** any interleaving of `(Transient*, Ack)` outcomes from the transport produces exactly one applied upsert on the receiver.
- **Revoke ordering:** `(propose, accept, revoke)` always ends in `revoked`; `(propose, revoke, accept)` rejects accept with `SharingDecisionKind::Revoked`.
- **Envelope round-trip:** signed payload → canonical JSON → parse → verify holds across arbitrary valid payloads.

### Integration (`cairn-workflows`)

- Two in-memory stores + `LoopbackTransport`. End-to-end propose → accept → record visible on receiver with correct provenance + tier cap.
- Crash-resume: kill mid-flight after job enqueue but before send; restart scheduler; assert single delivery.
- Backoff cap: transport stubbed to return `Transient` forever; assert Dead after 10 attempts, lint reports it.
- Revoke end-to-end: propose → accept → revoke → records tombstoned on receiver via existing `forget --record` Phase A+B; `consent_journal` shows Grant + Accept + Revoke + Forget entries.

### Wire-compat snapshot tests (`insta`)

- `cairn.federation.v1` MCP tool declarations frozen.
- `status.capabilities` + `status.extensions` snapshot when `federation_extension_ready()` returns `true`.
- Three golden envelope fixtures (signed with deterministic test key) — round-trip parse + verify.

### Consent/ReBAC blocking (explicit acceptance-criterion tests)

- Records flagged `private`-tier-only at source → `propose_share { grant_tier: team }` rejects with `NoRebacRelation` even with valid consent.
- Missing `PromotionConsentReceipt` for tier-crossing share → rejects with appropriate `SharingDecisionKind`.
- Receiver without ReBAC write relation → rejects with `NoRebacRelation`; outbound side gets `Permanent` from transport; goes Dead.

### Fixture-driven protocol tests

Golden JSON files for each envelope kind under `crates/cairn-core/tests/fixtures/federation/`; CI gate via existing fixture pattern (mirror of `tests/share_link.rs`, `tests/sharing_receipt.rs`).

## 8. Verification (matches issue verification block)

- [x] Run federation protocol fixture tests — covered by §7 fixture-driven protocol tests + wire-compat snapshots.
- [x] Run propagation retry/idempotency tests — covered by §7 property tests + integration crash-resume/backoff-cap tests.
- [x] Run consent/ReBAC blocking tests — covered by §7 consent/ReBAC blocking subsection.

Plus the standard `scripts/check-core-boundary.sh` (no new core deps), `cargo run -p cairn-idl --bin cairn-codegen` (new IDL envelope types for `cairn.federation.v1` require this), and `cargo run -p cairn-cli --bin cairn-docgen --write` (the new `cairn.federation.v1` tool definitions are user-facing MCP metadata).

## 9. Sequencing

This issue lands the protocol, workflow, and in-memory transport. Follow-up issues:

1. Real HTTP transport adapter crate (`cairn-federation-http` or under `cairn-mcp` HTTP transport).
2. CLI subcommands (`cairn share propose|accept|revoke`).
3. Hub-side Nexus projection for shared records (§3.0 P2 hub).
4. Cross-boundary `forget` fan-out beyond revoke (e.g. local `forget --record` triggering revoke at every peer that received the record).
5. Operator dashboard / `cairn admin federation` view for outbox state.
