# Desktop Packaging — macOS (Electron) Design

**Issue:** #139 — child slice of [P3] Build production desktop packaging for macOS, Linux, and Windows
**Brief:** §16 Distribution and Packaging · §13 UI/UX (Electron primary)
**Date:** 2026-05-25
**Status:** Draft — pending implementation
**Scope:** macOS only, Electron shell only. Linux, Windows, Tauri slim, brew cask, MSI/AppImage/deb, auto-update are explicit follow-ups.

---

## 1. Summary

Ship `Cairn.app` for macOS as a universal (arm64 + x86_64) DMG via electron-builder, with the Rust core (`cairn` binary) bundled as a sidecar inside the `.app` bundle. Code-signing and notarization are wired into CI but gated on optional secrets — builds work for any contributor; signed/notarized artifacts ship when ops provides credentials. User vaults live outside the app bundle; upgrades and uninstall never touch vault data.

## 2. Non-goals

- Linux (`.AppImage`, `.deb`, AUR) and Windows (`.msi`, winget, Scoop) — separate child issues under parent #32.
- Tauri slim variant (§13.2 alternative shell) — separate issue.
- Homebrew cask (`brew install --cask cairn`) — separate issue (requires a `homebrew-cairn` tap repo).
- Auto-update (electron-updater, Sparkle) — v1.0 ships manual updates via brew + GitHub Releases.
- Mobile or hosted SaaS — out of scope per issue #139 itself.

## 3. Design constraints (from CLAUDE.md + brief)

| Constraint | Source | How satisfied |
|---|---|---|
| Harness-agnostic | brief §2 invariant #1 | DMG ships the same `cairn` binary the CLI uses; Electron is just a process supervisor. |
| Stand-alone P0 | brief §2 invariant #2 | First launch fetches embedding model; everything else is offline. No cloud creds required. |
| CLI is ground truth | brief §2 invariant #3 | Electron spawns `cairn serve --port 0 --vault <path>` — a management command listed in brief §13.3. Same binary, same process boundary the CLI users get. Electron does not link the Rust core in-process. |
| Fail closed on capability | brief §2 invariant #6 | If model fetch is skipped or fails, embedding-backed verbs return `CapabilityUnavailable` — no silent fallback. |
| `#![forbid(unsafe_code)]`, no `unwrap` in core | CLAUDE.md §6.2 | Touched Rust returns typed errors; Electron try/catches every IPC boundary. |
| `tracing` for diagnostics; never log record bodies above `debug` | CLAUDE.md §6.6 | Sidecar inherits existing tracing setup. Renderer logs go to `desktop.log`; paths under `~` are redacted to `~` in user-visible dialogs. |
| Vault is user-owned, not app-owned | brief §3 (vault layout) | App-support holds only the registry + caches; vault path is user-chosen and never appears in uninstall `rm` arguments. |

## 4. Architecture

```
GitHub Releases (Cairn-<ver>-universal.dmg, .blockmap)
            ▲
   electron-builder pipeline (CI on tag push v*)
            ▲
   ┌────────┴────────────────────────────┐
   │ /Applications/Cairn.app/            │
   │ ├ Contents/                         │
   │ │  ├ MacOS/Cairn          (Electron main process)
   │ │  ├ Resources/                     │
   │ │  │  ├ app.asar         (renderer bundle)
   │ │  │  ├ bin/cairn        (universal Rust sidecar, lipo'd)
   │ │  │  ├ scripts/uninstall.sh
   │ │  │  └ icon.icns                   │
   │ │  ├ Frameworks/         (Electron, Chromium, helpers)
   │ │  └ Info.plist          (bundle ID com.cairn.desktop, entitlements)
   └─────────────────────────────────────┘
                 │ loopback HTTP (port 0 → ephemeral)
                 ▼
       ~/Library/Application Support/cairn/
                 ├ vault_registry.json     ← list of vault paths
                 ├ models/                 ← BGE/MiniLM ONNX (first-launch fetch)
                 ├ logs/
                 └ desktop.log

       ~/Documents/cairn/  (default; user-chosen at first launch)
                 ├ .cairn/cairn.db
                 ├ purpose.md
                 └ sources/  raw/  wiki/  skills/
```

**Boundary rules.**

