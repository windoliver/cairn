# Tool-squash and raw trace compaction (issue #72)

- **Issue:** [#72] Implement tool-squash and raw trace compaction
- **Parent epic:** [#12] Implement ingestion pipeline and extract/filter/classify/scope stages
- **Brief sections:** §5.0 Turn journey, §5.2 Write path (Tool-squash table row)
- **Date:** 2026-04-27

## Goal

Compact verbose tool-call output before extraction, deterministically and
without I/O. The function is a pure pipeline stage in `cairn-core`. Storage
of compacted bytes and audit metadata is a separate concern handled by the
pipeline driver in a later issue.

## Non-goals

- Persistence. The function returns owned bytes plus metadata; the caller
  decides what to write to disk or store.
- `CaptureEvent` integration. The caller dispatches based on payload variant
  (Terminal + Hook-with-`tool_name` only); the function takes raw bytes and
  config.
- Structured-field preservation. Brief §5.2 mentions "preserve structured
  fields when schemas exist." There is no tool-schema registry in P0.
  Filed as a follow-up issue.
- Persidio redaction. Lives in the Filter stage (issue #93), not squash.

## Surface

New module: `crates/cairn-core/src/pipeline/squash.rs` (creates the
`pipeline/` parent module).

```rust
/// Pure transformation: bytes → compacted bytes + metadata.
/// Infallible. Same `(raw, cfg)` always produces byte-identical output.
pub fn squash(raw: &[u8], cfg: &SquashConfig) -> SquashOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SquashConfig {
    /// Total byte cap on `compacted_bytes` (excluding small marker slack).
    pub max_bytes: usize,
    /// Lines preserved at the head when truncating.
    pub head_lines: usize,
    /// Lines preserved at the tail when truncating.
    pub tail_lines: usize,
    /// Minimum run length of identical adjacent lines that triggers
    /// collapse to `<line> [×N]`. `0` and `1` disable consecutive dedup.
    pub dedup_min_run: usize,
    /// Per-line byte cap; longer lines truncated mid-line on a UTF-8
    /// boundary with a `[…N bytes truncated]` suffix.
    pub max_line_bytes: usize,
}

impl Default for SquashConfig {
    // max_bytes = 16 KiB (16_384)
    // head_lines = 100
    // tail_lines = 100
    // dedup_min_run = 2
    // max_line_bytes = 4 KiB (4_096)
}

#[derive(Debug, Clone)]
pub struct SquashOutput {
    pub compacted_bytes: Vec<u8>,
    /// SHA-256 of `raw` (recomputed inside `squash` to close the audit
    /// chain — caller cannot lie about input).
    pub raw_hash: PayloadHash,
    pub raw_byte_len: usize,
    /// SHA-256 of `compacted_bytes`.
    pub compacted_hash: PayloadHash,
    pub compacted_byte_len: usize,
    pub stats: SquashStats,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SquashStats {
    pub ansi_stripped: bool,
    pub dedup_runs_collapsed: usize,
    pub lines_dropped_truncate: usize,
    pub bytes_dropped_truncate: usize,
    pub long_lines_truncated: usize,
    pub truncated: bool,
}
```

`PayloadHash` already exists in `cairn-core/src/domain/capture.rs`; reuse it.

## Transformation order

Each stage is deterministic. Stages run in this exact order so that the
output is well-defined for any input:

1. **Lossy UTF-8 decode.** Invalid byte sequences become U+FFFD. Tool stdout
   is effectively always UTF-8; lossy decode means the function is
   infallible and downstream stages always see valid UTF-8.
2. **ANSI strip.** Drop:
   - CSI sequences: `ESC [` followed by parameter bytes `0x30-0x3f`,
     intermediate bytes `0x20-0x2f`, terminated by a final byte `0x40-0x7e`.
   - OSC sequences: `ESC ]` … terminated by `BEL` (`0x07`) or `ESC \`
     (`0x1b 0x5c`).
   - Bare control characters in `0x00-0x1f` and `0x7f` **except** `\n`
     (`0x0a`) and `\t` (`0x09`).

   Set `stats.ansi_stripped = true` if any byte was removed.
3. **Line split on `\n`.** Trailing newline preserved iff present in input.
4. **Per-line cap.** For each line longer than `max_line_bytes`, truncate
   on the nearest preceding UTF-8 codepoint boundary and append a
   `[…N bytes truncated]` marker (where N is dropped bytes, not chars).
   Increment `stats.long_lines_truncated`.
5. **Consecutive-run dedup.** Walk lines; whenever an identical line
   repeats `dedup_min_run` or more times in a row, collapse the run to a
   single line `<line> [×N]`. Increment `stats.dedup_runs_collapsed`
   per collapsed run.
6. **Head/tail line truncate.** If `total_lines > head_lines + tail_lines`,
   keep the first `head_lines` and last `tail_lines`; insert one marker
   line `[…skipped K lines, X bytes…]` between them, where K is dropped
   line count and X is dropped byte count. Update
   `lines_dropped_truncate`, `bytes_dropped_truncate`, `truncated`.
7. **Final byte cap.** Re-join lines with `\n`. If the resulting byte
   length exceeds `max_bytes`, truncate from the tail on a UTF-8 boundary
   and append `[…N bytes truncated]`. Update `bytes_dropped_truncate`,
   `truncated`.
8. **Hash.** SHA-256 over `raw` → `raw_hash`; SHA-256 over
   `compacted_bytes` → `compacted_hash`.

`max_bytes + marker_slack` is the strict ceiling on `compacted_byte_len`,
where `marker_slack` is the deterministic length of the
`[…N bytes truncated]` marker (≤ 32 bytes).

## Determinism contract

For all `(raw, cfg)`: `squash(raw, cfg)` returns byte-identical
`compacted_bytes`, identical `stats`, identical hashes. No `HashMap`
iteration, no `rand`, no time-of-day, no system locale. Implementation
must keep this invariant — break it and the replay engine (issue #97)
breaks.

## Errors

None. The function is infallible. Every input byte sequence maps to some
`SquashOutput`. UTF-8 invalidity is absorbed by the lossy decode in stage 1.

## Module placement

```
crates/cairn-core/src/
├── pipeline/
│   ├── mod.rs           // re-export squash
│   └── squash.rs        // this file
├── domain/
│   └── ...
└── lib.rs               // pub mod pipeline;
```

The `pipeline/` module is new. Future siblings (filter, classify, rank)
land alongside as later issues ship.

## Tests

### Unit (`pipeline/squash.rs`)

- **ANSI strip:** CSI SGR colors, OSC titles, mixed sequences, lone ESC,
  preserved `\n`/`\t`.
- **Per-line cap:** ASCII line under cap, ASCII line at cap, ASCII line
  over cap, multi-byte UTF-8 line truncated mid-codepoint (must not split
  a codepoint).
- **Consecutive dedup:** runs of 1 (no-op), 2, N; non-adjacent duplicates
  preserved; `dedup_min_run = 0` and `1` disable dedup.
- **Head/tail truncate:** total ≤ head+tail (no-op), exactly equal,
  greater (one marker line inserted, counts correct).
- **Final byte cap:** under, exact, over (suffix marker present, UTF-8
  boundary respected).
- **Hashes:** `raw_hash` matches `PayloadHash::sha256(raw)`;
  `compacted_hash` matches the bytes returned.
- **Empty input:** returns empty compacted_bytes, both hashes equal
  SHA-256 of empty, stats all zero.

### Integration (`crates/cairn-core/tests/squash_fixtures.rs`)

Golden-file tests: input fixture + `cfg` snapshot + expected
`compacted_bytes`. Use `insta` for the snapshot. Fixtures cover:

- Cargo build output (long, ANSI-rich, dedup-friendly progress lines).
- A noisy `npm test` log with stack traces.
- A short-lived `ls` (no truncation needed).
- Binary-with-junk-bytes input (UTF-8 lossy path).

### Property (`proptest`)

- **Determinism:** `squash(raw, cfg) == squash(raw, cfg)` for arbitrary
  bytes and config.
- **Byte ceiling:** `compacted_byte_len <= max_bytes + marker_slack`.
- **Idempotence of byte cap:** if input is already short enough, byte-cap
  stage adds no bytes.
- **Hash agreement:** `compacted_hash == PayloadHash::sha256(&compacted_bytes)`.

## Replay determinism

The fixture suite doubles as the replay-cassette baseline (verification
checkbox 2 in the issue). Once #97 (replay engine) lands, those fixtures
move under its harness; today they live as integration tests in
`cairn-core/tests/`.

## Latency budget

The issue's "budget limit tests" verification checkbox: not a numerical
budget at this layer. Brief §5.2 sets a global p95 < 50 ms for
Capture → Memory Store. Squash is one of several stages; budgeting
happens in the pipeline driver. We add a `criterion` micro-bench in this
PR (`crates/cairn-core/benches/squash.rs`) to baseline single-call cost
on a representative cargo-build fixture, but no hard ceiling here.

## Out of scope (follow-up issues to file)

1. **Structured-field preservation.** Add a tool-schema registry; teach
   squash to detect and preserve declared fields when a schema is
   attached. Brief §5.2 mentions this; no schema registry exists today.
2. **Pipeline-driver wiring.** Sensor host or extract-chain entry point
   reads `payload_ref`, calls `squash`, persists `SquashOutput` next to
   the source event. Likely lands with issue #74 or its driver issue.
3. **Persistence shape.** Where does `compacted_bytes` live (vault path
   vs. store column) and how is `SquashOutput` linked to the
   `CaptureEvent` row? Decided when the store schema work (#46 + #56)
   lands.

## Verification (matches issue checkboxes)

- [x] Tool-output fixture tests → `tests/squash_fixtures.rs` (insta snapshots).
- [x] Deterministic replay tests → property tests + golden fixtures.
- [x] Budget limit tests → `compacted_byte_len` ceiling + `criterion` bench.

## Invariants touched (CLAUDE.md §4)

- Invariant 4 (pure functions otherwise): adds `cairn-core::pipeline::squash`.
- Invariant 9 (privacy by construction): `SquashOutput` carries hashes
  and counts; never logs body bytes above `debug`. Tracing instrumentation
  on the function uses metadata-only fields.
