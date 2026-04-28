# Issue #50 — Local identity provisioning with keychain-backed keys

- **Issue:** [#50](https://github.com/windoliver/cairn/issues/50) (parent epic [#7](https://github.com/windoliver/cairn/issues/7))
- **Phase / priority:** v0.1 minimum substrate · P0
- **Brief sections:** §4.2 (Identity), §14 (Privacy and Consent)
- **Dependency:** [#39](https://github.com/windoliver/cairn/issues/39) (config schema) — closed
- **Status:** design proposal
- **Author:** Claude Opus 4.7

---

## 1. Problem

Cairn’s P0 substrate needs a real identity layer before any record can be
signed, ranked, or audited. Brief §4.2 pins:

- An Ed25519 keypair per identity, lived in the platform keychain
  (Keychain on macOS, Secret Service on Linux, DPAPI on Windows). Never on
  disk in plaintext, never synced into the vault.
- Three identity kinds — `HumanIdentity`, `AgentIdentity`, `SensorIdentity`
  — sharing one wire form `<prefix>:<body>`.
- A key ring per identity (current + up to two predecessors) so records
  signed by an older key still verify until TTL expires.
- Public metadata, `key_version`, and revocation state live in
  `.cairn/cairn.db` so every write can resolve the issuer.

Today the repo has only the wire-form `Identity` newtype
(`crates/cairn-core/src/domain/identity.rs`). No keypair generation, no
keychain adapter, no SQLite identity table, no provisioning entry point.

This spec covers the P0 minimum to satisfy issue #50’s acceptance criteria
without overshooting into P2 territory (multi-hop chains, countersignatures,
trust scores, `IdentityProvider` plugin, shared-tier consent receipts).

## 2. Goals & non-goals

### 2.1 In scope

1. Generate Ed25519 keypairs for `HumanIdentity`, `AgentIdentity`, and
   `SensorIdentity`. Store private keys in the platform keychain.
2. Persist public identity metadata, the key ring, and revocation state in
   `.cairn/cairn.db`.
3. Bind defaults on first identity initialization: one local human identity
   (from the OS username) and one Claude Code agent identity. Identity
   initialization is a distinct step from `cairn bootstrap` (see §3.7).
   Sensors are not bound here.
4. Provision sensor identities lazily on first enable, not at unrelated
   startup.
5. Surface the `cairn identity` subcommand: `provision`, `list`, `show`,
   `rotate`, `revoke`.
6. Provide enough rotation + revocation plumbing for the AC verification
   fixtures to land. Older keys remain queryable for read-time signature
   verification (per brief §4.2 “Earlier operations remain valid…”).

### 2.2 Explicitly out of scope

- WAL coupling for identity mutations (issue #50 doesn’t require it; will
  ride on the §5.6 wiring issues for ingest/forget).
- `IdentityAdmin` countersig requirement on revocation (brief §4.2 P2).
- `status` verb wiring of identity counts (issue #51).
- Signing actual `MemoryRecord` envelopes or `ConsentReceipt`s (separate
  ingest / consent issues).
- `IdentityProvider` plugin trait for SSO/OIDC/hardware key (P1+).
- Multi-hop `actor_chain`, countersignatures, trust scoring (P2).
- Cross-deployment federation, share-link issuance.

## 3. Architectural choices (with rationale)

### 3.1 New crate `cairn-keychain`

Adapter crate, sole responsibility: implement the `Keystore` trait against
the OS secret store via the `keyring` crate.

- Keychain I/O is a contract distinct from SQLite I/O. CLAUDE.md §4 invariant
  4 (“Adapters implement one trait”) and §6.1 (“keep adapter crates free of
  cross-adapter imports”) push us to one crate per adapter.
- `cairn-mcp` and `cairn-sensors-local` will eventually need to sign without
  pulling `cairn-cli` into their dep tree. Putting the keystore in
  `cairn-cli` would force that.
- The `keyring` crate brings platform-specific transitive deps (Apple
  Security framework on macOS, libsecret on Linux). Isolating it in one
  adapter keeps the blast radius small and lets future per-OS feature gates
  live in one place.

Rejected alternatives:
- **Module inside `cairn-cli`**: violates adapter-per-crate rule; couples
  later consumers to the CLI.
- **Module inside `cairn-store-sqlite`**: blends two distinct adapter
  contracts (secret store + record store) into one crate.

### 3.2 Identity registry lives in `cairn-store-sqlite`

There is one authoritative SQLite file (`.cairn/cairn.db`, brief §3 / §4.2
durability topology). Identity is metadata that lives in that file; it
belongs to whichever crate owns the schema. New migration `0002_identity.sql`
adds the tables. The `IdentityRegistry` trait is defined in `cairn-core`
and implemented in `cairn-store-sqlite`.

### 3.3 Wire-form rename `usr:` → `hmn:` (atomic, multi-site migration)

Brief §4.2 specifies `hmn:<slug>:<rev>` for `HumanIdentity`. The current
tree hard-codes `usr:` in several places, not just the parser. The rename
must land atomically across **all** of them in this PR; partial migration
would cause records to parse in one site and fail validation in another.

Sites that must change in lockstep:

**The migration inventory is "every hit of `rg -n 'usr:' .`".** Any
file the search finds — hand-written, generated, fixture, snapshot,
test asset, doc — must be updated in the same PR. The table below
seeds the search with known load-bearing sites; it is **not**
exhaustive and the PR must run the sweep again at the end as a CI
gate, with zero non-historical hits remaining.

| Known site | Reason |
|---|---|
| `crates/cairn-core/src/domain/identity.rs:37–38` | Hand-written `Identity::parse` prefix branch. |
| `crates/cairn-core/src/domain/actor_chain.rs:51–57, 89–95` | Hand-written role validation: `Principal` is required to be a human. |
| `crates/cairn-core/src/domain/record.rs:463–547` | Hand-written `MemoryRecord` validator — checks issuer prefix on the write path. |
| `crates/cairn-core/src/domain/capture.rs` | Hand-written capture validator. |
| `crates/cairn-core/src/domain/canonical.rs` | Canonical-form serializer. |
| `crates/cairn-core/src/domain/capture_attribution.rs` | Capture attribution validator. |
| `crates/cairn-core/src/verifier.rs` | Envelope verifier. |
| `crates/cairn-idl/schema/common/primitives.json` | IDL `Identity` primitive regex; source of every generated validator below. |
| `crates/cairn-idl/src/codegen/emit_sdk.rs:429–435, 2623, 2700–2707` | IDL → SDK code generator — emits identity-prefix checks into every downstream SDK. Updating only the JSON without re-running the generator and updating the emitter constants leaves stale string literals in the generated SDK output. |
| `crates/cairn-core/src/generated/common/mod.rs:91–95` | Generated validator — regenerated from IDL. |
| `crates/cairn-core/src/generated/envelope/mod.rs:436, 507–513` | Generated envelope validator — regenerated from IDL. |
| Any other generated MCP/SDK schema artefact under `crates/*/src/generated/` or `crates/cairn-idl/schema/**`. | Sweep after codegen run. |
| `fixtures/v0/**` (record, envelope, capture fixtures, snapshot `*.snap`) | Hand-edit + `cargo insta review` regenerate cycle for every snapshot whose contents change. |
| Any other fixture, doctest, integration test, doc snippet, or sample payload that the sweep finds. | Update in lockstep. |

Implementation order in this PR (rides on impl-order step 1, §11):

1. Update IDL primitive `crates/cairn-idl/schema/common/primitives.json`.
2. Update the SDK emitter constants in `crates/cairn-idl/src/codegen/emit_sdk.rs`
   (lines 429–435, 2623, 2700–2707) so the regenerator emits `hmn:`.
3. Run `cargo run -p cairn-idl --bin cairn-codegen` and commit the
   regenerated `generated/` files across every crate.
4. Update `domain/identity.rs::parse` and `IdentityKind::Human` prefix.
5. Update `domain/actor_chain.rs` hand-written role checks.
6. Update `domain/record.rs` hand-written `MemoryRecord` validator.
7. Sweep `rg -n 'usr:' .` — must return zero non-historical hits before the
   PR moves on. CI gate `cargo nextest run --workspace` catches any missed
   site because the generated validators flip first.

Brief is source of truth (CLAUDE.md §1). This is a load-bearing rename.
Listed in the PR description’s “invariants touched” section.

### 3.4 Pure provisioning logic in `cairn-core`

`build_provisioning_plan(...)` in `cairn-core::domain::identity::provision`
is a pure function that takes inputs (slug, harness/model/role, sensor
descriptor) plus an injected RNG and returns a `ProvisioningPlan` describing
the keypair to mint, the registry rows to write, and the secret-store entry
to add. The CLI / sensor crate executes the plan against the two adapters.
Keeps the core dependency-free (CLAUDE.md §3 boundary rule) and makes the
provisioning logic deterministic-given-RNG, testable without keychain or
SQLite.

### 3.5 Cross-store provisioning atomicity (registry-first, with reconciliation)

Provisioning crosses two adapters (keychain + SQLite) that cannot share a
transaction. Order of operations and recovery behaviour are part of the
contract — not left to the implementation.

The state machine, persisted in `identities.provisioning_state`:

```
pending          → active                          (happy path)
pending          → (deleted)                       (reconciliation finds no keychain entry; row removed + audit log entry, not a persisted state)
active|revoked   → purge_pending → purged          (§3.10 purge)
active           → revoked                         (operator revocation)
```

There is no persisted `orphaned` value. A pending row whose keychain
entry never landed is hard-deleted by reconciliation; the deletion is
recorded in `tracing` (`outcome = "orphaned_pending_removed"`) but
nothing remains in the registry. This keeps the schema CHECK
constraint, visibility filters, and reconciliation behaviour aligned
on a single contract.

Provisioning flow (single identity). **Mint first, persist second:** the
keypair must exist before any row containing its public key is written, so
that reconciliation has real material to compare against.

1. **Mint.** Generate the Ed25519 keypair in memory using the injected
   CSPRNG. Pure, in-process, no I/O — it cannot crash between sub-steps.
   Derive the public key.
2. **Reserve.** Open one SQLite transaction:
   - Insert `identities` row with `provisioning_state = 'pending'` and
     `current_key_version = N`.
   - Insert `identity_keys` row with the **derived public key from step 1**.
   Commit. Reads through `IdentityRegistry::get_identity` filter pending
   rows out by default.
3. **Persist secret.** `Keystore::store_keypair` writes the private-key
   bytes under the `SecretHandle` for `(vault_id, identity, key_version)`.
   On failure: return the error; the pending row remains for reconciliation
   to clean up (no keychain entry → orphan path).
4. **Activate.** Single SQLite UPDATE flips `provisioning_state` to
   `'active'` and stamps `activated_at`. Failure here leaves the keychain
   entry with a pending registry row, both carrying matching public-key
   material — reconciliation can confirm and activate.

The public key written at step 2 is the **same bytes** that step 4 needs
to verify against in the keychain — minting is the only producer.

Reconciliation has two open paths so that startup hygiene cannot
block the very commands designed to fix problems:

- **`IdentityService::open()`** — used by issuer-dependent verbs
  (`ingest`, `capture_trace`, `forget`, `rotate`, `revoke`, etc.). Runs
  the full reconciliation sweep below; any per-identity
  `KeyMaterialMismatch` is reported as `tracing::error!` plus accumulated
  into a `ReconciliationReport` returned to the caller. The verbs gate
  via `require_default_issuer()` (§3.7) which surfaces the report.
  Per-identity mismatches are **never** propagated as fatal errors out
  of `open` itself — that would lock the operator out of recovery.
- **`IdentityService::open_for_maintenance()`** — used by the recovery
  and inspection commands (`list`, `show`, `reconcile`, `repair`,
  `purge`, `finalise-binding`, `vault-id-recover`, `init-defaults`,
  `provision`). Skips reconciliation entirely; opens the registry +
  keystore handles and returns. The maintenance commands run the
  reconciliation steps they need against the specific identities they
  target, never as a global blocking sweep.

The reconciliation sweep itself, when it does run, processes each
`pending` row:

- **Keychain entry missing** → delete the registry row, mark the outcome
  `orphaned` in the audit log (`tracing` warn). (Crash before or during
  step 3.)
- **Keychain entry present** → load the private key, derive its public key,
  and compare it against the `identity_keys.public_key` reserved at step 1.
  - **Match** → flip the row to `active`. (Crash between steps 3 and 4.)
  - **Mismatch** → record `KeyMaterialMismatch { id }` in the
    `ReconciliationReport` and log at `error`. The row stays `pending`;
    no destructive action runs. Recovery requires an explicit operator
    decision (§3.10): either restore the keychain backup whose private
    key matches the reserved public key, or run `cairn identity purge
    <id>` (audit-gap, requires the out-of-band ack file). `cairn
    identity repair` is reconciliation-only and **never** deletes
    keychain entries or registry rows on its own — mismatch is a strong
    signal of cross-vault namespace collision (§3.7) or out-of-band
    tampering and must not auto-recover. Because the mismatch is
    recorded in a report rather than thrown as an error from `open()`,
    it never blocks the operator from reaching `repair` or `purge`.

Idempotent re-provision of the same `(identity, key_version)` is a no-op
once `active`. A re-provision attempt while a different `pending` row
exists for the same identity returns `RegistryError::ProvisioningInFlight`
— the operator must wait for reconciliation or call
`cairn identity reconcile` explicitly.

`cairn identity reconcile` is added as a maintenance subcommand for the
case where the operator needs to force the sweep without restarting a
long-lived process (MCP server, workflow host).

This satisfies CLAUDE.md invariant 5 ("WAL + two-phase apply for every
mutation") in spirit even though identity mutations don’t enter the
record-WAL: the two-phase pattern lives inside this module.

### 3.6 Key retention model (private keys ring-bounded, public keys immortal)

Brief §4.2 says: "Each identity owns a key ring (current + up to two
predecessors)" — but it also says "records signed by an older version still
verify until TTL expires" and "Earlier operations remain valid…". Read
together: the **signing** ring is bounded; the **verification** material
must outlive any record that references it.

P0 split:

- **Keystore ring (private keys).** `Keystore` keeps current + ≤ 2
  predecessor private keys. On rotation, the eldest private key is deleted
  from the keychain. Reduces blast radius if a key is exfiltrated.
- **Registry archive (public keys).** `identity_keys` rows are
  **append-only**. Rotation inserts a new row; revocation marks the
  identity but never deletes prior public-key rows. Verification of any
  historical record is always possible via `IdentityRegistry::get_key`.
- **No TTL-based purge in this PR.** A future issue introduces a record
  scan that proves "no still-valid record references key version N" before
  permitting public-key garbage collection. Until that scan exists, the
  registry never deletes a public key.

`identity_keys` schema gains a `superseded_at TEXT` column that records
when a key version was rotated past — informational only; not used for
deletion.

### 3.7 Per-vault keystore namespacing + first-run gate

Two Cairn vaults owned by the same OS user must not share keychain
entries. Identity ids alone do not isolate them — `hmn:alice:v1` collides
with itself if Alice has two vaults. The keystore namespace must be
vault-scoped.

**Vault id.** `cairn bootstrap` writes `.cairn/vault.id` containing a
randomly minted ULID on first run. The file is committed to the
filesystem-only bootstrap contract — no DB write, no keychain access.
Subsequent bootstraps preserve the existing id (idempotent). The bootstrap
spec (`2026-04-26-bootstrap-design.md`) is updated in the same PR to add
this single artefact; the receipt grows a `vault_id` field. This is the
sole change to the bootstrap contract.

**`vault.id` is non-regenerable once a keychain binding exists.** Because
every keychain entry is namespaced under `cairn:<vault_id>`, regenerating
`vault.id` would point the process at a fresh keychain namespace and
desynchronize every keychain entry in one stroke. The binding is proved
through a single dedicated artifact, **not** by probing per-identity
keys (whose private halves can age out of the keystore under §3.6's
ring-depth policy).

**Vault witness.** The first time identity provisioning runs against a
vault, it mints a 32-byte random `vault_witness` and stores it in three
places. Order is load-bearing — the **filesystem sentinel is written
first** so a crash in any subsequent step still leaves bootstrap able to
detect that the namespace may be claimed.

| Where | What | Lifetime |
|---|---|---|
| Filesystem | `.cairn/vault.binding` containing `sha256(witness)` | **Written first**, before any keychain or DB write. Bootstrap reads this file as a sentinel without opening the DB or keychain. |
| Keychain | service `cairn:<vault_id>`, account `__vault_witness__`, secret = the 32-byte witness | Written second. Never rotated, never deleted (separate from the per-identity ring). |
| SQLite | `vault_meta(witness_sha256 BLOB NOT NULL)` row, set by migration `0002_identity.sql` during the same transaction that reserves the first identity | Written third, in the transaction that reserves the first pending identity. Authoritative copy used by recovery. |

**Cross-process serialization.** `commit_first_identity` and
`finalise-binding` both acquire an exclusive advisory file lock on
`.cairn/vault.binding.lock` (via `fs2::FileExt::try_lock_exclusive` or
equivalent on Windows / Unix) **before** any other I/O in the sequence.
The lock file is created on demand if absent. Concurrent
`init-defaults` runs serialize on this lock: the loser either waits
(default, with a 30 s timeout) or, if `--no-wait` is passed, returns
`IdentityServiceError::FirstBindInFlight` mapped to
`EX_TEMPFAIL = 75`. Once the lock is held, the holder re-checks for
`.binding` / `.binding.pending` and either runs `finalise-binding`
recovery or starts a fresh first-bind; the loser, on acquiring the
lock after the winner releases, sees the committed state and is a
no-op. Tests dispatch two concurrent `init-defaults` processes against
the same vault and assert exactly one binding lands.

Concretely, `IdentityService::commit_first_identity` writes a
**two-phase sentinel** under that lock so that a crash before the
keychain step is always recoverable from local disk alone:

1. `fs::write(".cairn/vault.binding.pending", VAULT_BINDING_PENDING_V1
   { vault_id, witness_bytes (32B) })` (mode 0600) and `fsync`. The
   pending file holds the actual witness bytes — recovery can re-drive
   step 2 from this file. Bootstrap treats `.cairn/vault.binding.pending`
   as equivalent to `.cairn/vault.binding` for the
   refuse-if-present check.
2. `Keystore::store_secret(witness_handle, witness_bytes)`.
3. `IdentityRegistry::reserve_first_identity(vault_id, record, key,
   witness_hash, binding_path)` — single SQLite txn that `stat`s
   `binding_path` inside the transaction, inserts the `vault_meta` row
   (with `vault_id` set from the argument so DB-first recovery in §3.7
   has the authoritative source), and reserves the first pending
   identity. Rolls back atomically if the sentinel is absent.
4. Atomically rename `.cairn/vault.binding.pending` →
   `.cairn/vault.binding` (final, hash-only — `fs::write(...,
   sha256(witness))` followed by `fs::remove_file(pending)`, or a true
   atomic rename if we choose to write the same hash format to both
   paths). The pending file's witness-bearing bytes only exist on disk
   between steps 1 and 4.
5. `Keystore::store_keypair(identity_handle, identity_secret)`.
6. `IdentityRegistry::activate_identity(...)`.

**Crash recovery from local disk alone:**

| State on disk | What recovery does |
|---|---|
| `.binding.pending` exists, `.binding` absent | `cairn identity finalise-binding` reads the persisted vault id + witness bytes from the pending file, idempotently writes the keychain witness (no-op if already present), idempotently drives the DB transaction, then performs the rename to `.binding`. No external backup required. |
| `.binding` exists, `.binding.pending` absent | Witness bytes are gone from disk by design (only the hash remains). If keychain + DB are healthy → vault is bound, nothing to do. If keychain witness is missing → `finalise-binding` requires the operator to supply `--vault-id` (or matches it via enumeration where supported) so it can re-discover and verify; if no recovery candidate is found, the path is `--abandon` (with `--vault-id`). |
| Both files exist | Inconsistent crash (rename interrupted). `finalise-binding` retries the rename. |
| Neither file exists | No binding committed; bootstrap mints fresh. |

The pending file holds 32 random bytes that are not signing material —
they exist solely as a binding tag — so brief on-disk presence at mode
0600 inside `.cairn/` is acceptable. The window is bounded to steps
1-4, which is a few SQLite calls plus a keychain write. Tests inject a
crash at every transition between steps 1 and 4 and confirm that
`finalise-binding` always recovers without external state.

**Canonical recovery contract.** The crash-state table above is the
single authoritative source. The two-phase sentinel guarantees that any
crash before the rename to `.binding` leaves `.binding.pending` with
the witness bytes intact, so step-2 crashes are always recoverable from
local disk via `finalise-binding` — there is no "abandon-only" branch
for that case.

`--abandon` is reserved for one specific situation: the operator has
decided the vault should not be bound (e.g., they accidentally started
provisioning in the wrong directory) and wants to clear the sentinel
state. The flow proves "no binding committed" via two checks:

1. **DB check** — confirm `.cairn/cairn.db` is missing **or** contains
   zero `vault_meta` rows. Always implementable.
2. **Keystore check** — confirm there is no orphan witness for the
   specific vault being abandoned. The check is always vault-scoped,
   never global (multi-vault coexistence requires it):
   - The operator passes `--vault-id <id>` (the original vault id they
     are abandoning, recovered from a backup of `.cairn/vault.id`, or
     known to have never been committed if this is a fresh-vault
     first-run failure where the random ULID was logged in CLI
     output).
   - On every backend the CLI calls
     `Keystore::load_secret(SecretHandle::for_witness(<id>))`. A
     definitive `NotFound` is the authoritative negative. The check
     touches only the candidate namespace; other vaults' namespaces
     are not enumerated and not consulted.
   - For first-run failures where the witness bytes are still on disk
     in `.cairn/vault.binding.pending` (see §3.7 below), the operator
     does not need `--abandon` at all — `cairn identity finalise-binding`
     drives the binding to completion using the persisted bytes.

The dropped global enumeration also removes the multi-vault deadlock:
vault A's abandon never inspects vault B's keychain entries.

3. Deletes `.cairn/vault.binding` (and `.cairn/vault.binding.pending`
   if present) and writes a `tracing::warn!`
   `audit_gap = "binding_abandoned"` line plus a JSON receipt entry
   recording `evidence = "vault_id_negative_probe"`.

Net invariant: bootstrap is always conservative (any sentinel present
→ refuse). Recovery is always available: `finalise-binding` (no
`--abandon`) drives any sentinel state to completion using either the
witness bytes from `.binding.pending` or the keychain witness reachable
via `--vault-id`. `--abandon --vault-id <id>` is the explicit
operator-initiated path to clear a sentinel after proving the
namespace is empty.

**Bootstrap blocks only on vault-local evidence.** Two Cairn vaults on
one machine is a supported configuration (each has its own `vault_id`
and its own keystore namespace), so the global keystore probe is **not**
used as a bootstrap blocker — that would refuse to bootstrap a fresh
checkout whenever any unrelated Cairn vault exists on the same machine.
Bootstrap reasons strictly from local-to-this-vault signals.

Operators who have nuked the entire `.cairn/` directory but want to
recover their pre-existing keystore-bound identities use the explicit
`cairn identity vault-id-recover` flow (§3.7 below) — that command
opts into keystore enumeration deliberately and pairs it with a
locally-supplied witness hash (read from a backup of `.cairn/vault.binding`
or `.cairn/cairn.db`). Recovery without any local evidence at all is
not supported: there is nothing to prove the vault id against.

**Bootstrap guard.** The bootstrap delta runs the following sequence
before deciding whether to mint a new `vault.id`:

1. If `.cairn/vault.id` exists → use it (idempotent path).
2. If `.cairn/vault.id` is missing → check vault-local durable
   evidence in priority order. **Bootstrap mints a fresh ULID only when
   every signal says no binding exists.** Any positive signal makes it
   fail closed.
   - **Sentinel check (filesystem only).** If `.cairn/vault.binding`
     or `.cairn/vault.binding.pending` exists → fail closed with
     `BootstrapError::VaultIdLost`.
   - **DB check.** Open `.cairn/cairn.db` read-only (no schema upgrade,
     no write). If the file exists, the `vault_meta` table is present,
     and it has the single row → fail closed with
     `BootstrapError::VaultIdLost`. The CLI hint instructs the operator
     to run `cairn identity vault-id-recover`, which (per §3.7) reads
     `vault_meta.vault_id` and rewrites both `.cairn/vault.id` and the
     binding sentinel from authoritative state. If the DB exists but
     the table is missing (pre-`0002_identity.sql` migration state), it
     contributes no signal — the table cannot have a row and the
     filesystem sentinel is the authority.
   - **All signals negative** (no sentinel, DB missing **or**
     sentinel-table missing-or-empty) → mint a fresh ULID and write
     `.cairn/vault.id`.

The DB read closes the previous reminting hole: even if both
filesystem sentinels are deleted out-of-band, a vault that committed
identities is still durably bound via `vault_meta`, and bootstrap
detects that. Recovery via `vault-id-recover` is then trivial because
the vault id is sitting in the DB.

The same hint shape applies to all positive signals:

   ```
   cairn bootstrap: .cairn/vault.id is missing but .cairn/vault.binding exists.
     A keychain witness is committed to this vault under an unknown vault id.
     Restore the original .cairn/vault.id from backup, or run:
       cairn identity vault-id-recover --probe-keychain
     Bootstrap will not mint a new vault id while a binding exists; doing so
     would desynchronize every keychain entry from the registry.
   ```

The guard is purely filesystem; it does not open `.cairn/cairn.db`, so
schema/migration state cannot create an upgrade-window false negative.
If `.cairn/vault.binding` exists but `.cairn/cairn.db` does not (the
binding sentinel was restored from backup but the DB was lost), bootstrap
still refuses to mint — the operator's keychain is still bound and must
remain so until they explicitly purge it.

**Recovery.** `cairn identity vault-id-recover [--probe-keychain | --vault-id <id>]`:

1. Read `vault_meta.vault_id` and `vault_meta.witness_sha256` from
   `.cairn/cairn.db`. The DB is now the authoritative source for the
   vault id when `.cairn/vault.id` is lost — recovery does not depend
   on keystore enumeration to discover the id. If `vault_meta` is
   reachable, the recovery happy path is:
   - Load the secret at `SecretHandle::for_witness(vault_meta.vault_id)`
     and confirm its SHA-256 matches `vault_meta.witness_sha256`.
   - Write `.cairn/vault.id` with the recovered ULID.
   - Done. This path works on every backend, including DPAPI, because
     it never enumerates.
2. If `vault_meta` is unreachable (DB missing, table absent, single row
   absent), fall back to the sentinel-only flow:
   - Load `.cairn/vault.binding` for the witness hash.
   - Discover candidates: `--probe-keychain` (default on macOS /
     Linux) calls `Keystore::list_vault_namespaces("cairn:")`;
     otherwise the operator supplies `--vault-id <id>`.
   - For each candidate, load the witness secret and compare hashes.
   - Accept exactly the unique candidate whose hash matches.
   - Without `vault_meta` *and* without backup of `vault.id` *and*
     with `DiscoveryUnsupported` → recovery genuinely impossible;
     restore from backup is the only path. Documented as such.

The DB-first flow makes the common case (DB intact, only `.cairn/vault.id`
deleted) trivially recoverable on every backend. The sentinel-only
flow is the fallback for "DB also lost"; on DPAPI it requires either
backup or operator-supplied `--vault-id`. This eliminates the previous
Windows brick scenario whenever the DB is intact.

Recovery does **not** depend on any per-identity key surviving in the
keystore, so routine rotation under §3.6 cannot brick recovery. The
witness is its own atomic binding artifact and is never aged out.

`vault_meta` is added by migration `0002_identity.sql` so it is always
co-resident with the `identities` and `identity_keys` tables.

Test matrix covers: vault with zero identities (no binding yet,
bootstrap allowed), vault with `vault.binding` but missing DB (bootstrap
refused), vault with binding + DB but `vault.id` lost (recovery succeeds
on the unique candidate; fails closed on injected duplicate witness in a
second namespace), vault after multiple rotations (recovery still works
because the witness is untouched).

**Secret handle format.** `Keystore` uses:

- service: `cairn:<vault_id>` (e.g., `cairn:01HXY…`)
- account: `<identity-wire-form>#k<version>`

Wrong-vault leakage is impossible: the service segment carries the vault
id and is verified by the keystore on every load.

**First-run gate (issuer-dependent verbs only).** `cairn bootstrap` exits
0 with a vault that has zero identities. Cairn's record model is
single-signer (every record carries one `signer_identity` /
`signer_key_version`), so the gate is the same for every issuer-dependent
verb: at least one live attributable signer must exist.

`require_attributable_signer(target: Option<&Identity>)` returns the
chosen signer (or an error). Selection rules:

- For **ordinary writes** (`ingest`, `capture_trace`, `forget`,
  no `target`) — pick whichever default is live, prefer the default
  agent (records authored by code paths attribute to the agent;
  human-attributable cases pass `target = Some(default_human)`).
  Either default alone is sufficient.
- For **trust-state mutations** (`cairn identity rotate <id>` /
  `revoke <id>`, `target = Some(<id>)`) — apply the §3.10 priority
  rules: a live default that is not `<id>`, falling through to `<id>`'s
  own key for non-default targets when both defaults are dead.

The two-step check (presence + §3.8 liveness) is the same in both
modes. Failures map to:

- `DefaultsNotInitialized` (`EX_USAGE = 64`) — no default identity
  rows exist at all.
- `NoLiveAttributableSigner` (`EX_UNAVAILABLE = 69`) — defaults exist
  but every candidate fails liveness.
- `KeyMaterialDesynchronized { id, reason }` (`EX_DATAERR = 65`) —
  surfaced when the chosen signer's keychain entry is gone or
  mismatched. Only one default needs to be healthy; the other can
  remain in this state and still let ordinary writes proceed under
  the healthy signer.

This avoids the previous availability bug: deleting either default
keychain entry no longer takes `ingest` down. The chosen signer is
recorded in the per-record `signer_identity` / `signer_key_version`
metadata so audit traces are intact.

The liveness step closes the partial-failure window: a row that says
`active` but whose keychain entry was deleted out-of-band is rejected
**before** any verb-specific work runs, instead of failing later when
the signer reaches for the key. The check is one `Keystore::load_signing_key`
+ in-memory pubkey derivation per default signer (≤ two of them at P0),
so the cost is sub-millisecond.

The presence-only mode (`require_default_issuer_presence`) is exposed
separately for read-only verbs that report on identity state without
actually signing — `cairn status`, future SDK introspection — so they
do not pay the keychain hit on the hot path.

```
cairn ingest: no default identities found
  run `cairn identity init-defaults` to provision the local human + agent identities
```

**Recovery + inspection commands bypass the gate.** `IdentityService::open`
itself never enforces the default-issuer check — that would lock the
operator out of the very commands they need to fix the problem. The
following subcommands open the service and return useful output even
when defaults are missing or desynchronized:

| Command | Behaviour without defaults |
|---|---|
| `cairn identity list` | Returns whatever rows exist (empty list is valid output). |
| `cairn identity show <id>` | Returns the row or `NotFound`. |
| `cairn identity provision …` | Allowed; this is how defaults get created. |
| `cairn identity init-defaults` | Allowed; primary remediation path. |
| `cairn identity reconcile` | Allowed; cleans up `pending` rows regardless of which identities exist. |
| `cairn identity repair <id>` | Allowed; reconciliation only — never mutates trust state (§3.10). |
| `cairn identity purge <id>` | Allowed; requires `.cairn/maintenance/purge-ack`. Tombstones the identity (does **not** delete `identity_keys`); audit gap is logged and surfaced in the receipt. |
| `cairn identity finalise-binding` | Allowed; finishes / abandons a partially-committed witness when the sentinel exists but the keychain or DB never landed. |
| `cairn identity vault-id-recover` | Allowed; runs without ever opening `IdentityService` in a write-capable way. |

`cairn identity rotate` and `cairn identity revoke` **do** require the
default-issuer gate because revocation/rotation is itself a signed
operation that must be attributable. The CLI hint when they fail tells
the operator to run `init-defaults` first, or — in genuinely
unrecoverable scenarios — to use `cairn identity purge` (§3.10) which
makes the audit gap explicit instead of silently bypassing attribution.

`cairn bootstrap` human-readable output gains a final line:

```
next:     cairn identity init-defaults
```

The split is now explicit at three layers (goals §2.1, design §3.7, CLI
§4.5) and the failure mode for "bootstrapped but no identities" is loud,
not silent.

### 3.8 Idempotent provision verifies key material, not just registry presence

`cairn identity provision` and `cairn identity init-defaults` treat an
`active` row as a no-op only after a **liveness check** on the keychain
entry:

1. Read the registry's current `key_version` for the identity.
2. `Keystore::load_signing_key` for the matching `SecretHandle`.
3. Derive the public key, compare to `identity_keys.public_key`.
4. **All three pass** → no-op (true idempotent path).
5. **Keychain entry missing or mismatched** → fail closed with
   `IdentityServiceError::KeyMaterialDesynchronized { id, reason }` mapped
   to `EX_DATAERR = 65`. The CLI hint follows the §3.10 contract exactly:
   `repair` cannot fix this (it is reconciliation-only and would never
   mutate trust state); the operator chooses between restoring a keychain
   backup, running `cairn identity rotate <id>` (signed, attributable —
   requires the default-issuer gate), or `cairn identity purge <id>`
   (audited operator-of-last-resort).

This closes the "active row, missing keychain secret" hole: the system
self-detects on the very next provision attempt rather than silently
treating a stuck identity as healthy.

`IdentityService::open` runs the same liveness check on every `active`
identity at startup and emits a `tracing` warn for any desynchronized
entry, so long-lived processes (MCP server, workflow host) surface the
condition without waiting for a verb to trip on it.

### 3.9 Username → identity slug normalization

The wire format limits the body to `[A-Za-z0-9._:-]+`. `whoami::username()`
returns the raw OS account name, which on real workstations may contain
spaces (`"Sophia Wang"`), apostrophes (`"o'connor"`), accented characters
(`"renée"`), or non-Latin scripts. Feeding the raw value into
`Identity::parse` would make `cairn identity init-defaults` fail on a
perfectly normal first-run machine — and because the first-run gate
(§3.7) blocks issuer-dependent verbs until defaults exist, that becomes
an availability bug, not a cosmetic one.

`cairn-core::domain::identity::provision::normalize_human_slug(raw: &str)
-> Result<String, DomainError>` defines the canonical normalization
exactly once. CLI bootstrap, MCP, and SDK all call it; nobody minds-their-
own-business about Unicode rules:

1. **NFKD normalize** the input.
2. Strip combining marks (drops accents).
3. Lowercase via `to_lowercase()`.
4. Replace any character not in `[a-z0-9._:-]` with `-`.
5. Collapse consecutive `-` to a single `-`.
6. Trim leading and trailing `-` and `.`.
7. If the result is empty (input was all punctuation / non-mappable
   script) → fall back to the literal string `local`.
8. If the result exceeds 63 bytes → truncate to 63 and re-trim trailing
   `-`/`.`.

`init-defaults` calls `normalize_human_slug(whoami::username())`. If the
resulting `hmn:<slug>:v1` already exists in the registry under a
**different** vault provenance (which can only happen via a
re-run after manual identity deletion), the command appends a
discriminator (`-2`, `-3`, …) and reports the chosen slug in its receipt.
Operators who want a stable slug across machines can pass
`--slug <explicit>` to `cairn identity provision --kind human`.

Tests cover: ASCII account, account with spaces, all-Unicode account
(`"伶悧"` → `local`), apostrophe (`"o'connor"` → `o-connor`), accented
(`"renée"` → `renee`), 100-byte name (truncated to 63), and the empty
fallback.

### 3.10 `repair` is reconciliation-only; trust-state mutations require attribution

Earlier drafts let `cairn identity repair` "atomically revoke the active
row and re-provision a new key version" without sitting behind the
default-issuer gate. That is the wrong contract: revocation is a signed,
attributable trust-state mutation, and authorising it precisely when
attribution is broken (no defaults / desynchronized identity) is exactly
the audit hole adversarial reviews flag.

P0 split, all gate-aware:

1. **`cairn identity repair <id>`** — reconciliation only. It runs the
   §3.5 reconciliation step against a single identity:
   - `pending` row, keychain entry missing → `delete_pending`.
   - `pending` row, keychain entry present, public-key match →
     `activate_identity`.
   - `pending` row, keychain entry present, public-key mismatch → fail
     closed (`KeyMaterialMismatch`); the operator must `purge` first.
   - `active` row, keychain entry healthy → no-op (already healthy).
   - `active` row, keychain entry missing or mismatched → fail closed
     with `KeyMaterialDesynchronized`; `repair` does **not** auto-revoke.
     The operator's options are recorded explicitly in the CLI hint:

     ```
     cairn identity repair <id>: active identity is desynchronized.
       Options:
         (a) restore the keychain backup that contains the original key, then re-run repair.
         (b) if the original key cannot be restored, run:
               cairn identity rotate <id>
             (requires the default-issuer gate; rotates the identity to a new key
              version while preserving the historical public-key archive.)
         (c) if the identity is unrecoverable and operator accepts the audit gap:
               cairn identity purge <id>
             (requires writing .cairn/maintenance/purge-ack manually; produces no
              signed revocation receipt; intended only for last-resort cleanup.)
     ```

   `repair` therefore stays safe to expose without the default-issuer
   gate: every code path it can take either no-ops, transitions a
   pending row, or fails closed.

2. **`cairn identity rotate <id>` and `cairn identity revoke <id>`** —
   require an attributable signer. Signer selection (in priority order):
   1. Any live default signer that is **not** `<id>` (the same path
      as before — a still-live default agent rotates a broken default
      human, etc.).
   2. If `<id>` is **not a default** and `<id>`'s own current key is
      still live (it can sign) → `<id>` may self-attribute the
      rotation/revocation. The persisted `RotationReceipt` records
      `signer = <id>` and the audit log emits
      `attributable_via = "self"` so the audit trail is explicit. This
      preserves attribution for non-default identities even when both
      defaults are unrecoverable.
   3. If neither is available — `<id>` is itself a default **and** the
      other default is also unavailable, **or** `<id>` is non-default
      but its own key is also broken — the operation returns
      `IdentityServiceError::NoLiveAttributableSigner` mapped to
      `EX_UNAVAILABLE = 69`. The CLI hint:

      ```
      cairn identity rotate <id>: no live attributable signer is available.
        Restore a keychain backup (for either default, or for the target identity itself), or
        accept the audit gap by running:
          cairn identity purge <id>
      ```

   Self-attribution is restricted to non-defaults because the defaults
   are the source of attribution for everything else; allowing a broken
   default to self-rotate would mean the audit trail can never assign
   blame off the identity that may itself be compromised. Non-defaults
   carry no such load-bearing role and self-rotation costs nothing
   beyond the explicit audit-log marker.

3. **`cairn identity purge <id>`** (new, in `cairn-cli`) — operator-of-
   last-resort. **Does not hard-delete.** It moves the identity to a
   `purged` provisioning state, deletes the corresponding **private**
   keys from the keychain (so no future signing is possible), and stamps
   `purged_at` + `purge_reason` on the row. The `identity_keys` archive
   (public keys, append-only per §3.6) is left intact so signature
   verification of historical records continues to work — the design's
   "earlier operations remain valid" promise is preserved.

   Requires the operator to have authored `.cairn/maintenance/purge-ack`
   on local disk (containing the identity wire form being purged) before
   the command runs. The CLI does not create or prompt for the file; it
   only verifies the contents match. The audit gap is explicit:
   `tracing::error!` records the purge with
   `audit_gap = "no_signed_revocation"` and the receipt JSON includes
   the same field. Tests assert the operation is impossible from the
   MCP surface (no filesystem access of the correct shape) and from the
   `cairn-cli` happy path (the ack file does not exist by default).

   **Two-phase state machine across registry + keystore.** SQLite and
   the keystore cannot transact together, so `purge` is explicit about
   ordering, verification, and recovery. The state machine adds one
   transitional state, `purge_pending`, between `active`/`revoked` and
   `purged`:

   ```
   active|revoked → purge_pending → purged
   ```

   Steps:

   1. **Reserve.** `IdentityRegistry::mark_purge_pending(id, ack,
      reason)`. Adapter UPDATEs `provisioning_state` to `purge_pending`
      and stamps `purged_at` + `purge_reason`. The identity is
      immediately unusable for signing through every gate
      (`require_full_default_issuer` and `require_attributable_signer`
      both reject `purge_pending` and `purged`).
   2. **Delete + verify each key.** Iterate every `identity_keys` row
      for the identity. The retained private-key ring is current + 2
      predecessors (§3.6); older versions have already been evicted
      from the keystore as part of normal rotation. The loop treats
      that case as already-clean and continues:
      a. `Keystore::delete_keypair(handle)`. If the call returns
         `KeystoreError::NotFound` **and** `key_version` is outside
         the retained ring (i.e. older than `current_key_version - 2`),
         the version is already purged from the keystore — record
         `outcome = "already_evicted"` in the audit trail and
         continue. Otherwise propagate the error.
      b. `Keystore::load_signing_key(handle)` and confirm the result
         is `KeystoreError::NotFound`. Any other outcome means the
         delete did not actually take effect; the loop aborts and the
         row stays `purge_pending`.

      Result: every retained version is positively verified deleted;
      every aged-out version is treated as already-deleted (which the
      keystore confirms by `NotFound` before the verify step). No
      `purge_pending` row gets stuck because of routine rotation
      eviction.
   3. **Finalise.** Once every version has been verified-deleted, call
      `IdentityRegistry::finalise_purge(id)`. The adapter's
      implementation is required to be a no-op when the row is already
      `purged` (idempotent reconciliation) and to fail closed
      (`RegistryError::PurgeIncomplete`) if the caller has not yet
      verified all keys — but enforcement in the registry is advisory
      only because it cannot itself observe keystore state. The CLI
      then consumes `.cairn/maintenance/purge-ack` and writes the
      audit-log entry.

   **Resume is explicit, not implicit.** Neither
   `IdentityService::open_for_maintenance` nor inspection commands like
   `list` / `show` scan or finalise `purge_pending` rows. The only path
   that re-drives steps 2-3 is `cairn identity purge --resume <id>`,
   which re-checks `.cairn/maintenance/purge-ack` (the same operator
   barrier required for the initial `purge`) before acting. This keeps
   an irreversible trust-state mutation behind a fresh, explicit
   operator acknowledgement instead of letting incidental maintenance
   traffic silently complete it.

   `cairn identity reconcile` is also explicit about its scope: it
   reconciles **identity-provisioning** state machines (the
   pending/active/orphaned cases from §3.5), not purge state. To
   resume a stuck purge the operator runs `purge --resume`, which is
   loud and audit-logged.

   `purged` is **only** reachable when every key version has been
   verified absent from the keystore, so the rest of the system can
   trust that a `purged` identity has no recoverable signing material.

   Effect summary:
   - Registry row state: `active` / `revoked` → `purge_pending` →
     `purged`. Row stays in all states.
   - `identity_keys` rows: untouched. Verification of historical
     records still resolves the issuer + key version.
   - Keychain entries for this identity (all key versions): deleted +
     verified absent before the row reaches `purged`. Failures park
     the row at `purge_pending` for reconciliation; they do not
     silently leave the row at `purged` with private keys still
     around.
   - The keychain witness for the vault is **not** touched.
   - `IdentityRegistry::get_identity(..., visibility:
     IdentityVisibility::Audit)` returns the row for forensic /
     audit reads; the default `IdentityVisibility::Operational` filters
     `pending`, `purge_pending`, and `purged` out.

This decouples "fix a half-completed provisioning" (safe, gateless) from
"mutate trust state without an attributable signer" (loud, deliberate,
auditable as an explicit gap rather than a silent override).

## 4. Crate-by-crate changes

### 4.1 `cairn-core`

New module `domain::identity` extensions:

- `KeyVersion(NonZeroU32)` newtype.
- `IdentityRevision(NonZeroU32)` newtype, wire-form `v<n>`.
- `PublicIdentityRecord { id: Identity, kind: IdentityKind, current_key_version: KeyVersion, created_at: DateTime<Utc>, revoked_at: Option<DateTime<Utc>>, revocation_signature: Option<Signature> }`
- `IdentityKeyEntry { identity_id: Identity, key_version: KeyVersion, public_key: VerifyingKey, signed_predecessor: Option<Signature>, created_at: DateTime<Utc> }`
- `Identity::parse` learns `hmn:`; drops `usr:`. `IdentityKind::Human`
  prefix becomes `hmn`. Unit tests cover all three kinds + invalid prefix
  rejection.

New module `domain::identity::provision` (pure):

- `mint_human_id(slug: &str, rev: IdentityRevision) -> Result<Identity, DomainError>`
- `mint_agent_id(harness: &str, model: &str, role: &str, rev: IdentityRevision) -> Result<Identity, DomainError>`
- `mint_sensor_id(family: &str, name: &str, host: &str, rev: IdentityRevision) -> Result<Identity, DomainError>`
- `ProvisioningPlan { identity: PublicIdentityRecord, key_entry: IdentityKeyEntry, secret_handle: SecretHandle }`
- `build_provisioning_plan(input: ProvisionInput, rng: &mut impl CryptoRng + RngCore, now: DateTime<Utc>) -> ProvisioningPlan`

New contract module `contract::keystore`:

```rust
pub trait Keystore: Send + Sync {
    // Ed25519 keypair operations (per-identity signing material).
    async fn store_keypair(&self, handle: &SecretHandle, secret: &SecretKeyMaterial) -> Result<(), KeystoreError>;
    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError>;
    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;

    // Opaque-bytes operations (vault witness; reserved for future
    // opaque-secret callers — strictly fewer guarantees than the
    // keypair API: no signing-key derivation, no zeroize on the
    // returned Vec, caller is responsible for handling sensitivity).
    // The witness is a 32-byte random tag, not a credential, so plain
    // bytes are appropriate.
    async fn store_secret(&self, handle: &SecretHandle, bytes: &[u8]) -> Result<(), KeystoreError>;
    async fn load_secret(&self, handle: &SecretHandle) -> Result<SecretBytes, KeystoreError>;
    async fn delete_secret(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;

    // Defence-in-depth bootstrap guard + recovery. Returns the set of
    // `vault_id`s for which a `__vault_witness__` entry is reachable
    // under service prefix `<service_prefix><vault_id>`. Backends that
    // cannot enumerate (notably DPAPI on Windows) return
    // `Err(KeystoreError::DiscoveryUnsupported)` rather than
    // `Ok(empty)`, so callers can fail closed instead of silently
    // assuming "no binding".
    async fn list_vault_namespaces(&self, service_prefix: &str) -> Result<Vec<VaultId>, KeystoreError>;
}
```

`SecretBytes` is a `ZeroizeOnDrop` newtype wrapping `Vec<u8>` with no
public byte accessor other than `as_slice()` returning a borrowed view —
matches the protection level the witness needs. Witness writes use
`store_secret`; witness probes use `load_secret`. The keypair API is
not overloaded for this purpose: a witness is not a signing key and
should not be typed as one.

`SecretHandle` is a typed struct with three fields:
`vault_id: VaultId`, `account: HandleAccount`, and `version: KeyVersion`.
`HandleAccount` is the closed enum `{ Identity(Identity), Witness }`
which controls whether the keystore addresses an identity keypair or
the per-vault witness. The on-the-wire keystore service string is
derived once via `format!("cairn:{vault_id}")` and the account string
via `account.encode(version)` (e.g. `<identity-wire-form>#k3` or
`__vault_witness__#k1`); callers never construct those strings by
hand. Constructors:

- `SecretHandle::for_identity(vault_id, identity, version)`
- `SecretHandle::for_witness(vault_id)`

Every `Keystore` operation accepts a `SecretHandle`, so the
vault-isolation guarantee is encoded at the type level, not by an
implied side channel. Recovery / discovery flows construct candidate
handles via `for_witness(<candidate vault_id>)` and probe explicitly.

`SigningKey` wraps `ed25519_dalek::SigningKey`, derives `ZeroizeOnDrop`,
no `Clone`, no public access to bytes.

`KeystoreError` (`#[non_exhaustive]`):
- `NotFound` — handle does not exist.
- `Locked` — keychain is locked (macOS prompt declined, etc.).
- `PermissionDenied`
- `DiscoveryUnsupported` — `list_vault_namespaces` is not implementable
  on this backend (e.g., DPAPI). Recovery commands fall back to the
  filesystem sentinel + operator-supplied `--vault-id` hint.
- `Backend(#[source] Box<dyn std::error::Error + Send + Sync>)`

New contract module `contract::identity_registry`:

```rust
pub trait IdentityRegistry: Send + Sync {
    // Provisioning state machine (§3.5). reserve + activate must be
    // separate so the runtime can persist the keychain entry between them.
    async fn reserve_identity(&self, record: &PublicIdentityRecord, key: &IdentityKeyEntry) -> Result<(), RegistryError>;
    async fn activate_identity(&self, id: &Identity, key_version: KeyVersion) -> Result<(), RegistryError>;
    async fn delete_pending(&self, id: &Identity, key_version: KeyVersion) -> Result<(), RegistryError>;
    async fn list_pending(&self) -> Result<Vec<PendingIdentityEntry>, RegistryError>;

    // Read paths. Visibility is typed so reconciliation, forensics,
    // and operational reads each pass exactly the filter they need.
    // The default operational read is `IdentityVisibility::Operational`
    // which excludes `pending`, `purge_pending`, and `purged`.
    async fn get_identity(&self, id: &Identity, visibility: IdentityVisibility) -> Result<Option<PublicIdentityRecord>, RegistryError>;
    async fn list_identities(&self, kind: Option<IdentityKind>, visibility: IdentityVisibility) -> Result<Vec<PublicIdentityRecord>, RegistryError>;
    async fn list_keys(&self, id: &Identity) -> Result<Vec<IdentityKeyEntry>, RegistryError>;

    // Counts used by §3.7 bootstrap guard and §3.8 vault-id-recover.
    // count_keys covers all identity_keys rows regardless of parent state.
    async fn count_keys(&self) -> Result<u64, RegistryError>;
    async fn list_all_keys(&self) -> Result<Vec<IdentityKeyEntry>, RegistryError>;

    // Atomic trust-state transitions. Each method runs in one SQLite
    // transaction that couples three mutations:
    //   * `apply_rotation`: append a new `identity_keys` row, advance
    //     `identities.current_key_version`, mark the predecessor row's
    //     `superseded_at`, and persist the `RotationReceipt` to
    //     `identity_receipts`.
    //   * `apply_revocation`: flip `identities.provisioning_state` to
    //     `revoked`, stamp `revoked_at`, and persist the
    //     `RevocationReceipt` to `identity_receipts`.
    // Splitting receipt-write from state-mutation as separate trait
    // methods would let an adapter persist authorization without
    // applying the change (or vice versa); the trait does not allow
    // that. Conformance tests assert post-call invariants on every
    // mutated row plus signature re-verification.
    async fn apply_rotation(&self, receipt: &RotationReceipt) -> Result<(), RegistryError>;
    async fn apply_revocation(&self, receipt: &RevocationReceipt) -> Result<(), RegistryError>;

    // First-bind transaction. Atomically inserts the `vault_meta` row
    // (with vault_id + witness_sha256 + binding_path) and the first
    // identity's pending row + key. The adapter `stat`s `binding_path`
    // inside the transaction and rolls back if the sentinel is absent.
    // There is no second method that can write `vault_meta`; the
    // contract is exactly-once for the lifetime of the registry.
    async fn reserve_first_identity(
        &self,
        vault_id: &VaultId,
        record: &PublicIdentityRecord,
        key: &IdentityKeyEntry,
        witness_hash: WitnessHash,
        binding_path: &Path,
    ) -> Result<(), RegistryError>;

    // Operator-of-last-resort two-phase tombstone (§3.10). The trait
    // exposes the state machine explicitly: there is no shortcut
    // method that flips straight to `purged`. Implementations must
    // expose both transitions so reconciliation can re-drive a stuck
    // `purge_pending` row. `identity_keys` rows are never deleted —
    // historical signature verification continues to resolve the
    // issuer even after `purged`. The caller must hold a per-vault
    // on-disk acknowledgement (`PurgeAcknowledgement`) so this cannot
    // be triggered by a remote MCP caller.
    async fn mark_purge_pending(
        &self,
        id: &Identity,
        ack: &PurgeAcknowledgement,
        reason: PurgeReason,
    ) -> Result<(), RegistryError>;
    async fn finalise_purge(&self, id: &Identity) -> Result<(), RegistryError>;
    async fn list_purge_pending(&self) -> Result<Vec<PurgePendingEntry>, RegistryError>;
}
```

`IdentityVisibility` (closed enum):

- `Operational` — `active` and `revoked` only. Default for every verb
  that issues a signed envelope or produces a user-visible record;
  filters all transitional and tombstoned states.
- `IncludingPending` — adds `pending`. Used by §3.5 reconciliation.
- `IncludingPurgePending` — adds `purge_pending`. Used by
  `cairn identity purge --resume`.
- `Audit` — adds `purged` (and includes `purge_pending`). Used by
  forensic / audit reads. Default operational reads still filter
  `purged` out.

`PendingIdentityEntry` carries `(identity, key_version, public_key,
created_at)` — everything reconciliation needs without a second round
trip. `reserve_identity` is the only writer that can leave a row in
`pending`; `activate_identity` is the only path from `pending → active`;
`delete_pending` is the only path from `pending → (gone)`. Together the
four methods make the §3.5 state machine implementable through the trait
alone, with no SQLite-specific escape hatch.

`PurgeAcknowledgement` is a typed token whose only constructor reads a
caller-supplied file path that must live on local disk at
`.cairn/maintenance/purge-ack`. The file must be **operator-authored
out-of-band before** invoking `cairn identity purge`; the CLI does **not**
create it, does **not** prompt to create it, and does **not** offer a
flag that creates it. The CLI only verifies the file is present, that
its contents match the identity wire form being purged, then consumes
(deletes) it after the registry call succeeds. MCP / SDK callers cannot
construct a `PurgeAcknowledgement` because they do not have local
filesystem access of the right shape; this is the same pattern the
brief uses for human-only operations (§14). Tests assert that no CLI
code path under any flag combination writes the ack file.

`RegistryError` (`#[non_exhaustive]`): `NotFound`, `IdentityExists { id }`,
`ProvisioningInFlight { id }`, `AlreadyRevoked { id }`,
`KeyMaterialMismatch { id }`,
`KeyVersionConflict { existing, attempted }`, `Backend(#[source] …)`.

`IdentityServiceError` (top-level error returned by `IdentityService`,
wraps the two adapter errors plus the cross-cutting cases):
`Keystore(#[source] KeystoreError)`,
`Registry(#[source] RegistryError)`,
`DefaultsNotInitialized`,
`KeyMaterialDesynchronized { id, reason }`,
`NoLiveAttributableSigner` (rotate/revoke target was the only live default; see §3.10),
`VaultIdMissing` (bootstrap not run or `.cairn/vault.id` removed).

`Cargo.toml` additions: `ed25519-dalek` (default-features-off, `+ zeroize`),
`zeroize`, `rand_core`. No new transitive surface beyond the cryptographic
primitives — kept dependency-light per CLAUDE.md §6.7.

### 4.2 `cairn-keychain` (new)

```
crates/cairn-keychain/
├── Cargo.toml            # keyring 3, ed25519-dalek, zeroize, secrecy
├── src/
│   ├── lib.rs            # pub use os::OsKeystore;
│   └── os.rs             # keyring-backed Keystore impl
└── tests/
    └── round_trip.rs     # store → load → delete; cfg-gated per OS
```

`OsKeystore::new(vault_id: &VaultId)` constructs the per-vault, scoped
keystore handle. Service string is `cairn:<vault_id>`; account string
is the `<identity-wire-form>#k<version>`. Uses `keyring::Entry::new` +
`set_secret` / `get_secret` — bytes only, no string encoding. The
keystore rejects loads whose `SecretHandle` carries a different
`vault_id` than the one it was constructed with — defence in depth
against caller mix-ups.

`OsKeystore::for_discovery()` is the vault-agnostic constructor used by
`vault-id-recover` and `finalise-binding` recovery paths. It can:

- `list_vault_namespaces(prefix)` — enumerate (where the backend
  supports it).
- `load_secret(handle)` and `delete_secret(handle)` against an
  *explicit* `SecretHandle` constructed with a candidate `vault_id`
  passed in by the operator (`--vault-id <id>`) or discovered through
  enumeration. The discovery handle does not enforce the per-vault
  pinning check, because the entire point is to inspect candidates.

Recovery commands therefore have an implementable path on every
backend: enumeration where supported, operator-supplied `--vault-id` as
the discovery-free fallback. `OsKeystore::new` is reserved for the
post-recovery normal operating mode where the vault id is known.

Headless / unsupported environments (e.g., CI Linux without
secret-service) return `KeystoreError::Backend`. The CLI maps this to
`EX_UNAVAILABLE = 69` per CLAUDE.md §6.5.

### 4.3 `cairn-test-fixtures`

New module `keystore` exposing `MemoryKeystore` — a `tokio::sync::Mutex`
hash map that satisfies the `Keystore` trait for tests. Lives behind
`cairn-test-fixtures::identity` so neither `cairn-core` nor `cairn-keychain`
depends on it (it stays a dev-dep).

### 4.4 `cairn-store-sqlite`

Migration `0002_identity.sql`:

```sql
CREATE TABLE identities (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('human', 'agent', 'sensor')),
    current_key_version INTEGER NOT NULL,
    provisioning_state TEXT NOT NULL CHECK (provisioning_state IN ('pending', 'active', 'revoked', 'purge_pending', 'purged')),
    created_at TEXT NOT NULL,
    activated_at TEXT,
    revoked_at TEXT,
    revocation_signature BLOB,
    purged_at TEXT,
    purge_reason TEXT
);

CREATE TABLE identity_keys (
    identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    key_version INTEGER NOT NULL,
    public_key BLOB NOT NULL,
    signed_predecessor BLOB,
    created_at TEXT NOT NULL,
    superseded_at TEXT,
    PRIMARY KEY (identity_id, key_version)
);

CREATE INDEX idx_identity_keys_identity ON identity_keys(identity_id);

CREATE TABLE vault_meta (
    -- Single-row table; CHECK enforces exactly one row.
    rowid INTEGER PRIMARY KEY CHECK (rowid = 1),
    vault_id TEXT NOT NULL,             -- ULID, mirror of .cairn/vault.id
    witness_sha256 BLOB NOT NULL,
    binding_path TEXT NOT NULL,
    witness_created_at TEXT NOT NULL
);

CREATE TABLE identity_receipts (
    -- Append-only audit log for attributable trust-state mutations.
    rowid INTEGER PRIMARY KEY,
    op_kind TEXT NOT NULL CHECK (op_kind IN ('rotation', 'revocation')),
    target_identity TEXT NOT NULL REFERENCES identities(id),
    signer_identity TEXT NOT NULL REFERENCES identities(id),
    signer_key_version INTEGER NOT NULL,  -- which signer key produced `signature`
    old_key_version INTEGER,              -- target's old key (NULL for first key)
    new_key_version INTEGER,              -- target's new key (NULL for revocation)
    issued_at TEXT NOT NULL,
    signed_payload BLOB NOT NULL,         -- canonical JSON of (op_kind, target, signer, signer_key_version, old/new key versions, issued_at)
    signature BLOB NOT NULL               -- ed25519 signature over signed_payload by (signer_identity, signer_key_version)
);

CREATE INDEX idx_identity_receipts_target ON identity_receipts(target_identity);
CREATE INDEX idx_identity_receipts_signer ON identity_receipts(signer_identity);
```

The `vault_meta` row is inserted exactly once: the first time
`reserve_identity` runs against an empty registry. Ordering follows
§3.7's canonical sentinel-first sequence — restated here so the storage
contract and the binding contract can never drift apart:

1. `.cairn/vault.binding` is written and `fsync`-ed (the operator's
   filesystem now records that this vault may be bound).
2. The `__vault_witness__` keychain entry is written.
3. The single SQLite transaction reserves the first pending identity
   **and** inserts `vault_meta(witness_sha256)` with the same hash that
   landed in step 1. Both rows commit atomically.

The store adapter rejects any caller that tries to write `vault_meta`
without the sentinel already on disk (it `stat`s the path inside the
transaction); store integration tests assert the rejection. There is no
sequence in which the witness lands in keychain or DB without the
sentinel already being durable, and `cairn identity finalise-binding`
(§3.7) is the only path that can resolve a sentinel-only state.

`identity_keys` is append-only: rotation inserts a new row and stamps the
prior row’s `superseded_at`; revocation flips `identities.provisioning_state`
but does not delete keys. The keystore (private-key) ring is bounded
separately to current + ≤ 2 predecessors (§3.6). Public-key garbage
collection is gated on a future record-reference scan and is not part of
this PR.

`SqliteIdentityRegistry` implements `IdentityRegistry`. Integration tests
hit a real SQLite file in `tempfile::tempdir()`. No mocking (CLAUDE.md
§6.4).

### 4.5 `cairn-cli`

New subcommand `cairn identity`:

```
cairn identity provision --kind {human|agent|sensor} [--slug …] [--harness …] [--model …] [--role …] [--family …] [--name …] [--host …] [--rev v1] [--json]
cairn identity init-defaults [--json]
cairn identity list [--kind …] [--json]
cairn identity show <id> [--json]
cairn identity rotate <id> [--json]
cairn identity revoke <id> [--json]
cairn identity reconcile [--json]
cairn identity repair <id> [--json]
cairn identity purge <id> [--resume] [--json]
cairn identity vault-id-recover [--probe-keychain | --vault-id <id>] [--json]
cairn identity finalise-binding [--abandon [--vault-id <id>]] [--json]
```

Shape mirrors brief §4.2 conventions; flags use `clap` derive + `ValueEnum`
for the kind enum. Snapshot tests cover human and `--json` output.

`init-defaults` is the explicit entry point that mints the local human and
the Claude Code agent identity:

1. Compute `slug` from `whoami::username()`. Mint `hmn:<slug>:v1`. Skip if
   the registry already has it.
2. Mint `agt:claude-code:opus-4-7:main:v1`. Skip if already present.
3. Do **not** mint sensor identities here (per AC: sensor identities
   provision lazily on first enable).

Idempotent: re-running `init-defaults` produces no registry diff.

**`cairn bootstrap` adds exactly one artefact: `.cairn/vault.id`.** Per
`docs/superpowers/specs/2026-04-26-bootstrap-design.md`, bootstrap is a
filesystem-only command that does not create `.cairn/cairn.db` and must
not depend on the OS keychain. Both invariants are preserved: minting a
ULID and writing it to a file is a pure filesystem operation. The
existing bootstrap design doc is amended in this PR to add `vault_id` to
`BootstrapReceipt` and the placeholder file table; the human-readable
output gains a `next: cairn identity init-defaults` hint line. Identity
provisioning itself remains out of bootstrap because it requires the DB +
keychain.

The recommended first-run path is two commands: `cairn bootstrap` (creates
the directory tree, no DB, no keychain) followed by `cairn identity
init-defaults` (opens / creates the DB and writes the default identities).
Documenting this in the bootstrap human-readable hint output is a follow-up,
tracked in §8.

`IdentityService` struct in `cairn-cli/src/identity.rs` holds
`Arc<dyn Keystore>` + `Arc<dyn IdentityRegistry>` and exposes the verbs
the CLI handlers call. CLI handlers stay thin (CLAUDE.md §6.1).
`IdentityService::open` runs §3.5 reconciliation before returning.

### 4.6 `cairn-sensors-local`

Adds a `provision_sensor_identity(service: &IdentityService, descriptor:
SensorDescriptor) -> Result<Identity, _>` helper. The sensor enable
callsite (which lands with the relevant sensor issues) invokes this
lazily — first time the user enables a sensor, identity gets minted and
persisted. No eager bootstrap call.

For this PR we only ship the helper + a unit test that drives it through
`MemoryKeystore` + an in-memory SQLite registry. No CLI command for sensor
enable yet (out of scope).

## 5. Failure modes & error mapping

| Condition | Error | CLI exit | Notes |
|---|---|---|---|
| Keychain unavailable / locked | `KeystoreError::Backend` / `Locked` | `EX_UNAVAILABLE = 69` | Maps to `CapabilityUnavailable` in capability advertisement. |
| Re-provision same identity (idempotent path) | none | 0 | No-op only after §3.8 liveness check passes (registry row + keychain entry + matching public key). |
| Active row, keychain entry missing or pubkey mismatch | `IdentityServiceError::KeyMaterialDesynchronized { id, reason }` | 65 (`EX_DATAERR`) | CLI hint: run `cairn identity repair <id>`. |
| `.cairn/vault.id` missing, registry empty | `BootstrapError::VaultIdLost` raised by next bootstrap; restored on re-run | 65 (`EX_DATAERR`) at boot; n/a at identity layer (registry is empty so `IdentityService` opens cleanly once vault id is restored). |
| `.cairn/vault.id` missing, identities exist | `BootstrapError::VaultIdLost` (refuses to mint new id) | 65 (`EX_DATAERR`) | CLI hint: restore from backup or `cairn identity vault-id-recover --probe-keychain`. |
| `IdentityService::open` succeeds with zero defaults | none — service opens; recovery commands stay usable; issuer-dependent verbs gate themselves via `require_default_issuer` | 0 / 64 depending on verb | First-run gate is per-verb, not per-open. |
| Re-provision while another attempt is `pending` | `RegistryError::ProvisioningInFlight` | 75 (`EX_TEMPFAIL`) | Run `cairn identity reconcile` or restart. |
| Re-provision conflicts (different `key_version`) | `RegistryError::KeyVersionConflict` | 1 | Operator must `rotate` or `revoke` first. |
| Crash between keychain write and registry activate | recovered by §3.5 reconciliation on next `IdentityService::open` | n/a | Pending row flipped to active. Audit log entry at `info`. |
| Crash before keychain write | recovered by §3.5 reconciliation: pending row deleted | n/a | Audit log warns "orphaned pending identity removed". |
| Rotation when private-key ring is full | oldest **private key** deleted from keychain; corresponding public key **kept** in registry | 0 | Logged at `info`. Verification of historical records remains intact. |
| Revoke an already-revoked identity | `RegistryError::AlreadyRevoked` | 1 | Fail closed, no overwrite. |
| Sensor identity request before keystore is configured | `KeystoreError::Backend` | `EX_UNAVAILABLE = 69` | Sensor enable surfaces a typed error. |

Every typed error preserves source via `#[source]` per CLAUDE.md §6.2. No
`anyhow` in libraries.

## 6. Privacy & secret handling

- Private key bytes never leave `cairn-keychain`. `SigningKey` wraps
  `ed25519_dalek::SigningKey` and derives `ZeroizeOnDrop`. No accessor
  returns the secret bytes.
- `cairn-keychain` deletes via `keyring::Entry::delete_password` (or
  `delete_secret`) on `record_revocation` for ring entries that age out —
  brief §4.2 says ring depth is bounded.
- Tracing: identity ids logged at `info` (they are public); key bytes never
  logged at any level. `Debug` for `SigningKey` is implemented as
  `f.debug_struct("SigningKey").field("redacted", &true).finish()`.
- Vault check (test): integration test asserts `tempdir`/.cairn contains no
  files matching the keypair bytes after provisioning (AC verification).

## 7. Testing strategy

| Layer | Test | Tool |
|---|---|---|
| `cairn-core::domain::identity` | parse/format round-trip across `hmn:`, `agt:`, `snr:`; reject unknown prefix; reject empty body | `proptest` + unit |
| `cairn-core::domain::identity::provision` | deterministic plan given seeded RNG; rev wraparound rejected; `normalize_human_slug` covers ASCII / spaces / apostrophe / accented / non-Latin / 100-byte / empty-fallback (§3.9) | unit |
| `cairn-keychain` | round-trip `store / load / delete`; `NotFound` on missing handle; `Locked` mapping | per-OS `#[cfg]` integration |
| `cairn-store-sqlite` | reserve/activate/delete-pending state-machine transitions; `list_pending` correctness; `count_keys` covers all states; key-ring depth ≤ 3; revocation atomicity; foreign-key cascade; `purge_identity` two-phase state machine (active → `purge_pending` → `purged` only after every key version is verified-deleted from the keystore; injected `delete_keypair` failure parks the row at `purge_pending`; `cairn identity purge --resume <id>` (and only that path, after re-checking `.cairn/maintenance/purge-ack`) re-runs the loop and reaches `purged`; `open_for_maintenance` and inspection commands do **not** advance `purge_pending`); `identity_keys` rows preserved across purge so post-purge signature verification still resolves the public key; rejects writes if `PurgeAcknowledgement` is missing or wrong; `vault_meta` insert rejected if `.cairn/vault.binding` is not present on disk (sentinel-first storage contract); `reserve_first_identity` is exactly-once; `record_rotation` / `record_revocation` persist the `RotationReceipt` / `RevocationReceipt` verbatim and the conformance test re-verifies the stored signature against the persisted signer + key versions | integration (real SQLite tempdir) |
| Cross-crate (in `cairn-cli` integration tests) | `init-defaults` idempotency (incl. §3.8 liveness check); rotation fixture (private-key ring bounded, public-key archive intact, **witness untouched**); revocation fixture; vault contains no plaintext key bytes (incl. no plaintext witness); reconciliation recovers from injected mid-flow crash; reconciliation **fails closed on injected pubkey mismatch**; `repair` reconciles pending rows and **never** mutates active trust state (§3.10); `purge` two-phase tombstone (state → `purge_pending` → `purged`, with private-key deletion + verification between transitions; `purge_pending` rejects all signing; `identity_keys` rows preserved; historical signature verification still resolves the public key after `purged`); **purge crash recovery is opt-in** (kill the process during step 2 of `purge` with one key version deleted and one still present → `cairn identity list` and `show` are no-ops on the `purge_pending` row; only `cairn identity purge --resume <id>` (which re-checks the ack file) completes the deletion and advances to `purged`); **purge does not auto-resume on inspection** (running `cairn identity list` after a crashed purge does not delete the remaining keys); `purge` requires the ack file and emits the audit-gap log line; `purge` is unreachable from the MCP surface; **two-vault isolation** (provisioning in vault A leaves vault B's keychain entries untouched); first-run gate (`cairn ingest` before `init-defaults` returns `EX_USAGE = 64`); **liveness-gate test** (`cairn ingest` after default keychain entry deleted out-of-band returns `EX_DATAERR = 65` *before* the ingest pipeline runs); **purge ack barrier** (no CLI flag combination, including `--yes`/`--force`/`--no-confirm`, causes the CLI to write the ack file); **single-default-broken ordinary write** (default human keychain entry deleted out-of-band → `cairn ingest` succeeds attributed under the live default agent; `signer_identity` and `signer_key_version` reflect the agent on the resulting record); **single-default-broken rotation** (broken default rotated under the live other default; §3.10 priority 1); **rotation atomicity** (kill mid-`apply_rotation` → either both the new key row + advanced `current_key_version` *and* the receipt land, or neither lands; conformance test asserts no orphan receipt or orphan key row); **rotation receipt records signer_key_version** (after default agent rotation, an earlier rotation receipt that was signed by agent v1 still verifies against agent's archived v1 public key, even after agent has rotated to v2); **non-default self-rotation** (both defaults broken, target's own key still live, target is non-default → rotation succeeds with `signer = <id>` and `attributable_via = "self"` in the receipt); **all-broken degrades to purge** (defaults broken **and** target key broken → `rotate` returns `NoLiveAttributableSigner` mapped to `EX_UNAVAILABLE = 69`); **maintenance-open isolation** (inject pending mismatch; `cairn identity list`, `repair`, `purge`, `finalise-binding` all open + run successfully via `open_for_maintenance`; only issuer-dependent verbs surface the mismatch through `require_default_issuer`); **finalise-binding abandon** (sentinel-only state with no keychain or DB binding → `--abandon` deletes sentinel and is recorded as `binding_abandoned` audit-gap); **concurrent first-bind** (two `init-defaults` processes against the same vault; advisory lock on `.cairn/vault.binding.lock` serializes them; exactly one binding lands; the loser observes the committed state and returns 0 as a no-op); **first-bind --no-wait** (loser returns `EX_TEMPFAIL = 75` when lock is held); **finalise-binding finalise from `.binding.pending`** (crash between sentinel write and keychain write → next run reads pending file, idempotently completes steps 2-4, no external backup needed); **finalise-binding finalise from `.binding`** (sentinel + keychain witness exist, DB never wrote → `--vault-id <id>` finalises and writes `vault_meta`); **DPAPI vault-id-recover with intact DB** (Windows simulated `DiscoveryUnsupported`, `.cairn/vault.id` deleted, DB intact → recovery reads `vault_meta.vault_id`, verifies witness, restores `vault.id` without operator-supplied flag); **vault-scoped abandon** (`--abandon --vault-id <id>` probes only that namespace; vault B's keychain entries do not block vault A's abandon); **abandon refused without `--vault-id`** on every backend; **DPAPI abandon** with `--vault-id` and confirmed `NotFound` probe records `evidence = "vault_id_negative_probe"`; **rotation receipt persisted** (after `cairn identity rotate`, the `identity_receipts` table contains a row whose signature still verifies against the stored signer's public key); recovery commands (`list`, `reconcile`, `repair`, `purge`, `vault-id-recover`, `finalise-binding`) succeed with zero defaults; `rotate` / `revoke` fail with `DefaultsNotInitialized` when defaults missing; `KeyMaterialDesynchronized` raised when keychain entry deleted out-of-band; **`vault.id` regeneration refused** when *any* durable evidence exists: `.cairn/vault.binding`, `.cairn/vault.binding.pending`, **or** `vault_meta` row in `.cairn/cairn.db`. **DB-only durable binding test**: delete both filesystem sentinels, leave DB intact → bootstrap still refuses; `vault-id-recover` reads `vault_meta.vault_id` and restores both files. Unrelated keystore namespaces from other vaults on the machine do **not** trip the refusal (vault-local check only). **sentinel-first crash test** (kill between sentinel write and keychain witness write → bootstrap still refuses, `finalise-binding` resolves); **multi-vault coexistence test** (vault A bound, fresh checkout in different directory bootstraps cleanly); `vault-id-recover` survives multiple rotations; ambiguous-match fail-closed; `--vault-id <id>` fallback works when `Keystore::list_vault_namespaces` returns `DiscoveryUnsupported`; **schema-skew safety** test (DB exists, identity migration not yet applied, but `vault.binding` exists → bootstrap refuses) | integration via `MemoryKeystore` |
| `cairn-cli` | snapshot tests for `cairn identity list --json`, `show`, `provision` (success + duplicate) | `insta` |

CI commands (per CLAUDE.md §8):

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

## 8. Risks & open questions

- **`keyring` crate maturity on Linux without secret-service.** Headless CI
  must opt into `MemoryKeystore`. Acceptable for P0 — operator without a
  desktop session is not the P0 target.
- **DPAPI on Windows.** No CI coverage in this repo today; `keyring`
  upstream tests this. Documented in PR risk section, not gating.
- **`hmn:` rename ripple.** Search for `usr:` across the workspace before
  the PR lands; expect zero non-test references because no records exist
  yet. Confirmed in §3.3.
- **Rotation signature semantics.** Brief §4.2 says `signed_predecessor` is
  signed with the previous key; we store but do not verify here. Verifier
  wiring lands with the ingest signature-check issue.

## 9. Migration / rollout

- Migration `0002_identity.sql` runs the first time the SQLite store is
  opened. The store is opened by `cairn identity init-defaults`, by
  any future verb that touches the registry, or explicitly by the operator.
- The `usr:` → `hmn:` rename is a breaking change to the IDL primitive but
  no signed records exist in any vault, so there is no on-disk migration.
- Existing dev vaults (none in production) initialize identities via
  `cairn identity init-defaults`; idempotent. `cairn bootstrap` is
  unchanged and remains safe to run in headless / keychain-less
  environments.

## 10. Acceptance check (from issue #50)

| AC | Where it’s verified |
|---|---|
| Private keys are not stored in plaintext in the vault | §6, §7 “vault contains no plaintext key bytes” test |
| Every write can resolve an issuer identity and current key version | `IdentityRegistry::get_identity` + `list_keys`; integration test asserts records returned with `current_key_version` |
| Sensor identity provisioning happens when a sensor is first enabled | §4.6 — `provision_sensor_identity` helper; neither `bootstrap` nor `init-defaults` mint sensor identities |
| Run identity provisioning tests with a keychain mock | `MemoryKeystore` in `cairn-test-fixtures` (§4.3) |
| Run key rotation and revocation fixture tests | §7 “rotation fixture; revocation fixture” |
| Inspect vault files to confirm private keys are absent | §7 cross-crate test asserts no plaintext key bytes under `tempdir`/.cairn |

## 11. Implementation order (preview, not the plan)

This is a teaser; the real implementation plan is written next via the
`writing-plans` skill.

1. Brief alignment + IDL primitive: `usr:` → `hmn:`. Re-run codegen,
   update hand-written `actor_chain.rs` role checks, sweep `rg -n 'usr:'`
   for stragglers (§3.3).
2. `cairn-core` types + traits + provisioning logic + tests (incl. the
   `pending → active → revoked` state machine as pure functions, and the
   `normalize_human_slug` helper from §3.9).
3. `cairn-test-fixtures::keystore::MemoryKeystore`.
4. `cairn-store-sqlite` migration + `SqliteIdentityRegistry` + reconciliation
   on `open` + tests.
5. `cairn-keychain` crate + per-OS round-trip tests.
6. `cairn-cli identity` subcommand (`provision`, `init-defaults`, `list`,
   `show`, `rotate`, `revoke`, `reconcile`, `repair`, `purge`,
   `vault-id-recover`) + snapshot tests. First identity provision writes
   the witness keychain entry, the `vault_meta` row, and
   `.cairn/vault.binding` together (§3.7). Bootstrap delta: mint
   `.cairn/vault.id` only when `.cairn/vault.binding` is absent, extend
   `BootstrapReceipt` with `vault_id`, add `next: cairn identity
   init-defaults` hint line, update bootstrap test snapshots, and add
   regression tests for the `VaultIdLost`-with-binding refusal and the
   schema-skew safety case.
7. `cairn-sensors-local::provision_sensor_identity` helper + test.
8. Verification checklist run; PR.
