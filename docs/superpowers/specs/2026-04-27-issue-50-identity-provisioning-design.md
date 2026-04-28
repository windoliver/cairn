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

### 3.3 Wire-form rename `usr:` → `hmn:`

Brief §4.2 specifies `hmn:<slug>:<rev>` for `HumanIdentity`. The current
`Identity::parse` accepts `usr:` (`crates/cairn-core/src/domain/identity.rs`,
lines 37–38). Brief is source of truth (CLAUDE.md §1). Fixing it now is one
parser site + a handful of tests; deferring it would mean a breaking change
to every signed record once writes start landing. The IDL primitive at
`crates/cairn-idl/schema/common/primitives.json` is updated in lockstep and
codegen is re-run as part of this PR.

This is a load-bearing rename. Listed in the PR description’s “invariants
touched” section.

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
pending  → active     (happy path)
pending  → orphaned   (reconciliation finds no keychain entry)
active   → revoked    (operator revocation)
```

Provisioning flow (single identity):

1. **Reserve.** Insert `identities` row with `provisioning_state = 'pending'`
   plus the prospective `IdentityKeyEntry` in `identity_keys` inside one
   SQLite transaction. The registry row carries no signing capability while
   pending; reads through `IdentityRegistry::get_identity` filter pending
   rows out by default.
2. **Mint.** Generate the keypair in memory.
3. **Persist secret.** `Keystore::store_keypair`. On failure: roll forward
   to step 4 with an error; the pending row remains for reconciliation.
4. **Activate.** Single SQLite UPDATE flips `provisioning_state` to
   `'active'`. Failure here leaves the keychain entry with a pending
   registry row — reconciliation handles it.

Reconciliation runs at every `IdentityService::open` (i.e., once per
process start). For each `pending` row:

- **Keychain entry missing** → delete the registry row, mark the outcome
  `orphaned` in the audit log (`tracing` warn). (Crash before or during
  step 3.)
- **Keychain entry present** → load the private key, derive its public key,
  and compare it against the `identity_keys.public_key` reserved at step 1.
  - **Match** → flip the row to `active`. (Crash between steps 3 and 4.)
  - **Mismatch** → fail closed: leave the row `pending`, surface
    `RegistryError::KeyMaterialMismatch { id }`, log at `error`, require
    operator intervention via `cairn identity repair <id> --force` (which
    deletes both the pending row and the conflicting keychain entry, then
    re-runs provisioning). Mismatch is a strong signal of cross-vault
    namespace collision (§3.7) or out-of-band tampering and **must not**
    auto-recover.

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

**Secret handle format.** `Keystore` uses:

- service: `cairn:<vault_id>` (e.g., `cairn:01HXY…`)
- account: `<identity-wire-form>#k<version>`

Wrong-vault leakage is impossible: the service segment carries the vault
id and is verified by the keystore on every load.

**First-run gate.** `cairn bootstrap` exits 0 with a vault that has zero
identities. Any verb that needs an issuer (`ingest`, `capture_trace`,
`forget`, `cairn identity rotate`, etc.) opens `IdentityService`, which
checks for at least one `active` identity of kind `Human` **and** kind
`Agent`. If absent, the verb fails fast with a typed
`IdentityServiceError::DefaultsNotInitialized` mapped to `EX_USAGE = 64`
and a human-readable hint:

```
cairn ingest: no default identities found
  run `cairn identity init-defaults` to provision the local human + agent identities
```

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
   to `EX_DATAERR = 65`. The CLI hint advises
   `cairn identity repair <id>` (which atomically revokes the active row
   and re-provisions a new key version, preserving prior public-key rows
   for verification).

This closes the "active row, missing keychain secret" hole: the system
self-detects on the very next provision attempt rather than silently
treating a stuck identity as healthy.

`IdentityService::open` runs the same liveness check on every `active`
identity at startup and emits a `tracing` warn for any desynchronized
entry, so long-lived processes (MCP server, workflow host) surface the
condition without waiting for a verb to trip on it.

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
    async fn store_keypair(&self, handle: &SecretHandle, secret: &SecretKeyMaterial) -> Result<(), KeystoreError>;
    async fn load_signing_key(&self, handle: &SecretHandle) -> Result<SigningKey, KeystoreError>;
    async fn delete_keypair(&self, handle: &SecretHandle) -> Result<(), KeystoreError>;
}
```

`SecretHandle` opaque newtype, formatted as `cairn / <identity-wire-form>#k<version>`
under the hood. `SigningKey` wraps `ed25519_dalek::SigningKey`, derives
`ZeroizeOnDrop`, no `Clone`, no public access to bytes.

