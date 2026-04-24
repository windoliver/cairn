# ADR 0001 — Default LLM setup for the P0 local tier

- **Status:** Accepted — 2026-04-24
- **Deciders:** Cairn maintainers
- **Issue:** [#146](https://github.com/windoliver/cairn/issues/146)
- **Design-brief sections:** §0 (priority legend), §4.0 (`LLMProvider`), §5.2.a
  (`LLMExtractor`), §20 Q2 (Open Questions)

## Context

P0 ships a single Rust binary that must work on a fresh laptop, offline, with
zero cloud credentials (brief §52). Several verbs and background workers
(`LLMExtractor`, `LLMDreamWorker`, `Consolidate`, `assemble_hot`) can enrich
their output with an LLM, but the substrate — ingest, write, keyword search,
retrieve, forget, vault layout, WAL — must remain fully functional without one
(brief §54).

Two candidate defaults were on the table:

1. **Bundle Ollama** with the `cairn` binary so semantic extraction "just
   works" out of the box. Installer downloads a model on first run.
2. **Ship no bundled LLM.** Provide one OpenAI-compatible `LLMProvider`
   adapter that talks to any endpoint the operator configures (local Ollama,
   LM Studio, vLLM, llama.cpp, LiteLLM, or any cloud provider). When no
   provider is configured, LLM-dependent verbs fail closed; LLM-free paths
   keep working.

## Decision

**Adopt option 2.** The default P0 shape is:

- One `LLMProvider` implementation in-tree: `cairn-llm-openai-compat`, built
  on [`async-openai`](https://github.com/64bit/async-openai) ≥ 0.36 (MIT).
  Honours `OpenAIConfig::with_api_base()` so the same adapter serves OpenAI,
  Ollama (`http://localhost:11434/v1`), LM Studio (`:1234/v1`), vLLM,
  LiteLLM, Groq, Together, and OpenRouter. If strict deserialization rejects
  a non-compliant payload in practice, fall back to the drop-in
  [`async-openai-compat`](https://lib.rs/crates/async-openai-compat) fork.
- **No bundled Ollama, no installer download, no network call at install
  time.** `cairn init` completes with zero credentials.
- **Detection, not adoption.** `cairn status` probes
  `GET http://localhost:11434/api/tags` with a ≤300 ms timeout and reports
  the result under `detected.ollama`. It never auto-configures the provider
  — the operator still writes one config line to enable LLM features.
- **Fail closed when unconfigured.** Any verb that requires an LLM returns
  the typed `CapabilityUnavailable { code, remediation }` error and exits
  with a sysexits-style code. Substrate verbs (`ingest`, `retrieve`,
  `search --mode=keyword`, `forget`, `capture_trace`, `lint`) continue to
  work and exit 0.

### Reversibility

This decision pins the **default P0 adapter** and the **contract for
missing-LLM behavior**. It does not pin the provider model: `LLMProvider`
is a trait (brief §4.0, contract #2), and additional adapters (Bedrock,
Gemini-native, Anthropic-native, bring-your-own) remain legitimate plugins.
Reversing the "no bundled Ollama" choice would require changes to the
installer, the binary size budget (§1.a: "~15 MB, no runtime deps"), and
the offline-install promise — a new ADR, not an in-place revision.

## Consequences

### Positive

- One crate, one set of types, one set of tests — same adapter handles
  every OpenAI-compatible endpoint via `base_url` override.
- P0 binary stays ~15 MB with zero runtime deps (brief §1.a holds).
- Operator keeps full control over which model processes their data; no
  implicit outbound traffic at install time.
- Works unchanged with future OpenAI-compatible providers that haven't
  launched yet.

### Negative

- New users who want LLM enrichment must install Ollama (or configure a
  cloud key) before the `llm`-mode extractor chain engages. The onboarding
  docs bear the burden of making this frictionless.
- OpenAI-compat is not 100% uniform across providers. Known divergences we
  accept and document:
  - **Ollama**: no `tool_choice`, `logit_bias`, `user`, `n`, or logprobs;
    images are base64-only; auth field required-but-ignored. Source:
    `docs.ollama.com/api/openai-compatibility`.
  - **vLLM**: streaming tool-call deltas omit `"type":"function"` on the
    first chunk (vllm-project/vllm#16340). If encountered, swap the base
    client for `async-openai-compat`.
  - **llama.cpp server**: `response_format` JSON mode present; tool-calling
    quality varies by model.
- Verbs that depend on an LLM will hit new users as `CapabilityUnavailable`
  until configured. The error message and `cairn status` output must make
  the remediation obvious.

## Contract surface (pins brief §54)

### Error codes (stable, machine-readable)

Returned as the `code` field of `CapabilityUnavailable`; also written to
`.cairn/metrics.jsonl`.

| `code` | Meaning | Exit code |
|---|---|---|
| `llm.not_configured` | no provider set in config or env | `78` `EX_CONFIG` |
| `llm.provider_unreachable` | configured but host/port refuses, DNS fails, times out | `69` `EX_UNAVAILABLE` |
| `llm.auth_denied` | configured, reachable, 401/403 | `77` `EX_NOPERM` |
| `llm.capability_missing` | reachable, authed, but lacks a required capability (e.g. tool calling on a chat-only model) | `69` `EX_UNAVAILABLE` |

### Config precedence (pins `cairn-cli` behavior)

```
per-verb flag
  > global flag
  > CAIRN_LLM_PROVIDER / CAIRN_LLM_MODEL / CAIRN_LLM_BASE_URL
  > OPENAI_BASE_URL / OPENAI_API_KEY / OLLAMA_HOST
  > .cairn/config.yaml
  > ~/.config/cairn/config.yaml
  > compiled defaults (= no provider)
```

Both `OPENAI_API_BASE` and `OPENAI_BASE_URL` are accepted (community
convention: aider + the `llm` CLI + `async-openai` honour both).

### `cairn status` capability advertisement (wire-stable, brief §8.0.a)

```json
{
  "capabilities": {
    "llm.provider":  {"available": false, "reason": "not_configured",
                      "remediation": ["cairn config set llm.provider ollama"]},
    "llm.chat":      {"available": false},
    "llm.embed":     {"available": false},
    "llm.json_mode": {"available": false},
    "llm.tools":     {"available": false}
  },
  "detected": {
    "ollama": {"base_url": "http://localhost:11434/v1",
               "reachable": true,
               "models": ["qwen2.5:7b", "nomic-embed-text"]}
  }
}
```

Snapshot-tested byte-for-byte (brief §8.0.a). Verbs inspect
`capabilities.llm.*` before running; absent or `false` → fail closed.

### Human error format (stderr, `IsTerminal`-aware colors, `NO_COLOR` honored)

```
error: cairn assemble_hot requires an LLM provider, but none is configured
  capability: llm.chat
  code:       llm.not_configured
  help:       configure a local provider with
                cairn config set llm.provider ollama
                cairn config set llm.model llama3.2
              or see: cairn status
```

### JSON error format (`--json` or `--log-format json`)

```json
{"error":{"code":"llm.not_configured","capability":"llm.chat",
"message":"cairn assemble_hot requires an LLM provider, but none is configured",
"exit_code":78,"remediation":["cairn config set llm.provider ollama"],
"see_also":["cairn status"]}}
```

### MCP adapter

`CapabilityUnavailable` surfaces as
`CallToolResult { isError: true, content: [...] }` with the same `code`
and `remediation` fields. Reserve the JSON-RPC `error` object (server
range `-32000..-32099`) for malformed calls only. Matches MCP
specification 2025-11-25.

## Alternatives considered

1. **Bundle Ollama + a small default model.** Rejected: breaks the ~15 MB
   binary budget, introduces an install-time network dependency, and
   commits the project to shipping a model we haven't audited for every
   deployment. Operators who want this can script `brew install ollama &&
   ollama pull llama3.2 && cairn config set llm.provider ollama`.
2. **Ship `rig-core` as the default client.** Rejected: agent-framework
   abstractions over the raw LLM call blur Cairn's `LLMProvider` contract
   (brief §4.0, #2), the crate is on a fast-moving breaking-change track,
   and Cairn already owns the higher-level concerns (`AgentProvider`, the
   extractor chain) at its own layer.
3. **Ship `genai`.** Rejected: no tool-calling support at time of decision;
   Cairn's P2 `AgentExtractor` needs it.
4. **No LLMProvider at P0 at all.** Rejected: `LLMExtractor` and
   `LLMDreamWorker` are first-class in §5.2.a and §10; removing them from
   P0 would require a brief-level rewrite, not an ADR.

## Implementation checklist (tracks issue #146 acceptance criteria)

- [ ] This ADR committed under `docs/design/decisions/`.
- [ ] `docs/design/design-brief.md` §52, §54, §4.0 `LLMProvider` row, and
      §20 Q2 updated to reference this ADR and the error-code table.
- [ ] `README.md` "Install" section notes that Cairn works offline with
      zero LLM and gives a one-paragraph Ollama quickstart.
- [ ] `.cairn/config.yaml` example in docs shows the
      `llm: { provider, base_url, model, api_key }` shape.
- [ ] Open-issue sweep: any issue implying a bundled model or an installer
      download is updated to cite this ADR.

## References

- `async-openai`: https://github.com/64bit/async-openai
- `async-openai-compat` (fork): https://lib.rs/crates/async-openai-compat
- Ollama OpenAI compatibility: https://docs.ollama.com/api/openai-compatibility
- vLLM OpenAI-compatible server: https://docs.vllm.ai/en/stable/serving/openai_compatible_server/
- MCP specification 2025-11-25: https://modelcontextprotocol.io/specification/2025-11-25
- sysexits.h: https://www.man7.org/linux/man-pages/man3/sysexits.h.3head.html
- `clig.dev` — CLI Guidelines: https://clig.dev/
- Rust CLI Book — error reporting: https://rust-cli.github.io/book/tutorial/errors.html
- Simon Willison's `llm`: https://llm.datasette.io/en/stable/setup.html
- aider config precedence: https://aider.chat/docs/config/options.html
