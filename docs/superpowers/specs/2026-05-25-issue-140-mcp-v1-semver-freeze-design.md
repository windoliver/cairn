# `cairn.mcp.v1` Semver Freeze & Compatibility Policy — Design

**Issue**: [#140](https://github.com/windoliver/cairn/issues/140) — `[P3] Freeze cairn.mcp.v1 semver contract and compatibility policy`
**Parent**: [#32](https://github.com/windoliver/cairn/issues/32) — `[P3] Ship production packaging, desktop GA, and MCP semver freeze`
**Phase**: v1.0 (Production GA) · priority P3
**Brief sections**: §19 v1.0 Production, §8 Contract surfaces, §8.0.a-bis Contract version, §8.0.a Extension namespaces, §15 Evaluation (wire-compat)
**Status**: Spec — pending implementation plan

---

## 1. Goal

Codify the `cairn.mcp.v1` semver freeze that v1.0 ships under. Specifically:

1. **Author an ADR** (`docs/design/decisions/0004-mcp-v1-semver-freeze.md`) declaring
   the frozen surface, additive rules, deprecation lifecycle, the breaking-change
   procedure that produces `cairn.mcp.v2`, and the extension-namespace policy.
2. **Add an operator-facing summary page** (`docs/site/src/maintainers/mcp-semver-policy.md`)
   that points at the ADR and tells release operators which CI job enforces it.
3. **Name the existing `contract-drift` CI job (added in #98) as the release-blocking
   v1-freeze gate** in CI docs and the beta-readiness runbook (#138). Required-status
   on `main` for v1.0+.
4. **Wire references** — capability matrix gains a "Stability" column, beta-readiness
   gains a "MCP contract freeze verified" checklist row, brief §8.0.a-bis carries a
   one-line pointer to the ADR, traceability matrix gets a v1.0 row.

The infrastructure exists: #67 landed the conformance suite; #98 landed wire-compat
fingerprints and the capability-matrix snapshot; #138 froze v0.4 docs. This PR
turns scattered enforcement into a **named, governed freeze contract**.

## 2. Non-goals

- **Net-new compat machinery.** No deprecation registry crate, no typed
  `status.deprecated[]` field, no v2 namespace scaffold. The ADR documents
  procedures; code lands when the first breaking change is proposed.
- **Extension-namespace collision lint.** ADR commits to the rule; the CI check
  is a separate small PR when we register the second extension (only
  `cairn.admin.v1` exists today; `aggregate`, `federation`, `sessiontree` are
  separate namespaces with independent freeze cadences).
- **Freezing later extensions under `cairn.mcp.v1`.** `cairn.aggregate.v1`,
  `cairn.federation.v1`, and `cairn.sessiontree.v1` each carry their own
  `<namespace>.v1` semver and freeze independently.
- **Brief rewrite.** §8.0.a-bis already pins the rule "breaking change yields
  `cairn.mcp.v2` and both versions run side by side during deprecation". This PR
  references the ADR; it does not relocate the brief sentence.
- **New Rust code.** Docs + ADR only.

## 3. Source of truth in the brief

| Brief excerpt | This design's response |
|---|---|
| §19 v1.0 — "Semver commitment on MCP surface (`cairn.mcp.v1` frozen)." | The ADR is that commitment; the maintainer page is its operator-facing surface. |
| §8.0.a-bis — "the entire verb set below is frozen under this name; a breaking change yields `cairn.mcp.v2`" | ADR §3 enumerates the frozen set verbatim from §8.0 + §8.0.a-bis; ADR §6 defines the v2 procedure. |
| §8.0.a — Extension namespaces table (`cairn.admin.v1`, `cairn.aggregate.v1`, `cairn.federation.v1`, `cairn.sessiontree.v1`) | ADR §7 declares each extension as its own versioned contract; only `cairn.admin.v1` is frozen alongside core at v1.0. |
| §8.0.b — Envelope (request/response/signed_intent) | ADR §3 includes envelope shape in the frozen set. |
| §15 — "the CI wire‑compat matrix confirms `cairn.mcp.v1` verb set + declared capabilities match the runtime" | Maintainer page names the existing `contract-drift` job as the enforcement mechanism and pins it as required on `main` for v1.0+. |
| CLAUDE.md §4 invariant 6 — "Capability advertisement decisions live in `cairn-core::status::advertise`" | ADR §5 cites this: capability codes are frozen identifiers; the advertised set is runtime-decided and gated by `advertise()`. |

## 4. Architecture

### 4.1 Files added

```
docs/design/
  decisions/
    0004-mcp-v1-semver-freeze.md       # ADR — source of truth
docs/site/src/
  maintainers/
    mcp-semver-policy.md               # operator-facing summary, links ADR
```

### 4.2 Files modified

```
docs/design/design-brief.md            # §8.0.a-bis: one-line pointer to ADR 0004
docs/design/traceability.md            # §19 row gets ADR ref; §8 row notes ADR
docs/site/src/SUMMARY.md               # mdbook nav for the new maintainer page
docs/site/src/reference/capability-matrix.md
                                       # add "Stability" column; cross-link to ADR
docs/site/src/maintainers/beta-readiness.md
                                       # add "MCP contract freeze verified" checklist row
docs/site/src/maintainers/ci.md        # name contract-drift as the v1-freeze gate
```

No code changes. No schema changes. No CI workflow changes (job already exists; PR
elevates it in docs).

## 5. ADR 0004 — content outline

### §1. Status & date
Accepted, 2026-05-25. Authors: maintainers list. Supersedes none.

### §2. Context
Brief §19 commits v1.0 to a frozen MCP surface. §8.0.a-bis names the contract
`cairn.mcp.v1` and asserts breaking changes mint `cairn.mcp.v2`. Multiple PRs
landed the enforcement plumbing (#67, #98). Until this ADR, the rules describing
what is frozen, what counts as additive, and how v2 ships were scattered across
brief excerpts and PR descriptions. v1.0 release requires a single named policy
document.

### §3. Frozen surface (exhaustive enumeration)
The following constitutes `cairn.mcp.v1`. Any change here is **breaking** and
requires `cairn.mcp.v2`.

- **Eight core verbs** (per brief §8.0 table):
  `ingest`, `search`, `retrieve`, `summarize`, `assemble_hot`,
  `capture_trace`, `lint`, `forget` — verb IDs, CLI command names, MCP tool
  names, SDK function names.
- **Prelude verbs** (brief §8.0.a):
  `status` (deterministic capability discovery — MCP `initialize` payload),
  `handshake` (fresh challenge mint).
- **Envelope** (brief §8.0.b): `envelope/request.json`,
  `envelope/response.json`, `envelope/signed_intent.json` — all required fields
  and their semantics, including `contract`, `verb`, `operation_id`, `status`,
  `policy_trace`.
- **Error model**: `errors/error.json` — every variant of `ErrorCode`,
  the `data.remediation` shape, and the sysexits-style exit-code mapping
  (notably `CapabilityUnavailable` → 69).
- **Capabilities registry**: `capabilities/capabilities.json` — the **set of
  capability code identifiers** (e.g., `cairn.mcp.v1.search.semantic`) is frozen
  string-by-string. The **advertised set returned at runtime** is runtime-decided
  by `cairn-core::status::advertise` and is *not* frozen (see §5 below).
- **`cairn.admin.v1` extension**: operator verbs `snapshot`, `restore`,
  `replay_wal` (brief §8.0.a). Frozen alongside core because v1.0 ships with this
  extension always-registered for operator deployments.

### §4. NOT frozen under `cairn.mcp.v1`
- `cairn.aggregate.v1` (ships v0.2) — independent semver.
- `cairn.federation.v1` (ships v0.3) — independent semver.
- `cairn.sessiontree.v1` (ships v0.3) — independent semver.
- Today these extensions' schema files live under `crates/cairn-idl/schema/verbs/`
  alongside core verb schemas. Directory layout is an IDL implementation detail;
  **namespace ownership is the `extensions/registry.json` map**, which is the
  authoritative binding between verb ID and namespace.

### §5. Additive changes — permitted without a version bump

> **Note:** The final accepted policy is ADR 0004 §3 in the repo. This
> spec section is preserved as the original design draft; the wording
> here is superseded by the ADR's stricter rules around closed-serde
> enums. For implementation, follow the ADR.

The following are additive and ship under `cairn.mcp.v1`:

- **Reserving** a new identifier in any closed-enum schema
  (`capabilities/capabilities.json`, `errors/error.json`,
  `verbs/retrieve.json` target arms). Adding the string locks it to
  v1; *emitting* it on the wire is governed by ADR 0004 §4 because the
  generated enums are closed for serde.
- New optional field on **request args** (verb args / signed-intent
  payload). `Option<T>` + `#[serde(default)]` so older clients
  default-omit. NOT additive on response / status / envelope (those
  use `#[serde(deny_unknown_fields)]`).
- New extension namespace (`cairn.<name>.v1`).
- New tool description text, new examples, new docs — non-wire surface.
- Bug-fixing the runtime decision in `advertise()` so the advertised
  set more accurately reflects what the runtime can execute, provided
  the new set is a subset of identifiers older v1 clients already know.

### §6. Breaking changes — require `cairn.mcp.v2`
The following changes are **breaking** and trigger a v2 cut:

- Remove or rename a verb (including capitalization, underscore changes).
- Remove a required field from any verb's args or envelope.
- Change the type or semantics of an existing field.
- Change envelope shape (e.g., move `operation_id` out of the response root).
- Remove a capability code that has shipped in any `v1.x` release.
- Change the meaning of a `status` field within a single daemon incarnation
  (per brief §8.0.a "byte-identical after canonical JSON ordering").

### §7. Deprecation lifecycle
1. **Announce.** A change destined for v2 lands in a `DEPRECATION` CHANGELOG
   entry and a corresponding ADR amendment.
2. **Mark.** Verbs / fields / codes scheduled for removal are flagged in their
   schema with `x-cairn-deprecated: <iso8601-date>` and noted in
   `mcp-semver-policy.md` under "Currently deprecated".
3. **Window.** Minimum two minor releases between announcement and v2 cutover.
4. **Cut.** `cairn.mcp.v2` ships in a separate `crates/cairn-idl/schema-v2/`
   directory with its own manifest. The v1 `status.contract` field stays a
   scalar `cairn.mcp.v1` and the v1 `prelude/status.json` schema
   (`additionalProperties: false`) stays as-is — mutating it would itself be
   breaking. v2 negotiation rides on the MCP `serverCapabilities.experimental`
   map (already in use for `experimental["cairn.status"]`); v2-capable servers
   add a sibling key `experimental["cairn.contracts"]`. Alternative: a separate
   `cairn mcp --contract v2` endpoint that v1 clients never connect to.
5. **Retire.** After the deprecation window plus one full minor release past
   v2 cutover, `cairn.mcp.v1` retirement ships as a separate major Cairn release.

### §8. Extension-namespace policy
- Each extension lives in its own `cairn.<name>.vN` namespace with **independent**
  semver.
- Extensions MUST NOT define a verb ID already used by core or by another
  extension. Collisions are a contract bug; the policy commits to a CI lint
  here when a second extension lands.
- Each extension's freeze status (stable / beta / reserved) is tracked in this
  ADR + maintainer page, NOT on the wire — v1 `status.extensions[]` is closed
  (`additionalProperties: false`). At v1.0, `cairn.admin.v1` is reserved
  (dispatch deferred); the other four extensions ship under their own
  independent `<namespace>.v1` semver.
- An extension's breaking change does **not** trigger `cairn.mcp.v2`. It triggers
  `cairn.<name>.v2` on its own cadence.

### §9. Enforcement
The release-blocking enforcement mechanism is the existing **`contract-drift`
CI job** (added in #98). It runs:

- `crates/cairn-idl/tests/wire_compat_v1.rs` —
  SHA256 fingerprint of every contract file (manifest + envelope + errors +
  capabilities + extensions + common + prelude + verbs + plugin) plus an
  exact-equality check on `index.json#x-cairn-files`.
- `crates/cairn-core/tests/capability_matrix_v1.rs` —
  `HashSet<Capabilities>` equality against `advertise()` for five scenario
  configurations.
- `crates/cairn-mcp/tests/mcp_conformance.rs` —
  envelope-replay over canonical fixtures; gap-fill happy-path coverage for
  every v0.1 verb.

The job is named the **v1-freeze gate** and is required on `main` for v1.0+.
A red `contract-drift` job blocks merge regardless of other approvals.

### §10. Consequences
- Codifies what was implicit; downstream consumers (Codex, Gemini, Koi) can
  pin against a written contract.
- Future contributors learn the rules from one place rather than reading PR
  threads.
- Costs a small docs surface to maintain (this ADR + the maintainer page);
  no new code paths to debug.
- Slight risk: a deprecation marker in schema (`x-cairn-deprecated`) is not
  enforced by the IDL today. Acceptable — the marker is documentation; the
  rule that removal requires v2 is enforced by the wire-compat fingerprint.

## 6. Maintainer page outline (`docs/site/src/maintainers/mcp-semver-policy.md`)

Short, operator-facing (~150 lines). Sections:

1. **What's frozen** — three-line summary, link to ADR §3.
2. **Adding a capability** — two-step recipe: (a) add code to
   `capabilities/capabilities.json`, (b) flip the `wiring::*_WIRED` constant
   when dispatch is ready (per CLAUDE.md §4 invariant 6).
3. **Adding an optional field** — `Option<T>` + `#[serde(default)]` + IDL
   re-codegen + commit snapshot deltas.
4. **Proposing a breaking change** — file an ADR amendment; do not edit
   `crates/cairn-idl/schema/` for breaking deltas; open a v2 design issue.
5. **Currently deprecated** — table (empty at v1.0).
6. **Enforcement** — names `contract-drift` as the gate; lists the three
   tests it runs; tells operators what a red job means.
7. **Cross-references** — ADR 0004, brief §8 / §8.0.a / §8.0.a-bis,
   capability-matrix, beta-readiness checklist, CLAUDE.md §4.

## 7. Cross-doc updates

### 7.1 Capability matrix (`docs/site/src/reference/capability-matrix.md`)
Add a **Stability** column on the capability codes table:

| Row | Capability codes | Stability |
|-----|------------------|-----------|
| Core verbs | `cairn.mcp.v1.<verb>` | frozen v1.0 |
| `search` modes | `cairn.mcp.v1.search.{keyword,semantic,hybrid,federation}` | frozen v1.0 |
| `forget` modes | `cairn.mcp.v1.forget.{record,session,scope}` | frozen v1.0 |
| Extension: admin | `cairn.admin.v1.*` | frozen v1.0 |
| Extension: aggregate | `cairn.aggregate.v1.*` | independent v1.0 |
| Extension: federation | `cairn.federation.v1.*` | independent v1.0 |
| Extension: sessiontree | `cairn.sessiontree.v1.*` | independent v1.0 |
| Sensors | `cairn.sensors.v1.<sensor>` | sensors namespace (independent) |

Add a footer line: "Stability is governed by [ADR 0004](../../design/decisions/0004-mcp-v1-semver-freeze.md)."

### 7.2 Beta-readiness (`docs/site/src/maintainers/beta-readiness.md`)
Add to the checklist a row under "Contract enforcement":

> - [ ] MCP contract freeze verified — `contract-drift` CI job green on the
>       release SHA; no `x-cairn-deprecated` markers slipped in without an ADR
>       amendment. See [MCP semver policy](mcp-semver-policy.md) and
>       [ADR 0004](../../design/decisions/0004-mcp-v1-semver-freeze.md).

### 7.3 CI docs (`docs/site/src/maintainers/ci.md`)
Rename the existing `contract-drift` job entry to call out its dual role:
"contract-drift / v1-freeze gate (release-blocking on v1.0+)".

### 7.4 Brief (`docs/design/design-brief.md` §8.0.a-bis)
One-line pointer immediately after the existing
"`cairn.mcp.v1` — the entire verb set below is frozen under this name" line:

> See [ADR 0004 — `cairn.mcp.v1` semver freeze](decisions/0004-mcp-v1-semver-freeze.md)
> for the frozen surface, additive/deprecation rules, and v2 procedure.

### 7.5 Traceability (`docs/design/traceability.md`)
- §19 row: cite ADR 0004 in the Decisions column.
- §8 row: append "ADR 0004 codifies the v1.0 semver freeze."

## 8. Testing

Pure docs PR. Verification is mdbook + rustdoc + spot-check.

- `mdbook build docs/site` — validates new maintainer page renders and links
  resolve.
- `cargo run -p cairn-cli --bin cairn-docgen -- --check` — generated docs
  match (no regeneration needed; this PR doesn't touch flags/config).
- `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc
  --workspace --no-deps --document-private-items --locked` — clean.
- Existing `contract-drift` job remains green — this PR does not touch schema
  or `advertise()`.
- Manual: open the rendered maintainer page; confirm every link
  (ADR, brief, capability-matrix, beta-readiness, CLAUDE.md) resolves.

## 9. Edge cases & explicit non-coverage

- **`propose_share` et al. live in `verbs/` directory.** The IDL schema-file
  layout currently groups all verb schemas together regardless of namespace.
  ADR §4 calls this out as an IDL implementation detail; namespace ownership
  is `extensions/registry.json`. No file moves in this PR.
- **`status.deprecated[]` typed field.** Out of scope. ADR §7's "Mark" step
  uses an `x-cairn-deprecated` schema marker, which is documentation today.
  When the first deprecation lands, a follow-up issue adds the typed field.
- **Multi-version coexistence (v1 + v2 in the same binary).** Procedure
  documented in ADR §7 step 4. No code today; lands when v2 cutover is
  scheduled.
- **Sensor-namespace freeze.** `cairn.sensors.v1.*` is referenced in the
  capability matrix but lives outside MCP-verb space. The ADR does not
  freeze it; sensors are an internal capability namespace, not part of the
  MCP wire contract.
- **Public deprecation registry crate.** Not built. ADR + CHANGELOG suffice
  until volume justifies machinery.

## 10. Risks

- **Brief contradicts ADR later.** Mitigation: brief §8.0.a-bis adds an
  explicit pointer to the ADR. Standard ADR-supersession applies if a
  conflict arises.
- **Operators read maintainer page, miss ADR nuance.** Mitigation: page header
  explicitly says "ADR 0004 is authoritative; this page is the operator
  summary." Every section cross-links the ADR sub-section.
- **`contract-drift` job is renamed in CI later.** Maintainer page would go
  stale. Mitigation: ADR §9 names the **enforcement responsibility** rather
  than the job slug; the maintainer page lists the job slug today and is
  expected to update with CI rename PRs.
- **A second extension lands and a verb-ID collision slips through.** ADR §8
  commits to a CI lint when this happens; until then, code review enforces.
  Acceptable risk given only `cairn.admin.v1` is registered today.

## 11. Acceptance criteria

Mapping the GitHub issue acceptance criteria to deliverables:

| Issue criterion | Where satisfied |
|---|---|
| `cairn.mcp.v1` has an explicit semver guarantee | ADR 0004 §3 (frozen surface) + §5 (additive) + §6 (breaking) |
| Breaking changes require a new namespace/version | ADR 0004 §6 + §7 (deprecation lifecycle) |
| Compatibility tests block accidental drift | ADR 0004 §9 names `contract-drift` as the release-blocking gate; maintainer page repeats; beta-readiness checklist row |
| Run wire fixture compatibility suite | `wire_compat_v1.rs` + `capability_matrix_v1.rs` + `mcp_conformance.rs` — all gated by `contract-drift` (existing) |
| Run docs review against generated schema | mdbook build + docgen `--check` (existing) |
| Run extension namespace compatibility tests | ADR §8 + capability-matrix Stability column; CI lint deferred per non-goal |

## 12. Open questions

None. Defaults chosen:
- Deprecation window: ≥2 minor releases.
- v2 schema directory: `crates/cairn-idl/schema-v2/`.
- Stability tiers: `stable | beta` (mirror of common semver-ish terminology).
- Maintainer page lives under `docs/site/src/maintainers/` (matches the
  `beta-readiness.md` precedent).

## 13. Implementation order

1. Write ADR 0004.
2. Write maintainer page; link ADR.
3. Add Stability column to capability matrix; cross-link ADR.
4. Add checklist row to beta-readiness.
5. Update CI docs to name the freeze gate.
6. Brief §8.0.a-bis pointer.
7. Traceability matrix rows.
8. SUMMARY.md nav entry for the new maintainer page.
9. Run verification commands (mdbook + docgen check + rustdoc + clippy + nextest).
10. Open PR citing brief sections §19 and §8.0.a-bis, link issue #140.
