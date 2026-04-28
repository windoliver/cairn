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

| File / generator | Reason |
|---|---|
| `crates/cairn-core/src/domain/identity.rs:37–38` | Hand-written `Identity::parse` prefix branch. |
| `crates/cairn-core/src/domain/actor_chain.rs:51–57, 89–95` | Hand-written role validation: `Principal` is required to be a human. |
| `crates/cairn-idl/schema/common/primitives.json` | IDL `Identity` primitive regex; source of every generated validator below. |
| `crates/cairn-core/src/generated/common/mod.rs:91–95` | Generated validator — regenerated from IDL. |
| `crates/cairn-core/src/generated/envelope/mod.rs:436, 507–513` | Generated envelope validator — regenerated from IDL. |
| Any fixture or doctest that asserts on a `usr:` prefix. | Find via `rg -n 'usr:' crates/ tests/ fixtures/` before committing. |

Implementation order in this PR (rides on impl-order step 1, §11):

1. Update IDL primitive `crates/cairn-idl/schema/common/primitives.json`.
2. Run `cargo run -p cairn-idl --bin cairn-codegen` and commit the
   regenerated `generated/` files.
3. Update `domain/identity.rs::parse` and `IdentityKind::Human` prefix.
4. Update `domain/actor_chain.rs` hand-written role checks.
5. `rg -n 'usr:' .` must return zero non-historical hits before the PR
   moves on. CI gate `cargo nextest run --workspace` catches any missed
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
pending  → active     (happy path)
pending  → orphaned   (reconciliation finds no keychain entry)
active   → revoked    (operator revocation)
```

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

**`vault.id` is non-regenerable once any keychain-backed key material
exists.** Because every keychain entry is namespaced under
`cairn:<vault_id>`, regenerating `vault.id` would point the process at a
fresh keychain namespace and desynchronize every keychain entry in one
stroke — including those for revoked, pending, or otherwise non-active
identities. To prevent that, the bootstrap delta defined in this PR runs
an explicit guard before minting a new id:

1. If `.cairn/vault.id` exists → use it (idempotent path).
2. If `.cairn/vault.id` is missing **and** `.cairn/cairn.db` does not
   exist (or contains zero `identity_keys` rows) → mint a fresh ULID and
   write the file.
3. If `.cairn/vault.id` is missing **and** `.cairn/cairn.db` contains
   one or more `identity_keys` rows (any state — active, pending, or
   superseded) → bootstrap fails with `BootstrapError::VaultIdLost`
   mapped to `EX_DATAERR = 65`. The guard reads `identity_keys`, not
   `identities`, because each row corresponds to a real keychain entry
   that needs the original namespace.

   ```
   cairn bootstrap: .cairn/vault.id is missing but the registry holds N keychain-backed keys.
     Restore the original .cairn/vault.id from backup, or run:
       cairn identity vault-id-recover --probe-keychain
     Bootstrap will not mint a new vault id while keys exist; doing so
     would desynchronize every keychain entry from the registry.
   ```

`cairn identity vault-id-recover --probe-keychain` (new, in `cairn-cli`)
walks every row in `identity_keys` regardless of its parent identity's
`provisioning_state` (so revoked-only and pending-only vaults can still
recover). For each candidate `vault_id` discovered in the keychain
(`keyring` lets us enumerate by service prefix `cairn:`), it derives the
candidate `SecretHandle` for **every** `identity_keys` row, loads the
private key, derives the public key, and compares against the row's
stored `public_key`. The recovery succeeds only when there is exactly
one `vault_id` for which **every** `identity_keys` row matches:

- Multiple matching candidates → ambiguous; fail closed.
- Zero matching candidates → fail closed; operator must restore from
  backup.
- One matching candidate, but a subset of rows fail to match → fail
  closed; the registry and keychain disagree on at least one key, which
  is a tampering or partial-restore signal.

Bootstrap guard, recovery proof model, and reconciliation are all keyed
off `identity_keys` (not `identities`), so they agree on what counts as
"key material exists" in every state combination. Test matrix covers the
four state mixes: only-active, only-revoked, only-pending, and mixed.

**Secret handle format.** `Keystore` uses:

- service: `cairn:<vault_id>` (e.g., `cairn:01HXY…`)
- account: `<identity-wire-form>#k<version>`

Wrong-vault leakage is impossible: the service segment carries the vault
id and is verified by the keystore on every load.

**First-run gate (issuer-dependent verbs only).** `cairn bootstrap` exits
0 with a vault that has zero identities. Verbs that need to **issue** a
signed envelope — `ingest`, `capture_trace`, `forget`, `cairn identity
rotate`, `cairn identity revoke` — call
`IdentityService::require_default_issuer()` before doing work. If no
`active` identity of kind `Human` **and** kind `Agent` is present, they
fail fast with a typed `IdentityServiceError::DefaultsNotInitialized`
mapped to `EX_USAGE = 64` and a human-readable hint:

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
| `cairn identity repair <id>` | Allowed; needed to restore a desynchronized identity even if it is one of the defaults. |
| `cairn identity vault-id-recover` | Allowed; runs without ever opening `IdentityService` in a write-capable way. |

`cairn identity rotate` and `cairn identity revoke` **do** require the
default-issuer gate because revocation/rotation is itself a signed
operation that must be attributable. The CLI hint when they fail tells
the operator to run `init-defaults` first.

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
cairn identity vault-id-recover [--probe-keychain] [--json]
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
| `cairn-store-sqlite` | identity CRUD; key-ring depth ≤ 3; revocation atomicity; foreign-key cascade | integration (real SQLite tempdir) |
| Cross-crate (in `cairn-cli` integration tests) | `init-defaults` idempotency (incl. §3.8 liveness check); rotation fixture (private-key ring bounded, public-key archive intact); revocation fixture; vault contains no plaintext key bytes; reconciliation recovers from injected mid-flow crash; reconciliation **fails closed on injected pubkey mismatch**; `cairn identity repair` round-trip; **two-vault isolation** (provisioning in vault A leaves vault B's keychain entries untouched); first-run gate (`cairn ingest` before `init-defaults` returns `EX_USAGE = 64`); recovery commands (`list`, `reconcile`, `repair`, `vault-id-recover`) succeed with zero defaults; `KeyMaterialDesynchronized` raised when keychain entry deleted out-of-band; **`vault.id` regeneration refused** when DB has identities; `vault-id-recover --probe-keychain` round-trip success + ambiguous-match fail-closed | integration via `MemoryKeystore` |
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
   `show`, `rotate`, `revoke`, `reconcile`, `repair`, `vault-id-recover`)
   + snapshot tests. Bootstrap delta: mint `.cairn/vault.id` only when
   safe (§3.7 guard), extend `BootstrapReceipt` with `vault_id`, add
   `next: cairn identity init-defaults` hint line, update bootstrap test
   snapshots, and add a regression test for the `VaultIdLost`-with-active-
   identities refusal.
7. `cairn-sensors-local::provision_sensor_identity` helper + test.
8. Verification checklist run; PR.
