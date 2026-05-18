# Issue 115 Electron GUI Alpha Design

## Summary

Issue [#115](https://github.com/windoliver/cairn/issues/115) builds the first
desktop GUI alpha for Cairn. The alpha is a narrow full vertical slice: an
Electron shell and React renderer talk to a Rust GUI backend over local
HTTP/JSON, inspect a fixture vault, render derived graph data, show search and
lint surfaces, and send record edits through a reconcile-shaped backend path.

This issue depends on the `FrontendAdapter` contract from #113, which is now
present in `cairn-core`. The GUI must use backend APIs and reconcile request
types rather than mutating vault files, SQLite, or core state from the
renderer.

## Design Sources

- `docs/design/design-brief.md` §13.2 Electron + Rust + TipTap
- `docs/design/design-brief.md` §13.4 desktop GUI
- `docs/design/design-brief.md` §13.5.c backend to frontend bridge
- `docs/design/design-brief.md` §13.5.d `FrontendAdapter` contract
- Issue #113 `FrontendAdapter` contract and conformance suite
- Issue #115 acceptance criteria

## Scope

### In Scope

- Add a Rust GUI backend crate, `cairn-desktop`, that exposes GUI-facing
  JSON endpoints over localhost.
- Add an Electron + React + Vite renderer under `frontend/desktop-electron`.
- Load a repo fixture vault and expose records, folders, graph edges, search
  results, lint findings, and reconcile review results through the backend.
- Render a usable alpha shell with vault inspector, record/folder views, graph
  view, search panel, lint panel, and reconcile review flow.
- Ensure renderer edits become backend reconcile requests and cannot directly
  mutate DB or vault state.
- Add Rust tests, frontend tests/build, and a fixture smoke test.
- Document Tauri as the slim alternative without implementing it in this PR.

### Out of Scope

- Production desktop packaging, signing, auto-update, installers, or release
  channels.
- Full TipTap rich-text editing. The alpha may use a textarea or plain
  markdown editor component while preserving the future TipTap boundary.
- A complete `cairn ui` CLI command unless implementation discovers a small,
  low-risk hook already available.
- Direct SQLite mutation, direct vault-file writes from Electron, or renderer
  filesystem access.
- Real OS keychain, user-presence, plugin attestation, or daemon-minted
  signed-intent flows.
- Tauri implementation.

## Problem

The repo has the P1 `FrontendAdapter` contract but no concrete desktop GUI
surface. Issue #115 asks for an Electron GUI alpha that can inspect records,
folders, graph edges, search results, lint findings, and reconcile edits
through backend validation. There is currently no `package.json`, no Electron
app, and no desktop-facing Rust backend crate.

The risk is scope sprawl: the design brief describes a production-class
desktop surface, but the first PR should prove the cross-boundary architecture
and acceptance criteria with one real fixture path before packaging or richer
editor work begins.

## Goals

- Prove the default desktop stack from §13.2: Electron shell, React renderer,
  and Rust-owned backend behavior.
- Keep the GUI backend authoritative for data, graph derivation, search, lint,
  and reconcile validation.
- Keep the renderer operational and dense, suitable for repeated inspection
  rather than marketing or landing-page presentation.
- Make the alpha testable without external services or cloud credentials.
- Preserve future room for TipTap, MCP transport, and production `cairn ui`
  wiring.

## Non-Goals

- Shipping a polished desktop app to end users.
- Adding new core graph algorithms for the GUI.
- Replacing the existing CLI, SDK, MCP, or skill surfaces.
- Solving all editor/plugin trust flows from §13.5.c.

## Proposed Approach

### 1. Rust GUI Backend

Create `crates/cairn-desktop` as a small workspace crate responsible for the
desktop alpha backend. It will depend on `cairn-core` and fixture/test helpers
as needed, but it must not move GUI concerns into `cairn-core`.

The backend exposes JSON endpoints for the renderer:

- `GET /health`
- `GET /api/v1/vault`
- `GET /api/v1/records`
- `GET /api/v1/records/{id}`
- `GET /api/v1/folders`
- `GET /api/v1/graph`
- `GET /api/v1/search?q=...`
- `GET /api/v1/lint`
- `POST /api/v1/reconcile/preview`
- `POST /api/v1/reconcile/apply`

For the alpha, these endpoints may use a fixture-backed repository rather than
the full production store path. The important boundary is that data still flows
through the Rust backend. The renderer never opens SQLite, never reads vault
files directly, and never computes authoritative graph edges.

### 2. Fixture Vault

Add `fixtures/desktop-gui-alpha` with a tiny vault model that includes:

- at least three records in two folders
- frontmatter-style metadata for kind, tags, confidence, version, and source
  hash
- wikilink or edge-like references between records
- one lint finding
- one editable record body
- one immutable field example for rejected reconcile tests

The fixture exists to make the vertical slice deterministic. It should be
small enough for unit tests to reason about without snapshot fragility.

### 3. GUI Data Models

Define backend DTOs in `cairn-desktop` for the alpha:

- `DesktopVaultSummary`
- `DesktopFolder`
- `DesktopRecordSummary`
- `DesktopRecordDetail`
- `DesktopGraph`
- `DesktopGraphNode`
- `DesktopGraphEdge`
- `DesktopSearchResult`
- `DesktopLintFinding`
- `DesktopReconcilePreviewRequest`
- `DesktopReconcilePreview`
- `DesktopReconcileApplyRequest`
- `DesktopReconcileApplyResult`

The DTOs should be frontend-friendly, serializable, and separate from internal
core types. That keeps the renderer stable while allowing core contracts to
evolve independently.

Graph edges are derived from fixture/backend record relationships. The graph
view consumes `DesktopGraph`; it does not add graph logic to `cairn-core`.

### 4. Reconcile Flow

The alpha implements a constrained reconcile flow:

1. Renderer loads a record detail with version and backend hash.
2. User edits mutable content in the record view.
3. Renderer posts a preview request with record id, expected version, backend
   hash, and field diff.
4. Backend validates the diff using `FrontendFieldPolicy` and
   `FrontendAdapter`-shaped reconcile types.
5. Backend returns accepted mutable fields or typed rejection reasons.
6. Renderer shows a reconcile review panel.
7. Apply returns a deterministic accepted/rejected result for the fixture.

The alpha may keep fixture apply state in memory for tests. It must explicitly
reject immutable fields and version/hash mismatches so the safety boundary is
visible and covered.

### 5. Electron + React Renderer

Create `frontend/desktop-electron` using Electron, React, Vite, TypeScript,
Tailwind, and a lightweight state layer. The renderer should be a usable app
on the first screen:

- left sidebar for folders and records
- center record detail/editor
- right or lower panels for graph, search, lint, and reconcile review
- command/search input for record search
- graph view using derived backend nodes/edges
- clear loading, empty, and error states

The design should feel like an operational memory workbench: quiet, dense,
scannable, and optimized for inspection.

### 6. Tauri Alternative Documentation

Document the Tauri slim build as an alternative in a short repo doc or the
desktop README. The implementation remains Electron-first per §13.2 and issue
#115.

## API Shape

The backend JSON API is intentionally local and alpha-scoped. Version it under
`/api/v1` so future production transport changes can coexist.

Example record detail response:

```json
{
  "id": "rec-alpha-001",
  "title": "Project memory scaffold",
  "folderId": "folder-core",
  "body": "Markdown body with [[linked memory]].",
  "kind": "skill",
  "tags": ["alpha", "frontend"],
  "version": 2,
  "backendHash": "sha256:fixture-alpha-001",
  "confidence": 0.86,
  "sourceHash": "sha256:source-alpha-001",
  "links": ["rec-alpha-002"]
}
```

Example reconcile preview response:

```json
{
  "accepted": true,
  "targetId": "rec-alpha-001",
  "expectedVersion": 2,
  "mutableDiff": {
    "body": "Updated body"
  },
  "rejectedFields": []
}
```

Immutable-field changes return `accepted: false` with a stable rejection code,
for example `immutable_field_changed`.

## Error Handling

- Backend endpoints return structured JSON errors with stable `code`,
  `message`, and optional `field` properties.
- Renderer displays inline errors in the relevant panel and keeps the rest of
  the app usable.
- Reconcile failures are normal UI states, not crashes.
- Backend startup errors should be explicit about fixture path, port binding,
  and malformed fixture data.
- The frontend API client should convert network and schema failures into
  typed UI states.

## Testing Strategy

### Rust

- Unit tests for fixture loading and DTO construction.
- Unit tests for graph edge derivation from fixture links.
- Unit tests for search and lint responses.
- Reconcile tests for accepted mutable edits.
- Reconcile tests for immutable-field rejection.
- Reconcile tests for version/hash mismatch rejection.

Primary command:

```bash
cargo test -p cairn-desktop
```

### Frontend

- API client tests using mocked `fetch`.
- Component tests for vault loading, record selection, graph rendering state,
  lint/search panels, and reconcile review.
- Build verification for the Electron renderer.

Commands will be defined in `frontend/desktop-electron/package.json`, with
expected names:

```bash
npm test
npm run build
```

If the repo standardizes on Bun during implementation, the package scripts may
be run through `bun` while keeping the script names stable.

### Smoke

Add a smoke test that starts the GUI backend against
`fixtures/desktop-gui-alpha` and verifies the renderer/API path can load vault
summary, records, graph, lint, and reconcile preview data.

The smoke test may be split between Rust integration tests and frontend tests
depending on what keeps the first PR reliable in CI.

## Acceptance Criteria Mapping

- GUI can inspect records, folders, edges, search results, and lint findings:
  covered by `cairn-desktop` endpoints, React panels, and fixture smoke tests.
- GUI edits go through `ReconcileRequest` and backend validation: covered by
  reconcile preview/apply endpoints and immutable-field rejection tests.
- Graph view uses derived edge data and does not add graph logic to core:
  covered by `DesktopGraph` derivation in `cairn-desktop` and a renderer that
  consumes graph DTOs.
- Frontend build/test command: covered by `frontend/desktop-electron`
  `package.json` scripts.
- Adapter-backed edit tests: covered by reconcile tests using
  `FrontendFieldPolicy` and `FrontendAdapter`-shaped request/response types.
- Smoke test against fixture vault: covered by the desktop fixture smoke test.

## Open Decisions Resolved For This PR

- Alpha target: full vertical slice, not scaffold-only.
- Shell: Electron default; Tauri documented only.
- Editor: plain markdown component now, TipTap boundary preserved for later.
- Data source: deterministic fixture vault for the first PR.
- Transport: localhost HTTP/JSON for the first implementation, with room to
  move to MCP or sidecar process wiring later.

## Risks

- **Scope creep:** Keep production packaging, rich editing, and real keychain
  trust flows out of this PR.
- **Backend duplication:** DTOs may duplicate small pieces of core shape, but
  that is acceptable at the GUI boundary. Do not move GUI-specific DTOs into
  `cairn-core`.
- **Frontend dependency weight:** Electron adds a Node supply chain. Keep the
  package focused and document commands clearly.
- **False sense of persistence:** If fixture apply is in-memory, label it in
  code/tests as alpha behavior and do not imply production persistence.

## Verification Checklist

Before opening a PR:

```bash
cargo test -p cairn-desktop
cd frontend/desktop-electron && npm test
cd frontend/desktop-electron && npm run build
```

Run any additional workspace checks required by touched Rust crates if shared
types or workspace manifests change.
