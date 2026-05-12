# Record-at-rest signature verification for `lint` (issue #256)

**Status:** draft (proposed scope)
**Issue:** [#256](https://github.com/windoliver/cairn/issues/256)
**Parent:** [#96](https://github.com/windoliver/cairn/issues/96) — `lint` checks for
privacy/provenance/schema/policy drift
**Brief sections:** §1223 (read-time chain verification), §1273-1301 (signature
ordering), §1348 (key ring), §1163 (P0 vs P2 chain rules)

---

## 1. Goal

Surface tampered, revoked-key, or expired-key records via `cairn lint`. Brief
§1223 specifies four states: `valid | expired_key | revoked | broken`. Lint
must report any record whose `chain_status` is not `valid`.

---

## 2. The wrinkle

`StoredRecord` (`crates/cairn-core/src/contract/memory_store.rs:43`) is
`{record: MemoryRecord, version: u32}`. It carries:

- `record.signature: Ed25519Signature` — over `canonical_bytes_signed_payload(record)`
- `record.actor_chain` — `[{role, identity, at}]` (no `key_version` field)
- no `target_hash`, no `SignedIntent`, no `key_version`

The signed envelope (`SignedIntent`) is **not persisted alongside the record**.
That envelope carried `target_hash`, `key_version`, `nonce`, `sequence`, and
the issuer's signature over the intent itself. Once `validate_against_intent`
gates the write at the trust boundary, the intent is consumed.

That means "record-at-rest verification" at the lint layer can only check what
travels with the record:

| Check | Needs | Available at P0? |
|---|---|---|
| `record.signature` is well-formed `ed25519:<128 hex>` | `record` | ✅ already in `validate()` |
| Re-derive `canonical_bytes_signed_payload(record)` and verify `record.signature` against an author public key | `record` + `IdentityRegistry` + Ed25519 | ❌ Ed25519 deferred to P1 (`verifier.rs:8-22`) |
| Author identity lifecycle (`Active` vs `Revoked*`/`Purged*`) | `IdentityRegistry` | ✅ |
| Author key version is current or in predecessor ring | `IdentityRegistry` + `key_version` on record | ❌ no `key_version` field on `MemoryRecord` or `ActorChainEntry` |

So at P0 the only structurally-checkable items are (a) syntactic envelope
checks (already covered by `MemoryRecord::validate`), and (b) author identity
lifecycle.

---

## 3. Three options

### Option A — P0 scope = identity-lifecycle check only

Walk active records, look up `author.identity` in `IdentityRegistry`, emit a
`KeyRevoked` finding when `provisioning_state ∈ {Revoked, RevokePending,
Purged, PurgePending}`. Skip body-integrity and key-version-ring checks until
P1 lands the schema/crypto.

- ✅ Honest about what's checkable today
- ✅ Self-contained, no schema changes
- ❌ Doesn't catch at-rest body tampering — the marquee case the issue title
  implies
- ❌ Doesn't surface `expired_key` or `broken` from §1223

### Option B — Extend `MemoryRecord` to persist `key_version` (and stash `target_hash`?)

Add `key_version: KeyVersion` to `ActorChainEntry` (or `MemoryRecord`). Lint
then checks `key_version` against the issuer's current key ring (§1348).

For body integrity: persist `target_hash` from the consumed `SignedIntent` on
`MemoryRecord` (or recompute via `CanonicalRecordHash::compute(record)` and
check against a stored copy). Either way, schema change.

- ✅ Catches stale-key and re-rotation-after-revocation cases
- ✅ Body integrity checkable without crypto (re-hash and compare)
- ❌ **Breaking schema change** — every record gains a new required field
- ❌ Wider than #256, blocks on #96 + identity team review

### Option C — Pull Ed25519 verify forward to P0

Drop the verifier-crate deferral note; do real Ed25519 verify of
`record.signature` against `canonical_bytes_signed_payload(record)` using
author pubkeys cached from `IdentityRegistry`.

- ✅ Catches body tampering and revoked-key signing in one pass
- ✅ Matches brief §1223 fully
- ❌ Pulls in keychain integration (`cairn-keystore` contract exists in
  `contract/keystore.rs`) — much wider than #256
- ❌ Contradicts the explicit "P1+" deferral in `verifier.rs:8-22`

---

## 4. Recommendation

**Option A for #256 itself**, with a follow-up issue carved out per missing
piece:

- **#256 (this PR):** `KeyRevoked` finding only. Pure check fn
  `cairn-core::pipeline::lint::signature::check`, takes `&MemoryRecord` and
  a `&dyn IdentityRegistry`, returns `Option<Finding>`. Tests against the
  fixture registry from `cairn-test-fixtures`.
- **New issue (P0):** persist `key_version` on `ActorChainEntry`, gate
  `KeyExpired` finding behind it.
- **New issue (P0/P1 boundary):** persist `target_hash` (or commit to
  re-deriving via `CanonicalRecordHash`) so `ChainBroken` body-integrity
  finding can ship without Ed25519.
- **P1:** keychain wiring + Ed25519 verify (already on the roadmap per
  `verifier.rs`).

Why this split:

1. Each PR is independently reviewable and won't block on identity-team
   schema decisions.
2. The dispatch shell #96 is building doesn't need every finding kind on day
   one — it just needs *one* end-to-end check to validate the surface. #256
   becomes that proof.
3. The `lint` IDL only needs one new finding kind today
   (`key_revoked`) instead of three speculative ones.

---

## 5. Proposed P0 deliverables (Option A)

### IDL

`crates/cairn-idl/schema/verbs/lint.json` — extend `kind` enum:

```diff
-{ "type": "string", "enum": ["contradiction", "orphan", "stale", "missing_concept", "data_gap"] }
+{ "type": "string", "enum": ["contradiction", "orphan", "stale", "missing_concept", "data_gap", "key_revoked"] }
```

Run codegen: `cargo run -p cairn-idl --bin cairn-codegen`.

### Core

```rust
// crates/cairn-core/src/pipeline/lint/mod.rs            (new module)
// crates/cairn-core/src/pipeline/lint/signature.rs      (new file)

pub struct SignatureFinding {
    pub record_id: RecordId,
    pub status: ChainStatus,
    pub message: String,
}

pub enum ChainStatus { KeyRevoked }   // grows in follow-ups

pub fn check_signature(
    record: &MemoryRecord,
    registry: &dyn IdentityRegistryRead,  // narrowed read-only trait
) -> Result<Option<SignatureFinding>, LintError>
```

The pure-function shape preserves `cairn-core`'s no-I/O invariant — async I/O
stays at the dispatch layer (which #96 owns).

### Tests

Table-driven via `rstest`:

| Case | Author state | Expected finding |
|---|---|---|
| Active issuer | `Active` | `None` |
| Revoke pending | `RevokePending` | `Some(KeyRevoked)` |
| Revoked | `Revoked` | `Some(KeyRevoked)` |
| Purge pending | `PurgePending` | `Some(KeyRevoked)` |
| Purged | `Purged` | `Some(KeyRevoked)` |
| Identity not in registry | n/a | `Some(KeyRevoked)` (fail closed) |

Snapshot test for `lint-report.md` rendering deferred to #96's dispatch PR.

---

## 6. Out of scope

- Real Ed25519 signature verify (P1, `verifier.rs`)
- Body-integrity / `target_hash` recompute (depends on schema decision)
- `key_version` ring check (depends on schema decision)
- Attestation chain / countersignatures (P2, §1163)
- Replay-ledger consistency (P1)

---

## 7. Open questions for the maintainer

1. Confirm Option A scope — single `key_revoked` finding for #256?
2. File follow-up issues for `key_version` and `target_hash` persistence, or
   fold them into #96?
3. Is the `IdentityRegistryRead` narrowing trait acceptable, or should the
   check fn accept the full `IdentityRegistry`?
