# Issue 84 Local Sensors Design

## Context

Issue #84 implements the local hook, IDE, terminal, and clipboard sensor adapter slice from the design brief §9 Sensors and §19 v0.1 local substrate. The prerequisites are now on `origin/main`: `CaptureEvent` and sensor-label validation live in `cairn-core`, and the five hook CLI handlers live in `cairn-cli`. The remaining gap is that `crates/cairn-sensors-local` still registers a `SensorIngress` plugin with all capability flags disabled and no event-emission surface.

This design keeps the work scoped to the adapter crate. It does not revise the `SensorIngress` contract, add a daemon, poll the OS clipboard, or wire a full LSP/shell runtime. It provides deterministic adapter APIs that receive already-observed local inputs and emit validated `CaptureEvent`s, which is the safe unit needed before runtime capture loops can call into the crate.

## Goals

- Emit valid `CaptureEvent`s for hook, IDE, terminal, and clipboard observations.
- Enforce per-sensor enablement at the source: disabled sensors return no event and do not hash or retain payload bytes.
- Attach declared local sensor labels that match `cairn-core`'s P0 manifest families.
- Represent sensor authorship by binding `sensor_id` and the `actor_chain` author to the same `snr:` identity for Mode A auto captures, then validating with `CaptureEvent::try_new`.
- Apply budget checks before event creation so over-budget observations are dropped at source.
- Redact or drop sensitive terminal and clipboard data before payload hashing and event construction.

## Non-Goals

- No change to the `SensorIngress` trait surface in `cairn-core`.
- No background daemon, file watcher, shell hook installer, clipboard polling loop, or LSP client runtime.
- No direct persistence to SQLite or the markdown vault. The adapter emits `CaptureEvent`s; `capture_trace` and downstream pipeline code persist them.
- No new cryptographic key-management layer. Keychain-backed signing is already owned by the identity and signed-envelope work. This slice uses the existing P0 event attribution contract: `sensor_id`, sensor label, and sensor-authored `actor_chain`, all validated by core.

## Architecture

Add focused modules under `crates/cairn-sensors-local/src/`:

- `config.rs`: local adapter configuration, with `SensorToggle`s for `hooks`, `ide`, `terminal`, and `clipboard`, plus byte/item budgets.
- `event.rs`: shared event-construction helpers for event IDs, timestamps, payload hashes, payload refs, and sensor-authored actor chains.
- `budget.rs`: reusable budget gate that returns `EmitDecision::Emit` or `EmitDecision::Drop(DropReason)`.
- `policy.rs`: terminal and clipboard redaction/drop policy.
- `hook.rs`, `ide.rs`, `terminal.rs`, `clipboard.rs`: per-sensor observation structs and builders.

`LocalSensorIngress` remains the plugin registration type. Its capabilities become `batches: true`, `streaming: false`, `consent_aware: true`, because this slice provides deterministic batch-style event construction with source-side gating, not a long-running stream.

## Data Flow

Each adapter follows the same flow:

1. Caller passes a typed observation and `LocalSensorConfig`.
2. The adapter checks the per-sensor `enabled` flag.
3. The adapter checks item and byte budgets.
4. Terminal and clipboard adapters run redaction/drop policy on sensitive fields.
5. The adapter computes `payload_hash` from the sanitized payload bytes and constructs a `sources/<family>/<event_id>.json` payload ref.
6. The adapter constructs a `sensor_id` with the canonical local label for that family:
   - `snr:local:hook:cc-session:v1` by default for hook observations.
   - `snr:local:ide:default:v1`.
   - `snr:local:terminal:default:v1`.
   - `snr:local:clipboard:default:v1`.
7. The adapter creates an `actor_chain` containing one `Author` entry with that same sensor identity.
8. The adapter calls `CaptureEvent::try_new`, which validates payload family, mode/family compatibility, sensor-label shape, actor-chain attribution, payload refs, and terminal `context`.

The output is `EmitOutcome`, which either contains one validated `CaptureEvent` or an explicit drop reason. Disabled or dropped observations produce no event.

## Sensor Shapes

Hook observations carry a hook name, optional tool name, and optional session/turn/tool refs. The default hook sensor label is the existing Claude Code canonical label because §19 names Claude Code as the v0.1 reference consumer; the API allows selecting `cc-session`, `codex-session`, or `gemini-session` so later harness wiring does not need a new type.

IDE observations carry a workspace-relative file path and event kind such as `edit`, `diagnostic`, `test`, or `lsp`. They use `CapturePayload::Ide`.

Terminal observations carry command, optional exit code, terminal context, optional stdout/stderr metadata, and sanitized payload bytes. They use `CapturePayload::Terminal`; fresh events must include `TerminalContext` so `CaptureEvent::try_new` passes the write-boundary validation added for #218.

Clipboard observations carry MIME type, byte length, and sanitized bytes. Text snippets are allowed only after redaction. Non-text payloads are emitted as metadata-only captures when the MIME type is allowed; otherwise they are dropped.

## Redaction And Drop Policy

Terminal policy:

- Redact common secret assignments in command/output text, including `TOKEN=...`, `API_KEY=...`, `SECRET=...`, `PASSWORD=...`, and `Authorization: Bearer ...`.
- Drop observations that contain private-key block markers.
- Preserve exit code and terminal context even when command text is partially redacted.

Clipboard policy:

- Redact the same secret patterns for `text/plain`.
- Drop private-key blocks and obvious high-risk credential blobs.
- Drop unsupported MIME types unless the caller explicitly marks the observation metadata-only.

The redacted bytes are what get hashed. This prevents raw sensitive bytes from becoming the durable identity of an event.

## Error Handling

Adapters return a small typed result:

- `EmitOutcome::Emitted(CaptureEvent)`.
- `EmitOutcome::Dropped { sensor, reason }`.

Drop reasons include `Disabled`, `BudgetExceeded`, `PolicyRejected`, and `MalformedObservation`. Core validation errors are surfaced as `MalformedObservation` with the original `DomainError` as source where possible. This keeps source-side decisions explicit without inventing downstream persistence behavior.

## Testing

Tests are added before implementation:

- Enabled hook, IDE, terminal, and clipboard adapters emit `CaptureEvent`s that pass `validate_for_capture`.
- Disabled sensors emit `EmitOutcome::Dropped { reason: Disabled }` and no event.
- Sensor identities use the canonical manifest labels and match the emitted source family.
- Terminal observations with missing `TerminalContext` fail before emission.
- Terminal and clipboard secret patterns are redacted before hashing.
- Private-key clipboard and terminal payloads are dropped.
- Budget limits drop observations before hashing/event creation.
- `LocalSensorIngress::capabilities()` advertises batch and consent-aware support.

The focused verification command is `cargo test -p cairn-sensors-local`. Boundary confidence is covered by `cargo test -p cairn-core capture` when core validation behavior is touched; this design avoids touching core.

## Follow-Up Work

- Runtime loops for shell integration, clipboard polling, and IDE/LSP watching.
- CLI or daemon commands to enable sensors and persist per-sensor config.
- Cryptographic signing integration if the runtime wants to wrap emitted events in signed envelopes before persistence.
- Voice, screen, neuroskill, and recording-batch adapters.
