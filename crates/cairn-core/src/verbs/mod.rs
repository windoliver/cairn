//! Verb implementations. See brief §8.0.
//!
//! Each verb is a pure function (or a small, pure-function tree) over a
//! typed input snapshot. Adapters live outside this module; bridging
//! adapters → verb inputs is the job of `cairn-cli`.

pub mod accept_share;
pub mod assemble_hot;
pub mod ingest;
pub mod lint;
pub mod propose_share;
pub mod retrieve;
pub mod revoke_share;
pub mod search;
pub mod summarize;
