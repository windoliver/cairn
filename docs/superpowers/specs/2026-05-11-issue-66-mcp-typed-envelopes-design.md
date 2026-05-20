# Issue #66 — MCP typed verb envelopes and responses

- **Issue:** [#66](https://github.com/windoliver/cairn/issues/66)
- **Parent epic:** [#10](https://github.com/windoliver/cairn/issues/10)
- **Phase / priority:** v0.1 minimum substrate · P0
- **Brief sections:** §8.0 Core verbs, §8.0.a status and handshake, §8.0.b shared envelope, §15 wire compatibility
- **Date:** 2026-05-11

## 1. Goal

Map MCP `tools/call` requests for the eight core `cairn.mcp.v1` verbs into
generated typed verb arguments and canonical response envelopes.

The current MCP adapter already handles stdio lifecycle, `tools/list`,
capability-aware status, prelude `handshake`, graph extension tools, and a
wired `search` path. The remaining issue #66 gap is that core verb calls still
return either bare `SearchData` or ad hoc text errors instead of the shared
§8.0.b response envelope with `verb`, `status`, `operation_id`, typed `data`
or typed `error`, and `policy_trace`.

After this work, MCP callers should be able to parse the first text content
item of any known core verb result as `cairn_core::generated::envelope::Response`.
That gives the MCP surface the same machine-readable operation ID and error
shape as CLI and SDK.

## 2. Decision

MCP tool inputs stay as per-verb `Args` schemas. The MCP adapter maps every
known core tool call into generated `RequestVerb` and `RequestArgs` values,
then hands those typed values to the runtime path that owns any required
`SignedIntent` construction or verification.

This preserves the existing MCP UX: model clients call a tool named `search`
with `{query, mode, ...}`, not a redundant envelope containing `verb:
"search"`. The canonical response envelope remains the machine-readable output
contract. The adapter must not invent fake signatures to satisfy the request
schema; if a signed runtime path is unavailable, the known verb returns a typed
aborted response instead of bypassing identity checks.

## 3. Current base

`origin/main` already includes:

- `crates/cairn-mcp/src/handler.rs` with `CairnMcpHandler`, stdio `rmcp`
  integration, status advertisement, prelude `handshake`, graph tools, and a
  store-wired `search` dispatcher.
- `crates/cairn-mcp/src/generated/mod.rs` with one `ToolDecl` per core verb,
  per-verb input schemas, and auth/capability override metadata.
- `cairn_core::generated::envelope::{Request, RequestArgs, RequestVerb,
  Response, ResponseData, ResponseStatus, ResponseVerb}` generated from the
  IDL.
- `crates/cairn-sdk/src/transport.rs` with SDK-side validators and response
  projection helpers that can serve as parity references.
- `crates/cairn-cli/src/verbs/envelope.rs` and `crates/cairn-cli/src/verbs/signed.rs`
  with canonical CLI response construction helpers.

Baseline verification on `codex/issue-66`:

- `cargo check -p cairn-mcp` passes.

## 4. Scope

### In scope

- Add adapter-owned helpers that map known core MCP tool names to typed
  request/response verb enums.
- Deserialize `CallToolRequestParams.arguments` into generated per-verb args
  with schema-equivalent validation where direct Rust construction bypasses
  generated checks.
- Convert successful `search` dispatch into a full `Response` envelope rather
  than bare `SearchData`.
- Convert malformed core-verb payloads into typed `InvalidArgs` rejected
  response envelopes.
- Convert unsupported but known core verbs into typed `Internal` aborted
  response envelopes that preserve the requested `verb` and a fresh
  `operation_id`.
- Preserve `operation_id` on every committed, rejected, or aborted core-verb
  response emitted through MCP.
- Keep existing capability rejection behavior, but return the typed
  `CapabilityUnavailable` envelope shape instead of plain text.

### Out of scope

- Replacing MCP tool input schemas with the full request envelope.
- Implementing the remaining unwired verb runtimes.
- Changing graph extension tools or prelude `handshake` behavior.
- Adding new IDL fields for MCP request IDs.
- Changing capability advertisement rules from issue #53.
- Reworking `rmcp` transport lifecycle from issue #64.

## 5. Architecture

Add a focused MCP envelope adapter module:

```text
crates/cairn-mcp/src/verb_envelope.rs
  -> core_verb_for_tool(name) -> Option<RequestVerb>
  -> response_verb(RequestVerb) -> ResponseVerb
  -> parse_args(RequestVerb, arguments) -> Result<RequestArgs, Response>
  -> committed(ResponseVerb, ResponseData, policy_trace) -> Response
  -> rejected_invalid_args(ResponseVerb, field, reason) -> Response
  -> rejected_capability_unavailable(ResponseVerb, capability) -> Response
  -> aborted_internal(ResponseVerb, message) -> Response
  -> call_result_from_response(Response) -> CallToolResult
```

`handler.rs` keeps transport routing. It should only decide whether a call is
a prelude tool, graph extension tool, unknown tool, wired `search`, or a known
core verb that is not wired yet. Envelope construction and serialization move
out of the transport method so tests can exercise them directly.

The helper should serialize canonical `Response` JSON as the first
`Content::text` item. For committed envelopes use `CallToolResult::success`;
for rejected or aborted envelopes use `CallToolResult::error`. This keeps MCP
`isError` meaningful while preserving the typed Cairn payload inside content.

## 6. Request Mapping

Known core tools are exactly the `TOOLS` entries generated from the IDL:

- `ingest` -> `RequestVerb::Ingest`
- `search` -> `RequestVerb::Search`
- `retrieve` -> `RequestVerb::Retrieve`
- `summarize` -> `RequestVerb::Summarize`
- `assemble_hot` -> `RequestVerb::AssembleHot`
- `capture_trace` -> `RequestVerb::CaptureTrace`
- `lint` -> `RequestVerb::Lint`
- `forget` -> `RequestVerb::Forget`

For issue #66, MCP calls continue to provide the `args` object only. The
adapter deserializes that object to the generated args type for the selected
verb and wraps it in `RequestArgs`. Full signed-intent construction and
verification remain owned by the verb execution path. Where a runtime is still
stubbed or cannot safely construct a signed envelope from configured MCP
identity context, the adapter returns an aborted typed envelope.

Malformed args reject before dispatch. The error body must be:

```json
{
  "code": "InvalidArgs",
  "message": "invalid args: <field>: <reason>",
  "data": { "field": "<field>", "reason": "<reason>" }
}
```

Unknown tool names stay outside the typed core verb path and may remain MCP
plain `isError` responses listing available tools.

## 7. Response Mapping

Every known core verb call returns a JSON object matching
`cairn_core::generated::envelope::Response`.

Committed `search` response:

- `contract = "cairn.mcp.v1"`
- `verb = "search"`
- `status = "committed"`
- `operation_id` is a fresh ULID until the lower store/WAL path supplies one.
- `policy_trace` comes from `SearchOutcome.policy_trace`.
- `data` is `ResponseData::Search(SearchData)`.
- `target = null` / omitted.

Rejected errors:

- `InvalidArgs`, `InvalidFilter`, and `CapabilityUnavailable` use
  `status = "rejected"` and the IDL-defined `error.data` shape.
- `policy_trace` is present, even if empty.
- `operation_id` is present.

Aborted errors:

- Store or transport-adjacent execution failures use `status = "aborted"` and
  `error.code = "Internal"` unless a more specific existing IDL error applies.
- Known but unwired verbs use this path instead of `dispatch_stub` text.

All response serialization must avoid non-finite floats in search hits or score
explain fields, matching the CLI and SDK envelope sanitization precedent.

## 8. Capability And Auth Semantics

This issue does not create new policy decisions. It preserves existing
capability gates:

- `search.mode` is checked against the same store/config capabilities used by
  `status_response`.
- `search.explain = true` must continue to fail closed if
  `cairn.mcp.v1.policy_trace` is not advertised.
- `retrieve`, `forget`, and other per-mode capability checks remain tied to
  the runtime that implements them. Until their MCP dispatch is wired, they
  return typed `Internal` aborted envelopes rather than advertising success.

Auth override metadata in `ToolDecl` remains a declaration surface. Issue #66
does not add a new authorization engine inside the adapter.

## 9. Testing Strategy

Use TDD. Add failing tests before implementation:

1. `search` success over the full MCP wire protocol returns `isError` absent
   or false and the first text content parses as `Response` with
   `verb=search`, `status=committed`, `operation_id`, `policy_trace`, and
   typed `SearchData`.
2. Malformed `search` arguments over MCP return `isError=true` and the first
   text content parses as `Response` with `status=rejected`,
   `verb=search`, and `error.code=InvalidArgs`.
3. Store-vector capability rejection for semantic search returns
   `isError=true` and a parseable `CapabilityUnavailable` response envelope
   preserving `error.data.capability`.
4. A known unwired core verb such as `retrieve` returns `isError=true` and a
   parseable `Internal` aborted response envelope with `verb=retrieve` and a
   valid `operation_id`.
5. Direct unit tests for name-to-verb mapping cover all eight core verbs and
   reject non-core tools like `handshake` and `graph.neighbours`.

Focused verification:

- `cargo test -p cairn-mcp search_tool handler_rejection smoke`
- `cargo test -p cairn-mcp verb_envelope`
- `cargo check -p cairn-mcp`

Final verification should scale to touched crates:

- `cargo fmt --all -- --check`
- `cargo clippy -p cairn-mcp --all-targets -- -D warnings`
- `cargo test -p cairn-mcp`
- `cargo run -p cairn-idl --bin cairn-codegen -- --check` if generated code or
  schema references change

## 10. Completion Criteria

Issue #66 is complete when:

- Every known core MCP verb result is represented as a typed Cairn response
  envelope, including unwired verbs.
- MCP `search` success, malformed payloads, and capability rejections are
  parseable as generated `Response` values.
- `operation_id` is present on all MCP core verb committed/rejected/aborted
  envelopes.
- Existing prelude and graph tool behavior is unchanged.
- Tests prove MCP response envelopes preserve Cairn error codes, capability
  hints, and correlation IDs.
