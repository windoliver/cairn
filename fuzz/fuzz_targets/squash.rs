#![no_main]

//! Fuzz target for the squash pipeline.
//!
//! Run with cargo-fuzz on nightly:
//!
//! ```sh
//! cargo install cargo-fuzz
//! cd fuzz
//! cargo +nightly fuzz run squash -- -max_total_time=600
//! ```
//!
//! The harness checks load-bearing invariants on every input:
//! - Output ALWAYS fits `cfg.max_bytes()`.
//! - Output is valid UTF-8.
//! - No `0x1B` (raw ESC) byte ever survives sanitization.
//! - `bytes_dropped_truncate` and `lines_dropped_truncate` are
//!   bounded relative to the raw input.
//!
//! Any panic, assert failure, or invariant violation is a finding.

use cairn_core::pipeline::squash_fuzz::{SquashConfig, fuzz_entrypoint};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let cfg = SquashConfig::default();
    let Some(out) = fuzz_entrypoint(data, &cfg) else {
        return;
    };

    // Output fits the budget.
    assert!(
        out.compacted_bytes.len() <= cfg.max_bytes(),
        "compacted={} > max_bytes={}",
        out.compacted_bytes.len(),
        cfg.max_bytes()
    );
    assert_eq!(out.compacted_bytes.len(), out.compacted_byte_len);

    // Output is valid UTF-8.
    assert!(std::str::from_utf8(&out.compacted_bytes).is_ok());

    // No raw ESC byte survives.
    assert!(
        !out.compacted_bytes.contains(&0x1B),
        "ESC leaked through sanitizer"
    );

    // Drop counters are bounded.
    assert!(out.stats.lines_dropped_truncate <= data.len());
    assert!(
        out.stats.bytes_dropped_truncate <= data.len() * 3 + cfg.max_bytes(),
        "bytes_dropped_truncate={} exceeds 3*raw + max_bytes",
        out.stats.bytes_dropped_truncate
    );
});
