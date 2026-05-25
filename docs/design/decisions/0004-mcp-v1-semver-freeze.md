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
- **`cairn.admin.v1` extension**: operator verbs `snapshot`, `restore`,
  `replay_wal` (brief §8.0.a). Frozen alongside core because v1.0 ships
  with this extension always-registered for operator deployments.

### 2. NOT frozen under `cairn.mcp.v1`

- `cairn.aggregate.v1` (ships v0.2) — independent semver.
- `cairn.federation.v1` (ships v0.3) — independent semver.
- `cairn.sessiontree.v1` (ships v0.3) — independent semver.

Today these extensions' schema files live under
`crates/cairn-idl/schema/verbs/` alongside core verb schemas. Directory
layout is an IDL implementation detail; **namespace ownership is the
`extensions/registry.json` map**, which is the authoritative binding
between verb ID and namespace.

### 3. Additive changes — permitted without a version bump

The following changes are **additive** and ship under `cairn.mcp.v1`:

- New capability code added to `capabilities/capabilities.json`. Clients
  fail closed on un-advertised codes (brief §8.0.a invariant 6), so
  registry growth never breaks an older client.
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
   `crates/cairn-idl/schema-v2/` directory with its own manifest;
   `cairn mcp` advertises both contracts in `status.contract` (the field
   becomes an array during the deprecation window); clients pin via the
   `contract` field on every envelope.
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
[#98](https://github.com/windoliver/cairn/issues/98)). It runs:

- `crates/cairn-idl/tests/wire_compat_v1.rs` — SHA256 fingerprint of
  every contract file (manifest + envelope + errors + capabilities +
  extensions + common + prelude + verbs + plugin) plus an exact-equality
  check on `index.json#x-cairn-files`.
- `crates/cairn-core/tests/capability_matrix_v1.rs` —
  `HashSet<Capabilities>` equality against `advertise()` for five
  scenario configurations.
- `crates/cairn-mcp/tests/mcp_conformance.rs` — envelope-replay over
  canonical fixtures; gap-fill happy-path coverage for every v0.1 verb.

The job is the **v1-freeze gate** and is required on `main` for v1.0+.
A red `contract-drift` job blocks merge regardless of other approvals.

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
