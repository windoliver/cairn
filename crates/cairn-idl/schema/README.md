# Cairn IDL Schema Sources

This directory holds the canonical IDL for the `cairn.mcp.v1` contract. Every
file is a draft-2020-12 JSON Schema. Cairn-specific metadata rides on
`x-cairn-*` vendor keys — the fixed vocabulary lives in
`docs/design/2026-04-23-cairn-idl-design.md`.

## Layout

- `index.json` — manifest: lists every schema file and freezes the
  eight-verb set. Codegen (issue #35) reads this first; integrity tests
  compare it against the filesystem.
- `envelope/` — shared request / response / signed-intent envelopes
  (§8.0.b + §4.2 in the design brief).
- `errors/` — closed error `code` enum (§4.2, §8.0.c, §8.0.d, §13.5.c).
- `capabilities/` — exhaustive P0 capability string enum (§8.0.a).
- `extensions/` — registry of extension namespaces (names + version only).
- `prelude/` — deterministic `status` + fresh `handshake` bodies (§8.0.a).
- `verbs/` — one file per core verb. Each carries `$defs.Args`, `$defs.Data`,
  `x-cairn-verb-id`, `x-cairn-cli`, `x-cairn-capability`, `x-cairn-auth`,
  `x-cairn-skill-triggers`.

## Authoring rules

- Every schema file has `$schema`, `$id`, `title`, and `x-cairn-contract`
  at the top level.
- `$id` follows `https://cairn.dev/schema/cairn.mcp.v1/<relative path>`.
- `x-cairn-contract` is always `cairn.mcp.v1` in this directory.
- Vendor keys outside the fixed vocabulary are not allowed; see the design
  spec for the full table.
- No comments inside JSON. Human rationale belongs in this README or in
  `docs/design/`.
- Every object schema sets `additionalProperties: false` by default. The
  only allowed exceptions are payloads that carry user-defined open maps —
  specifically `ingest.frontmatter` (arbitrary user-authored frontmatter),
  `search.scope`, `retrieve.ArgsScope.scope`, and `forget.ArgsScope.scope`
  (scope filter grammar is open and mirrors `SearchArgs.filters` — §8.0.d).
  These fields set `additionalProperties: true` explicitly so the intent
  is reviewable in diff. Any new open-object exception must be justified
  in this list before merge.

## Validation

`cargo test -p cairn-idl` runs the seven structural checks defined in
`crates/cairn-idl/tests/schema_files.rs`. Deep draft-2020-12 validator
wiring is deferred to issue #35.
