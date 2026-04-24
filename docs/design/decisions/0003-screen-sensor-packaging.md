# ADR 0003 — Screen sensor packaging and opt-in model for P0

- **Status:** Accepted — 2026-04-24
- **Deciders:** Cairn maintainers
- **Issue:** [#150](https://github.com/windoliver/cairn/issues/150)
- **Design-brief sections:** §0 (priority legend), §9 (Sensors), §9.1 (Source
  families), §14 (Privacy & consent), §19 (v0.1 working-set budget), §20
  (Open Questions)

## Context

The P0 binary must work on a fresh laptop, offline, with zero cloud
credentials and a small install footprint (brief §1.a, §52). The screen
sensor is called out as a P0 capability in the §9.1 sensor table but the
brief's original §9.1 row pinned the default capture backend to
[screenpipe](https://github.com/screenpipe/screenpipe) spawned as a
subprocess, with [`xcap`](https://github.com/nashaofu/xcap) +
[`tesseract`](https://github.com/tesseract-ocr/tesseract) as fallback. This
created three tensions:

1. **Footprint.** Screenpipe's runtime + default model set is ~500 MB and
   brings a subprocess + HTTP channel into a P0 whose thesis is "single Rust
   binary + SQLite + markdown". §19 already treated the ~500 MB as opt-in in
   its budget math, but the §9.1 prose still called screenpipe the primary
   path — an inconsistency.
2. **Dependency risk.** Pinning the primary path to a third-party subprocess
   with an evolving HTTP API makes the P0 capture contract fragile.
   Screenpipe itself depends on `xcap` under the hood across every platform
   it ships (`crates/screenpipe-screen/Cargo.toml` pins `xcap = "0.9"` on
   macOS, Windows, and Linux). Cairn can use the same underlying library
   directly without owning the subprocess lifecycle.
3. **Permission + consent.** A screen sensor that is "primary" at P0 implies
   default-on capture, which contradicts §14's consent model and the
   fail-closed rule in §4 invariant 6. The sensor must be present as a
   capability but must never capture until the operator has explicitly
   enabled it and granted OS permission.

Candidate shapes considered:

1. **Screenpipe subprocess primary (status quo of §9.1 prose).** Rejected —
   violates the P0 footprint thesis and the consent defaults.
2. **Screen sensor entirely opt-in at build time** (cargo feature required
   to get any capture). Rejected — contradicts issue #150's guidance that
   screen capture is an "always-present but off-by-default capability" and
   breaks the "it just works on enable" promise harnesses expect.
3. **Screen sensor always compiled in with a small in-binary backend;
   heavy subprocess is an opt-in upgrade.** Accepted — matches §19's budget
   math, matches issue #150's recommendation, and matches how
   screenpipe/Zed/RustDesk/Cap each structure their capture layer (native
   crate primary, heavier add-ons optional).

## Decision

**Adopt shape 3.** The P0 shape is:

- **Screen sensor code is always compiled into the default `cairn`
  binary** as the `screen` module of `cairn-sensors-local`. The default
  backend is [`xcap 0.9`](https://github.com/nashaofu/xcap) (Apache-2) for
  display/window enumeration and frame capture on macOS, Windows, and
  Linux (X11 + Wayland). OCR uses the OS-native engine where available
  (`Vision` on macOS, `Windows.Media.Ocr` on Windows) and falls back to
  [`tesseract`](https://github.com/tesseract-ocr/tesseract) via `leptess`
  on Linux or when the OS engine is disabled.
- **Runtime default is off.** `.cairn/config.yaml` ships with
  `sensors.screen.enabled: false`. The daemon never opens a capture stream
  until the operator runs `cairn sensors screen enable` (or flips the key
  in config), at which point Cairn triggers the OS permission prompt
  (`CGRequestScreenCaptureAccess` on macOS, Graphics Capture consent on
  Windows, portal request on Wayland) and records a consent-journal entry
  (§14) before any frame leaves the sensor.
- **The heavy path is opt-in at build time.** Operators who want
  screenpipe's event-driven accessibility-tree pipeline and bundled
  OCR/VLM models build with
  `cargo install cairn --features screenpipe-runtime`. That feature pulls
  in the screenpipe subprocess lifecycle crate and the HTTP client used
  to subscribe to its `/events` SSE and `/search` API. When the feature
  is compiled in **and** the operator opts in via config
  (`sensors.screen.backend: screenpipe`), `cairn daemon start` spawns the
  subprocess; otherwise the xcap path is used. The feature is not part
  of the default feature set and never downloads models at install time.
- **Capability advertisement, fail closed.** `status.capabilities`
  (§8.0.a flat array) advertises the installed screen backend. Possible
  states:
  - `cairn.sensor.v1.screen.xcap` — default backend compiled and runtime
    ready (but may be disabled or permission-missing; see `screen.state`)
  - `cairn.sensor.v1.screen.screenpipe` — heavy backend compiled
  - neither — screen sensor code was feature-stripped (future use; default
    build always includes xcap)
  A separate `status.sensors.screen.state` field reports the runtime
  state: `disabled` (default), `permission_missing`, `degraded` (e.g.,
  Wayland portal refused), `enabled`. A verb that requires screen capture
  while `state ∉ {enabled}` returns `CapabilityUnavailable` with a
  `screen.*` code (see below) and exit `69` `EX_UNAVAILABLE`. This is the
  standard fail-closed path; the sensor never silently downgrades to a
  narrower mode.

### Reversibility

This ADR pins the **default in-binary backend** (`xcap` + OS-native OCR)
and the **build-time feature name** (`screenpipe-runtime`). It does not
pin the `SensorIngress` contract itself — the trait in `cairn-core`
(brief §4, contract #4) accepts any screen backend. A future ADR can:

- Swap `xcap` for per-platform native bindings (`screencapturekit`,
  `windows-capture`, `libwayshot`) without touching `cairn-core`.
- Add additional optional backends behind new features (e.g., a
  `--features rewind-runtime` path) without reopening this ADR, as long
  as the default stays xcap.
- Reverse the "heavy path is opt-in" choice only by adding a new ADR that
  also revises the §19 binary-size budget and the §14 consent defaults.

The capture backend is intentionally a runtime choice layered on the
build-time feature set: the same binary can be reconfigured between
`xcap` and `screenpipe` (if both features are compiled in) via config,
without reinstall.

## Consequences

### Positive

- Default binary stays close to the §19 budget: screen capability adds
  approximately the xcap crate (~300 KB compiled) + a tesseract language
  data file (~15 MB, Linux only). macOS / Windows pull zero extra shipped
  bytes — OCR uses the OS API. Always-on default stays ~140 MB; heavy
  screenpipe build stays ~640 MB, matching the §19 math.
- Fail-closed posture. Capture cannot happen until the operator enables
  the sensor and the OS grants permission; the consent journal records
  the decision; revoking permission at the OS level surfaces as
  `permission_missing` in `status` without panic.
- Single contract surface. `SensorIngress` stays the only plug point;
  swapping `xcap` ↔ `screenpipe` ↔ future backend is a crate-boundary
  change, not a core change (brief §4 plugin rule).
- Cross-platform coverage without a dead fork. `xcap` is the de-facto
  cross-platform Rust capture crate — screenpipe itself uses it on every
  platform, and it is the only widely-used option with real Wayland
  support. The alternate `scap` crate was abandoned by its upstream
  (`CapSoftware/scap`, last commit 2025-08-05) and only survives as a
  private Zed fork, so adopting it at P0 would mean signing up to own a
  fork — a cost P0 cannot carry.

### Negative

- Two capture backends mean two code paths to keep tested. The
  `cairn-sensors-local` crate carries an integration-test matrix that
  must cover xcap on all three OSes and screenpipe on macOS + Windows
  (screenpipe does not target Linux).
- OS-native OCR quality varies (Apple Vision > Windows.Media.Ocr >
  Tesseract). Operators on Linux or with accessibility-heavy content
  will get measurably worse text than the screenpipe heavy path would
  give them. The ADR does not paper over this — `status.sensors.screen`
  includes a `backend` field so downstream consumers can gate on it.
- Operators running the heavy build must install platform prerequisites
  that screenpipe itself requires (accessibility permission, screen
  recording permission, ffmpeg on some platforms). The install docs
  carry this burden; the default binary does not.
- `xcap`'s upstream bus-factor is thin (~957 stars, one primary
  maintainer as of 2026-04-09). Mitigated by (a) the `SensorIngress`
  trait allowing swap to per-platform natives, and (b) screenpipe's own
  dependency pinning the crate, so if it breaks we are not alone.

## Contract surface (pins brief §9.1 and §8.0.a)

### Error codes (stable, machine-readable)

Returned as the `code` field of `CapabilityUnavailable`; also written to
`.cairn/metrics.jsonl`.

| `code` | Meaning | Exit code |
|---|---|---|
| `screen.disabled` | sensor compiled in but config has `sensors.screen.enabled: false` | `78` `EX_CONFIG` |
| `screen.permission_missing` | OS denied screen recording or accessibility permission | `77` `EX_NOPERM` |
| `screen.backend_unavailable` | requested `backend: screenpipe` but feature not compiled, or subprocess failed to spawn | `69` `EX_UNAVAILABLE` |
| `screen.degraded` | Wayland compositor lacks the screencopy portal, or macOS denied ScreenCaptureKit on the current space | `69` `EX_UNAVAILABLE` |

### Config schema (pins `.cairn/config.yaml`)

```yaml
sensors:
  screen:
    enabled: false            # default — no capture until flipped
    backend: xcap              # xcap (default) | screenpipe (requires
                               # --features screenpipe-runtime)
    ocr:
      engine: auto             # auto | vision | winrt | tesseract | off
    allow_apps: []             # empty = capture any focused app; list
                               # to restrict (§14 per-app allowlist)
    blur_password_fields: true # §14 privacy default
```

### Env precedence (pins `cairn-cli`)

```
CLI flag (--screen-backend, --screen-enable)
  > CAIRN_SCREEN_BACKEND / CAIRN_SCREEN_ENABLED
  > .cairn/config.yaml
  > ~/.config/cairn/config.yaml
  > compiled defaults (= xcap, disabled)
```

### `cairn status` capability advertisement

```json
{
  "capabilities": [
    "cairn.sensor.v1.screen.xcap",
    "cairn.sensor.v1.screen.ocr.vision"
  ],
  "sensors": {
    "screen": {
      "backend": "xcap",
      "state": "disabled",
      "ocr_engine": "vision",
      "permission": "not_requested"
    }
  }
}
```

`state ∈ {disabled, permission_missing, degraded, enabled}`.
`permission ∈ {not_requested, granted, denied, revoked}`.
`backend ∈ {xcap, screenpipe}`.
`ocr_engine ∈ {vision, winrt, tesseract, off}`.

The `capabilities` array is the wire-stable signal (byte-identical per
§8.0.a); the `sensors.screen` object is the human-and-operator-facing
detail block. A caller that needs "is screen capture actually running
right now" gates on `sensors.screen.state == "enabled"`; a caller that
needs "can this binary do screen capture at all" gates on presence of
`cairn.sensor.v1.screen.*` in `capabilities`.

### Install docs expectations

The README and install guide state:

- Default install includes the screen sensor; it is off until you run
  `cairn sensors screen enable`.
- On first enable, your OS will prompt for screen-recording permission.
  Cairn will not retain frames until permission is granted.
- For the heavier screenpipe-based pipeline (accessibility-tree
  extraction, bundled VLM captions), install with
  `cargo install cairn --features screenpipe-runtime` and set
  `sensors.screen.backend: screenpipe`. This adds approximately 500 MB
  of working-set memory and requires screenpipe's own platform
  prerequisites.

## Out of scope

- Frame retention, PII-scrub, and per-app blur policy — defined in §14,
  unchanged by this ADR.
- The recording-to-text batch pipeline (§9.1.a) — separate path, already
  uses `tesseract` directly; unaffected.
- A Tauri-based GUI for toggling the sensor — §13, P1 concern.
- A future `rewind-runtime` or cloud-OCR backend — can be added as a new
  feature + new capability string without reopening this ADR.
