# `CaptureEvent` fixtures

Replayable JSON envelopes for the §5.0.a / §9 ingestion pipeline. Each
fixture is a single `CaptureEvent` serialized via `serde_json` from
`cairn_core::domain::CaptureEvent`. The integration test
`crates/cairn-core/tests/capture_event.rs::fixtures_replay_and_revalidate`
parses every `*.json` file in this directory and re-runs `validate()`,
so adding a fixture here exercises the full domain-layer invariants
without further wiring.

The set is intentionally minimal: one fixture per capture mode (auto,
explicit, proactive) and one per source family that downstream pipeline
crates need a worked example for. Adding a fixture is cheap; ripping one
out should require updating the consuming test.

| File | Mode | Family | Why |
|------|------|--------|-----|
| `auto_hook.json` | auto | hook | the canonical Mode A event from a `PostToolUse` harness hook |
| `explicit_cli.json` | explicit | cli | Mode B event from `cairn ingest --kind user --body …` with an agent delegator |
| `proactive_feedback.json` | proactive | proactive | Mode C event the agent emits when the user corrects it |
| `auto_voice.json` | auto | voice | Mode A event from the voice sensor — exercises the per-modality payload variant |
| `auto_screen.json` | auto | screen | Mode A event from the screen sensor with a URL hint |
