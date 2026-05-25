# ADR 0004 — `cairn.mcp.v1` semver freeze and compatibility policy

- **Status:** Accepted — 2026-05-25
- **Deciders:** Cairn maintainers
- **Issue:** [#140](https://github.com/windoliver/cairn/issues/140)
- **Parent epic:** [#32](https://github.com/windoliver/cairn/issues/32)
- **Design-brief sections:** §8 (contract surfaces), §8.0.a (extension namespaces), §8.0.a-bis (contract version), §8.0.b (envelope), §15 (evaluation — wire-compat), §19 (v1.0 production)
- **Supersedes:** none

## Context

Brief §19 commits the v1.0 release to "Semver commitment on MCP surface
(`cairn.mcp.v1` frozen)." §8.0.a-bis names the contract and asserts that
breaking changes mint `cairn.mcp.v2` and that both versions run side by side
during deprecation. Until this ADR, the rules describing **what** is frozen,
**what** counts as additive, **how long** a deprecation window must be, and
**how** v2 ships were scattered across brief excerpts and PR descriptions
([#67](https://github.com/windoliver/cairn/issues/67),
[#98](https://github.com/windoliver/cairn/issues/98),
[#138](https://github.com/windoliver/cairn/issues/138)).

v1.0 release requires a single named policy document that downstream consumers
(Codex, Gemini, Koi) can pin against. This ADR is that policy.

## Decision

The following rules govern `cairn.mcp.v1` from v1.0 forward.

### 1. Frozen surface

The following constitutes `cairn.mcp.v1`. Any change here is **breaking** and
requires `cairn.mcp.v2`.

- **Eight core verbs** (per brief §8.0 table):
  `ingest`, `search`, `retrieve`, `summarize`, `assemble_hot`,
  `capture_trace`, `lint`, `forget` — verb IDs, CLI command names, MCP tool
  names, and SDK function names.
- **Prelude verbs** (brief §8.0.a):
  `status` (deterministic capability discovery — MCP `initialize` payload),
  `handshake` (fresh challenge mint).
- **Envelope** (brief §8.0.b): `envelope/request.json`,
  `envelope/response.json`, `envelope/signed_intent.json` — all required
  fields and their semantics, including `contract`, `verb`, `operation_id`,
  `status`, `policy_trace`.
- **Error model**: `errors/error.json` — every variant of `ErrorCode`,
  the `data.remediation` shape, and the sysexits-style exit-code mapping
  (notably `CapabilityUnavailable` → 69).
- **Capabilities registry**: `capabilities/capabilities.json` — the **set
  of capability code identifiers** (e.g., `cairn.mcp.v1.search.semantic`)
  is frozen string-by-string. The **advertised set returned at runtime** is
  runtime-decided by `cairn-core::status::advertise` and is not frozen
  (see §3 below).
- **Reserved namespace `cairn.admin.v1`**: the namespace and the verb
  IDs `snapshot`, `restore`, `replay_wal` (brief §8.0.a) are frozen
  *identifiers* — they cannot be reassigned to other meanings under
  `cairn.mcp.v1`. The extension is currently in the deferred-wiring
  bucket (`capability_matrix_v1.rs::DEFERRED_AT_PHASE`), so the runtime
  does not advertise `cairn.mcp.v1.extension.admin` today. When the
  dispatch path lands it becomes advertised under v1 (no version bump),
  per §3.

### 2. NOT frozen under `cairn.mcp.v1`

- `cairn.aggregate.v1` (ships v0.2) — independent semver.
- `cairn.coord.v1` (ships v0.3) — independent semver.
- `cairn.federation.v1` (ships v0.3) — independent semver.
- `cairn.sessiontree.v1` (ships v0.3) — independent semver.

Today these extensions' schema files live under
`crates/cairn-idl/schema/verbs/` alongside core verb schemas. Directory
layout is an IDL implementation detail; **namespace ownership is the
`extensions/registry.json` map**, which is the authoritative binding
between verb ID and namespace.

### 3. Additive changes — permitted without a version bump

The following changes are **additive** and ship under `cairn.mcp.v1`:

- New capability code added to `capabilities/capabilities.json` **as a
  reserved identifier**. The string is now permanently bound to its
  meaning under `cairn.mcp.v1` (no aliasing). Whether the code can be
  *advertised* in `status.capabilities` without breaking older clients
  depends on the wire-tolerance of the deserializer: today's generated
  `enum Capabilities` is a closed serde enum (no `#[serde(other)]`),
  so an older v1 client built against the current schema will fail to
  parse a `status` response containing a new code. Until the wire model
  grows tolerance (an open-enum variant or a `Vec<String>` typing),
  advertising a new code requires a coordinated server + client upgrade
  — it remains additive at the contract level (no v2 bump) but is
  release-coordinated rather than free. Tracked as a follow-up
  (open-enum tolerance is a v1.x compatibility improvement, not a v2
  trigger).
- New optional field on existing verb args. The field MUST be `Option<T>`
  in Rust with `#[serde(default)]` so older requests still deserialize.
- New variant on a `#[non_exhaustive]` enum (error codes, `retrieve`
  targets, capability codes).
- New extension namespace (`cairn.<name>.v1`).
- New tool description text, new examples, new docs — non-wire surface.
- Bug-fixing the runtime decision in `cairn-core::status::advertise` so
  the advertised set more accurately reflects what the runtime can
  execute. The set returned by `status` is not part of the contract; the
  **rule** that un-advertised capabilities are rejected with
  `CapabilityUnavailable` is part of the contract.

### 4. Breaking changes — require `cairn.mcp.v2`

The following changes are **breaking** and trigger a v2 cut:

- Remove or rename a verb (including capitalization or underscore changes).
- Remove a required field from any verb's args or envelope.
- Change the type or semantics of an existing field.
- Change envelope shape (e.g., move `operation_id` out of the response
  root).
- Remove a capability code that has shipped in any `v1.x` release.
- Change the meaning of a `status` field within a single daemon
  incarnation (per brief §8.0.a "byte-identical after canonical JSON
  ordering").

### 5. Deprecation lifecycle

1. **Announce.** A change destined for v2 lands in a `DEPRECATION`
   CHANGELOG entry and a corresponding ADR amendment.
2. **Mark.** Verbs / fields / codes scheduled for removal are flagged in
   their schema with `x-cairn-deprecated: <iso8601-date>` and listed in
   the maintainer page's "Currently deprecated" table.
3. **Window.** Minimum two minor releases between the announcement and
   v2 cutover.
4. **Cut.** `cairn.mcp.v2` ships in a separate
   `crates/cairn-idl/schema-v2/` directory with its own manifest. The
   v1 `status.contract` field stays a scalar `cairn.mcp.v1` (mutating it
   would itself be breaking per §4). v2 negotiation rides on an additive
   `supported_contracts: ["cairn.mcp.v1", "cairn.mcp.v2"]` field added
   to the `status` response — older v1 clients ignore it per §3.
   `cairn mcp` dispatches v1 vs v2 verbs by the inbound envelope's
   `contract` value; clients that wish to negotiate to v2 pin the
   contract on every envelope they send.
5. **Retire.** After the deprecation window plus one full minor release
   past v2 cutover, `cairn.mcp.v1` retirement ships as a separate major
   Cairn release.

### 6. Extension-namespace policy

- Each extension lives in its own `cairn.<name>.vN` namespace with
  **independent** semver.
- Extensions MUST NOT define a verb ID already used by core or by another
  extension. Collisions are a contract bug; this ADR commits to a CI lint
  enforcing the rule when a second extension lands.
- Extensions MAY ship inside the same Cairn release as core but advertise
  their freeze status separately
  (`status.extensions[].stability`: `stable` | `beta`). At v1.0, only
  `cairn.admin.v1` is `stable`.
- An extension's breaking change does **not** trigger `cairn.mcp.v2`.
  It triggers `cairn.<name>.v2` on its own cadence.

### 7. Enforcement

The release-blocking enforcement mechanism is the existing
**`contract-drift` CI job** (added in
[#98](https://github.com/windoliver/cairn/issues/98)). Per
`.github/workflows/ci.yml` it runs:

- `cairn-codegen --check` and `cairn-docgen --check` (no IDL or doc drift).
- `crates/cairn-idl/tests/wire_compat_v1.rs` — insta snapshot of the
  SHA256 fingerprint over every contract file (manifest + envelope +
  errors + capabilities + extensions + common + prelude + verbs +
  plugin), plus per-file snapshots and an exact-equality check on
  `index.json#x-cairn-files`.
- `crates/cairn-core/tests/capability_matrix_v1.rs` —
  `HashSet<Capabilities>` equality against `advertise()` for five
  scenario configurations.
- `crates/cairn-cli/tests/status_snapshot_insta.rs`,
  `crates/cairn-cli/tests/sdk_cli_parity.rs`,
  `crates/cairn-sdk/tests/surface.rs`,
  `crates/cairn-mcp/tests/init_status_parity.rs` — per-surface
  status / parity / SDK transport filter snapshots.

The job is the **v1-freeze gate** and is required on `main` for v1.0+.
A red `contract-drift` job blocks merge regardless of other approvals.

The full MCP envelope conformance replay
(`crates/cairn-mcp/tests/mcp_conformance.rs`, landed in
[#67](https://github.com/windoliver/cairn/issues/67)) runs as part of
the standard `test` jobs — it's a separate gate, not part of
`contract-drift`. Both are required by branch protection.

## Consequences

- Downstream consumers (Codex, Gemini, Koi) can pin against a written,
  versioned contract instead of inferring rules from brief excerpts.
- Future contributors learn the freeze rules from one ADR rather than
  reading PR threads.
- Small docs surface to maintain (this ADR + the maintainer page); no new
  code paths to debug.
- Risk: the `x-cairn-deprecated` schema marker is not enforced by the IDL
  today. Acceptable — the marker is documentation; the rule that removal
  requires v2 is enforced by the wire-compat fingerprint in `contract-drift`.

## Cross-references

- Brief: [§8 contract surfaces](../design-brief.md), [§8.0.a extension namespaces](../design-brief.md), [§8.0.a-bis contract version](../design-brief.md), [§15 evaluation](../design-brief.md), [§19 sequencing](../design-brief.md).
- Operator summary: [`docs/site/src/maintainers/mcp-semver-policy.md`](../../site/src/maintainers/mcp-semver-policy.md).
- Capability matrix: [`docs/site/src/reference/capability-matrix.md`](../../site/src/reference/capability-matrix.md).
- Beta readiness gate: [`docs/site/src/maintainers/beta-readiness.md`](../../site/src/maintainers/beta-readiness.md).
- Repo invariant: [`CLAUDE.md` §4 invariant 6](../../../CLAUDE.md).
