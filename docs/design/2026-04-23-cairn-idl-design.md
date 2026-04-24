# Canonical IDL for Verbs, Envelopes, Capabilities, Enums — Design

**Date:** 2026-04-23
**Issue:** [#34](https://github.com/windoliver/cairn/issues/34) (parent [#3](https://github.com/windoliver/cairn/issues/3))
**Phase:** v0.1 — Minimum substrate (P0)
**Depends on:** [#33](https://github.com/windoliver/cairn/issues/33) (workspace scaffold)
**Toolchain:** Rust 1.95.0, Edition 2024, Cargo resolver 3
**Design source:** §8.0 core verbs · §8.0.a preludes · §8.0.a-bis contract version · §8.0.b envelopes · §8.0.c `RetrieveArgs` · §8.0.d filter DSL · §13.3 commands · §13.5 single-IDL claim · §4.2 signed payload

## Goal

Check in one canonical machine-readable IDL source for the eight core verbs plus the two protocol preludes (`status`, `handshake`), the shared request / response / signed-intent envelopes, the closed error variant set, the exhaustive P0 capability list, and the extension namespace registry — sufficient for issue #35 to generate Rust structs, clap definitions, MCP `inputSchema` payloads, and SKILL.md triggers without hand-maintained duplicate schemas.

## Non-Goals

- Generating Rust / clap / TypeScript / SKILL.md bindings (that is #35 — codegen).
- Authoring extension verb schemas for `cairn.aggregate.v1` / `cairn.admin.v1` / `cairn.federation.v1` / `cairn.sessiontree.v1`. This IDL only registers the **namespaces**; each extension gets its own issue for its verb schemas.
- Full JSON Schema draft-2020-12 conformance validation (loaded by `jsonschema` crate at runtime). #34 ships structural parse + manifest-integrity checks only; deep validator wiring is #35.
- Verb behaviour, storage adapters, sensor capture, codegen output.

## Format Decision — Pure JSON Schema + `x-cairn-*` Vendor Keys

Author every file as draft-2020-12 JSON Schema. Cairn-specific metadata rides on JSON Schema's standard vendor extension channel (`x-*` / top-level unknown keywords).

Rationale:

- **MCP consumes JSON Schema natively.** `tools/list` returns `inputSchema` in JSON Schema; authoring the IDL in the same format means zero translation between IDL and wire.
- **Single parser dep (`serde_json`) — already in the workspace.** No YAML / TOML / DSL parser added at P0.
- **`x-*` vendor keys are the documented escape hatch.** They keep Cairn-specific semantics (CLI flags, Rust enum tags, capability gates, skill triggers) out of the spec-compliant core while staying in a reviewable single file per concept.
- **Reviewable diffs in PR.** One file per verb / envelope / extension registration; changes show exactly which surface moved.
- **YAML's comment advantage does not pay off here.** Schema files are mechanical truth. Human rationale lives in this design doc and in a sibling `README.md` at `crates/cairn-idl/schema/README.md`, never inside schema files.

## Filesystem Layout

```
crates/cairn-idl/
├── Cargo.toml                          # unchanged (scaffold already exists)
├── src/lib.rs                          # gains a single constant: path to the schema root
├── src/bin/cairn-codegen.rs            # unchanged — still fails closed (#35 owns generation)
├── schema/
│   ├── README.md                       # map of files + authoring rules
│   ├── index.json                      # manifest: contract version + every file listed
│   ├── envelope/
│   │   ├── request.json                # §8.0.b request envelope
│   │   ├── response.json               # §8.0.b response envelope (including policy_trace)
│   │   └── signed_intent.json          # §4.2 signed payload (ULID / nonce / sequence / challenge / key_version / chain_parents / signature)
│   ├── errors/
│   │   └── error.json                  # closed `code` enum + typed `data` payload
│   ├── capabilities/
│   │   └── capabilities.json           # exhaustive P0 capability string enum
│   ├── extensions/
│   │   └── registry.json               # namespace names + version + enabler flag (no verb schemas)
│   ├── prelude/
│   │   ├── status.json                 # deterministic status response
│   │   └── handshake.json              # fresh challenge mint
│   └── verbs/
│       ├── ingest.json
│       ├── search.json                 # SearchArgs.filters recursive DSL inline
│       ├── retrieve.json               # RetrieveArgs tagged union (6 targets)
│       ├── summarize.json
│       ├── assemble_hot.json
│       ├── capture_trace.json
│       ├── lint.json
│       └── forget.json                 # mode variants: record (always) / session / scope
└── tests/
    ├── smoke.rs                        # existing — unchanged
    └── schema_files.rs                 # new — structural integrity (see Validation section)
```

## Manifest Contract (`schema/index.json`)

Single source of truth for "what files compose `cairn.mcp.v1`". Codegen (issue #35) reads this file first; the §15 CI wire-compat tests diff its contents against the filesystem.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/index.json",
  "title": "Cairn contract manifest",
  "type": "object",
  "x-cairn-contract": "cairn.mcp.v1",
  "x-cairn-files": {
    "envelope": [
      "envelope/request.json",
      "envelope/response.json",
      "envelope/signed_intent.json"
    ],
    "errors":        ["errors/error.json"],
    "capabilities":  ["capabilities/capabilities.json"],
    "extensions":    ["extensions/registry.json"],
    "prelude":       ["prelude/status.json", "prelude/handshake.json"],
    "verbs": [
      "verbs/ingest.json",
      "verbs/search.json",
      "verbs/retrieve.json",
      "verbs/summarize.json",
      "verbs/assemble_hot.json",
      "verbs/capture_trace.json",
      "verbs/lint.json",
      "verbs/forget.json"
    ]
  },
  "x-cairn-verb-ids": [
    "ingest", "search", "retrieve", "summarize",
    "assemble_hot", "capture_trace", "lint", "forget"
  ]
}
```

Authoring rules:

- `x-cairn-verb-ids` matches §8.0 exactly, underscores, no dash aliases.
- Every path in `x-cairn-files` must exist under `schema/`; every `.json` file under `schema/` (except `index.json` itself) must be listed. Enforced by `tests/schema_files.rs`.
- `$id` version segment (`cairn.mcp.v1`) is the ground truth for the contract version string used across the CLI, MCP `initialize` response, and SDK constants.

## Envelope Schemas

**`envelope/request.json`.** Wraps every verb call. Shape (abridged):

```
{ contract: const "cairn.mcp.v1",
  verb: enum of the eight core verb ids at P0,
  signed_intent: $ref signed_intent.json,
  args: object (per-verb — the verb file supplies the concrete args schema) }
```

The `verb` enum is closed to the eight core verbs at #34; each extension issue extends it additively when its verb schemas land. `args` is constrained by an `allOf` of eight `if/then` arms — each arm matches `verb: const "<v>"` and binds `args` to `../verbs/<v>.json#/$defs/Args`. A JSON Schema validator rejects a request whose `verb` and `args` shapes disagree (e.g. `verb: "forget"` paired with an `ingest`-shaped payload). Codegen (#35) either resolves the `allOf` arms directly or loads each verb file separately — both forms converge on the same per-verb typed shape.

**`envelope/response.json`.**

```
{ contract: const "cairn.mcp.v1",
  verb: same enum as request,
  operation_id: string (ULID),
  status: enum("committed", "aborted", "rejected"),
  data: object | null,
  policy_trace: array of { gate: string, result: enum("pass", "deny", "error"), detail?: string },
  error?: $ref errors/error.json }
```

`policy_trace` is required on every response for a uniform wire shape — the field is always present and may be an empty array on read-only verbs. Codegen emits a `Vec<PolicyGate>` field (never `Option`) so consumers do not have to branch per verb.

**`envelope/signed_intent.json`.** Exact §4.2 shape:

```
{ operation_id (ULID), nonce (base64, 16B), sequence (u64, optional),
  target_hash (sha256:<hex>), scope { tenant, workspace, entity, tier },
  issuer (string), issued_at (RFC3339), expires_at (RFC3339),
  key_version (u32), server_challenge (base64, optional),
  chain_parents (array of operation ids),
  signature (ed25519:<hex>) }
```

Either `sequence` or `server_challenge` must be present (`oneOf` enforces mutual substitutability per §4.2 "Atomic replay + ordering check").

## Error Variants — Closed at P0

**`errors/error.json`.** One closed string enum keyed by `code`:

```
InvalidArgs, InvalidFilter, CapabilityUnavailable, UnknownVerb,
ExpiredIntent, ReplayDetected, OutOfOrderSequence, RevokedKey,
MissingSignature, Unauthorized, NotFound, ConflictVersion,
QuarantineRequired, PluginSuspended, Internal
```

Shape: `{ code: enum, message: string, data: object }` framed as a 15-branch `oneOf`: each branch pins `code: const "<Code>"` and constrains `data` to the matching `$defs/<Code>Data` schema. Variants that carry no structured data (`MissingSignature`, `Internal`) declare an empty `$defs` object. Codegen (#35) lowers the `oneOf` directly to a Rust `enum Error` with per-variant typed payloads; a JSON Schema validator rejects a `code: "InvalidArgs"` paired with data missing `field` or `reason`.

Every variant listed above is referenced somewhere in the design brief (§4.2, §8.0.c, §8.0.d, §13.5.c). Adding a new variant in a later PR is a compatible minor revision; removing one breaks `cairn.mcp.v1`.

## Capabilities — Exhaustive P0 Enum

**`capabilities/capabilities.json`.** A JSON Schema string `enum` listing every capability `status.capabilities` may advertise at P0:

```
cairn.mcp.v1.search.keyword
cairn.mcp.v1.search.semantic
cairn.mcp.v1.search.hybrid
cairn.mcp.v1.retrieve.record
cairn.mcp.v1.retrieve.session
cairn.mcp.v1.retrieve.turn
cairn.mcp.v1.retrieve.folder
cairn.mcp.v1.retrieve.scope
cairn.mcp.v1.retrieve.profile
cairn.mcp.v1.forget.record
cairn.mcp.v1.forget.session          (x-cairn-since: v0.2)
cairn.mcp.v1.forget.scope            (x-cairn-since: v0.3)
```

`x-cairn-since` is a vendor key recording the earliest runtime version that may advertise the capability — codegen uses it to emit correct defaults in `src/mcp/status.rs`; CI wire-compat (§15) asserts a v0.1 runtime never advertises a capability whose `x-cairn-since` is later than the build.

## Extension Registry — Names Only at P0

**`extensions/registry.json`.** Enumerates the four planned extension namespaces so codegen can emit the enum used by `status.extensions`:

```
cairn.aggregate.v1    (x-cairn-since: v0.2, enabler: agent.enable_aggregate)
cairn.admin.v1        (x-cairn-since: v0.1, enabler: operator role)
cairn.federation.v1   (x-cairn-since: v0.3, enabler: enterprise deployment)
cairn.sessiontree.v1  (x-cairn-since: v0.3, enabler: session.enable_tree)
```

No verb schemas for these extensions land in #34. Each extension gets its own issue that adds `schema/extensions/<name>/verbs/*.json` and updates this registry.

## Prelude Schemas

**`prelude/status.json`.** Deterministic body from §8.0.a:

```
{ contract: const "cairn.mcp.v1",
  server_info: { version, build, started_at, incarnation (ULID) },
  capabilities: array of $ref capabilities.json,
  extensions: array of $ref extensions/registry.json#/$defs/namespace }
```

**`prelude/handshake.json`.** Per-call fresh body:

```
{ contract: const "cairn.mcp.v1",
  challenge: { nonce (base64, 16B), expires_at (epoch ms) } }
```

Both schemas are what the CI wire-compat test (§15) diffs against the runtime response; they are frozen under `cairn.mcp.v1`.

## Core Verb Schemas — Shape Highlights

Every `verbs/<name>.json` file exposes `$defs.Args` (the arg shape) and `$defs.Data` (the response `data` shape). Common bindings:

```
x-cairn-verb-id:    <name>                            # matches §8.0
x-cairn-cli:        { command: "<name>", flags: [...] }
x-cairn-capability: "<capability string>" | null       # null = always present
x-cairn-auth:       "signed_chain" | "rebac" | "forget_capability" | ...
x-cairn-skill-triggers:
  positive: [ "use when the user says 'remember that…'", ... ]
  negative: [ "do NOT use for one-off computation results", ... ]
  exclusivity: "prefer this over other remember_* / save_* tools registered in this session"
```

Highlights that deserve explicit design calls:

- **`retrieve.json` — `RetrieveArgs` tagged union (§8.0.c).** Root shape is `oneOf` over six branches, each pinned by `{ target: const "record" | "session" | "turn" | "folder" | "scope" | "profile" }`. Every branch lists its own required fields (e.g. `turn` requires `session_id` + `turn_id`; `profile` requires at least one of `user` / `agent`). Codegen in #35 lowers this to the exact Rust enum quoted in §8.0.c. CLI form binding is per-branch in `x-cairn-cli`.
- **`search.json` — `SearchArgs.filters` recursive DSL (§8.0.d).** A `$defs.filter` schema is `oneOf`:
  - `{ and: [filter, ...] }`
  - `{ or: [filter, ...] }`
  - `{ not: filter }`
  - Leaf `{ field: string, op: string, value: any }` with per-field-type op constraints encoded via `oneOf` groups (`string_ops` / `number_ops` / `boolean_ops` / `array_ops` as in §8.0.d).
  - Unknown `op`-on-field combinations reject at parse time via `oneOf` exhaustion → `InvalidFilter` at runtime.
- **`forget.json` — mode gate.** `oneOf` over `{ mode: const "record", ... }` (always present), `{ mode: const "session", ... }` (`x-cairn-capability: cairn.mcp.v1.forget.session`, `x-cairn-since: v0.2`), `{ mode: const "scope", ... }` (`x-cairn-capability: cairn.mcp.v1.forget.scope`, `x-cairn-since: v0.3`). Runtime checks the capability before dispatch; CI wire-compat (§15) asserts a v0.1 build rejects non-`record` modes.
- **`search.json` `mode` field.** `enum("keyword", "semantic", "hybrid")`, each gated by `cairn.mcp.v1.search.*` capability via `x-cairn-capability`.
- **`summarize.json`, `assemble_hot.json`, `capture_trace.json`, `lint.json`, `ingest.json`** each carry a flat `Args` shape plus `x-cairn-cli` for clap, no discriminated unions. `summarize.persist: true` is tagged `x-cairn-auth: write_capability`; `lint.write_report: true` same.

## Vendor Extension Vocabulary (Fixed Set at P0)

| Key | Applies to | Role |
| --- | --- | --- |
| `x-cairn-contract` | top-level of every schema + index | e.g. `cairn.mcp.v1`; CI checks all files match |
| `x-cairn-verb-id` | verb file root | snake_case canonical name (matches §8.0) |
| `x-cairn-cli` | verb file root + `RetrieveArgs` branches + `forget.mode` branches | `{ command, flags: [{ name, short?, long, value_source }] }` drives clap |
| `x-cairn-capability` | verb root, search mode entries, forget mode entries | capability string from `capabilities.json`, or `null` for "always present" |
| `x-cairn-auth` | verb root or per-branch | string tag from a closed vocabulary (`signed_chain`, `rebac`, `forget_capability`, `write_capability`, `read_only`) |
| `x-cairn-since` | capabilities entries, extension registry entries, verb mode branches | runtime version floor (`v0.1` / `v0.2` / `v0.3`) |
| `x-cairn-skill-triggers` | verb root | `{ positive: [], negative: [], exclusivity: "" }` — feeds SKILL.md + MCP tool description gen in #35 and the skill-install issue |
| `x-cairn-files` | `index.json` only | manifest listing; CI checks against filesystem |
| `x-cairn-verb-ids` | `index.json` only | canonical verb name list |

No key outside this table is allowed in #34. Adding a new vendor key is a spec-level decision made in a follow-up issue.

## Validation at #34

`crates/cairn-idl/tests/schema_files.rs` runs on `cargo test -p cairn-idl` and enforces structural integrity only — deep draft-2020-12 conformance is deferred to #35's codegen pipeline. Checks:

1. **Every schema file parses as JSON** via `serde_json::from_slice`.
2. **Every schema file has `$schema`, `$id`, `title`** at the top level.
3. **`index.json` manifest integrity**: every path listed under `x-cairn-files` exists on disk; every `.json` under `schema/` (except `index.json`) is listed exactly once.
4. **`x-cairn-verb-ids` in `index.json` equals the eight-name set** from §8.0 (exact order: `ingest, search, retrieve, summarize, assemble_hot, capture_trace, lint, forget`). Catches dash-alias drift, renames, and reorderings.
5. **`x-cairn-contract` value equals `cairn.mcp.v1` in every schema file.** Catches partial version bumps.
6. **Every `x-cairn-capability` string referenced by verb / mode / search-mode schemas is a member of `capabilities/capabilities.json`'s enum.** Catches typos and forgotten capability additions.
7. **`$ref` resolvability**: every `$ref` in every schema points at either a `#/$defs/...` local ref or a sibling file under `schema/` that exists. Uses a lightweight walk — no full validator.

`cairn-idl/src/lib.rs` gains one public constant:

```rust
pub const SCHEMA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema");
```

So downstream crates (codegen, CLI, MCP) can locate the schema root without duplicating the path.

`cairn-codegen.rs` stays unchanged — still exits 2 with `"not yet implemented"`. Any caller that shells out to it still cannot silently treat generation as complete.

## Acceptance Mapping

| Issue acceptance criterion | How this design satisfies it |
| --- | --- |
| IDL can express every P0 verb request and response without hand-maintained duplicate schemas | Eight verb files under `schema/verbs/` plus envelope files — one file per concept; #35 codegen consumes the same files the MCP surface serves |
| Capabilities include local keyword, semantic, hybrid search, record-level forget, hooks, local sensors, handshake / status surfaces | `capabilities/capabilities.json` lists every string referenced in §8.0.a; prelude schemas model `status` + `handshake`; hooks and local sensors flow through `ingest` (hook/sensor specifics are §9 and are not verb-shaped) |
| Explicit version fields for `cairn.mcp.v1` and future extension namespaces | Every `$id` encodes `cairn.mcp.v1`; `x-cairn-contract` carries the same string for easy in-memory inspection; `extensions/registry.json` names the four planned extension namespaces with their own `cairn.<name>.v1` tag |
| Run IDL parser / schema validation command once available | `cargo test -p cairn-idl` runs the seven structural checks above; deep schema-validator wiring is #35 |
| Diff generated stubs after generation | Out of scope for #34 (no generator runs); #35 does the diff |
| Review all eight verb names against §8.0 and §13.3 | `x-cairn-verb-ids` list frozen to the exact §8.0 / §13.3 spelling; test 4 fails the build on drift |

## Verification Checklist

Matches the issue's **Verification** list adapted to the "no codegen yet" scope.

- `cargo test -p cairn-idl` — structural schema integrity (seven checks above).
- `cargo test --workspace` — green, no regression in other crates.
- `bash scripts/check-core-boundary.sh` — still green; `cairn-idl` remains standalone and `cairn-core` still lists no `cairn-*` deps.
- `cargo run -p cairn-idl --bin cairn-codegen` — exits `2` with `"not yet implemented"`; stdout empty; covered by existing `tests/smoke.rs`.
- Manual review of `schema/index.json` against §8.0 + §13.3 verb list — exact eight names in the order documented.

## Risks & Open Questions

- **Draft compatibility.** Draft-2020-12 is the MCP spec's pick. If any downstream tool in #35 (or any cloud MCP client) lags on 2020-12 support, per-file `$schema` lets us pin older drafts per file if needed. No accommodation in #34.
- **`$ref` across files.** `schemars` and `typify` handle both inline and cross-file refs. If #35's codegen struggles with cross-file `$ref`, the fix is bundling at generation time — no change to the authored IDL.
- **Vendor key collision.** `x-*` is Cairn-owned by convention; any collision with another tool's vendor keys (e.g. OpenAPI reuses some) is a deliberate Cairn decision recorded here. No external tool is expected to consume these schemas verbatim.
- **`signed_intent` optionality.** Some verbs (e.g. `lint` read-only) can technically omit the signed intent. The P0 choice is **request schema still requires it** — authentication is uniform per §8.0.b. Read-only verbs still require a valid signed envelope (signer just has read-only scope). Revisit only if a P1 harness integration proves this is a real friction point.
- **No `cairn.admin.v1` verb schemas** at P0, even though the extension itself ships at v0.1 (§8.0.a). This is deliberate: `snapshot` / `restore` / `replay_wal` are operator-mode verbs gated by hardware keys; authoring their schemas is a separate exercise. The extension is *registered* in `extensions/registry.json` so codegen and `status.extensions` can advertise it.

## Out of Scope (restated)

- Codegen output in any language (#35).
- Extension verb schemas for aggregate / admin / federation / sessiontree.
- Deep draft-2020-12 conformance validation.
- Authoring SKILL.md file contents — IDL carries the triggers, the skill-install issue generates the file.
- Verb behaviour, storage, sensor capture, hooks.
