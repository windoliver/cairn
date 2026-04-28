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

## Caller contract — what `squash` is for, and what it isn't

`squash` is for **`Terminal` `CapturePayload` bytes only** in P0 — the
shell-command stdout/stderr stream where ANSI noise, repeated progress
lines, and raw byte volume actually hurt downstream extraction. It is a
**lossy** transformation: ANSI bytes, duplicate lines, and overflow are
discarded.

### Pipeline-driver dispatch table (P0 contract)

The pipeline driver — wired in a follow-up issue — MUST follow this
dispatch table for every `CaptureEvent` reaching the Capture →
Extract boundary:

| `CapturePayload` variant | P0 path                                |
|--------------------------|----------------------------------------|
| `Terminal`               | `squash(UnstructuredTextBytes, cfg)`   |
| `Hook` (any tool)        | bypass squash; raw bytes to Extract    |
| `Ide`                    | bypass squash; raw bytes to Extract    |
| `Clipboard`              | bypass squash; raw bytes to Extract    |
| `Voice` / `Screen` / `RecordingBatch` | bypass — different modality, never text-compacted |
| `Cli` / `Mcp` / `Proactive` | bypass — metadata-only events       |

**Only `Terminal` payloads reach `squash` in P0.** Every other variant
bypasses unconditionally: their raw bytes flow to Extract unchanged.

This rule is intentionally strict. The alternative (a heuristic JSON-
parse classifier on Hook payloads) was rejected because parse failures
under schema drift, partial writes, or producer/consumer version skew
would route ostensibly-structured payloads into a lossy transform —
exactly the high-cost data-loss class this stage is supposed to
prevent. A safer Hook path (tool-schema registry + allowlist of
known-plain-text tools) is a follow-up issue.

The cost of unconditional Hook/IDE/Clipboard bypass: Extract receives
oversized noisy payloads on those paths and spends extra LLM tokens
on them. P0 accepts that cost. Compaction for those paths lands when
the registry/allowlist work does.

**Not all Terminal payloads are safe to squash.** Commands like
`kubectl get -o json`, `docker inspect`, or `cmd | jq` emit JSON to
stdout; squashing them strips meaningful punctuation and structure.
The dispatch driver MUST classify Terminal captures by **execution
context**, not just variant:

- **Interactive TTY** — output flowed to a real terminal session, no
  pipe, no redirection, no `-o json`-style structured-output flag.
  Safe to squash.
- **Non-TTY / piped / structured-output flag** — output left for
  another process or rendered through a known structured-output
  pathway. **Bypass squash**, raw bytes to Extract.

The sensor adapter is the only component that has the metadata to
distinguish these (process FD type at capture time, presence of
`-o json` / `--format=json` / etc. on the command line). The
`UnstructuredTextBytes` constructor accepts a `TerminalContext`
parameter the sensor sets at capture time; only
`TerminalContext::InteractiveTty` admits the bytes into `squash`.
See "Surface" below.

### Type-level scoping (this PR)

`squash`'s input wrapper (`UnstructuredTextBytes`) has exactly **one**
public constructor in P0:

- `try_from_terminal_event(&CaptureEvent, raw)` — requires
  `payload` is `Terminal` and `sha256(raw) == event.payload_hash`.

`Hook`, `Ide`, `Clipboard`, and the rest cannot reach `squash`
because no constructor accepts them. Adding a constructor is a
deliberate, reviewable change that the schema/allowlist follow-up
will justify.

### What the type wrapper does and doesn't guarantee

- It **prevents accidental misrouting:** the pipeline driver cannot
  pair Hook/Ide/Clipboard bytes with `squash` by mistake, because the
  only constructor (`UnstructuredTextBytes::try_from_event`) checks the
  variant tag and the `payload_hash` binding.