- **Sidecar = same `cairn` binary** built from `crates/cairn-cli`, just universal-linked. There is no special "desktop build" of the Rust core. Electron spawns `cairn serve` — a new subcommand wrapping `cairn-desktop`'s axum server.
- **Vault registry** is only an index of paths. Deleting it is recoverable by re-running first-launch.
- **Models dir** is regenerable cache; deletion triggers re-fetch.
- **Vault dir** (user-chosen, outside app-support) is never written to by uninstall and never referenced by the bundle.

## 5. Components

### 5.1 New files

| Path | Purpose |
|---|---|
| `frontend/desktop-electron/electron-builder.yml` | Packaging config. Universal target, DMG output, `extraResources` mounts the Rust binary, signing keyed off env vars, `afterSign` notarize hook. |
| `frontend/desktop-electron/scripts/build-sidecar.mjs` | `beforeBuild` hook. Runs `cargo build --release --target aarch64-apple-darwin -p cairn-cli` + same for `x86_64-apple-darwin`, then `lipo -create` into `resources/bin/cairn`. Idempotent (mtime check). |
| `frontend/desktop-electron/scripts/notarize.mjs` | `afterSign` hook. Calls `@electron/notarize` with `notarytool`. No-op when `APPLE_ID` unset (logs `notarize: skipped (secrets absent)`). On success, staples ticket. |
| `frontend/desktop-electron/electron/sidecar.mjs` | Main-process helper. Resolves `process.resourcesPath + '/bin/cairn'` in production, `target/debug/cairn` in dev. Spawns `cairn serve --port 0 --vault <path>`, parses port from first stdout line (existing bin uses `println!`), exposes via IPC. Owns lifecycle. Tracing logs (stderr) tee'd to `~/Library/Application Support/cairn/logs/desktop.log`. |
| `frontend/desktop-electron/electron/vault-registry.mjs` | Read/write `~/Library/Application Support/cairn/vault_registry.json`. Schema: `{version, vaults:[{id, path, label, last_opened}], active}`. |
| `frontend/desktop-electron/electron/first-launch.mjs` | Onboarding state machine. Pick vault path → `cairn init` if absent → stream `cairn bootstrap --fetch-models --json` to renderer → persist registry. |
| `frontend/desktop-electron/src/onboarding/` | React onboarding screens: vault picker, model download progress, done. Uses existing shadcn/Tailwind. |
| `crates/cairn-cli/src/serve.rs` | New `cairn serve --port <p> --vault <path>` management command wrapping the existing `cairn-desktop` axum server (sibling to existing `mcp.rs`, `repair.rs`). Listed in brief §13.3 as a management command — not a core verb. Electron sidecar spawns it as a subprocess. |
| `.github/workflows/desktop-macos.yml` | New GHA workflow. Triggers on PR (build-only) and tag push `v*` (build + sign + smoke + upload). |
| `.github/workflows/desktop-smoke.yml` | Reusable workflow called by `desktop-macos.yml` after package step. Mounts DMG, installs, launches with `--smoke-test`, verifies backend, exits. |
| `crates/cairn-desktop/tests/upgrade_fixture.rs` | Stage a v0 vault layout, run schema migration entry point, assert vault bytes unchanged. |
| `frontend/desktop-electron/tests/uninstall.test.ts` | Vitest. Mocks app-support + vault dirs, runs uninstall helper, asserts vault dirs untouched. |
| `frontend/desktop-electron/tests/vault-registry.test.ts` | Vitest. Read/write round-trip, missing file, corrupted JSON, unknown version, atomic write. |
| `frontend/desktop-electron/tests/sidecar.test.ts` | Vitest. Port parsing, spawn args, kill-on-quit, restart-on-crash, `ENOENT`. |
| `frontend/desktop-electron/tests/first-launch.test.ts` | Vitest. Onboarding state machine, model-progress event parsing. |
| `scripts/uninstall.sh` | Shipped in `Resources/scripts/`. User-invoked via `Cairn → Help → Uninstall…`. Removes app-support except registry (preserved by default); never touches vault paths. |

### 5.2 Touched files

| Path | Change |
|---|---|
| `frontend/desktop-electron/package.json` | Add `build`/`pack`/`dist` scripts; `electron-builder` + `@electron/notarize` devDeps; `build:` config block pointing at `electron-builder.yml`. |
| `frontend/desktop-electron/electron/main.mjs` | Wire sidecar + registry + first-launch + uninstall menu item. Honor `--smoke-test` flag. |
| `crates/cairn-cli/Cargo.toml` | Add `cairn-desktop` workspace dep for the `serve` subcommand. |
| `.gitignore` | `frontend/desktop-electron/dist/`, `frontend/desktop-electron/resources/bin/`. |
| `docs/site/src/reference/generated/` | Re-run `cargo run -p cairn-cli --bin cairn-docgen -- --write` after `serve` subcommand lands (CLAUDE.md §8). |

