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
  of capability code identifiers** is frozen string-by-string. The
  **advertised set** at runtime is still decided by
  `cairn-core::status::advertise` and is not frozen.
- `cairn.admin.v1` extension (operator verbs).

NOT frozen under `cairn.mcp.v1` — each carries its own
`<namespace>.v1` semver:

- `cairn.aggregate.v1` (ships v0.2)
- `cairn.federation.v1` (ships v0.3)
- `cairn.sessiontree.v1` (ships v0.3)

See [ADR 0004 §1–§2](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
for the exhaustive list.

## Adding a capability (additive, no version bump)

1. Add the code identifier to
   `crates/cairn-idl/schema/capabilities/capabilities.json`.
2. Flip the matching `wiring::*_WIRED` constant in `cairn-core` when the
   dispatch path is ready (per [CLAUDE.md §4 invariant 6](https://github.com/windoliver/cairn/blob/main/CLAUDE.md)).
3. Update the corresponding row in [Capability Matrix](../reference/capability-matrix.md)
   and the `REMEDIATION` table in `cairn-core::status::REMEDIATION` if the
   code can be returned in a `CapabilityUnavailable.data.remediation` hint.
4. Re-run codegen: `cargo run -p cairn-idl --bin cairn-codegen` and commit
   the snapshot deltas.

## Adding an optional field (additive, no version bump)

1. Add the field as `Option<T>` in the IDL.
2. Mark it `#[serde(default)]` (codegen does this automatically — verify).
3. Re-run codegen; commit the snapshot deltas.
4. The `contract-drift` job will pass because the change is additive.

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

| Test | Catches |
|------|---------|
| `crates/cairn-idl/tests/wire_compat_v1.rs` | Any edit to a contract file (manifest, envelope, errors, capabilities, extensions, common, prelude, verbs, plugin). |
| `crates/cairn-core/tests/capability_matrix_v1.rs` | Over- or under-advertise drift from `cairn-core::status::advertise`. |
| `crates/cairn-mcp/tests/mcp_conformance.rs` | Envelope-shape drift; missing happy-path coverage. |

A red `contract-drift` means **stop and reconcile**. Re-running
`cargo run -p cairn-idl --bin cairn-codegen` to regenerate snapshots
is the right move only if the change is intended and additive. If it
is breaking, follow the "Proposing a breaking change" section instead.

## Cross-references

- [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md) — authoritative policy.
- Brief: [§8](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md), [§8.0.a](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md), [§8.0.a-bis](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md).
- [Capability Matrix](../reference/capability-matrix.md).
- [Beta Readiness](beta-readiness.md).
- [CI](ci.md).
- [CLAUDE.md](https://github.com/windoliver/cairn/blob/main/CLAUDE.md) §4 invariant 6.