- It **does NOT provide producer authentication.** A compromised or
  buggy sensor that tags structured JSON as `CapturePayload::Terminal`
  and computes a matching `payload_hash` will pass the gate. That is
  out of scope for this layer; producer authentication belongs to the
  signed-envelope layer (issue #51) and the per-sensor identity
  registry (issue #50). `squash` trusts that the upstream
  envelope/identity stack has already validated provenance, just like
  every other pure pipeline stage in `cairn-core`.

Calling the wrapper a "trust boundary" therefore overstates the
guarantee. It is a **scoping boundary**: it ensures one specific class
of accidental bug (a downstream caller passing the wrong byte slice to
the wrong variant) cannot happen by inspection. Defense against
malicious or compromised upstream sensors is the signed-envelope
layer's job, not this one.

## Non-goals

- Persistence. The function returns owned bytes plus metadata; the
  caller decides what to write to disk or store.
- `CaptureEvent` integration. The caller dispatches based on payload
  variant — **only `Terminal` in P0**. The function takes an
  `UnstructuredTextBytes` wrapper plus config.
- Hook / IDE / Clipboard / other source families. Bypass `squash`
  unconditionally in P0. Revisit when the tool-schema registry +
  per-tool unstructured-text allowlist follow-up lands.
- Schema-aware preservation **inside** `squash`. Schema-bearing payloads
  bypass `squash` entirely (see Caller contract above). Wiring the
  bypass into the pipeline driver is a follow-up issue once the
  tool-schema registry exists.
- Presidio redaction. Lives in the Filter stage (issue #93), not squash.

## Surface

New module: `crates/cairn-core/src/pipeline/squash.rs` (creates the
`pipeline/` parent module).

```rust
/// Newtype wrapper around terminal command stdout/stderr bytes,
/// bound to the `CaptureEvent` they came from.
///
/// One public constructor in P0:
///   - `try_from_terminal_event(event, raw)` — Terminal variant +
///     hash match.
///
/// The check forms a **scoping boundary**, not an authentication
/// one. It prevents a downstream caller from accidentally pairing
/// Hook/IDE/Clipboard/other bytes with `squash` by mistake, by
/// binding the supplied bytes to the Terminal variant the upstream
/// sensor declared at capture time. It does **not** defend against
/// a malicious or buggy sensor that mis-tags structured payloads as
/// `Terminal` — producer authentication is the signed-envelope
/// layer's job (issue #51) and the identity registry (issue #50).
/// See "Caller contract" above.
///
/// The byte slice field is private; users cannot mutate it after
/// construction. The constructor also computes and stores the SHA-256
/// of `raw`, which `squash` reuses as `SquashOutput::raw_hash` (no
/// double-hash on the hot path).
#[derive(Debug)]
pub struct UnstructuredTextBytes<'a> {
    bytes: &'a [u8],
    /// `sha256(bytes)`, also equal to `event.payload_hash` by
    /// constructor invariant. Used as `SquashOutput::raw_hash`.
    raw_hash: PayloadHash,
}

impl<'a> UnstructuredTextBytes<'a> {
    /// Terminal payload constructor. Verifies:
    ///   1. `event.payload` is `CapturePayload::Terminal`.
    ///   2. `sha256(raw) == event.payload_hash`.
    ///   3. `context == TerminalContext::InteractiveTty` —
    ///      non-TTY/piped Terminal captures are rejected because
    ///      their output is often structured (JSON, CSV) and lossy
    ///      compaction would corrupt it.
    /// Only public constructor in P0.
    pub fn try_from_terminal_event(
        event: &CaptureEvent,
        raw: &'a [u8],
        context: TerminalContext,
    ) -> Result<Self, UnstructuredBindError>;

    pub fn as_bytes(&self) -> &[u8];
    pub fn raw_hash(&self) -> &PayloadHash;
}

/// Sensor-supplied context describing how the Terminal payload was
/// produced. Set at capture time by the Terminal-sensor adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TerminalContext {
    /// Output flowed to a real interactive TTY (process stdout was a
    /// tty FD, no detected structured-output flag). Safe to squash.
    InteractiveTty,
    /// Output was piped, redirected to a file, or produced by a
    /// command invocation that requested structured output (e.g.,
    /// `-o json`). Must NOT be squashed; the dispatch driver
    /// bypasses for this context.
    NonInteractiveOrStructured,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UnstructuredBindError {
    #[error("expected CapturePayload::Terminal; got a different source family")]
    NotTerminalPayload,
    #[error("payload_hash mismatch: bytes do not match the captured payload's sha256")]
    HashMismatch,
    #[error(
        "Terminal capture was non-interactive or structured-output; \
         lossy compaction would corrupt machine-readable bytes — \
         dispatch driver must bypass squash for this context"
    )]
    StructuredContextRejected,
}

/// Pure transformation: unstructured text bytes → compacted bytes +
/// metadata. Infallible. Same `(raw, cfg)` always produces
/// byte-identical output.
///
/// `cfg` must be a normalized `SquashConfig` (constructed via
/// `SquashConfig::new` or `SquashConfig::default()`); the function
/// runtime-asserts the invariants below: `SquashConfig` invariants
/// are debug-only (`debug_assert!`, since `SquashConfig::new` already
/// enforces them); the marker-length bound is `assert!` (load-bearing
/// for the strict `max_bytes` ceiling — fires in release too). See
/// `MARKER_MAX_LEN` notes below. `raw.raw_hash()` is reused
/// as `SquashOutput::raw_hash` — no second SHA-256 pass.
pub fn squash(raw: UnstructuredTextBytes<'_>, cfg: &SquashConfig) -> SquashOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SquashConfig {
    max_bytes: usize,
    head_lines: usize,
    tail_lines: usize,
    dedup_min_run: usize,
    max_line_bytes: usize,
}

/// Maximum byte length of any truncation marker emitted by `squash`.
///
/// Markers have one of two forms:
///   - skip-line:    `[…skipped K lines, X bytes…]`
///   - tail/inline:  `[…N bytes truncated]`
///
/// The fixed text is at most ~40 ASCII bytes (the multibyte ellipsis
/// `…` is 3 bytes UTF-8 each, ×2 per marker). The numeric fields `K`,
/// `X`, `N` are `usize` decimal renderings; `usize` is at most 64-bit
/// on every supported target, so each renders to at most 20 ASCII
/// digits. Worst case: 40 (text) + 3 × 20 (digits) = 100 bytes. We
/// round up for slack:
pub const MARKER_MAX_LEN: usize = 128;

/// Worst-case stage-6 layout overhead beyond `max_line_bytes` and
/// `MARKER_MAX_LEN`: separator newlines between (head?, marker, tail)
/// plus a trailing newline. Conservative slack:
pub const LAYOUT_OVERHEAD: usize = 4;

pub const MIN_MAX_BYTES: usize = 4 * MARKER_MAX_LEN;   // 512
pub const MIN_MAX_LINE_BYTES: usize = MARKER_MAX_LEN;  // 128
pub const MIN_TAIL_LINES: usize = 1;                    // tail-preservation invariant
// head_lines may be 0; dedup_min_run < 2 disables dedup.

// Implementation requirement (load-bearing): the impl `assert!`s
// every emitted marker's byte length is ≤ MARKER_MAX_LEN at runtime
// in BOTH debug and release builds. The assertion is the strict-
// `max_bytes` ceiling's enforcement mechanism — without it, a
// marker-template change in a future PR could silently overflow
// `max_bytes` in release builds. A panic here is the right
// behavior: it's an internal invariant violation, not a recoverable
// input-validation failure. If the assertion ever fires, the bug is
// in the impl (marker rendered larger than MARKER_MAX_LEN) and the
// constant must be re-derived.

impl SquashConfig {
    /// Validates and constructs a `SquashConfig`. Returns
    /// `SquashConfigError` for any field below its minimum, or for
    /// the cross-field check
    /// `max_line_bytes + MARKER_MAX_LEN + LAYOUT_OVERHEAD ≤
    /// max_bytes`. The cross-field check accounts for a single
    /// per-line-capped tail line plus the skip-marker line plus
    /// separator newlines; without it the stage-6 layout could not
    /// fit a marker + final preserved line within `max_bytes` and
    /// would have to either violate the strict ceiling or drop the
    /// supposedly-preserved last line.
    pub fn new(
        max_bytes: usize,
        head_lines: usize,
        tail_lines: usize,
        dedup_min_run: usize,
        max_line_bytes: usize,
    ) -> Result<Self, SquashConfigError>;

    // Field accessors (read-only): max_bytes, head_lines, tail_lines,
    // dedup_min_run, max_line_bytes.
}

impl Default for SquashConfig {
    // max_bytes      = 16 KiB (16_384)
    // head_lines     = 100
    // tail_lines     = 100
    // dedup_min_run  = 2
    // max_line_bytes = 4 KiB (4_096)
    // All ≥ their MIN_* constants; `Default` cannot fail.
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SquashConfigError {
    #[error("max_bytes must be ≥ {min}, got {value}")]
    MaxBytesTooSmall { value: usize, min: usize },
    #[error("max_line_bytes must be ≥ {min}, got {value}")]
    MaxLineBytesTooSmall { value: usize, min: usize },
    #[error("tail_lines must be ≥ {min}, got {value}")]
    TailLinesTooSmall { value: usize, min: usize },
    #[error(
        "max_line_bytes ({line}) + MARKER_MAX_LEN ({marker}) + \
         LAYOUT_OVERHEAD ({overhead}) must be ≤ max_bytes ({total}); \
         the stage-6 layout could not fit a marker + final preserved \
         line within the budget"
    )]
    LineCapExceedsLayoutBudget {
        line: usize,
        marker: usize,
        overhead: usize,
        total: usize,
    },
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
output is well-defined for any input.

The pipeline carries **one load-bearing invariant — last-content-line
preservation:** for every normalized `SquashConfig` and every input
with at least one non-empty content line, the **last content line**
of the input (per stage 3's content-line definition, capped to
`max_line_bytes` per stage 4) appears as a suffix of
`compacted_bytes` — preceding the optional trailing newline. This is
the single guarantee callers can rely on universally. Stack traces,
final error messages, and command-final state always sit on a
content line, never in a trailing empty segment after `\n`.

Beyond that one invariant there is a **best-effort tail block**: when
the configured `tail_lines * (max_line_bytes + 1)` plus marker plus
head plus newlines fits in `max_bytes`, the planner emits the full
configured tail block verbatim (post-per-line cap). When the budget
is too tight for that, the planner drops entire leading lines from
the tail block — never bytes mid-line — until the remaining suffix
plus marker fits. By construction of `MIN_MAX_BYTES ≥
MIN_MAX_LINE_BYTES + MARKER_MAX_LEN + LAYOUT_OVERHEAD`, at least one
line of tail always fits, so the last-line invariant holds.

In neither case does any size-enforcement step touch tail bytes
mid-line: head and the marker block are sacrificed first; only when
both are gone does the tail block lose entire leading lines.

1. **Lossy UTF-8 decode.** Invalid byte sequences become U+FFFD. Tool
   stdout is effectively always UTF-8; lossy decode means the function
   is infallible and downstream stages always see valid UTF-8.
2. **ANSI strip.** Drop:
   - CSI sequences: `ESC [` followed by parameter bytes `0x30-0x3f`,
     intermediate bytes `0x20-0x2f`, terminated by a final byte
     `0x40-0x7e`.
   - OSC sequences: `ESC ]` … terminated by `BEL` (`0x07`) or `ESC \`
     (`0x1b 0x5c`).
   - Bare control characters in `0x00-0x1f` and `0x7f` **except** `\n`
     (`0x0a`) and `\t` (`0x09`).

   Set `stats.ansi_stripped = true` if any byte was removed.
3. **Line split on `\n`.** Define **lines** as the byte slices
   between consecutive `\n`s, including any empty segments produced by
   adjacent newlines (interior blank lines are full lines, byte-for-
   byte preserved through the no-truncation path). The single
   exception is a trailing empty segment produced when input ends with
   `\n`: it is **not** a line — it's recorded as a separate
   "trailing newline" flag and re-emitted at the end of the output if
   set. Dedup, layout, and the last-line invariant operate on the
   line list. The "last content line" invariant uses **last non-empty
   line in the line list**.
4. **Per-line cap.** `max_line_bytes` is the **emitted** line budget
   inclusive of any inline truncation marker. For each line whose
   byte length exceeds `max_line_bytes`, truncate at the nearest
   preceding UTF-8 codepoint boundary so that the prefix plus a
   `[…N bytes truncated]` marker (N = dropped source bytes) is **≤
   `max_line_bytes`**. The emitted line is therefore always
   ≤ `max_line_bytes`, regardless of whether truncation occurred.
   Increment `stats.long_lines_truncated`. Applies uniformly to every
   line (head, middle, tail) before the size-enforcement stages run.
5. **Consecutive-run dedup.** Walk lines; whenever an identical line
   repeats `dedup_min_run` or more times in a row, collapse the run to a
   single line `<line> [×N]`. Increment `stats.dedup_runs_collapsed`
   per collapsed run.
6. **Plan size-enforced layout (head / marker / tail).** Compute the
   joined byte length. If it fits in `max_bytes`, emit the lines as-is
   (no marker). Otherwise:

   a. Reserve the **tail block** first: take the last
      `min(tail_lines, total)` lines verbatim. Compute its byte length
      `T`.
   b. Reserve a skip-marker line slot of size `MARKER_MAX_LEN`. The
      *actual* marker rendered in step 6d may be shorter (smaller `K`,
      `X`); reserving the upper bound up front means the rendered
      marker always fits the reserved slot, so byte-budget math is
      single-pass and the strict `max_bytes` ceiling holds without
      reflow.
   c. Compute the **head budget** =
      `max_bytes − T − MARKER_MAX_LEN − newline_overhead`.
      - If the head budget is **negative** (the tail alone exceeds
        `max_bytes`), drop entire leading lines from the front of the
        tail block (never mid-line bytes) until the remaining lines
        fit. Set head to empty. The single marker line is still emitted
        to record what was dropped. The tail-preservation invariant
        narrows from "all `tail_lines`" to "as many trailing lines as
        fit"; the LAST line is always preserved by construction
        (per-line cap from stage 4 guarantees a single line ≤
        `max_line_bytes ≤ max_bytes`, and `MIN_MAX_BYTES` is large
        enough to hold it plus the marker).
      - If the head budget is **non-negative**, take the first
        `head_lines` lines, then drop trailing head lines (closest to
        the marker) one at a time until the head fits in the budget.
   d. Render the marker text with the actual `K` (dropped line count)
      and `X` (dropped byte count). The rendered marker length is by
      construction `≤ MARKER_MAX_LEN` (`assert!`-checked in both debug and release builds); the
      reserved slot is not pad-resized — the marker is emitted at its
      natural length and the overall layout still fits in `max_bytes`
      because the head budget reserved the upper bound. Update
      `stats.lines_dropped_truncate`, `stats.bytes_dropped_truncate`,
      `stats.truncated`.
7. **Hash.** Reuse `raw.raw_hash()` (computed once at
   `UnstructuredTextBytes::try_from_event` time and cryptographically
   bound to `event.payload_hash`) as `SquashOutput::raw_hash`. SHA-256
   over `compacted_bytes` → `compacted_hash`.

`max_bytes` is the strict ceiling on `compacted_byte_len`. When the
marker is present, the planner reserves `MARKER_MAX_LEN` bytes for it
and the rendered marker is by construction ≤ that bound, so head +
rendered marker + tail ≤ `max_bytes`. When no marker is needed (input
fits as-is), the output is the joined lines, which by construction ≤
`max_bytes`. No reflow / second-pass adjustment is ever needed.

The earlier two-stage "head/tail truncate then byte-cap-from-tail"
design was rejected: the final byte cap could chop off the very tail
the head/tail step was supposed to preserve. Folding both into a single
budgeted layout in stage 6 makes tail preservation an invariant rather
than a happy-path guarantee.

## Determinism contract

For all `(raw, cfg)`: `squash(raw, cfg)` returns byte-identical
`compacted_bytes`, identical `stats`, identical hashes. No `HashMap`
iteration, no `rand`, no time-of-day, no system locale. Implementation
must keep this invariant — break it and the replay engine (issue #97)
breaks.

## Errors

`squash` itself is infallible — every input byte sequence maps to some
`SquashOutput`. UTF-8 invalidity is absorbed by the lossy decode in
stage 1.

`SquashConfig::new` returns `SquashConfigError` if any field is below
its `MIN_*` constant. Invalid `SquashConfig` instances cannot exist: the
fields are private, `Default` is in-bounds by construction, and `new`
is the only other constructor. `squash` therefore never sees a
zero-or-too-small `max_bytes` / `max_line_bytes` / `tail_lines` and
needs no in-function validation. (`debug_assert!` covers the private
invariants in dev builds.)

## Dependencies

No new workspace dependencies. `sha2` is already used by `PayloadHash`
in `cairn-core/src/domain/capture.rs`; `thiserror` is already in
workspace deps.

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

- **`SquashConfig::new` validation:** below-min `max_bytes`,
  `max_line_bytes`, `tail_lines` each return their distinct error
  variant; cross-field violation (`max_line_bytes + MARKER_MAX_LEN >
  max_bytes`) returns `LineCapExceedsLayoutBudget`; valid inputs
  round-trip; `Default` is always valid; `MIN_MAX_BYTES` and
  `MIN_MAX_LINE_BYTES` are derived from `MARKER_MAX_LEN` (compile-time
  `const _: () = assert!(...)`).
- **Marker length bound:** `assert!` (release-mode) runtime checks (and
  one explicit unit test that drives stage 6 with the largest possible
  `K`/`X` values for `usize::MAX` to confirm the rendered marker fits
  in `MARKER_MAX_LEN`).
- **`UnstructuredTextBytes::try_from_terminal_event` gating:**
  - non-`Terminal` `CapturePayload` variants → `NotTerminalPayload`.
  - `Terminal` variant + bytes whose sha256 does not match
    `event.payload_hash` → `HashMismatch`.
  - `Terminal` variant + matching bytes +
    `TerminalContext::NonInteractiveOrStructured` →
    `StructuredContextRejected`.
  - `Terminal` variant + matching bytes +
    `TerminalContext::InteractiveTty` → `Ok`; `as_bytes()` returns
    the constructor's raw slice unchanged; `raw_hash()` matches both
    `event.payload_hash` and `PayloadHash::sha256(raw)`.
- **ANSI strip:** CSI SGR colors, OSC titles, mixed sequences, lone
  ESC, preserved `\n`/`\t`.
- **Per-line cap:** ASCII line under cap, ASCII line at cap, ASCII line
  over cap, multi-byte UTF-8 line truncated mid-codepoint (must not
  split a codepoint).
- **Consecutive dedup:** runs of 1 (no-op), 2, N; non-adjacent
  duplicates preserved; `dedup_min_run = 0` and `1` disable dedup.
- **Head/marker/tail layout (stage 6):**
  - input fits in `max_bytes` → no marker emitted, output equals joined
    lines verbatim.
  - input over `max_bytes` with comfortable budget → head, marker, tail
    all present; full `tail_lines` preserved verbatim.
  - input over `max_bytes` with **tight** budget → head shrinks line by
    line from the marker side until the layout fits; tail block is
    untouched.
  - input where the **tail alone** exceeds `max_bytes` → head empty,
    leading tail lines drop from the front, last line of input always
    preserved verbatim, marker present and accurate.
  - byte ceiling: `compacted_byte_len ≤ max_bytes` whenever the marker
    is present.
- **Last-content-line invariant test:** synthetic inputs whose last
  content line carries a sentinel — both with and without a trailing
  `\n` — return `compacted_bytes` whose last content line begins with
  that sentinel under every config in the normalized parameter grid.
  An input of pure trailing whitespace (`b"\n\n\n"`) returns empty
  `compacted_bytes` plus the trailing-newline flag.
- **Best-effort tail-block test (Tier-A-equivalent):** with the
  default config and inputs of various sizes, all `tail_lines`
  trailing lines of input appear verbatim (post-per-line-cap) in
  `compacted_bytes`.
- **Hashes:** `raw_hash` matches `PayloadHash::sha256(raw)`;
  `compacted_hash` matches the bytes returned.
- **Empty input:** returns empty `compacted_bytes`, both hashes equal
  SHA-256 of empty, stats all zero.

### Integration (`crates/cairn-core/tests/squash_fixtures.rs`)

Golden-file tests: input fixture + `cfg` snapshot + expected
`compacted_bytes`. Use `insta` for the snapshot. Fixtures cover:

- Cargo build output (long, ANSI-rich, dedup-friendly progress lines).
- A noisy `npm test` log with stack traces.
- A short-lived `ls` (no truncation needed).
- Binary-with-junk-bytes input (UTF-8 lossy path).

### Property (`proptest`)

Configs come from a strategy that produces only **normalized**
(`SquashConfig::new` Ok) configs. Invariants checked over arbitrary
bytes:

- **Determinism:** `squash(raw, cfg) == squash(raw, cfg)`.
- **Byte ceiling:** when `stats.truncated`, `compacted_byte_len ≤ cfg.max_bytes`.
  When not truncated, `compacted_byte_len ≤ cfg.max_bytes` still holds.
- **Last-content-line preservation:** for every input with at least
  one non-empty content line, the last content line (per stage 3,
  capped per stage 4) appears in `compacted_bytes` immediately before
  the optional trailing newline. Newline-terminated logs do not
  satisfy this invariant by emitting only an empty trailing segment.
- **No-truncation passthrough:** if (post-stage-1 lossy-decode +
  stage-2 ANSI-strip) output already fits in `max_bytes` and contains
  no over-cap lines (stage 4 no-op) and has no consecutive runs of
  duplicate lines (stage 5 no-op), `compacted_bytes` equals the
  ANSI-stripped, lossy-decoded input byte-for-byte. Interior blank
  lines are preserved verbatim. Trailing newline preserved iff
  present.
- **Hash agreement:** `compacted_hash == PayloadHash::sha256(&compacted_bytes)`.
- **Config validation:** any tuple with a field below its `MIN_*`
  constant produces `SquashConfigError` from `SquashConfig::new`.

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

1. **Tool-schema registry + dispatch bypass.** Caller needs a registry
   to know "tool X declares schema S; bypass squash, hand raw bytes to
   Extract." Brief §5.2 implies the registry; doesn't define it. New
   issue: design the registry and wire the bypass into the pipeline
   driver.
2. **Pipeline-driver wiring.** Sensor host or extract-chain entry
   point reads `payload_ref`, dispatches (squash for unstructured,
   bypass for schema-bearing), persists `SquashOutput` next to the
   source event. Likely lands with issue #74 or its driver issue.
3. **Persistence shape.** Where does `compacted_bytes` live (vault
   path vs. store column) and how is `SquashOutput` linked to the
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
