# Issue 106 OpenAI Semantic Degradation Design

## Context

Issue #106 implements the P1 provider-selection slice from design brief
§19 v0.2 backend options and §5.1 read path. The current `origin/main` already
contains an opt-in `cairn-embeddings-openai` crate, `cairn-cli` `openai`
feature gating, provider readiness checks, OpenAI-native embedding model kinds,
mock wire-format tests, status/search capability tests, and credential redaction
coverage.

The remaining gap is contract wording: issue #106 and design brief §3.0 / §19
name `semantic_degraded=true` for transient external-provider outage, while the
current search response exposes richer `degraded_legs` entries. This design keeps
the richer diagnostics and adds the explicit compatibility flag.

## Recommendation

Add an optional `semantic_degraded` boolean to the `search` response data.
Surface it as `true` when a search result was served despite a transient
semantic-provider outage. Leave it absent for healthy results and for
fail-closed capability errors.

This preserves the current search API contract and satisfies the literal issue
acceptance criterion without replacing `degraded_legs`.

## Architecture

The implementation should stay behind existing boundaries:

- `cairn-idl/schema/verbs/search.json` defines the new optional response field.
- Generated search types carry `semantic_degraded: Option<bool>`.
- `cairn-core::verbs::search::SearchOutcome` carries a boolean derived from
  existing internal search degradation information.
- `cairn-cli` maps `SearchOutcome.semantic_degraded` into the generated response
  data.

No new provider registry or parallel status mechanism is needed. The OpenAI
adapter remains opt-in through `cairn-cli --features openai`, and local candle
embeddings remain the default.

## Semantics

`semantic_degraded` means:

- `true`: the requested search returned results using a fallback or surviving
  leg after a transient semantic provider outage.
- absent: no semantic provider outage affected this response.

The field should not be used for:

- compile-time feature absence
- missing credentials
- unsupported provider/model configuration
- local candle model missing
- an unadvertised semantic or hybrid capability

Those cases must continue to fail closed with `CapabilityUnavailable` and no
successful search response.

For the first implementation, the flag can be derived from the existing
degradation model when a `DegradedLeg::Semantic` entry carries a transient
provider-outage reason. If the current internal enum lacks that reason, add the
narrowest reason needed rather than broadening all semantic degradation into
`semantic_degraded=true`.

## Testing

Use TDD for each behavior:

- A generated-envelope or CLI serialization test proves healthy search output
  omits `semantic_degraded`.
- A core search test proves a transient semantic-provider outage sets
  `SearchOutcome.semantic_degraded=true` while preserving `degraded_legs`.
- A JSON output test proves `semantic_degraded: true` appears in the wire data
  for the degraded success path.
- Existing OpenAI readiness tests continue to prove feature-off, missing-key,
  whitespace-key, and unsupported-model cases fail closed.
- Existing credential-redaction tests continue to prove API keys are not written
  into output or debug text.

## Non-Goals

- Do not add Cohere, Voyage, Ollama, or LiteLLM routing in this slice.
- Do not change v0.1 local search defaults.
- Do not replace `degraded_legs`.
- Do not persist provider credentials in the vault.
