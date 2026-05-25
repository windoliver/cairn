# MCP Semver Policy

> **Operator summary.** [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
> is authoritative; this page is the day-to-day reference for release
> operators and contributors touching the contract surface.

`cairn.mcp.v1` is the frozen MCP contract that v1.0 ships under. This page
tells you what's frozen, how to make a change without breaking it, and which
CI job enforces the freeze.

## What's frozen

Frozen under `cairn.mcp.v1` (any change here mints `cairn.mcp.v2`):

- The eight core verbs: `ingest`, `search`, `retrieve`, `summarize`,
  `assemble_hot`, `capture_trace`, `lint`, `forget`.
- Prelude verbs: `status`, `handshake`.
- The envelope (`envelope/request.json`, `envelope/response.json`,
  `envelope/signed_intent.json`).
- The error model (`errors/error.json`, including the
  `CapabilityUnavailable` → exit-code 69 mapping).
- The capabilities registry (`capabilities/capabilities.json`) — the **set
  of capability code identifiers** is frozen string-by-string (existing
  codes can't be renamed or repurposed). The **advertised set** at
  runtime is still decided by `cairn-core::status::advertise` and is
  not frozen. Caveat: today's generated `enum Capabilities` is a closed
  serde enum, so adding a new code to the registry is contract-additive
  but requires coordinated client upgrades to be wire-additive — see
  "Adding a capability" below.
- Reserved namespace `cairn.admin.v1` (verb IDs `snapshot`, `restore`,
  `replay_wal` are frozen identifiers). Dispatch is currently in the
  deferred-wiring bucket; the namespace + verb names are reserved for
  v1 so they can't be reassigned elsewhere.

NOT frozen under `cairn.mcp.v1` — each carries its own
`<namespace>.v1` semver:

- `cairn.aggregate.v1` (ships v0.2)
- `cairn.coord.v1` (ships v0.3)
- `cairn.federation.v1` (ships v0.3)
- `cairn.sessiontree.v1` (ships v0.3)

See [ADR 0004 §1–§2](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
for the exhaustive list.

## Reserving a capability identifier (the only safe additive path under v1)

**Wire constraint.** The generated `enum Capabilities` is a closed serde
enum with no `#[serde(other)]` catch-all. An older v1 client built
against the current schema will fail to deserialize a `status` response
that contains a code it doesn't know. Same constraint applies to
`enum ErrorCode` and `enum ResponseTarget`.

So under v1, only **reservation** is additive. *Advertising* a newly
reserved code (or returning a new error code, or returning a new
retrieve target) breaks older v1 clients during deserialization.

**Reserve-only recipe** (safe under v1):

1. Add the code identifier to
   `crates/cairn-idl/schema/capabilities/capabilities.json` with the
   correct `x-cairn-since` phase.
2. **Do not** flip any `wiring::*_WIRED` constant. **Do not** update
   `capability_matrix_v1.rs::expected_full_p0()` or any of the
   phase-test expected sets — the identifier should remain in
   deferred-wiring.
3. Update the [Capability Matrix](../reference/capability-matrix.md)
   "Deferred wiring" bucket (and `REMEDIATION` if the code can appear
   in a `CapabilityUnavailable.data.remediation` hint).
4. Re-run codegen: `cargo run -p cairn-idl --bin cairn-codegen`.
5. Accept the wire-compat snapshot updates:
   ```bash
   cargo nextest run -p cairn-idl --test wire_compat_v1   # expect FAIL
   cargo insta review                                     # diff must show only the new code
   cargo insta accept
   cargo nextest run -p cairn-idl --test wire_compat_v1   # expect PASS
   ```
6. Run the capability-matrix tests to confirm advertise() still emits
   the same set (the new code stays out of every test scenario):
   ```bash
   cargo nextest run -p cairn-core --test capability_matrix_v1
   ```
7. Commit the schema change, regenerated code, and accepted snapshots.

The code is now reserved — its string is frozen under v1, can't be
reassigned, and is not emitted on the wire.

## Advertising a reserved code (not safe under v1 today)

Going from reserved → advertised requires older v1 clients to be able
to deserialize the new variant. They cannot, because the generated
enums are closed for serde. Until open-enum tolerance lands as a v1.x
compatibility upgrade, an advertise step is **breaking under v1** and
the change must follow "Proposing a breaking change" below — either
roll the deployment under `cairn.mcp.v2`, or upgrade every existing v1
client to a build that includes the new variant before any server
flips the wiring flag.

The same constraint applies to **emitting new `ErrorCode`** or
**new `ResponseTarget`** values: reserve freely, never emit to v1.

## Adding an optional field

Splits on direction:

- **Request args** (verb args, signed-intent payload): adding an
  `Option<T>` is additive. Older clients won't send the field; the
  server defaults / ignores it. `#[serde(default)]` is auto-emitted by
  codegen for `Optional` fields (`cairn-idl::codegen::emit_sdk`).
  Re-run codegen, accept the wire-compat snapshot, commit.
- **Responses / `StatusResponse` / envelope** (anything the server
  emits to v1 clients): **NOT additive under v1**. The generated
  response structs use `#[serde(deny_unknown_fields)]`, so older v1
  clients reject responses carrying any field they don't know. Treat
  this case as breaking and follow "Proposing a breaking change" —
  either route the new field through `experimental["cairn.contracts"]`
  (per [ADR 0004 §5.4](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md))
  or roll under `cairn.mcp.v2`.

## Proposing a breaking change

Breaking changes (rename, type change, required-field removal, envelope
reshape) do **not** edit `crates/cairn-idl/schema/`. Instead:

1. Open a v2 design issue citing [ADR 0004 §4](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md).
2. Land a `DEPRECATION` CHANGELOG entry and an ADR amendment.
3. Wait for the deprecation window (≥ two minor releases) before opening
   the v2-cutover PR.

## Currently deprecated

| Surface | Deprecated since | Removal target | ADR amendment |
|---------|------------------|----------------|----------------|
| (none at v1.0) | — | — | — |

## Enforcement

The release-blocking gate is the **`contract-drift` CI job**
(`.github/workflows/ci.yml`). It runs:

| Step | Catches |
|------|---------|
| `cairn-codegen --check` | IDL ↔ generated-code drift. |
| `cairn-docgen --check` | IDL ↔ generated-docs drift. |
| `crates/cairn-idl/tests/wire_compat_v1.rs` | Any edit to a contract file (manifest, envelope, errors, capabilities, extensions, common, prelude, verbs, plugin). Uses insta snapshots over a SHA256 fingerprint + per-file bytes. |
| `crates/cairn-core/tests/capability_matrix_v1.rs` | Over- or under-advertise drift from `cairn-core::status::advertise`. |
| `crates/cairn-cli/tests/status_snapshot_insta.rs` | Default + degraded `cairn status` surfaces drifted. |
| `crates/cairn-cli/tests/sdk_cli_parity.rs` | CLI ↔ SDK signature drift. |
| `crates/cairn-sdk/tests/surface.rs` | SDK transport capability filter drift. |
| `crates/cairn-mcp/tests/init_status_parity.rs` | MCP `initialize` ↔ `status` drift. |

The full MCP envelope conformance replay
(`crates/cairn-mcp/tests/mcp_conformance.rs`) ships separately as part
of the standard `test` jobs. Both gates are required by branch
protection.

A red `contract-drift` means **stop and reconcile**. If the change is
intended and additive, follow the recipe in "Adding a capability" or
"Adding an optional field" above (which includes the snapshot-accept
step). If it is breaking, follow "Proposing a breaking change" instead.

## Cross-references

- [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md) — authoritative policy.
- Brief: [§8](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md), [§8.0.a](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md), [§8.0.a-bis](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md).
- [Capability Matrix](../reference/capability-matrix.md).
- [Beta Readiness](beta-readiness.md).
- [CI](ci.md).
- [CLAUDE.md](https://github.com/windoliver/cairn/blob/main/CLAUDE.md) §4 invariant 6.
