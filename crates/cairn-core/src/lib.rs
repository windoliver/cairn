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
pub mod domain;
pub mod generated;
// `pipeline` is crate-internal until the dispatch driver (#217) and
// persisted `TerminalContext` (#218) land. The squash gate's eligibility
// must be derivable from persisted capture data, not a caller-supplied
// side input — keep the surface inside the crate to prevent misuse.
pub(crate) mod pipeline;
pub mod verifier;
