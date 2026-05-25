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
- `cairn.coord.v1` (ships v0.3)
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
4. Re-run codegen: `cargo run -p cairn-idl --bin cairn-codegen`.
5. Accept the wire-compat snapshot updates. The fingerprint over every
   contract file (`crates/cairn-idl/tests/snapshots/`) will change
   because `capabilities.json` changed:
   ```bash
   cargo nextest run -p cairn-idl --test wire_compat_v1   # expect FAIL
   cargo insta review    # review the diff — must show ONLY the new code
   cargo insta accept    # accept after review
   cargo nextest run -p cairn-idl --test wire_compat_v1   # expect PASS
   ```
6. Same for the capability-matrix advertise snapshot:
   ```bash
   cargo nextest run -p cairn-core --test capability_matrix_v1
   ```
7. Commit the schema change, regenerated code, and accepted snapshots
   together. `contract-drift` should now be green.

## Adding an optional field (additive, no version bump)

1. Add the field as `Option<T>` in the IDL (`#[serde(default)]` is
   auto-emitted by codegen for `Optional` fields — see
   `cairn-idl::codegen::emit_sdk`).
2. Re-run codegen: `cargo run -p cairn-idl --bin cairn-codegen`.
3. The wire-compat fingerprint will change because the verb schema file
   changed. Follow steps 5–7 of "Adding a capability" above to review
   and accept the snapshot deltas, then commit.

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
