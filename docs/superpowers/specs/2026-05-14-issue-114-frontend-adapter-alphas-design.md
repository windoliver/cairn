# Issue 114 Frontend Adapter Alphas Design

## Goal

Implement P1 alpha `FrontendAdapter` plugins for Obsidian, VS Code, and Logseq
so each adapter can project backend records into editor-friendly markdown
surfaces, translate frontend edits into backend reconcile requests, and pass the
shared conformance suite from issue #113.

## Design Sources

- `docs/design/design-brief.md` §13.1: Three skins, one vault format.
- `docs/design/design-brief.md` §13.5.a: Obsidian and markdown editors as
  frontends.
- `docs/design/design-brief.md` §13.5.c: backend-to-frontend projection,
  immutable field policy, signed intent, optimistic version checks, quarantine,
  and conformance expectations.
- `docs/design/design-brief.md` §13.5.d: `FrontendAdapter` contract and built-in
  adapters.

## Scope

This slice adds Rust alpha adapters that exercise the existing core contract.
It does not build real Obsidian community plugin, VS Code extension, or Logseq
package artifacts. Those editor-specific packages remain later distribution
work; the alpha crates provide the backend-facing adapter behavior and
registration points needed by Cairn's plugin conformance system.

## Architecture

Create three workspace crates:

- `cairn-frontend-obsidian`
- `cairn-frontend-vscode`
- `cairn-frontend-logseq`

Each crate exports a zero-config adapter type implementing
`cairn_core::contract::frontend_adapter::FrontendAdapter` and
`FrontendAdapterPlugin`, plus a `register` function via the existing plugin
macro pattern. The adapters are untrusted translators: they never write SQLite
or apply edits. `project` returns markdown body, frontmatter, and sidecars;
`reconcile` validates the synthetic alpha edit envelope and returns a
`FrontendReconcileRequest` or a typed rejection.

The three adapters use the same safety behavior because §13.5.c says backend
validation is authoritative for every frontend. They differ only in declared
capabilities and projection shape:

- Obsidian: frontmatter, sidecar files, live plugin, graph view.
- VS Code: frontmatter, sidecar files, optional live plugin, no graph view.
- Logseq: frontmatter, sidecar files, live plugin, graph-oriented outlining.

## Projection

All adapters preserve `StoredRecord.record.body` as the projected body and
include backend-owned metadata in frontmatter:

- `version`
- `kind`
- `visibility`
- `source_hash`

Adapters that support sidecars emit lightweight alpha sidecars:

- `timeline.md`: version summary
- `evidence.md`: evidence confidence fields
- `consent.md`: consent/visibility summary
- `backlinks.md`: target/source/tag backlink metadata for editor graph views
- `live.md`: adapter name, live-plugin status, target hash, and version used by
  live-update clients

Logseq also emits an `outline.md` sidecar to prove the adapter can carry
outline-aware metadata without changing the shared contract.

## Reconcile Policy

The alpha reconcile implementation is deliberately fail-closed and mirrors the
existing conformance fixtures:

- Reject any field that `FrontendFieldPolicy` marks immutable.
- Reject replay sentinel edits.
- Reject expired signed intents.
- Quarantine unknown principals.
- Reject optimistic version mismatches.
- Reject target-hash mismatches.
- Return a `FrontendReconcileRequest` only for mutable fields with a valid
  principal, expected version, target hash, and non-expired intent.

This keeps adapter behavior aligned with §13.5.c while leaving real daemon
signature minting, editor process binding, and quarantine artifact persistence
to future integration issues.

## Testing

Use test-driven development. Add integration tests for each adapter crate that:

- verify capability declarations,
- verify projection frontmatter and sidecar shape,
- snapshot projection output for each adapter,
- register the adapter in a `PluginRegistry`,
- run `run_conformance_for_plugin` and assert all cases pass.

Run focused package tests first, then run the existing core frontend contract
tests to ensure no regression in #113 behavior.

## Out of Scope

- TypeScript editor packages.
- Runtime file watchers or daemon IPC.
- Real keychain-backed signed intent minting.
- SQLite writes, WAL apply, or editor distribution manifests.
