//! Cairn core — contract traits, domain types, and error enums.
//!
//! P0 scaffold. Verb behaviour, domain types, and error enums land in
//! follow-up issues (#4, #34, #35). Core depends on no adapter crate.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL and
//! must not be hand-edited — see `docs/dev/codegen.md`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod contract;
pub mod coord;
pub mod domain;
pub mod error;
pub mod generated;
pub mod mcp_auth;
pub mod pipeline;
pub mod policy_trace;
pub mod replay;
pub mod search;
pub mod status;
pub mod time;
pub mod verbs;
pub mod verifier;
pub mod wal;
