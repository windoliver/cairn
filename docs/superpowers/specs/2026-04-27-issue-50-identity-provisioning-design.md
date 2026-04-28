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
3. Bind defaults at bootstrap: one local human identity (from the OS
   username) and one Claude Code agent identity. Sensors are not bound at
   bootstrap.
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
`KeyVersionConflict { existing, attempted }`, `Backend(#[source] …)`.

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

`OsKeystore::new(service: &str)` defaults service to `cairn`. Account
string is the wire form of `SecretHandle`. Uses `keyring::Entry::new` +
`set_secret` / `get_secret` — bytes only, no string encoding.

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
    created_at TEXT NOT NULL,
    revoked_at TEXT,
    revocation_signature BLOB
);

CREATE TABLE identity_keys (
    identity_id TEXT NOT NULL REFERENCES identities(id) ON DELETE CASCADE,
    key_version INTEGER NOT NULL,
    public_key BLOB NOT NULL,
    signed_predecessor BLOB,
    created_at TEXT NOT NULL,
    PRIMARY KEY (identity_id, key_version)
);

CREATE INDEX idx_identity_keys_identity ON identity_keys(identity_id);
```

Key-ring depth (current + ≤ 2 predecessors) is enforced in `record_rotation`
by deleting the oldest entry once the ring exceeds three. Keeps the schema
simple; depth is a policy knob, not a wire constraint.

`SqliteIdentityRegistry` implements `IdentityRegistry`. Integration tests
hit a real SQLite file in `tempfile::tempdir()`. No mocking (CLAUDE.md
§6.4).

### 4.5 `cairn-cli`

New subcommand `cairn identity`:

```
cairn identity provision --kind {human|agent|sensor} [--slug …] [--harness …] [--model …] [--role …] [--family …] [--name …] [--host …] [--rev v1] [--json]
cairn identity list [--kind …] [--json]
cairn identity show <id> [--json]
cairn identity rotate <id> [--json]
cairn identity revoke <id> [--json]
```

Shape mirrors brief §4.2 conventions; flags use `clap` derive + `ValueEnum`
for the kind enum. Snapshot tests cover human and `--json` output.

Bootstrap (`cairn init` — already scaffolded per spec
`docs/superpowers/specs/2026-04-26-bootstrap-design.md`) gains an
identity-provisioning step:

1. Compute `slug` from `whoami::username()`. Mint `hmn:<slug>:v1`. Skip if
   the registry already has it.
2. Mint `agt:claude-code:opus-4-7:main:v1`. Skip if already present.
3. Do **not** mint sensor identities here.

Idempotent: re-running bootstrap on an existing vault produces no diff.

`IdentityService` struct in `cairn-cli/src/identity.rs` holds
`Arc<dyn Keystore>` + `Arc<dyn IdentityRegistry>` and exposes the verbs
the CLI handlers call. CLI handlers stay thin (CLAUDE.md §6.1).

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
| Re-provision same identity (idempotent path) | none | 0 | `upsert_identity` is a no-op when row matches. |
| Re-provision conflicts (different `key_version`) | `RegistryError::KeyVersionConflict` | 1 | Operator must `rotate` or `revoke` first. |
| Rotation when ring is full | drop oldest | 0 | Logged at `info`. |
| Revoke an already-revoked identity | `RegistryError::IdentityExists` (revoked variant) | 1 | Fail closed, no overwrite. |
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
| Cross-crate (in `cairn-cli` integration tests) | bootstrap idempotency; rotation fixture; revocation fixture; vault contains no plaintext key bytes | integration via `MemoryKeystore` |
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

- Migration `0002_identity.sql` runs once on next `cairn init` or any
  store-opening path. No data migration — no records exist yet.
- The `usr:` → `hmn:` rename is a breaking change to the IDL primitive but
  no signed records exist in any vault, so there is no on-disk migration.
- Bootstrap upgrade path: existing dev vaults (none in production) get
  default identities provisioned on next `cairn init` call; idempotent.

## 10. Acceptance check (from issue #50)

| AC | Where it’s verified |
|---|---|
| Private keys are not stored in plaintext in the vault | §6, §7 “vault contains no plaintext key bytes” test |
| Every write can resolve an issuer identity and current key version | `IdentityRegistry::get_identity` + `list_keys`; integration test asserts records returned with `current_key_version` |
| Sensor identity provisioning happens when a sensor is first enabled | §4.6 — `provision_sensor_identity` helper; bootstrap does **not** mint sensor identities |
| Run identity provisioning tests with a keychain mock | `MemoryKeystore` in `cairn-test-fixtures` (§4.3) |
| Run key rotation and revocation fixture tests | §7 “rotation fixture; revocation fixture” |
| Inspect vault files to confirm private keys are absent | §7 cross-crate test asserts no plaintext key bytes under `tempdir`/.cairn |

## 11. Implementation order (preview, not the plan)

This is a teaser; the real implementation plan is written next via the
`writing-plans` skill.

1. Brief alignment + IDL primitive: `usr:` → `hmn:`. Re-run codegen.
2. `cairn-core` types + traits + provisioning logic + tests.
3. `cairn-test-fixtures::keystore::MemoryKeystore`.
4. `cairn-store-sqlite` migration + `SqliteIdentityRegistry` + tests.
5. `cairn-keychain` crate + per-OS round-trip tests.
6. `cairn-cli identity` subcommand + bootstrap wiring + snapshot tests.
7. `cairn-sensors-local::provision_sensor_identity` helper + test.
8. Verification checklist run; PR.
