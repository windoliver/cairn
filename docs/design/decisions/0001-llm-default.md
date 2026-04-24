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
- **No bundled Ollama, no LLM runtime shipped with the binary, no LLM
  network call at install time.** `cairn init` completes with zero LLM
  credentials. (Note: this ADR scopes only the `LLMProvider`. The local
  embedding model path documented in brief §3/§1.a — a ~25 MB `candle`
  model fetched on first run for `sqlite-vec` semantic search — is a
  separate concern tracked by `store.*` / `embedding.*` capabilities, not
  by `llm.*`. A fully offline P0 operator opts out via
  `search.local_embeddings: false` per brief §3.)
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
- OpenAI-compat is not 100% uniform across providers, and the divergence
  list moves over time. **Cairn does not hard-code a per-provider feature
  matrix in the binary.** Instead, the `LLMProvider` adapter probes at
  runtime (capabilities endpoint where available, otherwise a single
  dry-call per feature) and advertises the result by including or omitting
  the `cairn.mcp.v1.llm.chat` / `.embed` / `.json_mode` / `.tools` strings
  in `status.capabilities` — the flat-array surface defined by §8.0.a (see
  the "`cairn status` capability advertisement" section below for the full
  shape; there is no second `capabilities.llm.*` map and no `detected.*`
  field). Observed divergences at the time of writing — useful for
  implementers to sanity-check their probes against, not a contract:
  - **Ollama**: historically rejected or ignored several OpenAI chat-request
    fields (including `logit_bias`, `n > 1`, image URLs, logprobs, and
    parts of the tool-call protocol); auth field required-but-ignored
    (send any non-empty string). Behaviour has been evolving — verify
    against the installed Ollama version, do not trust a static list.
    Source to re-check at implementation time:
    `docs.ollama.com/api/openai-compatibility`.
  - **vLLM**: streaming tool-call deltas have been observed omitting
    `"type":"function"` on the first chunk (vllm-project/vllm#16340),
    breaking strict `async-openai` deserialization. If encountered, swap
    the base client for `async-openai-compat`.
  - **llama.cpp server**: `response_format` JSON mode present; tool-calling
    quality varies sharply by model.
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
  > CAIRN_LLM_PROVIDER / CAIRN_LLM_MODEL / CAIRN_LLM_BASE_URL / CAIRN_LLM_API_KEY
  > OPENAI_BASE_URL  (preferred — matches async-openai's own env name)
  > OPENAI_API_BASE  (legacy alias — community convention in aider, the `llm`
                     CLI, older OpenAI SDKs; read by Cairn explicitly)
  > OPENAI_API_KEY  (credential only — never an explicit-intent signal; see below)
  > OLLAMA_HOST     (explicit-intent signal: implies `provider: ollama`,
                     resolved into `base_url = http://$OLLAMA_HOST/v1`)
  > .cairn/config.yaml   (repo-local)
  > ~/.config/cairn/config.yaml
  > compiled defaults (= no provider)
```

**Cairn reads every env var above explicitly** and normalises the resolved
value into `OpenAIConfig::with_api_base()` before constructing the
`async-openai` client. The CLI never delegates base-URL resolution to the
upstream crate's implicit env handling: `async-openai` 0.36 only reads
`OPENAI_BASE_URL` implicitly, so relying on upstream behaviour for
`OPENAI_API_BASE` would silently fall back to
`https://api.openai.com/v1` whenever `OPENAI_API_KEY` is present, breaking
the "no LLM call leaves the laptop unless configured" invariant. Conflict
resolution: when both `OPENAI_BASE_URL` and `OPENAI_API_BASE` are set,
`OPENAI_BASE_URL` wins; Cairn logs a `warn` once per run.

**Ambient `OPENAI_API_KEY` alone does NOT count as "configured".** A
common operator state is having `OPENAI_API_KEY` exported in the shell for
other tooling (aider, `llm`, OpenAI SDKs) while **not** intending Cairn to
dial OpenAI's cloud. To preserve the §52 invariant that no LLM call
leaves the laptop unless the operator configured a cloud endpoint, Cairn
treats `llm` as configured **only when at least one of the following
explicit-intent signals is present**:

- `--llm-provider` / `--llm-base-url` CLI flag on the verb, or
- `CAIRN_LLM_PROVIDER` or `CAIRN_LLM_BASE_URL` env var set, or
- `OPENAI_BASE_URL` or `OPENAI_API_BASE` env var set, or
- `OLLAMA_HOST` env var set (implies `provider: ollama`), or
- `llm.provider` key present (non-null) in `.cairn/config.yaml` or the
  user config.

A bare `OPENAI_API_KEY` with none of the above yields `llm.not_configured`
(`status.capabilities` omits the `cairn.mcp.v1.llm.*` strings; verbs fail
closed with exit `78`). Cairn logs a one-time `warn` at startup:
`OPENAI_API_KEY detected but no LLM provider configured — ignoring key;
set llm.provider or CAIRN_LLM_BASE_URL to enable LLM features`.

**Explicit-intent signals that name a base URL win over `OLLAMA_HOST`.** If
both `OPENAI_BASE_URL` (or `OPENAI_API_BASE`) and `OLLAMA_HOST` are set,
the OpenAI-style URL wins and `provider` resolves to `openai-compatible`;
`OLLAMA_HOST` is ignored with a one-time `warn`. `OLLAMA_HOST` alone is
sufficient — it both fixes the provider and resolves the base URL.

**Test matrix to commit alongside the adapter** (golden cases for
`cairn-cli` config resolution):

| `OPENAI_API_KEY` | `OPENAI_BASE_URL` | `OPENAI_API_BASE` | `OLLAMA_HOST` | `llm.provider` (yaml) | Expected outcome |
|---|---|---|---|---|---|
| set | unset | unset | unset | unset | `llm.not_configured`, warn-log, exit `78` on LLM verbs |
| unset | unset | unset | `localhost:11434` | unset | configured, `provider: ollama`, `base_url: http://localhost:11434/v1` |
| set | `http://localhost:1234/v1` | unset | unset | unset | configured, OpenAI-compat to LM Studio |
| set | `https://api.openai.com/v1` | unset | `localhost:11434` | unset | configured to OpenAI; `OLLAMA_HOST` ignored, warn-log |
| set | unset | `http://gateway/v1` | unset | unset | configured, OpenAI-compat to gateway via legacy alias |
| set | `http://a/v1` | `http://b/v1` | unset | unset | configured to `a/v1`; `OPENAI_API_BASE` ignored, warn-log |
| unset | unset | unset | unset | `ollama` (no base_url) | configured if `model` set; otherwise `llm.not_configured` |

### `cairn status` capability advertisement

The existing §8.0.a contract is honoured **unchanged**: `status.capabilities`
is a flat array of `cairn.mcp.v1.*` strings, byte-identical across a daemon
incarnation, compared by CI wire-compat tests. This ADR extends the
vocabulary with four LLM-feature strings, advertised only when the
corresponding feature is actually executable:

```
cairn.mcp.v1.llm.chat         — provider configured, reachable, chat usable
cairn.mcp.v1.llm.embed        — embeddings endpoint reachable + usable
cairn.mcp.v1.llm.json_mode    — provider accepts response_format: json_schema
cairn.mcp.v1.llm.tools        — provider accepts tool_choice / tools array
```

Inclusion rules (enforced by the §8.0.a invariant "`status.capabilities`
matches the verbs/modes the runtime will actually execute"):

- String absent from the array ⇒ any verb relying on that capability returns
  `CapabilityUnavailable` with a `code` from the table above.
- No second capability surface, no `providers.*` field, no object-shaped
  capability map. Operator-facing detail (detected local Ollama, current
  `base_url`, remediation hints, error-code strings) is out-of-band:
  returned under a separate **non-wire-stable** `provider_hints` field in
  `cairn status --json` (explicitly not part of the `capabilities` contract
  and not snapshot-tested), and under the `error.*` payload of
  `CapabilityUnavailable` when a verb fails closed:

```jsonc
// cairn status --json  (non-wire-stable hints, for humans and --verbose)
{
  "contract": "cairn.mcp.v1",
  "server_info": { "version": "0.1.0", …, "incarnation": "01HQZ…" },
  "capabilities": [
    "cairn.mcp.v1.search.keyword",
    "cairn.mcp.v1.search.semantic",
    "cairn.mcp.v1.retrieve.record",
    /* cairn.mcp.v1.llm.*  absent — no provider configured */
    …
  ],
  "extensions": [],
  "provider_hints": {               // NOT wire-stable. Advisory only.
    "llm":   { "configured": false, "code": "llm.not_configured",
               "remediation": ["cairn config set llm.provider ollama"] },
    "ollama_local": { "reachable": true, "base_url": "http://localhost:11434/v1",
                      "models": ["qwen2.5:7b","nomic-embed-text"] }
  }
}
```

CI wire-compat (§8.0.a tests a–c) asserts byte-identity only over the
`contract` + `server_info.incarnation`-stripped + `capabilities` +
`extensions` fields. `provider_hints` is deliberately mutable: it can
evolve per release.

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

`CapabilityUnavailable` is a **tool execution error**, not a protocol
error: it surfaces as `CallToolResult { isError: true, content: [...] }`
with the same `code` and `remediation` fields that the CLI returns.
Protocol-level JSON-RPC errors (MCP specification 2025-11-25) follow the
standard JSON-RPC mapping:

- `-32600` *Invalid Request* — malformed JSON-RPC envelope.
- `-32601` *Method Not Found* — unknown MCP method.
- `-32602` *Invalid Params* — tool invoked with schema-invalid arguments.
- `-32603` *Internal Error* — unexpected Cairn-internal failure while
  handling the request (panic, WAL corruption, unhandled error variant).
- `-32000..-32099` *server-defined range* — reserved for Cairn-specific
  protocol-level failures only (e.g. capability negotiation mismatch).

"Configured feature currently unavailable" never lands here — it is a
domain outcome of a well-formed tool call and belongs in
`CallToolResult.isError`.

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
