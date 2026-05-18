# Nexus Sandbox Sidecar Design - Issue #104

**Date:** 2026-05-18
**Issue:** [#104 - Add Nexus sandbox sidecar lifecycle and config profile](https://github.com/windoliver/cairn/issues/104)
**Brief sections:** section 3.0 Storage topology; section 5.6 P1 durable messaging; section 19 v0.2 Nexus sandbox
**Status:** Approved

---

## 1. Scope

Implement the full issue #104 scope in one PR: an opt-in `store.kind: nexus-sandbox`
profile that manages the Nexus sandbox as a derived projection sidecar while preserving
`.cairn/cairn.db` as the sole authority.

This PR adds:

- Nexus sandbox config fields with v0.1 config backward compatibility.
- A runtime sidecar supervisor that starts, health-checks, and stops a configured Nexus
  process.
- `nexus-data/` projection directory handling, always treated as rebuildable derived
  state.
- Status output that distinguishes authoritative SQLite DB health from Nexus sidecar
  projection health.
- Tests for mock process lifecycle, v0.1-to-v0.2 config loading, and degraded sidecar
  status.

Out of scope: Nexus full hub/federation, remote federation status, and replacing SQLite as
the MemoryStore authority.

---

## 2. Architecture

The implementation uses the existing crate boundaries.

| Layer | Location | Responsibility |
|---|---|---|
| Config model | `cairn-core::config` | Pure serde config structs and validation. No I/O. |
| Config loading | `cairn-cli::config` | Existing figment loader, unchanged precedence. |
| Sidecar runtime | `cairn-cli::nexus` | Process spawn/stop, health polling, endpoint parsing, projection path resolution. |
| Status assembly | `cairn-cli::verbs::status` | Load config, probe SQLite authority and optional Nexus projection, render split health. |

The selected approach is supervisor-first with a narrow Nexus client. It avoids a new
`cairn-store-nexus` crate in this PR because the current store dispatch is still scaffolded,
but it creates the runtime boundary that a future store adapter can reuse.

---

## 3. Config Profile

`StoreKind::NexusSandbox` already exists. This design extends `StoreConfig` with an
optional `nexus` profile block:

```yaml
store:
  kind: nexus-sandbox
  nexus:
    data_dir: nexus-data
    command: nexus
    args: ["sandbox", "serve"]
    endpoint: "http://127.0.0.1:8765"
    health_path: "/health"
    health_timeout_ms: 5000
    shutdown_timeout_ms: 2000
```

Defaults are only activated when `store.kind` is `nexus-sandbox`. Existing v0.1 configs
with only `store.kind: sqlite` or no `store` block continue to deserialize to the same P0
defaults. A v0.1 config can migrate in place by changing only `store.kind` to
`nexus-sandbox`; missing Nexus fields are filled from defaults.

`data_dir` is vault-relative unless absolute. The default resolves to
`<vault>/nexus-data`, beside `.cairn/`, matching section 3.0. The config validation rejects
empty command strings, empty health paths, zero timeouts, and any `data_dir` that resolves
inside `.cairn/cairn.db` or `.cairn/` authority paths.

---

## 4. Sidecar Lifecycle

The sidecar supervisor is explicit and testable:

1. Resolve the Nexus profile from `CairnConfig` and the vault root.
2. Create `nexus-data/` when the sandbox profile is active.
3. Spawn the configured command with arguments and environment:
   - `CAIRN_VAULT_DIR=<vault root>`
   - `CAIRN_NEXUS_DATA_DIR=<resolved nexus-data path>`
   - `CAIRN_SQLITE_DB=<vault>/.cairn/cairn.db`
4. Poll `endpoint + health_path` until healthy or timeout.
5. Return `ProjectionHealth::Healthy` or `ProjectionHealth::Degraded`.
6. On shutdown, send a graceful termination signal, wait for the configured timeout, then
   force-kill if needed.

Lifecycle failures never make the SQLite vault unusable. If spawn or health check fails,
the supervisor reports degraded projection state and leaves the authoritative DB path
available to status and existing verbs.

The supervisor never reads or writes Nexus-internal files under `nexus-data/` except to
ensure the directory exists. Nexus owns its internal layout.

---

## 5. Status Model

Status gains an internal health summary that separates authority from projection:

```rust
pub struct StatusHealth {
    pub authority_db: AuthorityDbHealth,
    pub nexus_projection: Option<NexusProjectionHealth>,
}

pub enum AuthorityDbHealth {
    Healthy { path: PathBuf },
    Missing { path: PathBuf },
    Unavailable { path: PathBuf, reason: String },
}

pub enum ProjectionState {
    Disabled,
    Healthy,
    Degraded,
}
```

For `store.kind: sqlite`, Nexus projection status is absent or disabled. For
`store.kind: nexus-sandbox`, status reports the DB authority independently from sidecar
projection health:

- DB healthy, Nexus healthy: normal v0.2 optional projection.
- DB healthy, Nexus degraded: SQLite vault remains usable; projection-dependent features
  can fail closed or fall back to P0 behavior according to their own capability gates.
- DB unavailable: authoritative vault health is failed regardless of Nexus state.

The generated `StatusResponse` should gain a `health` object in the IDL schema and then be
regenerated through `cairn-codegen`; generated Rust and JSON schema files must not be edited
by hand. Human status output renders the same split health in compact text.

---

## 6. Migration Behavior

Migration is config-level and in-place:

- Existing v0.1 vaults keep `store.kind: sqlite` by default.
- Bootstrapped defaults remain P0 SQLite unless explicitly configured otherwise.
- Setting `store.kind: nexus-sandbox` fills missing Nexus fields from defaults and creates
  `nexus-data/` on first sidecar start.
- Disabling Nexus by switching back to `sqlite` leaves `.cairn/cairn.db` readable and does
  not delete `nexus-data/`.
- `nexus-data/` can be deleted at any time. It is derived state and rebuildable from
  `.cairn/cairn.db` by the planned `cairn reindex --from-db` path.

No data migration moves record bodies, frontmatter, edges, WAL state, replay ledger, or
consent journal out of `.cairn/cairn.db`.

---

## 7. Error Handling

Errors are typed at the boundary where they occur:

- Config validation errors stay in `cairn-core::config::ConfigError`.
- Runtime sidecar failures use a CLI/runtime error enum with variants for spawn failure,
  health timeout, invalid endpoint, and shutdown failure.
- Status converts sidecar runtime failures into degraded projection health, not process
  failure, when SQLite authority is still usable.

The only status failures that should produce a failing CLI exit are failures to assemble
the authoritative status response itself, such as invalid config or an unreadable
authoritative DB when the status command is explicitly checking DB availability.

---

## 8. Testing

Tests are written first.

1. Config migration tests:
   - Empty v0.1-style config still loads as `StoreKind::Sqlite`.
   - `store.kind: nexus-sandbox` with no `nexus` block fills default Nexus values.
   - Disabling Nexus by loading `store.kind: sqlite` ignores Nexus defaults for runtime
     activation.

2. Sidecar lifecycle tests:
   - A mock process that exposes a health endpoint reaches healthy state.
   - A mock process that never becomes healthy reports degraded and is stopped.
   - Shutdown escalates to kill when the process ignores graceful termination.

3. Status tests:
   - SQLite store reports no active Nexus projection.
   - Nexus sandbox with healthy mock sidecar reports DB healthy and projection healthy.
   - Nexus sandbox with unavailable sidecar reports DB healthy and projection degraded.

4. Regression checks:
   - `cargo nextest run -p cairn-core -p cairn-cli`
   - `cargo test --doc --workspace`
   - `scripts/check-core-boundary.sh`

---

## 9. Open Constraints

The concrete Nexus HTTP apply/search protocol is intentionally not invented in this PR.
This design defines the process lifecycle, projection directory, health boundary, and
status semantics required by #104. Follow-up search/reindex/apply work can attach calls
behind the same sidecar client without changing the config migration or DB authority rules.
