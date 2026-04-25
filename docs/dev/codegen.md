# cairn-codegen — maintainer guide

`cairn-codegen` is the maintainer-time binary that re-emits the four artefact
bundles derived from the IDL under `crates/cairn-idl/schema/`:

| Tree | Purpose |
|---|---|
| `crates/cairn-core/src/generated/` | SDK Rust types — verb registry, per-verb `Args` / `Data`, common types, errors enum, prelude responses. |
| `crates/cairn-cli/src/generated/`  | `clap::Command` subcommand tree (`pub fn command()`). |
| `crates/cairn-mcp/src/generated/`  | MCP tool declarations + canonical JSON schemas (cross-language artefact). |
| `skills/cairn/`                    | `SKILL.md`, `conventions.md`, `.version` — the shippable Cairn skill. |

## When to run

Whenever any file under `crates/cairn-idl/schema/` changes, or after editing
emitter logic in `crates/cairn-idl/src/codegen/`.

## How to run

```bash
cargo run -p cairn-idl --bin cairn-codegen
```

This rewrites every artefact under the four trees. Commit the diff in the
same PR as the IDL or emitter change.

## What CI does

The `codegen-drift` job (`.github/workflows/ci.yml`) runs:

```bash
cargo run -p cairn-idl --bin cairn-codegen -- --check
```

`--check` compares emitter output to the on-disk bytes; any difference exits
non-zero. The error message lists the first 20 differing files. Fix:

```bash
cargo run -p cairn-idl --bin cairn-codegen
git add -A
git commit -m "regenerate codegen artefacts"
```

## Adding a new verb

1. Drop the verb file under `crates/cairn-idl/schema/verbs/<id>.json` with
   the standard envelope: `x-cairn-contract`, `x-cairn-verb-id`,
   `x-cairn-cli`, `x-cairn-skill-triggers`, `x-cairn-auth`,
   optional `x-cairn-capability`, plus `$defs.Args` and `$defs.Data`.
2. Append the new path to `crates/cairn-idl/schema/index.json` under
   `x-cairn-files.verbs` AND `x-cairn-verb-ids`.
3. Run `cargo run -p cairn-idl --bin cairn-codegen`.
4. Run `cargo nextest run -p cairn-idl` to confirm parity / determinism /
   snapshot tests still pass.
5. If the snapshot tests fail because the new verb appears in
   `verbs/mod.rs`, accept the snapshot diff: `cargo insta review`.
6. Commit everything in a single PR.

## Adding new IR / emitter logic

The pipeline is `loader → ir → emit_*`:

1. **Loader changes** when adding new structural validation. Update the test
   suite in `crates/cairn-idl/tests/codegen_loader.rs`.
2. **IR changes** when adding a new lowering rule. The lowering table is in
   `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md` §4.2 — keep
   that table and the IR in sync.
3. **Emitter changes** affect specific output trees. The snapshot tests
   (`crates/cairn-idl/tests/codegen_snapshot.rs`) lock down byte-level
   output; review with `cargo insta review` when the change is intentional.

## Determinism

Three rules every emitter obeys:

- Stable iteration (`BTreeMap`, sorted `Vec`, never `HashMap`).
- Canonical JSON via `cairn_idl::codegen::fmt::write_json_canonical` (sorted
  keys, two-space indent, trailing newline).
- Atomic file writes via `tempfile::NamedTempFile::persist`.

The `codegen_determinism` test runs codegen 5× into fresh tempdirs and
asserts byte-equal trees — any leak (e.g. accidental `HashMap` iteration)
fails CI.

## Filter recursion bound

The `Filter` enum in `crates/cairn-core/src/generated/verbs/search.rs` is
collapsed from the IDL's unrolled `filter_L0..L8` into a single recursive
type. The depth bound stays a JSON-Schema assertion only — the runtime
depth check lives in the search verb implementation (#9 / #63). A
hand-crafted deeply-nested `Filter` value bypasses the schema; the verb
must reject it.

## Cross-references

- Spec: `docs/superpowers/specs/2026-04-24-cairn-codegen-design.md`
- Brief sections this PR implements: §8.0 (four surfaces), §8.0.a
  (handshake/status preludes), §8.0.b (envelope), §8.0.c (`RetrieveArgs`),
  §13.5 (language split), §18.d (Cairn skill).
- Adjacent open issues: #36 (broader contract-drift gates), #59 (CLI
  command tree consumer), #9 (verb impls), #63 (`RetrieveArgs` semantics),
  #64 (MCP transport), #70 (skill-install validation), #98 (wire compat).
