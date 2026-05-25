# `cairn.mcp.v1` Semver Freeze Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land issue #140 — a docs-only PR that codifies the `cairn.mcp.v1` semver freeze: ADR 0004 + operator-facing maintainer page + capability-matrix Stability column + beta-readiness gate row + CI doc rename + brief pointer + traceability updates.

**Architecture:** No Rust changes. ADR `0004-mcp-v1-semver-freeze.md` is the source of truth. Maintainer page is the operator-facing summary. The existing #98 `contract-drift` CI job is named as the release-blocking v1-freeze gate; no workflow YAML changes.

**Tech Stack:** Markdown only. Verified by `mdbook build docs/site` and `RUSTDOCFLAGS="..." cargo doc`. The full beta-readiness command set still runs (existing CI is untouched), so `cargo fmt`, `clippy`, `nextest`, `core-boundary`, `codegen --check`, `docgen --check`, `cargo deny`, `cargo audit`, `cargo machete` all remain green by construction (no Rust touched).

**Spec:** [`docs/superpowers/specs/2026-05-25-issue-140-mcp-v1-semver-freeze-design.md`](../specs/2026-05-25-issue-140-mcp-v1-semver-freeze-design.md)

---

## File map

| File | Action | Responsibility |
|------|--------|----------------|
| `docs/design/decisions/0004-mcp-v1-semver-freeze.md` | Create | ADR — source of truth for the freeze (frozen set, additive/breaking rules, deprecation, v2 procedure, extension policy, enforcement). |
| `docs/site/src/maintainers/mcp-semver-policy.md` | Create | Operator-facing summary; links ADR. |
| `docs/site/src/SUMMARY.md` | Modify | Add nav entry under Maintainers for the new page. |
| `docs/site/src/reference/capability-matrix.md` | Modify | Add "Stability" column on the capability-codes table; footer link to ADR. |
| `docs/site/src/maintainers/beta-readiness.md` | Modify | Add "Contract freeze verified" checklist row under Gate 9 area; extend the sign-off block. |
| `docs/site/src/maintainers/ci.md` | Modify | Rename `contract-drift` entry to call out its dual role as the v1-freeze gate. |
| `docs/design/design-brief.md` | Modify | One-line pointer to ADR 0004 after §8.0.a-bis frozen-name sentence. |
| `docs/design/traceability.md` | Modify | §19 row gains ADR 0004 in Decisions column; §8 row's coverage note mentions ADR 0004. |

No code, no schema, no CI workflow YAML changed.

---

## Task 1 — ADR 0004 (the source-of-truth document)

**Files:**
- Create: `docs/design/decisions/0004-mcp-v1-semver-freeze.md`

- [ ] **Step 1: Create the ADR file**

Write `docs/design/decisions/0004-mcp-v1-semver-freeze.md`:

````markdown
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
5. **Retire.** After the deprecation window plus one full release cycle
   past v2 cutover, `cairn.mcp.v1` shutdown ships as a separate major
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
````

- [ ] **Step 2: Verify the ADR renders as plain Markdown**

Run: `mdbook build docs/site` from the repo root.
Expected: builds without warnings; note — `docs/design/` is outside the
site root, so the ADR is not in the rendered site; it's a repo-resident
doc.

- [ ] **Step 3: Commit**

```bash
git add docs/design/decisions/0004-mcp-v1-semver-freeze.md
git commit -m "$(cat <<'EOF'
docs(adr): 0004 cairn.mcp.v1 semver freeze (#140)

Codify the v1.0 freeze contract: enumerated frozen surface (8 core verbs,
prelude, envelope, error model, capabilities registry, cairn.admin.v1),
additive vs breaking rules, deprecation lifecycle, v2 procedure, and the
extension-namespace policy. Names the existing #98 contract-drift CI job
as the release-blocking v1-freeze gate.

Brief: §8.0.a-bis, §19.
EOF
)"
```

---

## Task 2 — Maintainer page (operator-facing summary)

**Files:**
- Create: `docs/site/src/maintainers/mcp-semver-policy.md`