## 6. Data flow

### 6.1 First launch (clean machine)

```
open Cairn.app
  → main.mjs: read vault_registry.json → ENOENT
  → first-launch.mjs: show onboarding window
  → user picks ~/Documents/cairn (default) or other path
  → spawn `cairn init <path>` if .cairn/ absent
  → spawn `cairn bootstrap --fetch-models --json`
       ├ stdout JSON lines: {"phase":"download","model":"bge-small","bytes":N,"total":T}
       └ renderer: progress bar
  → write vault_registry.json {version:1, vaults:[{id,path,label,last_opened}], active:id}
  → sidecar.mjs: spawn `cairn serve --port 0 --vault <path>`
  → parse "cairn-desktop listening on http://127.0.0.1:<port>" from stdout (first line; existing bin uses `println!`)
  → renderer: open main window at http://127.0.0.1:<port>
```

### 6.2 Subsequent launch

```
main.mjs → registry.active → sidecar.mjs spawns serve → ready (~200ms)
```

### 6.3 Upgrade (v0.1 → v0.2 via DMG replace)

```
user: drag new Cairn.app to /Applications, replace
  → next launch: same registry, same models, same vault
  → if vault_registry.json schema bumped:
       migrate in-place, write {version:2, vaults:[...]} + back up old as .bak
  → if model format incompatible (version pinned in cairn-embeddings-local):
       re-fetch (rare; ModelCache handles this)
  → vault dir untouched
```

No installer-level upgrade logic — `.app` replacement is atomic at the Finder level. App-support survives because it lives outside the bundle.

### 6.4 Uninstall (user-initiated)

```
user: Cairn → Help → Uninstall… (or manual drag .app to Trash)
  → menu action launches scripts/uninstall.sh via Terminal.app with confirm dialog
  → script reads vault_registry.json, prints "your vaults remain at: <paths>"
  → rm -rf ~/Library/Application Support/cairn/{models,logs,desktop.log}
  → preserves vault_registry.json by default (reinstall remembers vaults)
  → removes registry only with explicit --full flag
  → never touches anything outside ~/Library/Application Support/cairn
  → user manually trashes /Applications/Cairn.app
```

**Vault preservation guarantee.** `uninstall.sh` source is readable and committed; vault paths from the registry are loaded only for `echo` display, never passed to `rm`. Static analysis: `grep '$VAULT' scripts/uninstall.sh` must show no destructive commands. A test asserts this.

### 6.5 Sidecar lifecycle

```
Electron app.whenReady     → spawn cairn serve
Electron before-quit       → SIGTERM sidecar; 5s grace → SIGKILL
Sidecar process exit       → renderer shows "backend disconnected" + restart button
Sidecar crash (non-zero)   → main writes last 100 lines of stderr to logs/desktop.log,
                             auto-restarts once; second crash → user dialog with "Quit"/"Show logs"
```

### 6.6 Model fetch failure recovery

| Failure | Recovery |
|---|---|
| Network error mid-fetch | `ModelCache` writes `.partial` sentinel → resumes on next launch. |
| Checksum mismatch | Delete cache entry, retry once, then surface dialog with manual URL fallback. |
| Disk full | Dialog with current path; offer to change `CAIRN_MODEL_CACHE_DIR`. |
| User skips | Onboarding completes; embedding-backed verbs return `CapabilityUnavailable` until user runs `cairn bootstrap` manually. |

## 7. Error handling

