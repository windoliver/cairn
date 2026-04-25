//! Cairn core — contract traits, domain types, and error enums.
//!
//! P0 scaffold. Domain types and verb behaviour land in follow-up issues
//! (#4 et al.). This crate has zero dependencies on any adapter crate.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod contract;
