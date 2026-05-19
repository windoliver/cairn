//! Library surface for the `cairn` binary. Shared between `main.rs` and
//! integration tests in `crates/cairn-cli/tests/`.
//!
//! Note: this crate is not published — only `cairn-cli`'s own `main.rs`
//! and test targets consume it. `expect()` with documented reasons is
//! tolerated here per CLAUDE.md §6.2 (bins/tests).

pub mod command;
pub mod config;
pub mod coord;
pub mod docgen;
pub mod doctor;
pub(crate) mod generated;
pub mod hooks;
pub mod identity;
pub mod llm;
pub mod mcp;
pub mod metrics;
pub mod nexus;
pub mod nexus_cli;
pub mod plugins;
pub mod render;
pub mod repair;
pub mod sensor_gate;
pub mod session_source;
pub mod setup;
pub mod skill;
pub mod vault;
pub mod verbs;
