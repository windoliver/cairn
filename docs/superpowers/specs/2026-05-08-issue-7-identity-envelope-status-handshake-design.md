# Issue #7 — Identity, signed envelope, status, and handshake integration

- **Issue:** [#7](https://github.com/windoliver/cairn/issues/7)
- **Phase / priority:** v0.1 minimum substrate · P0
- **Brief sections:** §4.2 Identity, §8.0.a status and handshake, §8.0.b shared envelope, §14 Privacy and Consent
- **Child issues:** #50 identity provisioning, #51 signed envelope validation, #52 replay ledger and handshake challenge mode, #53 status and capability parity
- **Date:** 2026-05-08

## 1. Goal

Close the parent epic by verifying the integrated P0 identity substrate on
`origin/main` and patching any remaining acceptance gaps. The work is an
integration pass, not a rebuild: the child implementations for identity
provisioning, envelope verification, replay/challenge handling, and status
parity have already landed through their own specs and PRs.

The parent issue is complete only when the four user-facing acceptance
criteria hold together in one checkout:

1. Mutating verbs reject missing, expired, replayed, or invalid signatures
   before disk writes.
2. `status` is deterministic within one daemon or process incarnation.
3. Consecutive `handshake` calls return different valid challenges.
4. Advertised capabilities match actual runtime behavior.

## 2. Source of truth

The integration pass reads the brief sections first, then treats the child
issue specs as implementation references:

- `docs/superpowers/specs/2026-04-27-issue-50-identity-provisioning-design.md`
- `docs/superpowers/specs/2026-05-02-issue-51-envelope-verifier-design.md`
- `crates/cairn-store-sqlite/src/replay/mod.rs` and related #52 tests
- `docs/superpowers/specs/2026-05-06-issue-53-status-capability-parity-design.md`

If those sources disagree, the design brief wins. If implementation behavior
disagrees with this spec, the implementation is patched or the discrepancy is
documented in the issue as an explicit follow-up.

## 3. Scope

### In scope

- Audit the landed identity, verifier, replay, handshake, and status code on
  `origin/main`.
- Run focused tests covering #7 acceptance criteria before changing code.
- Add failing tests for any confirmed parent-epic gap before implementation.
- Patch the smallest layer that owns the gap:
  - `cairn-core` for pure envelope, identity, or status decision logic.
  - `cairn-store-sqlite` for replay ledger, sequence, challenge, or WAL
    transaction coupling.
  - `cairn-cli`, `cairn-mcp`, or `cairn-sdk` only for surface wiring or
    parity failures.
- Keep all mutating-path checks fail-closed and preserve the
  `VerifiedSignedIntent` trust boundary.

### Out of scope

- P2 actor-chain countersignature enforcement.
- Enterprise identity providers.
- Sharded replay databases.
- New public status fields beyond the v0.1 wire contract.
- Closing unrelated verb-runtime issues that merely consume the substrate.

## 4. Integration design

The parent epic is verified through five gates.

### 4.1 Identity provisioning gate

The audit confirms that local human, agent, and sensor identity types use the
brief §4.2 wire forms, that private key material is represented through a
keystore handle rather than vault plaintext, and that public identity metadata
and key lifecycle state live in SQLite. Tests stay in the existing identity
unit and integration suites; no new identity subsystem is introduced.

### 4.2 Signed envelope gate

The verifier must reject invalid issuer identity, expired intent, revoked or
wrong-version key, scope mismatch, and invalid signature before callers can
prepare WAL or mutate SQLite. The typed output remains
`VerifiedSignedIntent`; raw `SignedIntent` must not be accepted by record or
WAL admission APIs.

### 4.3 Replay and challenge gate

Replay admission consumes `operation_id`, `nonce`, issuer sequence, and
optional `server_challenge` inside the same SQLite transaction that prepares
the WAL operation. Sequence mode rejects repeats and lower sequence numbers
without advancing issuer state. Challenge mode consumes exactly one
outstanding challenge and rejects reuse, expiry, or missing challenge rows.

### 4.4 Status and capability gate

Capability advertisement is derived from the single status decision function
in `cairn-core`, then surfaced by CLI, MCP, and SDK. A capability is advertised
only when the runtime can honor it end-to-end; unsupported modes return
`CapabilityUnavailable` with stable data. Arrays are sorted or otherwise
stable where snapshots rely on deterministic output.

### 4.5 Handshake gate

`handshake` mints a fresh nonce on every call. When a vault and issuer are
bound, the challenge is persisted with an expiry and can be consumed once by
the replay layer. Ephemeral handshakes remain a compatibility path and are
clearly marked as not redeemable.

## 5. Error handling and privacy

All gates fail closed. Cryptographic or lifecycle failures produce typed
errors rather than partial admission. Backend I/O details are not exposed in
wire error bodies. Policy and capability rejections keep stable machine data
and optional remediation text; human strings remain secondary. No private key
bytes or raw sensitive record bodies are written to logs or fixtures.

## 6. Testing strategy

Run the existing focused suites first to establish the integrated baseline:

- `cargo nextest run -p cairn-core --locked verifier status identity`
- `cargo nextest run -p cairn-store-sqlite --locked replay`
- `cargo nextest run -p cairn-cli --locked handshake status identity`
- `cargo nextest run -p cairn-mcp --locked handshake init_status`
- `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
- `./scripts/check-core-boundary.sh`

If a gate fails or lacks direct coverage, add the smallest regression test
that proves the parent acceptance criterion. Watch that test fail for the
expected reason, then patch the owning layer and rerun the focused suite. The
final verification pass uses the repo checklist scaled to touched areas:
format, clippy, check, nextest, doctests, core-boundary, and codegen check.

## 7. Completion criteria

The issue is complete when:

- Each #7 acceptance criterion maps to at least one passing automated test.
- Any code changes are covered by a prior failing test.
- CLI, MCP, and SDK status surfaces agree on advertised capabilities for the
  same gates.
- The final response or PR body cites §4.2, §8.0.a, §8.0.b, and §14 and lists
  the exact verification commands run.