| Failure | Where caught | User-visible | Recovery |
|---|---|---|---|
| Sidecar binary missing from Resources | `sidecar.mjs` spawn → `ENOENT` | Dialog: "Cairn backend missing — reinstall." | Block app; exit. |
| Sidecar crash on start | `sidecar.mjs` reads exit code within 2s | Dialog with last 20 stderr lines + "Copy log" + "Quit" | Auto-retry once with fresh port; second failure → user action. |
| Sidecar healthy but unreachable (firewall) | port-poll timeout 15s | Banner: "Backend started but not responding. Open logs." | Renderer stays on loading screen; backend keeps running. |
| Vault registry corrupted (invalid JSON) | `vault-registry.mjs` parse | Dialog: "Vault index unreadable. We saved a backup at vault_registry.json.bak. Continue with fresh index?" | Rename corrupt file → start fresh → first-launch flow. |
| Vault registry schema unknown (downgrade) | version field > known | Dialog: "Vault index was written by newer Cairn (vN > vK). Upgrade Cairn or pick a different vault." | Block; do not migrate downward. |
| Vault path missing on disk | sidecar `cairn serve` returns `VaultNotFound` | Banner with "Locate…" / "Remove from list" | Update registry on user choice. |
| Model fetch network error | bootstrap exits with `NetworkError` | Onboarding: "Couldn't download model. Retry / Skip / Pre-staged path…" | Skip → defer to first embed call. Pre-staged → user points at local `.onnx`. |
| Model checksum mismatch | bootstrap exits with `ChecksumMismatch` | Onboarding: "Model file corrupted. Retry?" | Delete entry, retry once, surface to user. |
| Disk full mid-fetch | bootstrap exits with `DiskFull` | Onboarding: "Not enough disk space at <path>." | Allow change of `CAIRN_MODEL_CACHE_DIR`. |
| Gatekeeper rejects unsigned build | macOS UI before main.mjs runs | (Outside app control) | DMG ships a README with right-click→Open instructions when unsigned. |
| Notarize failure in CI | `notarize.mjs` throws | CI job fails, no upload | Tag rejected until fixed. |
| `cairn serve` subcommand absent in older sidecar | spawn returns "unknown subcommand" | Dialog: "Backend version mismatch — reinstall." | Block; should never happen because sidecar ships in same bundle. |
| Universal binary missing one arch | `lipo -info` post-build check in CI | CI fail | Pipeline gates on `lipo -info Resources/bin/cairn` listing both arches. |

**Cross-cutting rules.**

- All sidecar IO → `~/Library/Application Support/cairn/logs/desktop.log` (rotated at 10MB, 3 generations).
- Renderer never sees raw stderr; main process redacts paths under `~` to `~` in user-visible dialogs.
- No silent capability downgrade — see brief §2 invariant #6.
- No `unwrap`/`expect` in Rust touched (CLAUDE.md §6.2). Electron `try/catch` at every IPC boundary; uncaught renderer exceptions logged via `electron.crashReporter` (local file only, no network).

## 8. Testing

### 8.1 Unit / Vitest (every PR)

- `vault-registry.test.ts` — round-trip, missing file, corrupted JSON, unknown version, migration v0→v1, atomic write (kill mid-write, assert `.bak` recoverable).
- `sidecar.test.ts` — port parsing, spawn args, kill-on-quit, restart-on-crash, `ENOENT`.
- `uninstall.test.ts` — mocked app-support + vault dirs, run helper, assert vault dirs untouched, app-support cleaned, registry preserved (default) or removed (`--full`). Additionally `grep` the script for `rm.*$VAULT` and assert no match.
- `first-launch.test.ts` — onboarding state machine, model-progress event parsing.

### 8.2 Rust integration / nextest (every PR)

- `crates/cairn-cli/tests/serve_subcommand.rs` — `cairn serve --port 0 --vault <tmp>` binds, prints port to stderr, responds to `GET /health`, exits cleanly on SIGTERM.
- `crates/cairn-desktop/tests/upgrade_fixture.rs` — stage v0 vault, run schema migration, assert vault file bytes unchanged + registry bumped.
- Insta snapshot test on `cairn serve --help` output (CLAUDE.md §6.4).

### 8.3 End-to-end DMG smoke (every macOS build)

`.github/workflows/desktop-smoke.yml`:

```
- build: electron-builder produces dist/Cairn-<ver>-universal.dmg
- mount: hdiutil attach dist/Cairn-<ver>-universal.dmg
- install: cp -R /Volumes/Cairn/Cairn.app /Applications/
- detach: hdiutil detach /Volumes/Cairn
- launch (headless smoke mode):
    open -a Cairn --args --smoke-test --vault $RUNNER_TEMP/smoke-vault
- assertions:
    - app exited 0
    - logs/desktop.log contains "sidecar cairn-desktop listening on http://127.0.0.1:"
    - vault at $RUNNER_TEMP/smoke-vault has .cairn/cairn.db
    - app-support has models/ entry (if model cached in GHA) or skip with explicit log
- cleanup: rm -rf /Applications/Cairn.app
```

The `--smoke-test` flag (new main-process arg) disables window creation, picks an ephemeral vault, runs through the boot path, and `app.quit()`s on first successful `GET /health`. Lives in `main.mjs`; no production code path takes the branch.

