//! Deterministic formatting helpers shared by every emitter.
//!
//! Provides canonical JSON serialisation (recursive key sort, two-space
//! indent, trailing newline) and a small Rust source-builder that always
//! ends files with exactly one trailing newline.
