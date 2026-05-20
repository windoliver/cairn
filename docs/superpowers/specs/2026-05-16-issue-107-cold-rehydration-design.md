# Issue 107 Cold Rehydration Design

## Context

Issue #107 covers the v0.2 cold rehydration retrieval path. The relevant brief anchors are section 7 Hot Memory, section 8.0.c `retrieve --session --rehydrate`, section 10.0 ExpirationWorkflow cold lifecycle, and section 15/18.c US6 latency and replay gates.

The current CLI already accepts `retrieve --session <id> --rehydrate`, but the handler ignores the generated `rehydrate` field. Session retrieval already applies scope checks, visibility checks, include-based redaction, cursoring by turn, and a deterministic read budget trace.

## Scope

This slice makes rehydration explicit and observable without introducing the future Nexus cold-bundle store. It preserves the fast path for normal session retrieval and creates a policy-traced hook point that later cold storage can replace with real unpack/restore behavior.

## Design

Thread the generated `RetrieveArgs::Session.rehydrate` flag into the CLI session request. When false or omitted, keep the existing session retrieval behavior and policy trace unchanged.

When true, session retrieval still uses the authorized SQLite session read path, applies the same limit/cursor/read-budget trimming, and emits an additional body-free `read.rehydrate` policy trace. The trace records that rehydration was explicitly requested, the current source tier (`hot_or_warm` in this slice), elapsed milliseconds, the read budget, in/out item counts, in/out turn counts, and whether trimming occurred.

This trace is evaluation-facing metadata only. It must not include record bodies, snippets, raw secrets, or unredacted tool/reasoning content. Existing include flags remain the only way to request tool-call or reasoning bodies.

## Tests

Add CLI integration tests in `crates/cairn-cli/tests/issue_61_signed_verbs.rs`:

- `retrieve_session_rehydrate_adds_body_free_trace` captures a session, calls `retrieve --session --rehydrate --json`, and verifies a `read.rehydrate` pass trace with deterministic body-free details.
- `retrieve_session_default_path_omits_rehydrate_trace` captures the same shape without `--rehydrate` and verifies the fast path does not emit `read.rehydrate`.

Existing retrieve budget and redaction tests continue to cover scoped, budgeted, body-free behavior.

## Deferred

Actual cold snapshot unpacking, warm-tier restore, Nexus bundle reads, object storage, and full US6 replay stories remain deferred until the cold storage substrate from the parent epic is available.