### 8.4 Upgrade/uninstall fixture (release-candidate tags only)

Runs only on `v*-rc*` tags (~3 min):

```
- Build current DMG (vN)
- Download previous release DMG (vN-1) from GitHub Releases (GHA cache by tag)
- Install vN-1 → run with --smoke-test → seed vault at fixed path
- Replace .app with vN (cp -R)
- Run vN with --smoke-test pointed at same vault
- Assert: vault byte-identical, registry version bumped if needed,
  log shows "migration: registry v0→v1"
- Run scripts/uninstall.sh non-interactively (--yes --keep-vaults)
- Assert: vault dir bytes still match pre-install; app-support models/ removed;
  registry preserved
```

### 8.5 Code-signing / notarization verification (when secrets present)

Conditional step in `desktop-macos.yml`:

```yaml
- if: env.APPLE_ID != ''
  run: |
    codesign --verify --deep --strict --verbose=2 dist/mac/Cairn.app
    spctl --assess --type execute --verbose=2 dist/mac/Cairn.app
    stapler validate dist/Cairn-*.dmg
```

When secrets absent: skip step, log `signing unverified (no secrets configured)`. Build still completes; artifact uploaded with `-unsigned` suffix.

### 8.6 What we deliberately don't test

- Visual rendering of onboarding UI — separate UI-test issue if desired.
- Cross-version downgrade — blocked by design at registry version check.
- Linux / Windows behavior — separate issues under parent #32.

## 9. Acceptance criteria mapping (from issue #139)

| Issue criterion | How satisfied |
|---|---|
| Installers launch the GUI and connect to the local backend | §8.3 DMG smoke asserts `.app` launches, sidecar binds a port, `/status` responds. |
| Upgrades preserve existing vaults and configs | §8.4 RC fixture installs vN-1, seeds vault, upgrades to vN, asserts vault byte-identical + registry migrated. |
| Uninstall does not delete vault data without explicit user action | §6.4 `uninstall.sh` never references vault paths in destructive commands; §8.1 test greps for `rm.*$VAULT`; §8.4 fixture asserts vault bytes survive uninstall. |
| Run package build smoke tests per OS target where available | §8.3 covers macOS. Linux/Windows in follow-up issues. |
| Run upgrade/uninstall fixture tests | §8.4. |
| Run app launch smoke test | §8.3. |

## 10. Open questions / decisions deferred

- **Bundle ID.** Proposed `com.cairn.desktop`. Confirm naming with maintainer before first signed build (changing later requires new cert + first-launch nag for existing users).
- **Apple Team ID.** Required for notarization. Lives in repo secret `APPLE_TEAM_ID`. Not in scope to acquire — issue can land with the codepath behind the secret.
- **Model URL canonicalization.** `cairn bootstrap --fetch-models` currently fetches from a hard-coded URL in `cairn-embeddings-local`. Whether to mirror that URL to a Cairn-hosted CDN is a separate concern outside packaging.
- **Logs PII.** `desktop.log` may contain user paths. Brief §14 says nothing above `debug` should log record bodies — paths are metadata so they're fine in `info`, but the GUI surfaces logs to users. Acceptable for v1; consider opt-in telemetry redaction later.
- **Fixture-only alpha gap.** Today `cairn-desktop` ships with `DesktopFixture::load_default()` (canned data) — there is no path from a real vault dir into the server's `DesktopRepository`. This packaging spec assumes `cairn serve --vault <path>` opens a real vault. If that path isn't built yet, this slice should either (a) ship with the fixture and surface a "alpha data only" banner in onboarding, or (b) depend on a sibling issue that wires `DesktopRepository::from_vault(path)`. Recommend (a) — keeps this slice purely about packaging and defers vault-binding to its own change. Flag for confirmation when writing the plan.

## 11. Sequencing into PRs

Single PR for the slice (target ~1500 lines incl. tests), but logically:

1. Rust: `cairn serve` subcommand + tests + docgen regen.
2. Electron: sidecar/registry/first-launch + Vitest suite.
3. Packaging: `electron-builder.yml`, `build-sidecar.mjs`, `notarize.mjs`, `uninstall.sh`.
4. CI: `desktop-macos.yml` + `desktop-smoke.yml`.
5. Fixture: `upgrade_fixture.rs` + RC-tagged GHA job.

Each commit individually buildable; PR is mergeable when all five land.
