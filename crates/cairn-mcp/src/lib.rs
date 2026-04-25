//! Cairn MCP adapter — exposes the verb layer over MCP transports.
//!
//! P0 scaffold. Transport wiring lands in follow-up issues.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod generated;