- [ ] **Step 1: Create the maintainer page**

Write `docs/site/src/maintainers/mcp-semver-policy.md`:

````markdown
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
`cargo run -p cairn-idl --bin cairn-codegen --` is the right move only
if the change is intended and additive. If it is breaking, follow the
"Proposing a breaking change" section instead.

## Cross-references

- [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md) — authoritative policy.
- Brief: [§8](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md), [§8.0.a](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md), [§8.0.a-bis](https://github.com/windoliver/cairn/blob/main/docs/design/design-brief.md).
- [Capability Matrix](../reference/capability-matrix.md).
- [Beta Readiness](beta-readiness.md).
- [CI](ci.md).
- [CLAUDE.md](https://github.com/windoliver/cairn/blob/main/CLAUDE.md) §4 invariant 6.
````

- [ ] **Step 2: Verify the page builds**

Run from repo root: `mdbook build docs/site`
Expected: builds successfully; note an unresolved nav warning for the new
page — the next step fixes that by adding it to `SUMMARY.md`.

- [ ] **Step 3: Commit**

```bash
git add docs/site/src/maintainers/mcp-semver-policy.md
git commit -m "$(cat <<'EOF'
docs(maintainers): MCP semver policy operator page (#140)

Operator-facing summary of ADR 0004: what's frozen, how to add a
capability or optional field without a version bump, how to propose a
breaking change, and which CI job enforces the freeze. Currently
deprecated table ships empty.
EOF
)"
```

---

## Task 3 — Wire the maintainer page into `SUMMARY.md`

**Files:**
- Modify: `docs/site/src/SUMMARY.md`

- [ ] **Step 1: Add the nav entry**

Locate the existing Maintainers section in `docs/site/src/SUMMARY.md`:

```markdown
# Maintainers

- [Codegen](maintainers/codegen.md)
- [Docs](maintainers/docs.md)
- [CI](maintainers/ci.md)
- [Beta Readiness](maintainers/beta-readiness.md)
```

Add the new page directly after `Beta Readiness`:

```markdown
# Maintainers

- [Codegen](maintainers/codegen.md)
- [Docs](maintainers/docs.md)
- [CI](maintainers/ci.md)
- [Beta Readiness](maintainers/beta-readiness.md)
- [MCP Semver Policy](maintainers/mcp-semver-policy.md)
```

- [ ] **Step 2: Verify mdbook builds cleanly with the new entry**

Run: `mdbook build docs/site`
Expected: builds with no warnings about unresolved entries.

- [ ] **Step 3: Commit**

```bash
git add docs/site/src/SUMMARY.md
git commit -m "docs(site): nav entry for mcp-semver-policy maintainer page (#140)"
```

---

## Task 4 — Capability matrix: add Stability column

**Files:**
- Modify: `docs/site/src/reference/capability-matrix.md`

- [ ] **Step 1: Add a Stability column on the capability-codes table**

Locate the capability-codes table in `docs/site/src/reference/capability-matrix.md`
(lines ~41-49). Replace this block:

```markdown
| Row | Representative capability codes |
|-----|----------------------------------|
| Core verbs | `cairn.mcp.v1.<verb>` for each of the eight verbs |
| `search` modes | `cairn.mcp.v1.search.keyword`, `.semantic`, `.hybrid`, `.federation` |
| Session reload | `cairn.mcp.v1.retrieve.session`, `.rehydrate` |
| `forget` modes | `cairn.mcp.v1.forget.record`, `.session`, `.scope` |
| Consolidation tiers | `cairn.mcp.v1.summarize.rolling`, `.reflection`, `.rem`, `.deep` |
| Extension namespaces | `cairn.admin.v1.*`, `cairn.aggregate.v1.*`, `cairn.federation.v1.*`, `cairn.sessiontree.v1.*` |
| Sensors | `cairn.sensors.v1.<sensor>` (local + remote) |
```

With the same table extended by a `Stability` column:

```markdown
| Row | Representative capability codes | Stability |
|-----|----------------------------------|-----------|
| Core verbs | `cairn.mcp.v1.<verb>` for each of the eight verbs | frozen v1.0 |
| `search` modes | `cairn.mcp.v1.search.keyword`, `.semantic`, `.hybrid`, `.federation` | frozen v1.0 |
| Session reload | `cairn.mcp.v1.retrieve.session`, `.rehydrate` | frozen v1.0 |
| `forget` modes | `cairn.mcp.v1.forget.record`, `.session`, `.scope` | frozen v1.0 |
| Consolidation tiers | `cairn.mcp.v1.summarize.rolling`, `.reflection`, `.rem`, `.deep` | frozen v1.0 |
| Extension namespaces | `cairn.admin.v1.*` | frozen v1.0 |
| Extension namespaces | `cairn.aggregate.v1.*`, `cairn.federation.v1.*`, `cairn.sessiontree.v1.*` | independent (per-namespace) |
| Sensors | `cairn.sensors.v1.<sensor>` (local + remote) | sensors namespace (independent) |
```

- [ ] **Step 2: Add a stability footer line under the table**

Right after the table, before the `## Where this is wired` heading, insert:

```markdown
Stability tiers and the freeze rules are governed by
[ADR 0004 — `cairn.mcp.v1` semver freeze](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md).
See [MCP Semver Policy](../maintainers/mcp-semver-policy.md) for the
operator-facing summary.
```

- [ ] **Step 3: Verify mdbook builds**

Run: `mdbook build docs/site`
Expected: builds cleanly; no link warnings.

- [ ] **Step 4: Commit**

```bash
git add docs/site/src/reference/capability-matrix.md
git commit -m "docs(reference): capability-matrix Stability column + ADR 0004 link (#140)"
```

---

## Task 5 — Beta readiness: add the contract-freeze gate row

**Files:**
- Modify: `docs/site/src/maintainers/beta-readiness.md`

- [ ] **Step 1: Add a new manual gate after Gate 9**

The current Gate 9 (Capability sync, manual) ends with the "Failure"
sentence at the line "Reconcile in `cairn-core::status::advertise` plus
the matching `wiring::*_WIRED` constant per CLAUDE.md §4 invariant 6."

Directly after Gate 9, **before** the line `### 10. Migration guide
review (manual)`, insert a new gate block:

```markdown
### 10. Contract freeze verified (manual)

Verify the `contract-drift` CI job is green on the release SHA. From the
PR or the release branch:

```bash
gh run list --branch "$(git rev-parse --abbrev-ref HEAD)" \
  --workflow ci.yml --limit 1 --json conclusion,jobs \
  | jq '.[0].jobs[] | select(.name == "contract-drift") | .conclusion'
```

Expected: `"success"`.

**Pass:** `contract-drift` succeeded on the release SHA, **and** no
schema file under `crates/cairn-idl/schema/` was changed without an
accompanying ADR amendment, **and** no `x-cairn-deprecated` markers were
added or removed since the previous release without a CHANGELOG entry.

**Failure:** `contract-drift` is red. Inspect the failing test
(`wire_compat_v1`, `capability_matrix_v1`, or `mcp_conformance`); if the
change is intended and additive, regenerate fixtures per the test's
inline guidance. If the change is breaking, **stop**: file a v2 design
issue and follow the procedure in
[MCP Semver Policy](mcp-semver-policy.md).

See [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
for the authoritative freeze rules.
```

- [ ] **Step 2: Renumber subsequent gates**

Sections currently numbered 10–14 shift to 11–15.

Search-replace inside `docs/site/src/maintainers/beta-readiness.md`:

- `### 10. Migration guide review` → `### 11. Migration guide review`
- `### 11. Known limitations` → `### 12. Known limitations`
- `### 12. Cassette replay` → `### 13. Cassette replay`
- `### 13. Privacy posture` → `### 14. Privacy posture`
- `### 14. Release notes draft` → `### 15. Release notes draft`

- [ ] **Step 3: Extend the sign-off block**

Locate the sign-off block near the end of the file (the `Beta readiness
sign-off — v0.X.Y-beta.N` fenced block). Add a new checkbox between the
existing gate 9 and gate 10 rows:

Before:
```markdown
- [ ] Gate 9: capability sync (manual)
- [ ] Gate 10: migration guide review (manual)
- [ ] Gate 11: known limitations (manual)
- [ ] Gate 12: cassette replay (manual)
- [ ] Gate 13: privacy posture (manual)
- [ ] Gate 14: release notes draft (manual)
```

After:
```markdown
- [ ] Gate 9: capability sync (manual)
- [ ] Gate 10: contract freeze verified (manual)
- [ ] Gate 11: migration guide review (manual)
- [ ] Gate 12: known limitations (manual)
- [ ] Gate 13: cassette replay (manual)
- [ ] Gate 14: privacy posture (manual)
- [ ] Gate 15: release notes draft (manual)
```

- [ ] **Step 4: Update the "Failure remediation" table**

In the `## Failure remediation` table near the bottom, the existing
"Capability sync (gate 9)" row stays. Append a row for the new gate:

After the existing `Capability sync (gate 9)` row, add:

```markdown
| Contract freeze (gate 10) | `cairn-core::status::advertise`, `crates/cairn-idl/schema/`, ADR 0004. |
```

- [ ] **Step 5: Verify mdbook builds**

Run: `mdbook build docs/site`
Expected: builds cleanly with no link warnings.

- [ ] **Step 6: Commit**

```bash
git add docs/site/src/maintainers/beta-readiness.md
git commit -m "$(cat <<'EOF'
docs(maintainers): beta-readiness contract-freeze gate (#140)

Insert Gate 10 "contract freeze verified" between Gate 9 (capability
sync) and the existing Gate 10 (migration guide review); renumber
subsequent gates; extend sign-off block; add remediation row pointing
at ADR 0004.
EOF
)"
```

---

## Task 6 — CI docs: name `contract-drift` as the v1-freeze gate

**Files:**
- Modify: `docs/site/src/maintainers/ci.md`

- [ ] **Step 1: Rename the contract-drift entry**

Locate this block in `docs/site/src/maintainers/ci.md` (around line 33-36):

```markdown
- `contract-drift`: wire-compat and capability-matrix gate — runs
  `cairn-codegen --check`, `cairn-docgen --check`, the wire-compat fixtures,
  and the capability-matrix v0.1 advertise tests. Fails if any generated
  output or contract surface drifts from committed state.
```

Replace with:

```markdown
- `contract-drift` (a.k.a. **v1-freeze gate**, release-blocking on v1.0+):
  wire-compat and capability-matrix gate — runs `cairn-codegen --check`,
  `cairn-docgen --check`, the wire-compat fixtures
  (`crates/cairn-idl/tests/wire_compat_v1.rs`), the capability-matrix
  advertise tests (`crates/cairn-core/tests/capability_matrix_v1.rs`), and
  the MCP conformance suite (`crates/cairn-mcp/tests/mcp_conformance.rs`).
  Fails if any generated output or contract surface drifts from committed
  state. See [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
  and [MCP Semver Policy](mcp-semver-policy.md) for the freeze rules
  this gate enforces.
```

- [ ] **Step 2: Verify mdbook builds**

Run: `mdbook build docs/site`
Expected: builds cleanly with no link warnings.

- [ ] **Step 3: Commit**

```bash
git add docs/site/src/maintainers/ci.md
git commit -m "docs(ci): name contract-drift as the v1-freeze gate (#140)"
```

---

## Task 7 — Brief §8.0.a-bis pointer

**Files:**
- Modify: `docs/design/design-brief.md`

- [ ] **Step 1: Add an inline pointer to ADR 0004**

Locate the §8.0.a-bis "Contract version" paragraph in
`docs/design/design-brief.md` (search for the string
`the entire verb set below is frozen under this name`). The relevant
sentence currently ends with:

```text
... single source of truth across all four surfaces.
```

Append, on the same line at the very end of that sentence, the
following pointer:

```text
 See [ADR 0004 — `cairn.mcp.v1` semver freeze](decisions/0004-mcp-v1-semver-freeze.md) for the frozen surface, additive/deprecation rules, and v2 procedure.
```

The final result is one paragraph ending with both clauses, no
paragraph break.

- [ ] **Step 2: Verify rustdoc / mdbook do not complain**

Run: `mdbook build docs/site`
Expected: builds cleanly. (`docs/design/design-brief.md` is not part of
the rendered site but its relative link target is real.)

Run: `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked`
Expected: no broken intra-doc-link errors (cargo doc does not parse
design-brief, but this command catches collateral damage from any
worktree state).

- [ ] **Step 3: Commit**

```bash
git add docs/design/design-brief.md
git commit -m "docs(brief): §8.0.a-bis pointer to ADR 0004 freeze policy (#140)"
```

---

## Task 8 — Traceability matrix updates

**Files:**
- Modify: `docs/design/traceability.md`

- [ ] **Step 1: Update the §8 row's coverage note**

Locate the §8 row in `docs/design/traceability.md` (around line 81).
Append to the existing coverage-notes cell (after the period following
"...a single `contract-drift` CI job)."):

```text
 ADR 0004 codifies the v1.0 `cairn.mcp.v1` semver freeze (frozen surface, additive/deprecation/v2 rules, extension-namespace policy) and names `contract-drift` as the release-blocking enforcement gate.
```

- [ ] **Step 2: Update the §19 row's Decisions column**

Locate the §19 row (around line 98). The Decisions column today is `—`.
Change it to:

```text
ADR 0004 (resolved)
```

- [ ] **Step 3: Verify**

Run: `mdbook build docs/site`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add docs/design/traceability.md
git commit -m "docs(traceability): §8 + §19 cite ADR 0004 mcp-v1-semver-freeze (#140)"
```

---

## Task 9 — Final verification sweep

**Files:** none modified — read-only checks.

- [ ] **Step 1: mdbook build**

Run from repo root: `mdbook build docs/site`
Expected: exits 0; no warnings about unresolved links or missing
SUMMARY entries.

- [ ] **Step 2: Rustdoc broken-link check**

Run: `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked`
Expected: exits 0 with no broken-link errors.

- [ ] **Step 3: Format check (no Rust changed but defensive)**

Run: `cargo fmt --all --check`
Expected: exits 0.

- [ ] **Step 4: Clippy (defensive — no Rust changed)**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: exits 0.

- [ ] **Step 5: Tests still pass (defensive — contract-drift specifically)**

Run: `cargo nextest run -p cairn-idl -p cairn-core -p cairn-mcp --locked --no-fail-fast`
Expected: exits 0. This confirms `wire_compat_v1.rs`,
`capability_matrix_v1.rs`, and `mcp_conformance.rs` are still green
(they MUST be — this PR doesn't touch schemas or `advertise()`).

- [ ] **Step 6: Spot-check links manually**

Open `docs/site/book/maintainers/mcp-semver-policy.html` in a browser
(or `cat docs/site/book/maintainers/mcp-semver-policy.html | grep -E "href"`)
and confirm:
- ADR 0004 link resolves to `https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md`
- Capability Matrix link resolves to `../reference/capability-matrix.html`
- Beta Readiness link resolves to `beta-readiness.html`
- CI link resolves to `ci.html`

- [ ] **Step 7: Capability matrix Stability column manual review**

Open `docs/site/book/reference/capability-matrix.html`. Confirm the
capability-codes table shows three Stability tiers exactly: `frozen v1.0`,
`independent (per-namespace)`, `sensors namespace (independent)`.

---

## Task 10 — Open PR

**Files:** none modified.

- [ ] **Step 1: Verify the worktree is clean and on the working branch**

Run: `git status` and `git branch --show-current`
Expected: working tree clean; branch is the worktree branch.

- [ ] **Step 2: Push the branch**

Run: `git push -u origin "$(git branch --show-current)"`
Expected: branch pushed.

- [ ] **Step 3: Open the PR**

Run:

```bash
gh pr create --title "docs: freeze cairn.mcp.v1 semver contract (#140)" --body "$(cat <<'EOF'
## Summary

Codifies the `cairn.mcp.v1` semver freeze for v1.0 (issue #140, parent #32).

- New ADR `docs/design/decisions/0004-mcp-v1-semver-freeze.md` — frozen surface (8 core verbs + prelude + envelope + error model + capabilities registry + `cairn.admin.v1`), additive/breaking rules, deprecation lifecycle, v2 procedure, extension-namespace policy.
- New `docs/site/src/maintainers/mcp-semver-policy.md` — operator-facing summary, links the ADR.
- Capability matrix gains a Stability column.
- Beta readiness gains Gate 10 "contract freeze verified" (renumbers 10–14 → 11–15).
- CI doc names `contract-drift` as the release-blocking v1-freeze gate.
- Brief §8.0.a-bis adds a one-line pointer to the ADR.
- Traceability matrix §8 + §19 cite ADR 0004.

No code, no schema, no CI workflow YAML changes. Enforcement is the existing #98 `contract-drift` job.

## Brief sections

- §19 — v1.0 sequencing: "Semver commitment on MCP surface (`cairn.mcp.v1` frozen)."
- §8.0.a-bis — Contract version naming and v2 mint rule.
- §8.0.a — Extension namespaces (independent freeze cadences).
- §15 — Wire-compat as part of the evaluation harness.

## Invariants touched

- Invariant 6 (capability advertisement) — ADR §3 calls out that the capability **identifiers** are frozen, not the advertised **set** (which `advertise()` decides at runtime). No behavioral change.

## Test plan

- [ ] `mdbook build docs/site` succeeds with no warnings.
- [ ] `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked` clean.
- [ ] `cargo fmt --all --check` clean.
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` clean.
- [ ] `cargo nextest run -p cairn-idl -p cairn-core -p cairn-mcp --locked --no-fail-fast` green (confirms `wire_compat_v1`, `capability_matrix_v1`, `mcp_conformance` still pass — none touched).
- [ ] Manual: open the rendered maintainer page; confirm every link (ADR, brief, capability matrix, beta readiness, CLAUDE.md) resolves.
- [ ] Manual: confirm capability matrix Stability column renders three tiers.
EOF
)"
```

Expected: PR URL returned.

- [ ] **Step 4: Confirm CI starts and the contract-drift job is green**

Run: `gh pr checks --watch`
Expected: all checks pass, including `contract-drift` (which is the
freeze gate enforced by this PR — it MUST be green because no schema or
`advertise()` code was touched).

---

## Spec coverage self-check

| Spec section / requirement | Implementing task(s) |
|----------------------------|----------------------|
| §4.1 — ADR 0004 created | Task 1 |
| §4.1 — maintainer page created | Task 2 |
| §4.2 — SUMMARY.md modified | Task 3 |
| §4.2 — capability-matrix Stability column | Task 4 |
| §4.2 — beta-readiness checklist row | Task 5 |
| §4.2 — ci.md rename | Task 6 |
| §4.2 — brief §8.0.a-bis pointer | Task 7 |
| §4.2 — traceability §8 + §19 | Task 8 |
| §5 — ADR §1–§9 content | Task 1 (all subsections) |
| §6 — maintainer page sections 1–7 | Task 2 |
| §7.1 — capability-matrix table edit + footer | Task 4 |
| §7.2 — beta-readiness gate row + sign-off update | Task 5 |
| §7.3 — ci.md rename with cross-links | Task 6 |
| §7.4 — brief §8.0.a-bis pointer | Task 7 |
| §7.5 — traceability §19 row + §8 note | Task 8 |
| §8 — verification (mdbook, rustdoc, fmt, clippy, nextest) | Task 9 |
| §11 — acceptance-criteria mapping | Captured in PR body (Task 10) |
| §13 — implementation order | Tasks 1 → 10 follow the listed order |

No gaps. No placeholders. Type names are not applicable (docs-only). The
plan stays scoped to the spec; no drive-by refactors.