`KeystoreError` (`#[non_exhaustive]`):
- `NotFound` — handle does not exist.
- `Locked` — keychain is locked (macOS prompt declined, etc.).
- `PermissionDenied`
- `Backend(#[source] Box<dyn std::error::Error + Send + Sync>)`

New contract module `contract::identity_registry`:

```rust
pub trait IdentityRegistry: Send + Sync {
    async fn upsert_identity(&self, record: &PublicIdentityRecord, key: &IdentityKeyEntry) -> Result<(), RegistryError>;
    async fn get_identity(&self, id: &Identity) -> Result<Option<PublicIdentityRecord>, RegistryError>;
    async fn list_identities(&self, kind: Option<IdentityKind>) -> Result<Vec<PublicIdentityRecord>, RegistryError>;
    async fn list_keys(&self, id: &Identity) -> Result<Vec<IdentityKeyEntry>, RegistryError>;
    async fn record_rotation(&self, id: &Identity, new_key: &IdentityKeyEntry) -> Result<(), RegistryError>;
    async fn record_revocation(&self, id: &Identity, at: DateTime<Utc>, signature: Signature) -> Result<(), RegistryError>;
}
```

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

`OsKeystore::new(vault_id: &VaultId)` constructs the per-vault namespace.
Service string is `cairn:<vault_id>`; account string is the
`<identity-wire-form>#k<version>`. Uses `keyring::Entry::new` +
`set_secret` / `get_secret` — bytes only, no string encoding. The
keystore rejects loads whose `SecretHandle` carries a different
`vault_id` than the one it was constructed with — defence in depth
against caller mix-ups.

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
    provisioning_state TEXT NOT NULL CHECK (provisioning_state IN ('pending', 'active', 'revoked')),
    created_at TEXT NOT NULL,
    activated_at TEXT,
    revoked_at TEXT,
    revocation_signature BLOB
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
```

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
cairn identity repair <id> [--force] [--json]
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
| `.cairn/vault.id` missing | `IdentityServiceError::VaultIdMissing` | 78 (`EX_CONFIG`) | Operator must re-run `cairn bootstrap` (idempotent; preserves existing vault id if any). |
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
| `cairn-core::domain::identity::provision` | deterministic plan given seeded RNG; rev wraparound rejected | unit |
| `cairn-keychain` | round-trip `store / load / delete`; `NotFound` on missing handle; `Locked` mapping | per-OS `#[cfg]` integration |
| `cairn-store-sqlite` | identity CRUD; key-ring depth ≤ 3; revocation atomicity; foreign-key cascade | integration (real SQLite tempdir) |
| Cross-crate (in `cairn-cli` integration tests) | `init-defaults` idempotency (incl. §3.8 liveness check); rotation fixture (private-key ring bounded, public-key archive intact); revocation fixture; vault contains no plaintext key bytes; reconciliation recovers from injected mid-flow crash; reconciliation **fails closed on injected pubkey mismatch**; `cairn identity repair` round-trip; **two-vault isolation** (provisioning in vault A leaves vault B's keychain entries untouched); first-run gate (`cairn ingest` before `init-defaults` returns `EX_USAGE = 64`); `KeyMaterialDesynchronized` raised when keychain entry deleted out-of-band | integration via `MemoryKeystore` |
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

1. Brief alignment + IDL primitive: `usr:` → `hmn:`. Re-run codegen.
2. `cairn-core` types + traits + provisioning logic + tests (incl. the
   `pending → active → revoked` state machine as pure functions).
3. `cairn-test-fixtures::keystore::MemoryKeystore`.
4. `cairn-store-sqlite` migration + `SqliteIdentityRegistry` + reconciliation
   on `open` + tests.
5. `cairn-keychain` crate + per-OS round-trip tests.
6. `cairn-cli identity` subcommand (`provision`, `init-defaults`, `list`,
   `show`, `rotate`, `revoke`, `reconcile`, `repair`) + snapshot tests.
   Bootstrap delta: mint `.cairn/vault.id`, extend `BootstrapReceipt` with
   `vault_id`, add `next: cairn identity init-defaults` hint line, update
   bootstrap test snapshots.
7. `cairn-sensors-local::provision_sensor_identity` helper + test.
8. Verification checklist run; PR.
