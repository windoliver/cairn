//! Library surface for the `cairn` binary. Shared between `main.rs` and
//! integration tests in `crates/cairn-cli/tests/`.
//!
//! Note: this crate is not published — only `cairn-cli`'s own `main.rs`
//! and test targets consume it. `expect()` with documented reasons is
//! tolerated here per CLAUDE.md §6.2 (bins/tests).

pub mod config;
pub mod mcp;
pub mod plugins;
pub mod vault;
pub mod verbs;
