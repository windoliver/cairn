# Migration Guides

Cairn ships forward-looking migration guides for every phase boundary. Each
guide covers what changes between two releases and how to walk a vault from
one to the next.

| Pair | Phase delta | Status |
|------|-------------|--------|
| [v0.1 → v0.2](v0.1-to-v0.2.md) | P0 minimum substrate → P1 continuous learning + SRE | Forward-looking scaffold; concrete §19 deltas pinned. |
| [v0.2 → v0.3](v0.2-to-v0.3.md) | P1 → P2 propagation + collective | Forward-looking scaffold. |
| [v0.3 → v0.4](v0.3-to-v0.4.md) | P2 → P3 evaluation + polish | Forward-looking scaffold. |

The capability deltas in every per-pair guide cross-link back to the
[capability matrix](../../reference/capability-matrix.md) so the
phase-by-phase view stays single-sourced.

## The stability contract

The following surfaces never change shape across releases:

- **The eight verbs** — `ingest`, `search`, `retrieve`, `summarize`,
  `assemble_hot`, `capture_trace`, `lint`, `forget`. New verbs may be added
  under extension namespaces (`cairn.admin.v1.*`, `cairn.federation.v1.*`,
  …); the core eight never disappear.
- **The `cairn status` envelope** — fields and types stay byte-identical
  across an incarnation per brief §8.0.a. New capability codes are added to
  `capabilities[]`; existing ones never change meaning.
- **Vault layout roots** — `sources/`, `raw/`, `wiki/`, `skills/`,
  `purpose.md`, `.cairn/`. New subdirectories may appear under these roots;
  the roots themselves are load-bearing for every release.

## What may change between releases

| Surface | Rule |
|---------|------|
| Capability codes (`cairn.mcp.v1.*`) | Added across phases. Existing codes never change meaning. Removals require one release of deprecation. |
| Config schema (`.cairn/config.yaml`) | Additive. New keys ship with safe defaults. Removals deprecated one release first. |
| CLI flags | Additive same way. |
| WAL state machines | New states append; existing transitions never change semantics ([CLAUDE.md §6.11](https://github.com/windoliver/cairn/blob/main/CLAUDE.md)). |
| SQLite migrations | Append-only, never mutated. Each migration is a new file under `crates/cairn-store-sqlite/migrations/`. |
| MCP wire protocol | Frozen at v1.0; v0.x carries the `cairn.mcp.v1` namespace and may add capability codes without breaking incarnation contracts. |

## Standard upgrade steps

These steps apply to every per-pair migration:

1. Read the per-pair guide.
2. Back up `.cairn/cairn.db` and the vault root.
   - `cairn backup register --vault <path>` once `cairn backup` ships.
   - Until then: cold copy the vault directory (`cp -a vault vault.bak`).
3. Install the new binary side-by-side with the old one.
4. Diff `cairn status --json` output between the two binaries; verify
   advertised capabilities meet the deltas in the per-pair guide.
5. Run `cairn doctor` (once shipped) to verify config keys and vault layout.
6. Cut over: point your harness at the new binary, retire the old one.

## Dual-run pattern

Per brief §16.a and §18.b "First month":

1. Install the new binary side-by-side.
2. Point both at the same vault snapshot (read-only on the old, RW on the
   new).
3. Replay recent traffic through both.
4. Diff `search` / `retrieve` outputs. Tolerance bounds are documented in
   each per-pair guide.
5. Retire the old install only after parity is acceptable for a full cycle.

## Import recipes

Importing existing memory from legacy systems is covered by the
`cairn import` verb (lands in v0.2 per §18.b "First four hours" step 3).
Specific recipes will be linked here as connectors ship:

- Claude Code transcripts → `cairn import --from claude-code` (v0.2+)
- Codex sessions → `cairn import --from codex` (v0.2+)
- Generic markdown vault → `cairn import --from markdown` (v0.2+)

## Unsupported migrations

| Skip pattern | Recommendation |
|--------------|----------------|
| _None populated yet — pre-v0.1._ | _To be filled when phases ship and skip-paths become supported or explicitly rejected._ |
